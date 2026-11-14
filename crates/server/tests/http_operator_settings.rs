// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! `GET` and `PUT /operator/settings` over HTTP (spec A5, "PR 3 decisions"): the live values with
//! their source, the validated write with its audit row, and the swap that follows it.
mod common;

use common::*;
use printpapi_server::dispatch::Destinations;
use printpapi_server::proxy::TrustedProxies;
use printpapi_server::server::{Config, Terms};
use printpapi_server::store::{self, Db};
use serde_json::{Value, json};

const OP: &str = "operator-token-0123456789abcdef-xyz";
const ROOT: &str = "root-token-0123456789abcdef-xyz";
const BILLING: &str = "billing-secret-0123456789-xyz";
const CONSOLE: &str = "10.9.0.2";
const REASON: &str = "retention for the new terms";

/// Hosted the way `main` sets it up: the token, its peer and the private-URL deny.
fn hosted_cfg(c: &mut Config) {
    c.token = ROOT.into();
    c.operator_token = Some(OP.into());
    c.operator_from = TrustedProxies::parse(CONSOLE).unwrap();
    c.destinations = Destinations::PublicOnly;
    c.fetches_per_org = Some(4);
    c.public_url = "https://print.example.test".into();
    c.reset_enabled = true;
    c.billing_secret = Some(BILLING.into());
    c.trusted_proxies = TrustedProxies::parse("10.0.0.0/8, 192.168.1.5").unwrap();
}

fn app() -> App {
    App::with(hosted_cfg)
}

fn op<'a>(app: &'a App, method: &str, target: &str) -> Call<'a> {
    app.call(method, target).bearer(OP).socket(CONSOLE)
}

fn get(app: &App) -> Value {
    let r = op(app, "GET", "/operator/settings").send();
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()
}

fn put(app: &App, body: Value) -> Reply {
    op(app, "PUT", "/operator/settings").json(&body).send()
}

fn put_ok(app: &App, body: Value) -> Value {
    let r = put(app, body);
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()
}

fn rows(db: &Db) -> Vec<(String, String)> {
    db.with(|c| store::settings_rows(c).unwrap())
}

fn audit_of(app: &App, action: &str) -> Vec<Value> {
    let r = op(app, "GET", "/operator/audit?limit=200").send();
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == action)
        .cloned()
        .collect()
}

fn terms_body() -> Value {
    json!({"version": "2026-11", "terms_url": "https://x.test/terms",
           "privacy_url": "https://x.test/privacy"})
}

// --- GET ----------------------------------------------------------------------------------------

#[test]
fn get_shows_every_key_with_its_default_and_the_env_only_values() {
    let app = app();
    let r = op(&app, "GET", "/operator/settings").send();
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.header("cache-control"), Some("no-store"));
    let v = r.json();
    assert_eq!(
        v["settings"],
        json!({
            "signup": {"value": "closed", "source": "default"},
            "signup_quota": {"value": null, "source": "default"},
            "job_retention_days": {"value": null, "source": "default"},
            "terms": {"value": null, "source": "default"},
        })
    );
    assert_eq!(
        v["env_only"],
        json!({
            "fetches_per_org": 4,
            "private_urls": "deny",
            "trusted_proxies": ["10.0.0.0/8", "192.168.1.5"],
            "operator_from": ["10.9.0.2"],
            "public_url": "https://print.example.test",
            "mail": true,
        })
    );
}

#[test]
fn get_names_the_env_as_source_of_a_variable_that_is_set() {
    let app = App::with(|c| {
        hosted_cfg(c);
        c.signup_open = true;
        c.env_flags.signup_set = true;
        c.signup_quota = Some(25);
        c.env_flags.signup_quota_set = true;
        c.job_retention_days = Some(14);
        c.env_flags.job_retention_days_set = true;
        c.terms = Some(Terms {
            version: "v1".into(),
            terms_url: "https://x.test/t".into(),
            dpa_url: None,
            privacy_url: None,
        });
        c.env_flags.terms_set = true;
    });
    let s = &get(&app)["settings"];
    assert_eq!(s["signup"], json!({"value": "open", "source": "env"}));
    assert_eq!(s["signup_quota"], json!({"value": 25, "source": "env"}));
    assert_eq!(
        s["job_retention_days"],
        json!({"value": 14, "source": "env"})
    );
    assert_eq!(
        s["terms"],
        json!({"value": {"version": "v1", "terms_url": "https://x.test/t"}, "source": "env"})
    );
}

#[test]
fn get_marks_an_explicit_env_closed_as_locked() {
    let app = App::with(|c| {
        hosted_cfg(c);
        c.env_flags.signup_set = true;
        c.env_flags.signup_explicit_closed = true;
    });
    assert_eq!(
        get(&app)["settings"]["signup"],
        json!({"value": "closed", "source": "env-locked"})
    );
}

#[test]
fn get_never_shows_a_token_or_a_secret() {
    let app = app();
    put_ok(&app, json!({"set": {"terms": terms_body()}}));
    let body = op(&app, "GET", "/operator/settings").send().text();
    for secret in [OP, ROOT, BILLING] {
        assert!(!body.contains(secret), "{secret} leaked: {body}");
    }
    let put_body = put(&app, json!({"set": {"signup_quota": 3}})).text();
    for secret in [OP, ROOT, BILLING] {
        assert!(!put_body.contains(secret), "{secret} leaked: {put_body}");
    }
}

// --- PUT ----------------------------------------------------------------------------------------

#[test]
fn put_then_get_shows_the_values_as_db_and_answers_in_the_get_shape() {
    let app = app();
    let v = put_ok(
        &app,
        json!({"set": {"signup": "open", "signup_quota": 7, "terms": terms_body()}}),
    );
    assert_eq!(v, get(&app), "the PUT answers like the GET");
    let s = &v["settings"];
    assert_eq!(s["signup"], json!({"value": "open", "source": "db"}));
    assert_eq!(s["signup_quota"], json!({"value": 7, "source": "db"}));
    assert_eq!(s["terms"]["source"], "db");
    assert_eq!(s["terms"]["value"], terms_body());
    assert_eq!(s["job_retention_days"]["source"], "default");
    assert_eq!(v["env_only"]["private_urls"], "deny");
}

#[test]
fn the_new_values_apply_at_once() {
    let app = app();
    assert_eq!(app.get("/health", None).1["signup"], "closed");
    put_ok(&app, json!({"set": {"signup": "open"}}));
    assert_eq!(app.get("/health", None).1["signup"], "open");
    put_ok(&app, json!({"set": {"terms": terms_body()}}));
    assert_eq!(app.get("/health", None).1["terms"]["version"], "2026-11");
}

#[test]
fn unset_goes_back_to_the_env_value_or_the_default() {
    let app = App::with(|c| {
        hosted_cfg(c);
        c.signup_quota = Some(25);
        c.env_flags.signup_quota_set = true;
    });
    put_ok(&app, json!({"set": {"signup_quota": 7, "signup": "open"}}));
    let v = put_ok(&app, json!({"unset": ["signup_quota", "signup"]}));
    assert_eq!(
        v["settings"]["signup_quota"],
        json!({"value": 25, "source": "env"})
    );
    assert_eq!(
        v["settings"]["signup"],
        json!({"value": "closed", "source": "default"})
    );
    assert!(rows(&app.db).is_empty());
}

#[test]
fn a_set_and_an_unset_of_different_keys_go_in_one_request() {
    let app = app();
    put_ok(&app, json!({"set": {"signup_quota": 7}}));
    let v = put_ok(
        &app,
        json!({"set": {"terms": terms_body()}, "unset": ["signup_quota"]}),
    );
    assert_eq!(v["settings"]["signup_quota"]["source"], "default");
    assert_eq!(v["settings"]["terms"]["source"], "db");
    assert_eq!(rows(&app.db).len(), 1);
}

#[test]
fn every_malformed_request_is_a_400_that_writes_nothing() {
    let app = app();
    let long = "x".repeat(501);
    let bad = [
        json!({}),
        json!({"set": {}}),
        json!({"unset": []}),
        json!({"set": {"nope": 1}}),
        json!({"unset": ["nope"]}),
        json!({"set": {"signup_quota": 3}, "unset": ["signup_quota"]}),
        json!({"set": {"signup_quota": null}}),
        json!({"set": {"signup": "maybe"}}),
        json!({"set": {"signup": true}}),
        json!({"set": {"signup_quota": -1}}),
        json!({"set": {"signup_quota": "5"}}),
        json!({"set": {"job_retention_days": 0}, "reason": REASON}),
        json!({"set": {"terms": {"version": "", "terms_url": "https://x.test/t"}}}),
        json!({"set": {"terms": {"version": "v1"}}}),
        json!({"set": {"terms": {"version": "v1", "terms_url": "ftp://x.test/t"}}}),
        json!({"set": {"terms": "v1"}}),
        json!({"set": {"signup_quota": 3}, "reason": 5}),
        json!({"set": {"signup_quota": 3}, "reason": long}),
        json!({"set": [], "unset": []}),
        json!({"set": {"signup_quota": 3}, "extra": 1}),
    ];
    for body in bad {
        let r = put(&app, body.clone());
        assert_eq!(r.status, 400, "{body}: {}", r.text());
        assert!(
            r.json()["error"].as_str().is_some_and(|e| !e.is_empty()),
            "{body}: a message"
        );
    }
    assert!(rows(&app.db).is_empty());
    assert!(audit_of(&app, "settings.update").is_empty());
    // not a JSON object at all
    let r = op(&app, "PUT", "/operator/settings")
        .raw(b"not json")
        .send();
    assert_eq!(r.status, 400);
}

#[test]
fn signup_open_is_refused_with_409_while_private_urls_are_allowed() {
    let app = App::with(|c| {
        hosted_cfg(c);
        c.destinations = Destinations::Any;
    });
    let r = put(&app, json!({"set": {"signup": "open"}}));
    assert_eq!(r.status, 409, "{}", r.text());
    assert!(rows(&app.db).is_empty());
    // closing is fine, and so is everything else
    put_ok(
        &app,
        json!({"set": {"signup": "closed", "signup_quota": 3}}),
    );
}

#[test]
fn an_env_lock_refuses_every_open_and_leaves_the_other_keys_open() {
    let app = App::with(|c| {
        hosted_cfg(c);
        c.env_flags.signup_set = true;
        c.env_flags.signup_explicit_closed = true;
    });
    for body in [
        json!({"set": {"signup": "open"}}),
        json!({"set": {"signup": "open", "signup_quota": 3}}),
    ] {
        let r = put(&app, body.clone());
        assert_eq!(r.status, 409, "{body}: {}", r.text());
    }
    assert!(rows(&app.db).is_empty(), "nothing written on a 409");
    // the other keys stay open to change
    put_ok(&app, json!({"set": {"signup_quota": 3}}));
}

/// Env open, private URLs allowed: signup may be closed but never reopened over the API.
fn open_with_private_urls(c: &mut Config) {
    hosted_cfg(c);
    c.signup_open = true;
    c.env_flags.signup_set = true;
    c.destinations = Destinations::Any;
}

#[test]
fn unset_signup_cannot_reopen_signup_while_private_urls_are_allowed() {
    let app = App::with(open_with_private_urls);
    put_ok(&app, json!({"set": {"signup": "closed"}}));
    let r = put(&app, json!({"unset": ["signup"]}));
    assert_eq!(r.status, 409, "{}", r.text());
    assert_eq!(
        get(&app)["settings"]["signup"],
        json!({"value": "closed", "source": "db"})
    );
    assert_eq!(
        rows(&app.db),
        vec![("signup".to_string(), "closed".to_string())]
    );
    // a PUT that leaves signup alone is not affected
    put_ok(&app, json!({"set": {"signup_quota": 3}}));
    // and unset is fine when it does not reopen: the env says closed
    let app = App::with(|c| {
        hosted_cfg(c);
        c.destinations = Destinations::Any;
    });
    put_ok(&app, json!({"set": {"signup": "closed"}}));
    put_ok(&app, json!({"unset": ["signup"]}));
}

fn locked(c: &mut Config) {
    hosted_cfg(c);
    c.env_flags.signup_set = true;
    c.env_flags.signup_explicit_closed = true;
}

fn seed_row(db: &Db, key: &str, value: &str) {
    db.with(|c| {
        c.execute(
            "INSERT OR REPLACE INTO settings(key, value, updated_at) VALUES(?,?,1.0)",
            rusqlite::params![key, value],
        )
        .unwrap();
    });
}

#[test]
fn while_locked_only_closing_changes_are_accepted() {
    let db = mem();
    seed_row(&db, "signup", "open");
    let app = App::on(db, locked);
    let r = put(&app, json!({"set": {"signup": "open"}}));
    assert_eq!(r.status, 409, "{}", r.text());
    assert_eq!(rows(&app.db).len(), 1, "nothing written on a 409");
    put_ok(&app, json!({"set": {"signup": "closed"}}));
    assert_eq!(
        rows(&app.db),
        vec![("signup".to_string(), "closed".to_string())]
    );
    let v = put_ok(&app, json!({"unset": ["signup"]}));
    assert!(rows(&app.db).is_empty());
    assert_eq!(
        v["settings"]["signup"],
        json!({"value": "closed", "source": "env-locked"})
    );
}

#[test]
fn get_and_put_show_a_row_that_is_not_applied_as_ignored() {
    // env-locked: the stale open row is visible and can be cleared
    let db = mem();
    seed_row(&db, "signup", "open");
    let app = App::on(db, locked);
    assert_eq!(
        get(&app)["settings"]["signup"],
        json!({"value": "closed", "source": "env-locked", "ignored": "open"})
    );
    // the PUT answer has the same shape
    let v = put_ok(&app, json!({"set": {"signup_quota": 3}}));
    assert_eq!(v["settings"]["signup"]["ignored"], "open");
    assert!(v["settings"]["signup_quota"].get("ignored").is_none());
    let v = put_ok(&app, json!({"unset": ["signup"]}));
    assert!(v["settings"]["signup"].get("ignored").is_none());
    // the private-URL cross-rule
    let db = mem();
    seed_row(&db, "signup", "open");
    let app = App::on(db, |c| {
        hosted_cfg(c);
        c.destinations = Destinations::Any;
    });
    assert_eq!(
        get(&app)["settings"]["signup"],
        json!({"value": "closed", "source": "default", "ignored": "open"})
    );
    // a value that no longer validates; an applied row shows nothing
    let db = mem();
    seed_row(&db, "signup_quota", "-3");
    seed_row(&db, "job_retention_days", "14");
    let app = App::on(db, hosted_cfg);
    let v = get(&app);
    assert_eq!(v["settings"]["signup_quota"]["ignored"], "-3");
    assert!(v["settings"]["job_retention_days"].get("ignored").is_none());
    assert_eq!(v["settings"]["job_retention_days"]["source"], "db");
}

#[test]
fn a_terms_version_with_control_characters_or_over_100_chars_is_refused() {
    let app = app();
    for version in [
        "v1\u{1b}[2J".to_string(),
        "v1\nsetting x".to_string(),
        "v1\0".to_string(),
        "a".repeat(101),
    ] {
        let r = put(
            &app,
            json!({"set": {"terms": {"version": version, "terms_url": "https://x.test/t"}}}),
        );
        assert_eq!(r.status, 400, "{version:?}: {}", r.text());
    }
    // a URL with a control character or a space inside is refused as well
    for u in ["https://x.test/\u{1b}t", "https://x.test/a b"] {
        let r = put(
            &app,
            json!({"set": {"terms": {"version": "v1", "terms_url": u}}}),
        );
        assert_eq!(r.status, 400, "{u:?}");
    }
    assert!(rows(&app.db).is_empty());
    put_ok(
        &app,
        json!({"set": {"terms": {"version": "a".repeat(100), "terms_url": "https://x.test/t"}}}),
    );
}

#[test]
fn a_put_writes_one_settings_update_audit_row_without_an_email() {
    let app = app();
    app.db.with(|c| {
        store::signup(c, "Acme", "boss@acme.test", "PWHASH", None, None).unwrap();
    });
    put_ok(
        &app,
        json!({"set": {"signup_quota": 5, "terms": terms_body()},
               "unset": ["job_retention_days"], "reason": "new quota"}),
    );
    let rows = audit_of(&app, "settings.update");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["reason"], "new quota");
    assert_eq!(
        rows[0]["detail"],
        json!({"set": {"signup_quota": 5, "terms": terms_body()},
               "unset": ["job_retention_days"]})
    );
    assert_eq!(rows[0]["org_id"], Value::Null);
    assert!(!rows[0].to_string().contains('@'), "no e-mail address");
}

#[test]
fn two_puts_in_a_row_end_with_the_live_values_equal_to_the_rows() {
    let app = app();
    put_ok(&app, json!({"set": {"signup": "open", "signup_quota": 3}}));
    put_ok(
        &app,
        json!({"set": {"signup_quota": 9}, "unset": ["signup"]}),
    );
    let v = get(&app);
    assert_eq!(
        v["settings"]["signup"],
        json!({"value": "closed", "source": "default"})
    );
    assert_eq!(
        v["settings"]["signup_quota"],
        json!({"value": 9, "source": "db"})
    );
    assert_eq!(app.get("/health", None).1["signup"], "closed");
    assert_eq!(rows(&app.db), vec![("signup_quota".into(), "9".into())]);
    // and a later GET (a new snapshot) agrees with the first
    assert_eq!(get(&app), v);
}

// --- the retention reason rule ------------------------------------------------------------------

#[test]
fn enabling_or_lowering_retention_needs_a_reason_of_eight_characters() {
    let app = app();
    for body in [
        json!({"set": {"job_retention_days": 30}}),
        json!({"set": {"job_retention_days": 30}, "reason": "short"}),
        json!({"set": {"job_retention_days": 30}, "reason": "   short  "}),
    ] {
        let r = put(&app, body.clone());
        assert_eq!(r.status, 400, "{body}: {}", r.text());
    }
    assert!(rows(&app.db).is_empty());
    assert!(audit_of(&app, "settings.update").is_empty());
    let v = put_ok(
        &app,
        json!({"set": {"job_retention_days": 30}, "reason": REASON}),
    );
    assert_eq!(
        v["settings"]["job_retention_days"],
        json!({"value": 30, "source": "db"})
    );
    // raising it destroys nothing: no reason
    put_ok(&app, json!({"set": {"job_retention_days": 60}}));
    // lowering it again does
    assert_eq!(
        put(&app, json!({"set": {"job_retention_days": 10}})).status,
        400
    );
    put_ok(
        &app,
        json!({"set": {"job_retention_days": 10}, "reason": REASON}),
    );
    assert_eq!(audit_of(&app, "settings.update").len(), 3);
}

#[test]
fn unsetting_an_override_that_falls_back_to_a_shorter_env_value_needs_a_reason() {
    let app = App::with(|c| {
        hosted_cfg(c);
        c.job_retention_days = Some(14);
        c.env_flags.job_retention_days_set = true;
    });
    put_ok(&app, json!({"set": {"job_retention_days": 90}}));
    let r = put(&app, json!({"unset": ["job_retention_days"]}));
    assert_eq!(r.status, 400, "{}", r.text());
    assert_eq!(rows(&app.db).len(), 1, "still overridden");
    let v = put_ok(
        &app,
        json!({"unset": ["job_retention_days"], "reason": REASON}),
    );
    assert_eq!(
        v["settings"]["job_retention_days"],
        json!({"value": 14, "source": "env"})
    );
}

#[test]
fn unsetting_a_retention_override_that_falls_back_to_a_longer_value_needs_none() {
    let app = App::with(|c| {
        hosted_cfg(c);
        c.job_retention_days = Some(90);
        c.env_flags.job_retention_days_set = true;
    });
    put_ok(
        &app,
        json!({"set": {"job_retention_days": 30}, "reason": REASON}),
    );
    put_ok(&app, json!({"unset": ["job_retention_days"]}));
    // and falling back to keep-for-ever destroys nothing either
    let app = app_with_nothing();
    put_ok(
        &app,
        json!({"set": {"job_retention_days": 30}, "reason": REASON}),
    );
    put_ok(&app, json!({"unset": ["job_retention_days"]}));
}

fn app_with_nothing() -> App {
    app()
}

// --- who may call, and when ---------------------------------------------------------------------

#[test]
fn the_endpoints_are_401_for_root_a_wrong_token_and_a_foreign_peer() {
    let app = app();
    for (method, body) in [
        ("GET", None),
        ("PUT", Some(json!({"set": {"signup_quota": 1}}))),
    ] {
        let mk = |token: &str, peer: &str| {
            let mut c = app
                .call(method, "/operator/settings")
                .bearer(token)
                .socket(peer);
            if let Some(b) = &body {
                c = c.json(b);
            }
            c.send()
        };
        assert_eq!(mk(ROOT, CONSOLE).status, 401, "{method} root");
        assert_eq!(mk("wrong", CONSOLE).status, 401, "{method} wrong token");
        assert_eq!(mk(OP, "203.0.113.9").status, 401, "{method} foreign peer");
        assert_eq!(mk(OP, "127.0.0.1").status, 401, "{method} loopback");
        let mut anon = app.call(method, "/operator/settings").socket(CONSOLE);
        if let Some(b) = &body {
            anon = anon.json(b);
        }
        assert_eq!(anon.send().status, 401, "{method} no credential");
    }
    assert!(rows(&app.db).is_empty());
}

#[test]
fn without_the_operator_token_the_endpoints_are_404_and_the_table_is_never_written() {
    let app = App::with(|c| {
        c.destinations = Destinations::PublicOnly;
    });
    for (method, body) in [
        ("GET", None),
        ("PUT", Some(json!({"set": {"signup_quota": 1}}))),
    ] {
        let mut c = app
            .call(method, "/operator/settings")
            .bearer(OP)
            .socket(CONSOLE);
        if let Some(b) = &body {
            c = c.json(b);
        }
        assert_eq!(c.send().status, 404, "{method}");
    }
    assert!(rows(&app.db).is_empty());
}

#[test]
fn other_methods_and_paths_under_settings_are_404() {
    let app = app();
    for (method, path) in [
        ("DELETE", "/operator/settings"),
        ("POST", "/operator/settings"),
        ("GET", "/operator/settings/signup"),
    ] {
        assert_eq!(op(&app, method, path).send().status, 404, "{method} {path}");
    }
}

#[test]
fn the_overview_reports_the_settings_feature() {
    let app = app();
    let r = op(&app, "GET", "/operator/overview").send();
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["features"], json!(["support", "settings"]));
}
