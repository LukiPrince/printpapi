// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Provider-agnostic billing: the plan catalogue and the signed webhook event — the port of
//! `app/billing.py`. Pure: no IO, no DB, no clock.
//!
//! Deliberately not a payment integration: a link per plan, and one signed POST saying "this org is
//! on that plan now". The plan's `jobs` is written into `orgs.job_quota`, which the job path
//! already enforces — one number, one guard.
use crate::printnode::{py_int, py_trim};
use crate::pyfmt::{py_str, repr, truthy};
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use serde_json::Value;
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Statuses that mean "this subscription is paid for"; anything else downgrades to the default.
pub const ACTIVE: &[&str] = &["active", "trialing", "paid", "completed", "succeeded"];

/// A catalogue entry. Field order = the JSON order of `GET /plans`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Plan {
    pub id: String,
    pub name: String,
    /// Monthly job quota; None = unlimited.
    pub jobs: Option<i64>,
    /// Free-form display value, passed through as given.
    pub price: Value,
    /// The provider's payment link (`{org}` substituted per caller), passed through as given.
    pub checkout_url: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BillingError(pub String);

impl std::fmt::Display for BillingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BillingError {}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub org_id: Option<i64>,
    pub email: String,
    pub plan: Plan,
    pub status: String,
}

/// Parse PRINTAPI_PLANS (the JSON text). Empty/blank = Ok(vec![]) (billing off).
///
/// A plan is `{"id", "name"?, "jobs"?, "price"?, "checkout_url"?}`; `jobs` null = unlimited. The
/// first plan is the default, what an org lands on when a subscription ends.
pub fn load_plans(raw: &str) -> Result<Vec<Plan>, BillingError> {
    let raw = py_trim(raw);
    if raw.is_empty() {
        return Ok(vec![]);
    }
    let data: Value = crate::server::json_from_bytes(raw.as_bytes())
        .map_err(|e| BillingError(format!("plan catalogue is not valid JSON: {e}")))?;
    let items = match &data {
        Value::Array(a) if !a.is_empty() => a,
        _ => {
            return Err(BillingError(
                "plan catalogue must be a non-empty JSON array".into(),
            ));
        }
    };
    let mut plans: Vec<Plan> = Vec::with_capacity(items.len());
    for item in items {
        let Value::Object(obj) = item else {
            return Err(BillingError("every plan must be a JSON object".into()));
        };
        let field = |k: &str| obj.get(k).unwrap_or(&Value::Null);
        let id_v = field("id");
        let pid = if truthy(id_v) {
            py_str(id_v)
        } else {
            String::new()
        };
        let pid = py_trim(&pid).to_string();
        if pid.is_empty() {
            return Err(BillingError("every plan needs an id".into()));
        }
        if plans.iter().any(|p| p.id == pid) {
            return Err(BillingError(format!("duplicate plan id: {pid}")));
        }
        // ponytail: jobs beyond i64 are rejected (Python accepted any bigint); no quota is that big.
        let jobs = match field("jobs") {
            Value::Null => None,
            Value::Number(n) => match n.as_i64() {
                Some(j) if j >= 0 => Some(j),
                _ => return Err(jobs_error(&pid)),
            },
            _ => return Err(jobs_error(&pid)),
        };
        let name_v = field("name");
        let name = if truthy(name_v) {
            py_str(name_v)
        } else {
            pid.clone()
        };
        plans.push(Plan {
            id: pid,
            name,
            jobs,
            price: field("price").clone(),
            checkout_url: field("checkout_url").clone(),
        });
    }
    Ok(plans)
}

fn jobs_error(pid: &str) -> BillingError {
    BillingError(format!(
        "plan {pid}: jobs must be a non-negative integer or null"
    ))
}

/// `str(plan_id or "").strip()` matched against the ids.
pub fn find<'a>(plans: &'a [Plan], plan_id: &Value) -> Option<&'a Plan> {
    let pid = if truthy(plan_id) {
        py_str(plan_id)
    } else {
        String::new()
    };
    let pid = py_trim(&pid);
    plans.iter().find(|p| p.id == pid)
}

pub fn default_plan(plans: &[Plan]) -> Option<&Plan> {
    plans.first()
}

/// The plan's checkout_url with `{org}` filled in, so the provider echoes the org id back in its
/// webhook (unchanged when not a string or org is None).
pub fn checkout_url(plan: &Plan, org_id: Option<i64>) -> Value {
    match (&plan.checkout_url, org_id) {
        (Value::String(url), Some(org)) if !url.is_empty() => {
            Value::String(url.replace("{org}", &org.to_string()))
        }
        (url, _) => url.clone(),
    }
}

/// Constant-time hex sha256 HMAC check over the raw body, optional `sha256=` prefix.
///
/// ponytail: one shared secret, no timestamp, so a captured event can be replayed. Replaying only
/// re-applies the same plan, which is idempotent; add a signed timestamp window if a provider ever
/// sends events whose order matters.
pub fn verify(secret: &str, raw_body: &[u8], header: &str) -> bool {
    if secret.is_empty() || header.is_empty() {
        return false;
    }
    let got = match header.as_bytes().get(..7) {
        Some(p) if p.eq_ignore_ascii_case(b"sha256=") => &header[7..],
        _ => header,
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(raw_body);
    let want: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let got = py_trim(got).to_lowercase();
    got.as_bytes().ct_eq(want.as_bytes()).into()
}

/// A provider event -> the org (by id or owner e-mail), plan and status for the server to apply.
///
/// `status` defaults to active. A non-active status ignores the named plan and downgrades to the
/// default one: a cancellation carries the old plan id, and honouring it would keep the quota the
/// tenant just stopped paying for.
pub fn parse_event(body: &Value, plans: &[Plan]) -> Result<Event, BillingError> {
    let Value::Object(obj) = body else {
        return Err(BillingError("event must be a JSON object".into()));
    };
    let field = |k: &str| obj.get(k).unwrap_or(&Value::Null);
    let org_id = match field("org_id") {
        Value::Null => None,
        // providers echo custom data back as strings
        v => Some(
            py_int(&py_str(v))
                .and_then(|n| i64::try_from(n).ok())
                .ok_or_else(|| BillingError("org_id must be an integer".into()))?,
        ),
    };
    // ponytail: Python crashed (AttributeError on .strip()) for a truthy non-string e-mail; it is
    // refused with a 400 here instead of being read as text.
    let email = match field("email") {
        Value::String(s) => py_trim(s).to_lowercase(),
        v if truthy(v) => return Err(BillingError("email must be a string".into())),
        _ => String::new(),
    };
    if org_id.is_none() && email.is_empty() {
        return Err(BillingError("org_id or email required".into()));
    }
    let status_v = field("status");
    let status = if truthy(status_v) {
        py_str(status_v)
    } else {
        "active".to_string()
    };
    let status = py_trim(&status).to_lowercase();
    let plan = if ACTIVE.contains(&status.as_str()) {
        find(plans, field("plan")).ok_or_else(|| {
            let have: Vec<&str> = plans.iter().map(|p| p.id.as_str()).collect();
            BillingError(format!(
                "unknown plan: {} (have: {})",
                repr(field("plan")),
                have.join(", ")
            ))
        })?
    } else {
        // ponytail: Python returned plan=None for an empty catalogue; the server answers 503
        // before parsing then, so here it is an error rather than an Option in the signature.
        default_plan(plans).ok_or_else(|| BillingError("no plans configured".into()))?
    };
    Ok(Event {
        org_id,
        email,
        plan: plan.clone(),
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SECRET: &str = "whsec_test";

    fn plans_json() -> String {
        json!([
            {"id": "free", "name": "Free", "jobs": 50, "price": "0 EUR"},
            {"id": "pro", "name": "Pro", "jobs": 5000, "price": "9 EUR/mo",
             "checkout_url": "https://pay.example/pro?client_reference_id={org}"},
            {"id": "unlimited", "name": "Unlimited", "jobs": null, "price": "29 EUR/mo"},
        ])
        .to_string()
    }

    fn sign(secret: &str, raw: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(raw);
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn err(r: Result<impl std::fmt::Debug, BillingError>) -> String {
        r.unwrap_err().0
    }

    #[test]
    fn plans_parse_with_defaults_and_reject_junk() {
        let plans = load_plans(&plans_json()).unwrap();
        let ids: Vec<_> = plans.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["free", "pro", "unlimited"]);
        assert_eq!(plans[0].jobs, Some(50));
        assert_eq!(plans[2].jobs, None); // null = unlimited
        assert_eq!(default_plan(&plans).unwrap().id, "free"); // first = what a new org gets
        assert_eq!(find(&plans, &json!("pro")).unwrap().name, "Pro");
        assert!(find(&plans, &json!("gold")).is_none());
        assert_eq!(load_plans("").unwrap(), vec![]);
        assert_eq!(load_plans("  \n").unwrap(), vec![]);
        for bad in [
            "{",
            "[]",
            r#"[{"name": "no id"}]"#,
            r#"[{"id": "a"}, {"id": "a"}]"#,
            r#"[{"id": "a", "jobs": -1}]"#,
            r#"[{"id": "a", "jobs": "many"}]"#,
            r#""nope""#,
        ] {
            assert!(load_plans(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn plan_catalogue_error_texts_and_defaults_match_python() {
        assert!(err(load_plans("{")).starts_with("plan catalogue is not valid JSON: "));
        assert_eq!(
            err(load_plans("[]")),
            "plan catalogue must be a non-empty JSON array"
        );
        assert_eq!(
            err(load_plans(r#""nope""#)),
            "plan catalogue must be a non-empty JSON array"
        );
        assert_eq!(err(load_plans("[1]")), "every plan must be a JSON object");
        assert_eq!(err(load_plans(r#"[{"id": 0}]"#)), "every plan needs an id");
        assert_eq!(
            err(load_plans(r#"[{"id": "  "}]"#)),
            "every plan needs an id"
        );
        assert_eq!(
            err(load_plans(r#"[{"id":"a"},{"id":" a "}]"#)),
            "duplicate plan id: a"
        );
        let jobs = "plan a: jobs must be a non-negative integer or null";
        for bad in [
            "-1",
            r#""many""#,
            "true",
            "5.0",
            "1e2",
            "18446744073709551615",
        ] {
            assert_eq!(
                err(load_plans(&format!(r#"[{{"id":"a","jobs":{bad}}}]"#))),
                jobs,
                "{bad}"
            );
        }
        // an id of 5 becomes "5", a falsy name falls back to the id, price/checkout pass through
        let p = load_plans(r#"[{"id": 5, "name": 0, "jobs": 3, "price": 1.5}]"#).unwrap();
        assert_eq!(
            p,
            vec![Plan {
                id: "5".into(),
                name: "5".into(),
                jobs: Some(3),
                price: json!(1.5),
                checkout_url: Value::Null
            }]
        );
        let p = load_plans(r#"[{"id":" a ","name":"","price":null}]"#).unwrap();
        assert_eq!(
            (p[0].id.as_str(), p[0].name.as_str(), p[0].jobs),
            ("a", "a", None)
        );
        // JSON order of GET /plans
        let v = serde_json::to_value(&p[0]).unwrap();
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["id", "name", "jobs", "price", "checkout_url"]);
    }

    #[test]
    fn find_stringifies_and_strips_the_plan_id() {
        let plans = load_plans(r#"[{"id":"a"},{"id":"7"}]"#).unwrap();
        assert_eq!(find(&plans, &json!(" a ")).unwrap().id, "a");
        assert_eq!(find(&plans, &json!(7)).unwrap().id, "7");
        assert!(find(&plans, &json!(null)).is_none());
        assert!(find(&plans, &json!(0)).is_none());
    }

    #[test]
    fn the_checkout_link_carries_the_org_so_the_webhook_can_name_it() {
        let plans = load_plans(&plans_json()).unwrap();
        let pro = find(&plans, &json!("pro")).unwrap();
        assert_eq!(
            checkout_url(pro, Some(7)),
            json!("https://pay.example/pro?client_reference_id=7")
        );
        // root has no org to fill in
        assert!(checkout_url(pro, None).as_str().unwrap().ends_with("{org}"));
        let mut none = pro.clone();
        none.checkout_url = Value::Null;
        assert_eq!(checkout_url(&none, Some(7)), Value::Null);
        none.checkout_url = json!(5);
        assert_eq!(checkout_url(&none, Some(7)), json!(5));
    }

    #[test]
    fn a_signature_only_verifies_over_the_exact_body() {
        let raw = br#"{"org_id": 1, "plan": "pro"}"#;
        let sig = sign(SECRET, raw);
        assert!(verify(SECRET, raw, &sig));
        // providers differ on both
        assert!(verify(
            SECRET,
            raw,
            &format!("sha256={}", sig.to_uppercase())
        ));
        assert!(verify(SECRET, raw, &format!("SHA256={sig}")));
        assert!(verify(SECRET, raw, &format!("  {sig}\n")));
        assert!(!verify(
            SECRET,
            b"{\"org_id\": 1, \"plan\": \"pro\"} ",
            &sig
        ));
        assert!(!verify("other", raw, &sig));
        assert!(!verify(SECRET, raw, ""));
        assert!(!verify("", raw, &sig)); // no secret, no trust
        assert!(!verify(SECRET, raw, "sha256="));
        assert!(!verify(SECRET, raw, "é"));
        assert!(!verify(SECRET, raw, &sig[..10]));
    }

    #[test]
    fn parse_event_resolves_org_plan_and_status() {
        let plans = load_plans(r#"[{"id":"a"},{"id":"b"}]"#).unwrap();
        let ev = parse_event(&json!({"org_id": 7, "plan": "b"}), &plans).unwrap();
        assert_eq!(
            (
                ev.org_id,
                ev.email.as_str(),
                ev.plan.id.as_str(),
                ev.status.as_str()
            ),
            (Some(7), "", "b", "active")
        );
        // providers echo custom data back as strings; int() takes whitespace and a sign
        for v in [json!(" +7 "), json!("7"), json!("0_7")] {
            let ev = parse_event(&json!({"org_id": v, "plan": "a"}), &plans).unwrap();
            assert_eq!(ev.org_id, Some(7));
        }
        assert_eq!(
            parse_event(&json!({"org_id": "-3", "plan": "a"}), &plans)
                .unwrap()
                .org_id,
            Some(-3)
        );
        let ev = parse_event(&json!({"email": " OPS@Shop.test ", "plan": "a"}), &plans).unwrap();
        assert_eq!((ev.org_id, ev.email.as_str()), (None, "ops@shop.test"));
        // a cancellation carries the old plan id and still downgrades to the default
        let ev = parse_event(
            &json!({"org_id": 1, "plan": "b", "status": " Cancelled "}),
            &plans,
        )
        .unwrap();
        assert_eq!(
            (ev.plan.id.as_str(), ev.status.as_str()),
            ("a", "cancelled")
        );
        let ev = parse_event(
            &json!({"org_id": 1, "plan": "b", "status": "TRIALING"}),
            &plans,
        )
        .unwrap();
        assert_eq!(ev.plan.id, "b");
    }

    #[test]
    fn parse_event_error_texts_match_python() {
        let plans = load_plans(r#"[{"id":"a"},{"id":"b"}]"#).unwrap();
        let e = |v: Value| err(parse_event(&v, &plans));
        assert_eq!(e(json!([1])), "event must be a JSON object");
        assert_eq!(e(json!("x")), "event must be a JSON object");
        for bad in [
            json!(7.0),
            json!(true),
            json!("1.5"),
            json!("x"),
            json!([1]),
            json!({}),
        ] {
            assert_eq!(e(json!({"org_id": bad})), "org_id must be an integer");
        }
        assert_eq!(
            e(json!({"org_id": "99999999999999999999"})),
            "org_id must be an integer"
        );
        assert_eq!(e(json!({"plan": "a"})), "org_id or email required");
        assert_eq!(
            e(json!({"email": "  ", "plan": "a"})),
            "org_id or email required"
        );
        assert_eq!(e(json!({"org_id": 1})), "unknown plan: None (have: a, b)");
        assert_eq!(
            e(json!({"org_id": 1, "plan": 0})),
            "unknown plan: 0 (have: a, b)"
        );
        assert_eq!(
            e(json!({"org_id": 1, "plan": "gold"})),
            "unknown plan: 'gold' (have: a, b)"
        );
        assert_eq!(
            e(json!({"org_id": 1, "plan": "x'y"})),
            "unknown plan: \"x'y\" (have: a, b)"
        );
        assert_eq!(
            err(parse_event(
                &json!({"org_id": 1, "status": "cancelled"}),
                &[]
            )),
            "no plans configured"
        );
    }

    #[test]
    fn a_truthy_non_string_email_is_refused_where_python_crashed() {
        let plans = load_plans(r#"[{"id":"a"},{"id":"b"}]"#).unwrap();
        for bad in [
            json!(7),
            json!(true),
            json!(1.5),
            json!(["x"]),
            json!({"a": 1}),
        ] {
            for org in [json!(7), json!(null)] {
                let r = parse_event(&json!({"org_id": org, "email": bad, "plan": "b"}), &plans);
                assert_eq!(err(r), "email must be a string");
            }
        }
        // falsy values still read as "no e-mail"
        let ev = parse_event(&json!({"org_id": 7, "email": 0, "plan": "b"}), &plans).unwrap();
        assert_eq!(ev.email, "");
    }

    #[test]
    fn python_whitespace_u001c_to_u001f_is_stripped() {
        let plans = load_plans(r#"[{"id":"\u001ca\u001f"},{"id":"b"}]"#).unwrap();
        assert_eq!(plans[0].id, "a");
        let ev = parse_event(
            &json!({"org_id": "\u{1c}7", "email": "\u{1d}X\u{1e}", "plan": "\u{1f}b", "status": "\u{1c}active"}),
            &plans,
        )
        .unwrap();
        assert_eq!(
            (ev.org_id, ev.email.as_str(), ev.plan.id.as_str()),
            (Some(7), "x", "b")
        );
        assert_eq!(ev.status, "active");
        let raw = b"{}";
        assert!(verify(SECRET, raw, &format!("\u{1c}{}", sign(SECRET, raw))));
    }

    #[test]
    fn json_minus_zero_and_big_integers_are_deliberate_deviations() {
        // `-0` is the integer 0 for Python's json; json_from_bytes maps serde_json's -0.0 back.
        assert_eq!(
            load_plans(r#"[{"id":"a","jobs":-0}]"#).unwrap()[0].jobs,
            Some(0)
        );
        let plans = load_plans(r#"[{"id":"a"}]"#).unwrap();
        assert_eq!(
            err(parse_event(&json!({"org_id": -0.0, "plan": "a"}), &plans)),
            "org_id must be an integer"
        );
    }
}
