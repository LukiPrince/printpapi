// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Billing over HTTP: a plan catalogue the operator configures, a signed webhook that moves an org
//! onto a plan (and its quota with it), and org deletion for the tenant that leaves — the HTTP
//! tests of `tests/test_billing.py` (the catalogue, checkout link and signature tests are the unit
//! tests of `billing`).
mod common;

use common::*;
use hmac::{Hmac, KeyInit, Mac};
use printpapi_server::billing;
use printpapi_server::store::{self, DEFAULT_ORG};
use serde_json::{Value, json};
use sha2::Sha256;

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

fn serve(plans: &str, secret: Option<&str>) -> App {
    let plans = billing::load_plans(plans).unwrap();
    App::with(|c| {
        c.plans = plans;
        c.billing_secret = secret.map(String::from);
    })
}

fn app() -> App {
    serve(&plans_json(), Some(SECRET))
}

fn sign(secret: &str, raw: &[u8]) -> String {
    let mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .unwrap()
        .chain_update(raw)
        .finalize()
        .into_bytes();
    mac.iter().map(|b| format!("{b:02x}")).collect()
}

fn signed_with(app: &App, event: &Value, secret: &str) -> (u16, Value) {
    let raw = serde_json::to_vec(event).unwrap();
    let r = app
        .call("POST", "/billing/webhook")
        .header("x-signature", &format!("sha256={}", sign(secret, &raw)))
        .raw(&raw)
        .send();
    (r.status, r.json())
}

fn signed(app: &App, event: Value) -> (u16, Value) {
    signed_with(app, &event, SECRET)
}

// --- the webhook ------------------------------------------------------------------------------

#[test]
fn a_signed_event_moves_the_org_onto_the_plan_and_its_quota() {
    let app = app();
    let session = app.default_account();
    let (code, body) = signed(
        &app,
        json!({"org_id": DEFAULT_ORG, "plan": "pro", "status": "active"}),
    );
    assert!(code == 200 && body["plan"] == "pro" && body["job_quota"] == 5000);
    let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["ok", "org_id", "plan", "job_quota"]);
    let (_, org) = app.get(&format!("/orgs/{DEFAULT_ORG}"), Some(&session));
    assert!(org["plan"] == "pro" && org["job_quota"] == 5000); // the quota is what bites
}

#[test]
fn an_event_may_name_the_org_by_the_owner_e_mail() {
    let app = app();
    let session = app.default_account();
    assert_eq!(
        signed(&app, json!({"email": "OPS@shop.test", "plan": "unlimited"})).0,
        200
    );
    let (_, org) = app.get(&format!("/orgs/{DEFAULT_ORG}"), Some(&session));
    assert!(org["plan"] == "unlimited" && org["job_quota"].is_null());
    assert_eq!(
        signed(&app, json!({"email": "nobody@shop.test", "plan": "pro"})).0,
        404
    );
    assert_eq!(signed(&app, json!({"plan": "pro"})).0, 400); // neither selector
}

#[test]
fn a_cancelled_subscription_falls_back_to_the_default_plan() {
    let app = app();
    let session = app.default_account();
    assert_eq!(
        signed(&app, json!({"org_id": DEFAULT_ORG, "plan": "unlimited"})).0,
        200
    );
    // Any non-active status downgrades — the plan field of a cancellation is the *old* plan.
    let (code, body) = signed(
        &app,
        json!({"org_id": DEFAULT_ORG, "plan": "unlimited", "status": "cancelled"}),
    );
    assert!(code == 200 && body["plan"] == "free");
    let (_, org) = app.get(&format!("/orgs/{DEFAULT_ORG}"), Some(&session));
    assert!(org["plan"] == "free" && org["job_quota"] == 50);
}

#[test]
fn a_bad_signature_changes_nothing() {
    let app = app();
    let event = json!({"org_id": DEFAULT_ORG, "plan": "pro"});
    assert_eq!(signed_with(&app, &event, "guessed").0, 401);
    assert_eq!(app.post("/billing/webhook", None, event).0, 401); // unsigned
    let org = app
        .db
        .with(|c| store::get_org(c, DEFAULT_ORG))
        .unwrap()
        .unwrap();
    assert!(org.plan.is_none() && org.job_quota.is_none());
    assert_eq!(
        signed(&app, json!({"org_id": DEFAULT_ORG, "plan": "gold"})).0,
        400
    );
    assert_eq!(signed(&app, json!({"org_id": 9999, "plan": "pro"})).0, 404);
}

#[test]
fn a_signed_event_that_is_not_a_json_object_is_a_400() {
    let app = app();
    for raw in [&b"["[..], b"[1]", b"\"x\"", b"{"] {
        let sig = format!("sha256={}", sign(SECRET, raw));
        let r = app
            .call("POST", "/billing/webhook")
            .header("x-signature", &sig)
            .raw(raw)
            .send();
        assert_eq!(r.status, 400, "{raw:?}");
        assert!(r.json()["error"].as_str().is_some_and(|e| !e.is_empty()));
    }
    // an empty body reads as {} and then names neither an org nor an e-mail
    let sig = format!("sha256={}", sign(SECRET, b""));
    let r = app
        .call("POST", "/billing/webhook")
        .header("x-signature", &sig)
        .send();
    assert_eq!(r.status, 400);
}

#[test]
fn billing_is_off_unless_the_operator_configured_it() {
    let app = serve("", None);
    let session = app.default_account();
    let (code, body) = signed(&app, json!({"org_id": DEFAULT_ORG, "plan": "pro"}));
    assert!(code == 503 && body["error"].as_str().unwrap().contains("not configured"));
    assert_eq!(app.get("/plans", Some(&session)).1["plans"], json!([]));
    // plans without a secret are off as well
    let half = serve(&plans_json(), None);
    assert_eq!(
        signed(&half, json!({"org_id": DEFAULT_ORG, "plan": "pro"})).0,
        503
    );
}

#[test]
fn a_plan_quota_is_enforced_on_the_next_job() {
    let app = serve(
        &json!([{"id": "free", "jobs": 0}, {"id": "pro", "jobs": 10}]).to_string(),
        Some(SECRET),
    );
    let pid = register(&app.db, "pc", "agentkey", &[decl("HP", true)], DEFAULT_ORG).pid("HP");
    let key = app.post("/apikeys", Some("t"), json!({"label": "shop"})).1["key"]
        .as_str()
        .unwrap()
        .to_string();
    let job = json!({"printer_id": pid, "type": "raw_base64", "content": "eA=="});
    assert_eq!(
        signed(&app, json!({"org_id": DEFAULT_ORG, "plan": "free"})).0,
        200
    );
    assert_eq!(app.post("/jobs", Some(&key), job.clone()).0, 402);
    assert_eq!(
        signed(&app, json!({"org_id": DEFAULT_ORG, "plan": "pro"})).0,
        200
    );
    assert_eq!(app.post("/jobs", Some(&key), job).0, 200);
}

// --- the catalogue over HTTP ------------------------------------------------------------------

#[test]
fn plans_are_listed_with_the_callers_own_checkout_link() {
    let app = app();
    let session = app.default_account();
    assert_eq!(app.get("/plans", None).0, 401); // not a public price list
    let (code, body) = app.get("/plans", Some(&session));
    assert!(code == 200 && body["current"].is_null());
    let pro = body["plans"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "pro")
        .unwrap()
        .clone();
    assert!(
        pro["checkout_url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("client_reference_id={DEFAULT_ORG}"))
    );
    assert_eq!(pro["price"], "9 EUR/mo");
    let keys: Vec<&String> = pro.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["id", "name", "jobs", "price", "checkout_url"]);
    assert_eq!(
        signed(&app, json!({"org_id": DEFAULT_ORG, "plan": "pro"})).0,
        200
    );
    assert_eq!(app.get("/plans", Some(&session)).1["current"], "pro");
    // root has no org to fill into the link
    let root = app.get("/plans", Some("t")).1;
    assert!(root["current"].is_null());
    assert!(
        root["plans"][1]["checkout_url"]
            .as_str()
            .unwrap()
            .ends_with("{org}")
    );
}

#[test]
fn only_the_operator_moves_an_org_between_plans_by_hand() {
    let app = app();
    let session = app.default_account();
    let url = format!("/orgs/{DEFAULT_ORG}");
    let (code, body) = app.put(&url, Some(&session), json!({"plan": "unlimited"}));
    assert!(code == 403 && body["error"].as_str().unwrap().contains("billing")); // no self-upgrade
    let (code, body) = app.put(&url, Some("t"), json!({"plan": "gold"}));
    assert_eq!(
        (code, body["error"].as_str().unwrap()),
        (400, "unknown plan: 'gold'")
    );
    let (code, body) = app.put(&url, Some("t"), json!({"plan": "pro"}));
    assert!(code == 200 && body["job_quota"] == 5000);
    assert_eq!(
        serde_json::to_string(&body).unwrap(),
        r#"{"ok":true,"plan":"pro","job_quota":5000}"#
    );
    assert_eq!(app.get(&url, Some(&session)).1["plan"], "pro");
    // job_quota written first keeps its place in the answer when the plan overwrites it
    let (_, body) = app.put(&url, Some("t"), json!({"job_quota": 1, "plan": "free"}));
    assert_eq!(
        serde_json::to_string(&body).unwrap(),
        r#"{"ok":true,"job_quota":50,"plan":"free"}"#
    );
}

// --- the tenant that leaves -------------------------------------------------------------------

#[test]
fn root_deletes_an_org_with_everything_in_it_but_never_the_default_one() {
    let app = app();
    let gone = create_org(&app.db, "leaving");
    register(&app.db, "pc", "agentkey", &[decl("HP", true)], gone);
    let session = app.account(gone, "bye@shop.test");
    let key = app
        .post(
            "/apikeys",
            Some("t"),
            json!({"label": "shop", "org_id": gone}),
        )
        .1["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(app.delete(&format!("/orgs/{gone}"), Some(&session)).0, 401); // not the tenant's
    assert_eq!(
        app.delete(&format!("/orgs/{DEFAULT_ORG}"), Some("t")).0,
        400
    );
    assert_eq!(app.delete(&format!("/orgs/{gone}"), Some("t")).0, 200);
    assert_eq!(app.delete(&format!("/orgs/{gone}"), Some("t")).0, 404); // only once
    assert_eq!(app.get("/me", Some(&session)).0, 401); // session died
    assert_eq!(app.get("/printers", Some(&key)).0, 401); // key died
    let orgs = app.db.with(store::list_orgs).unwrap();
    assert_eq!(orgs.iter().map(|o| o.id).collect::<Vec<_>>(), [DEFAULT_ORG]);
    assert!(
        app.db
            .with(|c| store::get_user_by_email(c, "bye@shop.test"))
            .unwrap()
            .is_none()
    );
}
