// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The operator's write endpoints: org update, lock and unlock, delete, create with an invitation,
//! reset mail and key revoke. Each is one store function and one transaction with its audit row.
mod common;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use common::*;
use hmac::{Hmac, KeyInit, Mac};
use printpapi_server::billing;
use printpapi_server::proxy::TrustedProxies;
use printpapi_server::server::Config;
use printpapi_server::store::{self, Db};
use serde_json::{Value, json};
use sha2::Sha256;

const OP: &str = "operator-token-0123456789abcdef-xyz";
const CONSOLE: &str = "10.9.0.2";
const REASON: &str = "support ticket 4711";

fn hosted_cfg(c: &mut Config) {
    c.operator_token = Some(OP.into());
    c.operator_from = TrustedProxies::parse(CONSOLE).unwrap();
}

/// Hosted, with mail and a public URL (so reset mails and invitations work), a signup quota and a
/// plan catalogue.
fn mail_cfg(c: &mut Config) {
    hosted_cfg(c);
    c.reset_enabled = true;
    c.public_url = "https://print.example".into();
    c.signup_quota = Some(25);
    c.plans = billing::load_plans(
        &json!([
            {"id": "free", "name": "Free", "jobs": 50, "price": "0 EUR"},
            {"id": "pro", "name": "Pro", "jobs": 5000, "price": "9 EUR/mo"},
        ])
        .to_string(),
    )
    .unwrap();
}

fn app() -> App {
    App::with(mail_cfg)
}

fn op<'a>(app: &'a App, method: &str, target: &str) -> Call<'a> {
    app.call(method, target).bearer(OP).socket(CONSOLE)
}

fn send(app: &App, method: &str, target: &str, body: Value) -> (u16, Value) {
    let r = op(app, method, target).json(&body).send();
    (r.status, r.json())
}

fn sql(db: &Db, q: &str, p: impl rusqlite::Params) {
    db.with(|c| c.execute(q, p).unwrap());
}

fn count(db: &Db, q: &str, p: impl rusqlite::Params) -> i64 {
    db.with(|c| c.query_row(q, p, |r| r.get(0)).unwrap())
}

/// An org with one user, as a signup makes one. Returns (org id, user id).
fn customer(db: &Db, name: &str, email: &str) -> (i64, i64) {
    let s = db
        .with(|c| store::signup(c, name, email, "PWHASHSENTINEL", Some(100), None))
        .unwrap();
    (s.org_id, s.user_id)
}

/// A printer of `org` plus a client key `key-<org>`: (printer id, key).
fn printer_and_key(db: &Db, org: i64) -> (i64, String) {
    let reg = register(
        db,
        &format!("pc{org}"),
        &format!("agent-key-{org}"),
        &[decl("HP", true)],
        org,
    );
    let key = format!("key-{org}");
    add_key(db, "k", &key, org);
    (reg.pid("HP"), key)
}

fn audit_rows(app: &App) -> Vec<Value> {
    let r = op(app, "GET", "/operator/audit?limit=200").send();
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["entries"].as_array().unwrap().clone()
}

fn audit_of<'a>(rows: &'a [Value], action: &str) -> Vec<&'a Value> {
    rows.iter().filter(|r| r["action"] == action).collect()
}

fn org_name(db: &Db, org: i64) -> Option<String> {
    db.with(|c| store::get_org(c, org)).unwrap().map(|o| o.name)
}

fn quota(db: &Db, org: i64) -> Option<i64> {
    db.with(|c| store::get_org(c, org))
        .unwrap()
        .unwrap()
        .job_quota
}

fn state_of(db: &Db, job: i64) -> (String, Option<String>) {
    db.with(|c| {
        c.query_row("SELECT state, error FROM jobs WHERE id=?", [job], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
    })
}

// --- the gate -----------------------------------------------------------------------------------

#[test]
fn every_write_needs_the_gate_and_root_is_refused() {
    let app = app();
    let (org, user) = customer(&app.db, "Acme", "boss@acme.test");
    let calls = [
        ("PUT", format!("/operator/orgs/{org}")),
        ("POST", "/operator/orgs".to_string()),
        ("POST", format!("/operator/orgs/{org}/lock")),
        ("POST", format!("/operator/orgs/{org}/unlock")),
        ("DELETE", format!("/operator/orgs/{org}")),
        ("POST", format!("/operator/users/{user}/reset")),
        ("DELETE", "/operator/keys/1".to_string()),
    ];
    for (m, p) in &calls {
        let r = app.call(m, p).socket(CONSOLE).json(&json!({})).send();
        assert_eq!(r.status, 401, "{m} {p} without a token");
        let r = app
            .call(m, p)
            .bearer("t")
            .socket(CONSOLE)
            .json(&json!({}))
            .send();
        assert_eq!(r.status, 401, "{m} {p} with root");
        let r = app
            .call(m, p)
            .bearer(OP)
            .socket("127.0.0.1")
            .json(&json!({}))
            .send();
        assert_eq!(r.status, 401, "{m} {p} from another peer");
    }
    // and without a token configured they do not exist
    let plain = App::new();
    for (m, p) in &calls {
        assert_eq!(plain.call(m, p).bearer(OP).send().status, 404, "{m} {p}");
    }
    // nothing changed
    assert_eq!(org_name(&app.db, org).as_deref(), Some("Acme"));
    assert!(audit_rows(&app).is_empty());
}

#[test]
fn a_write_answers_404_for_the_wrong_method() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    for (m, p) in [
        ("GET", format!("/operator/orgs/{org}/lock")),
        ("PUT", format!("/operator/orgs/{org}/lock")),
        ("DELETE", format!("/operator/orgs/{org}/jobs")),
        ("POST", format!("/operator/orgs/{org}")),
        ("GET", "/operator/keys/1".to_string()),
        ("POST", "/operator/keys/1".to_string()),
        ("GET", "/operator/users/1/reset".to_string()),
    ] {
        assert_eq!(
            op(&app, m, &p).json(&json!({})).send().status,
            404,
            "{m} {p}"
        );
    }
}

// --- PUT /operator/orgs/{id} ---------------------------------------------------------------------

#[test]
fn put_org_updates_name_quota_and_plan_in_one_go() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    let created: f64 = app.db.with(|c| {
        c.query_row("SELECT created_at FROM orgs WHERE id=?", [org], |r| {
            r.get(0)
        })
        .unwrap()
    });
    let (st, v) = send(
        &app,
        "PUT",
        &format!("/operator/orgs/{org}"),
        json!({"name": "Acme Print", "plan": "pro", "reason": REASON}),
    );
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(org_name(&app.db, org).as_deref(), Some("Acme Print"));
    assert_eq!(quota(&app.db, org), Some(5000));
    let plan: Option<String> = app.db.with(|c| {
        c.query_row("SELECT plan FROM orgs WHERE id=?", [org], |r| r.get(0))
            .unwrap()
    });
    assert_eq!(plan.as_deref(), Some("pro"));

    // an explicit quota wins over the plan's and null means unlimited
    let (st, _) = send(
        &app,
        "PUT",
        &format!("/operator/orgs/{org}"),
        json!({"job_quota": null}),
    );
    assert_eq!(st, 200);
    assert_eq!(quota(&app.db, org), None);
    let (st, _) = send(
        &app,
        "PUT",
        &format!("/operator/orgs/{org}"),
        json!({"plan": "free", "job_quota": 7}),
    );
    assert_eq!(st, 200);
    assert_eq!(quota(&app.db, org), Some(7));

    // one audit row per call, with ids, numbers and the reason, never the name
    let rows = audit_rows(&app);
    let rows = audit_of(&rows, "org.update");
    assert_eq!(rows.len(), 3);
    let first = rows.last().unwrap();
    assert_eq!(first["actor"], "operator");
    assert_eq!(first["org_id"], org);
    assert_eq!(first["org_created_at"], created);
    assert_eq!(first["reason"], REASON);
    assert_eq!(first["detail"]["plan"], "pro");
    assert!(!first.to_string().contains("Acme"));
}

#[test]
fn put_org_validates_everything_before_it_writes_anything() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    for body in [
        json!({"name": "New", "plan": "nonsense"}),
        json!({"name": "New", "job_quota": -1}),
        json!({"name": "New", "job_quota": "ten"}),
        json!({"name": ""}),
        json!({"name": "   "}),
        json!({"name": 7}),
        json!({"name": "x".repeat(201)}),
        json!({"reason": "y".repeat(501), "name": "New"}),
        json!({"reason": 5, "name": "New"}),
    ] {
        let (st, v) = send(&app, "PUT", &format!("/operator/orgs/{org}"), body.clone());
        assert_eq!(st, 400, "{body} -> {v}");
    }
    assert_eq!(org_name(&app.db, org).as_deref(), Some("Acme"));
    assert_eq!(quota(&app.db, org), Some(100));
    // nothing to update
    let (st, _) = send(&app, "PUT", &format!("/operator/orgs/{org}"), json!({}));
    assert_eq!(st, 400);
    let (st, _) = send(
        &app,
        "PUT",
        &format!("/operator/orgs/{org}"),
        json!({"reason": REASON}),
    );
    assert_eq!(st, 400);
    // not JSON
    let r = op(&app, "PUT", &format!("/operator/orgs/{org}"))
        .raw(b"[1,2]")
        .send();
    assert_eq!(r.status, 400);
    assert!(audit_rows(&app).is_empty());
}

#[test]
fn put_org_of_an_unknown_org_is_404_and_writes_no_audit_row() {
    let app = app();
    let (st, _) = send(&app, "PUT", "/operator/orgs/999", json!({"name": "x"}));
    assert_eq!(st, 404);
    assert!(audit_rows(&app).is_empty());
}

#[test]
fn put_org_does_not_touch_a_lock() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    assert_eq!(
        send(
            &app,
            "POST",
            &format!("/operator/orgs/{org}/lock"),
            json!({"reason": REASON})
        )
        .0,
        200
    );
    let (st, _) = send(
        &app,
        "PUT",
        &format!("/operator/orgs/{org}"),
        json!({"job_quota": 3}),
    );
    assert_eq!(st, 200);
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM org_locks", []), 1);
}

// --- lock and unlock ------------------------------------------------------------------------------

#[test]
fn lock_inserts_the_row_cancels_queued_jobs_and_leaves_the_quota_alone() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    let (other, _) = customer(&app.db, "Other", "o@other.test");
    let (pid, _) = printer_and_key(&app.db, org);
    let (opid, _) = printer_and_key(&app.db, other);
    let flying = enqueue(&app.db, pid, b"A", |j| j.org_id = Some(org));
    let waiting = enqueue(&app.db, pid, b"B", |j| j.org_id = Some(org));
    let agent = app
        .db
        .with(|c| store::authenticate_agent(c, &format!("agent-key-{org}")))
        .unwrap()
        .unwrap();
    // claim takes the oldest queued job, so A is in flight and B still waits
    claim(&app.db, agent).unwrap();
    let theirs = enqueue(&app.db, opid, b"C", |j| j.org_id = Some(other));

    let (st, v) = send(
        &app,
        "POST",
        &format!("/operator/orgs/{org}/lock"),
        json!({"reason": REASON}),
    );
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["cancelled_jobs"], 1);
    assert_eq!(state_of(&app.db, flying).0, "claimed");
    assert_eq!(
        state_of(&app.db, waiting),
        ("cancelled".into(), Some("org locked".into()))
    );
    assert_eq!(state_of(&app.db, theirs).0, "queued");
    assert_eq!(quota(&app.db, org), Some(100));

    // the detail and the list say so
    let (_, d) = {
        let r = op(&app, "GET", &format!("/operator/orgs/{org}")).send();
        (r.status, r.json())
    };
    assert_eq!(d["locked"], true);
    assert_eq!(d["status"], "locked");
    assert_eq!(d["lock"]["reason"], REASON);
    assert_eq!(d["job_quota"], 100);

    let rows = audit_rows(&app);
    let rows = audit_of(&rows, "org.lock");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["org_id"], org);
    assert_eq!(rows[0]["reason"], REASON);
    assert_eq!(rows[0]["detail"]["cancelled_jobs"], 1);
    let lock_created: f64 = app.db.with(|c| {
        c.query_row(
            "SELECT org_created_at FROM org_locks WHERE org_id=?",
            [org],
            |r| r.get(0),
        )
        .unwrap()
    });
    assert_eq!(rows[0]["org_created_at"], lock_created);
}

#[test]
fn lock_needs_a_reason_of_eight_characters_and_an_existing_unlocked_org() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    for body in [
        json!({}),
        json!({"reason": "short"}),
        json!({"reason": 12345678}),
    ] {
        let (st, _) = send(&app, "POST", &format!("/operator/orgs/{org}/lock"), body);
        assert_eq!(st, 400);
    }
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM org_locks", []), 0);
    let (st, _) = send(
        &app,
        "POST",
        "/operator/orgs/999/lock",
        json!({"reason": REASON}),
    );
    assert_eq!(st, 404);
    let (st, _) = send(
        &app,
        "POST",
        &format!("/operator/orgs/{org}/lock"),
        json!({"reason": REASON}),
    );
    assert_eq!(st, 200);
    let (st, _) = send(
        &app,
        "POST",
        &format!("/operator/orgs/{org}/lock"),
        json!({"reason": REASON}),
    );
    assert_eq!(st, 409);
    // only the one successful lock left a row
    assert_eq!(audit_rows(&app).len(), 1);
}

fn shopify_post(app: &App, target: &str, secret: &str) -> u16 {
    let body = json!({"id": 450789469, "order_number": 1001, "name": "#1001",
        "line_items": [{"quantity": 1, "sku": "A", "title": "W", "price": "1.00"}]});
    let data = serde_json::to_vec(&body).unwrap();
    let digest = B64.encode(
        Hmac::<Sha256>::new_from_slice(secret.as_bytes())
            .unwrap()
            .chain_update(&data)
            .finalize()
            .into_bytes(),
    );
    app.call("POST", target)
        .header("content-type", "application/json")
        .header("x-shopify-hmac-sha256", &digest)
        .header("x-shopify-shop-domain", "acme.myshopify.com")
        .raw(&data)
        .send()
        .status
}

/// The four ways a job gets in, each as a customer of `org` would use them. Returns the statuses
/// and bodies in the order POST /jobs, POST /orders, Shopify webhook, PrintNode POST /printjobs.
fn submit_everywhere(app: &App, org: i64, pid: i64, key: &str, n: u32) -> [(u16, Value); 4] {
    let (c1, b1) = app.post(
        "/jobs",
        Some(key),
        json!({"printer_id": pid, "type": "raw_base64", "content": "QUJD",
               "idempotency_key": format!("j{n}")}),
    );
    let (c2, b2) = app.post(
        "/orders",
        Some(key),
        json!({"printer_id": pid, "order": {"number": format!("{n}"), "customer": "J",
               "address": ["x"], "lines": [{"qty": 1, "sku": "A", "name": "W", "total": "1"}],
               "totals": [["Total", "1"]]}}),
    );
    app.db
        .with(|c| store::set_org_shopify_secret(c, org, Some("s3cret")))
        .unwrap();
    let hook = format!("/integrations/shopify/orders?key={key}&printer_id={pid}");
    let c3 = shopify_post(app, &hook, "s3cret");
    let (c4, b4) = app.pn(
        "POST",
        "/printjobs",
        key,
        Some(
            json!({"printerId": pid, "title": "L", "contentType": "raw_base64", "content": "QUJD"}),
        ),
    );
    [(c1, b1), (c2, b2), (c3, Value::Null), (c4, b4)]
}

#[test]
fn nothing_changes_for_an_org_without_a_lock_row() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    let (other, _) = customer(&app.db, "Other", "o@other.test");
    let (pid, key) = printer_and_key(&app.db, org);
    // someone else's lock does not reach this org
    assert_eq!(
        send(
            &app,
            "POST",
            &format!("/operator/orgs/{other}/lock"),
            json!({"reason": REASON})
        )
        .0,
        200
    );
    let r = submit_everywhere(&app, org, pid, &key, 1);
    assert_eq!(r[0].0, 200, "{:?}", r[0]);
    assert_eq!(r[1].0, 200, "{:?}", r[1]);
    assert_eq!(r[2], (200, Value::Null));
    assert_eq!(r[3].0, 201, "{:?}", r[3]);
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM jobs", []), 4);
}

#[test]
fn a_locked_org_is_refused_on_every_submit_path_with_403() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    let (pid, key) = printer_and_key(&app.db, org);
    assert_eq!(
        send(
            &app,
            "POST",
            &format!("/operator/orgs/{org}/lock"),
            json!({"reason": REASON})
        )
        .0,
        200
    );
    let r = submit_everywhere(&app, org, pid, &key, 1);
    assert_eq!(
        (r[0].0, r[0].1["error"].as_str()),
        (403, Some("org locked")),
        "POST /jobs"
    );
    assert_eq!(
        (r[1].0, r[1].1["error"].as_str()),
        (403, Some("org locked")),
        "POST /orders"
    );
    assert_eq!(r[2].0, 403, "the Shopify webhook");
    assert_eq!(r[3].0, 403, "PrintNode POST /printjobs");
    assert!(r[3].1.to_string().contains("org locked"), "{}", r[3].1);
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM jobs", []), 0);

    // the same with a session instead of a key, and root naming the org's printer
    let session = app.account(org, "second@acme.test");
    let (st, _) = app.post(
        "/jobs",
        Some(&session),
        json!({"printer_id": pid, "type": "raw_base64", "content": "QUJD"}),
    );
    assert_eq!(st, 403);
    let (st, _) = app.post(
        "/jobs",
        Some("t"),
        json!({"printer_id": pid, "type": "raw_base64", "content": "QUJD"}),
    );
    assert_eq!(st, 403);
    // an idempotent resubmit does not slip past the lock either
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM jobs", []), 0);

    // logins, the dashboard's reads and the agent keep working, so the customer can see why
    let (st, _) = app.get("/jobs", Some(&session));
    assert_eq!(st, 200);
    let (st, _) = app.login("second@acme.test", PW);
    assert_eq!(st, 200);
    let r = app.agent("GET", "/agent/jobs", &format!("agent-key-{org}"), None);
    assert_ne!(r.status, 401);
    assert_ne!(r.status, 403);
}

#[test]
fn the_lock_is_checked_before_a_payload_is_fetched() {
    let app = app();
    app.fetch.gets.lock().unwrap().clear();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    let (pid, key) = printer_and_key(&app.db, org);
    send(
        &app,
        "POST",
        &format!("/operator/orgs/{org}/lock"),
        json!({"reason": REASON}),
    );
    let (st, v) = app.post(
        "/jobs",
        Some(&key),
        json!({"printer_id": pid, "type": "raw_uri", "url": "https://files.example/a.zpl"}),
    );
    assert_eq!((st, v["error"].as_str()), (403, Some("org locked")));
    assert!(
        app.fetch.gets.lock().unwrap().is_empty(),
        "fetched for a locked org"
    );
}

#[test]
fn unlock_deletes_the_row_and_submits_work_again() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    let (pid, key) = printer_and_key(&app.db, org);
    // not locked: 409; unknown org: 404
    let (st, _) = send(
        &app,
        "POST",
        &format!("/operator/orgs/{org}/unlock"),
        json!({}),
    );
    assert_eq!(st, 409);
    let (st, _) = send(&app, "POST", "/operator/orgs/999/unlock", json!({}));
    assert_eq!(st, 404);
    send(
        &app,
        "POST",
        &format!("/operator/orgs/{org}/lock"),
        json!({"reason": REASON}),
    );
    assert_eq!(
        app.post(
            "/jobs",
            Some(&key),
            json!({"printer_id": pid, "type": "raw_base64", "content": "QUJD"})
        )
        .0,
        403
    );
    let (st, v) = send(
        &app,
        "POST",
        &format!("/operator/orgs/{org}/unlock"),
        json!({"reason": "paid the invoice"}),
    );
    assert_eq!(st, 200, "{v}");
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM org_locks", []), 0);
    let r = submit_everywhere(&app, org, pid, &key, 1);
    assert_eq!([r[0].0, r[1].0, r[2].0, r[3].0], [200, 200, 200, 201]);
    let rows = audit_rows(&app);
    let unlocks = audit_of(&rows, "org.unlock");
    assert_eq!(unlocks.len(), 1);
    assert_eq!(unlocks[0]["reason"], "paid the invoice");
    assert_eq!(unlocks[0]["org_id"], org);
    // the reason is optional
    send(
        &app,
        "POST",
        &format!("/operator/orgs/{org}/lock"),
        json!({"reason": REASON}),
    );
    let (st, _) = send(
        &app,
        "POST",
        &format!("/operator/orgs/{org}/unlock"),
        json!({}),
    );
    assert_eq!(st, 200);
}

// --- DELETE /operator/orgs/{id} -------------------------------------------------------------------

#[test]
fn delete_needs_the_exact_name_and_a_reason() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    let path = format!("/operator/orgs/{org}");
    for body in [
        json!({"reason": REASON}),
        json!({"reason": REASON, "confirm": "acme"}),
        json!({"reason": REASON, "confirm": "Acme "}),
        json!({"reason": REASON, "confirm": 7}),
        json!({"confirm": "Acme"}),
        json!({"reason": "", "confirm": "Acme"}),
    ] {
        let (st, v) = send(&app, "DELETE", &path, body.clone());
        assert_eq!(st, 400, "{body} -> {v}");
    }
    assert!(org_name(&app.db, org).is_some());
    assert!(audit_rows(&app).is_empty());
    let (st, _) = send(
        &app,
        "DELETE",
        "/operator/orgs/999",
        json!({"reason": REASON, "confirm": "x"}),
    );
    assert_eq!(st, 404);
}

#[test]
fn delete_never_removes_the_default_org() {
    let app = app();
    let name = org_name(&app.db, store::DEFAULT_ORG).unwrap();
    let (st, _) = send(
        &app,
        "DELETE",
        &format!("/operator/orgs/{}", store::DEFAULT_ORG),
        json!({"reason": REASON, "confirm": name}),
    );
    assert_eq!(st, 400);
    assert!(org_name(&app.db, store::DEFAULT_ORG).is_some());
}

#[test]
fn delete_removes_the_org_with_its_lock_and_keeps_the_audit_rows() {
    let app = app();
    let (org, user) = customer(&app.db, "Acme", "boss@acme.test");
    let (pid, _) = printer_and_key(&app.db, org);
    enqueue(&app.db, pid, b"A", |j| j.org_id = Some(org));
    let created: f64 = app.db.with(|c| {
        c.query_row("SELECT created_at FROM orgs WHERE id=?", [org], |r| {
            r.get(0)
        })
        .unwrap()
    });
    send(
        &app,
        "POST",
        &format!("/operator/orgs/{org}/lock"),
        json!({"reason": REASON}),
    );
    let (st, v) = send(
        &app,
        "DELETE",
        &format!("/operator/orgs/{org}"),
        json!({"reason": "asked to be forgotten", "confirm": "Acme"}),
    );
    assert_eq!(st, 200, "{v}");
    assert!(org_name(&app.db, org).is_none());
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM org_locks", []), 0);
    assert_eq!(
        count(&app.db, "SELECT COUNT(*) FROM jobs WHERE org_id=?", [org]),
        0
    );
    assert_eq!(
        count(&app.db, "SELECT COUNT(*) FROM users WHERE id=?", [user]),
        0
    );
    // lock and delete rows are still there, the second one with the org's creation time
    let rows = audit_rows(&app);
    assert_eq!(audit_of(&rows, "org.lock").len(), 1);
    let del = audit_of(&rows, "org.delete");
    assert_eq!(del.len(), 1);
    assert_eq!(del[0]["org_id"], org);
    assert_eq!(del[0]["org_created_at"], created);
    assert_eq!(del[0]["reason"], "asked to be forgotten");
    assert!(!rows.iter().any(|r| r.to_string().contains("Acme")));
    // gone is gone
    let (st, _) = send(
        &app,
        "DELETE",
        &format!("/operator/orgs/{org}"),
        json!({"reason": REASON, "confirm": "Acme"}),
    );
    assert_eq!(st, 404);
}

#[test]
fn root_deleting_an_org_removes_its_lock_row_too() {
    // the legacy route shares delete_org: a lock must not outlive its org and bite the next one
    // that reuses the id
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    send(
        &app,
        "POST",
        &format!("/operator/orgs/{org}/lock"),
        json!({"reason": REASON}),
    );
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM org_locks", []), 1);
    let (st, _) = app.delete(&format!("/orgs/{org}"), Some("t"));
    assert_eq!(st, 200);
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM org_locks", []), 0);
}

// --- POST /operator/orgs --------------------------------------------------------------------------

#[test]
fn create_makes_the_org_and_its_first_user_and_mails_the_invitation() {
    let app = app();
    let (st, v) = send(
        &app,
        "POST",
        "/operator/orgs",
        json!({"name": "Fresh GmbH", "email": " New@Fresh.Test ", "reason": "onboarding call"}),
    );
    assert_eq!(st, 200, "{v}");
    let (org, user) = (
        v["org_id"].as_i64().unwrap(),
        v["user_id"].as_i64().unwrap(),
    );
    assert_eq!(org_name(&app.db, org).as_deref(), Some("Fresh GmbH"));
    // the signup quota, as a self-signup would get
    assert_eq!(quota(&app.db, org), Some(25));
    // a real hash of a random password, never a NULL one
    let hash: Option<String> = app.db.with(|c| {
        c.query_row("SELECT password_hash FROM users WHERE id=?", [user], |r| {
            r.get(0)
        })
        .unwrap()
    });
    let hash = hash.expect("a hash");
    assert!(hash.starts_with("scrypt$"), "{hash}");
    let email: String = app.db.with(|c| {
        c.query_row("SELECT email FROM users WHERE id=?", [user], |r| r.get(0))
            .unwrap()
    });
    assert_eq!(email, "new@fresh.test");
    // the invitation is the reset mail, and the token is in the mail only
    let mails = app.mail.all();
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].to, "new@fresh.test");
    assert!(
        mails[0].body.contains("https://print.example/?reset=sess_"),
        "{}",
        mails[0].body
    );
    let tok = mails[0]
        .body
        .split("?reset=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert!(!v.to_string().contains(&tok));
    // and it works: the invited person sets a password with it
    let (st, _) = app.post(
        "/password/reset/confirm",
        None,
        json!({"token": tok, "password": "a-new-password-1"}),
    );
    assert_eq!(st, 200);
    assert_eq!(app.login("new@fresh.test", "a-new-password-1").0, 200);

    let rows = audit_rows(&app);
    let rows = audit_of(&rows, "org.create");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["org_id"], org);
    assert_eq!(rows[0]["reason"], "onboarding call");
    assert!(rows[0]["org_created_at"].as_f64().unwrap() > 1.0e9);
    let text = rows[0].to_string();
    assert!(
        !text.contains("Fresh") && !text.contains("fresh.test"),
        "{text}"
    );
}

#[test]
fn create_without_mail_or_public_url_is_409_and_creates_nothing() {
    for tweak in [
        (|c: &mut Config| {
            hosted_cfg(c);
        }) as fn(&mut Config),
        |c: &mut Config| {
            hosted_cfg(c);
            c.reset_enabled = true; // mail, but no PUBLIC_URL
        },
        |c: &mut Config| {
            hosted_cfg(c);
            c.public_url = "https://print.example".into(); // a URL, but no mail
        },
    ] {
        let app = App::with(tweak);
        let before = count(&app.db, "SELECT COUNT(*) FROM orgs", []);
        let (st, v) = send(
            &app,
            "POST",
            "/operator/orgs",
            json!({"name": "Fresh", "email": "new@fresh.test"}),
        );
        assert_eq!(
            (st, v["error"].as_str()),
            (409, Some("mail is not configured"))
        );
        assert_eq!(count(&app.db, "SELECT COUNT(*) FROM orgs", []), before);
        assert!(app.mail.all().is_empty());
        assert!(audit_rows(&app).is_empty());
    }
}

#[test]
fn create_rejects_bad_input_and_a_taken_email_and_leaves_nothing_behind() {
    let app = app();
    customer(&app.db, "Acme", "boss@acme.test");
    let orgs = count(&app.db, "SELECT COUNT(*) FROM orgs", []);
    for body in [
        json!({"email": "x@y.test"}),
        json!({"name": "", "email": "x@y.test"}),
        json!({"name": "N"}),
        json!({"name": "N", "email": "nobody"}),
        json!({"name": 4, "email": "x@y.test"}),
    ] {
        let (st, _) = send(&app, "POST", "/operator/orgs", body.clone());
        assert_eq!(st, 400, "{body}");
    }
    let (st, _) = send(
        &app,
        "POST",
        "/operator/orgs",
        json!({"name": "Dup", "email": "BOSS@acme.test"}),
    );
    assert_eq!(st, 409);
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM orgs", []), orgs);
    assert!(app.mail.all().is_empty());
    assert!(audit_rows(&app).is_empty());
}

#[test]
fn create_with_a_broken_mail_server_keeps_the_org_and_logs_the_failure() {
    let app = app();
    *app.mail.fail.lock().unwrap() = true;
    let (st, v) = send(
        &app,
        "POST",
        "/operator/orgs",
        json!({"name": "Fresh", "email": "new@fresh.test"}),
    );
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["mail_sent"], false);
    assert!(org_name(&app.db, v["org_id"].as_i64().unwrap()).is_some());
    let rows = audit_rows(&app);
    assert_eq!(audit_of(&rows, "org.create").len(), 1);
    let failed = audit_of(&rows, "user.reset_mail_failed");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["org_id"], v["org_id"]);
    assert!(!failed[0].to_string().contains("fresh.test"));
}

// --- POST /operator/users/{id}/reset --------------------------------------------------------------

#[test]
fn reset_mails_a_token_and_never_returns_it() {
    let app = app();
    let (org, user) = customer(&app.db, "Acme", "boss@acme.test");
    let (st, v) = send(
        &app,
        "POST",
        &format!("/operator/users/{user}/reset"),
        json!({"org_id": org}),
    );
    assert_eq!(st, 200, "{v}");
    assert_eq!(v, json!({"ok": true}));
    let mails = app.mail.all();
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].to, "boss@acme.test");
    assert!(mails[0].body.contains("?reset=sess_"));
    assert_eq!(
        count(
            &app.db,
            "SELECT COUNT(*) FROM password_resets WHERE user_id=?",
            [user]
        ),
        1
    );
    let rows = audit_rows(&app);
    let rows = audit_of(&rows, "user.reset");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["org_id"], org);
    assert_eq!(rows[0]["target"], format!("user:{user}"));
    let text = rows[0].to_string();
    assert!(!text.contains("boss") && !text.contains("sess_"), "{text}");
}

#[test]
fn reset_checks_the_org_the_user_and_the_mail_setup() {
    let app = app();
    let (org, user) = customer(&app.db, "Acme", "boss@acme.test");
    let (other, _) = customer(&app.db, "Other", "o@other.test");
    let path = format!("/operator/users/{user}/reset");
    // a stale page naming the wrong org
    let (st, _) = send(&app, "POST", &path, json!({"org_id": other}));
    assert_eq!(st, 409);
    let (st, _) = send(&app, "POST", &path, json!({}));
    assert_eq!(st, 400);
    let (st, _) = send(&app, "POST", &path, json!({"org_id": "2"}));
    assert_eq!(st, 400);
    let (st, _) = send(
        &app,
        "POST",
        "/operator/users/9999/reset",
        json!({"org_id": org}),
    );
    assert_eq!(st, 404);
    // the seeded legacy user has no e-mail and nothing to mail
    let (st, _) = send(
        &app,
        "POST",
        &format!("/operator/users/{}/reset", store::DEFAULT_USER),
        json!({"org_id": store::DEFAULT_ORG}),
    );
    assert_eq!(st, 404);
    assert!(app.mail.all().is_empty());
    assert_eq!(
        count(&app.db, "SELECT COUNT(*) FROM password_resets", []),
        0
    );
    assert!(audit_rows(&app).is_empty());
    // no mail setup: 409
    let plain = App::with(hosted_cfg);
    let (org, user) = customer(&plain.db, "Acme", "boss@acme.test");
    let (st, v) = send(
        &plain,
        "POST",
        &format!("/operator/users/{user}/reset"),
        json!({"org_id": org}),
    );
    assert_eq!(
        (st, v["error"].as_str()),
        (409, Some("mail is not configured"))
    );
    assert_eq!(
        count(&plain.db, "SELECT COUNT(*) FROM password_resets", []),
        0
    );
}

#[test]
fn a_failed_reset_mail_is_a_502_and_a_second_audit_row() {
    let app = app();
    let (org, user) = customer(&app.db, "Acme", "boss@acme.test");
    *app.mail.fail.lock().unwrap() = true;
    let (st, _) = send(
        &app,
        "POST",
        &format!("/operator/users/{user}/reset"),
        json!({"org_id": org}),
    );
    assert_eq!(st, 502);
    let rows = audit_rows(&app);
    assert_eq!(audit_of(&rows, "user.reset").len(), 1);
    let failed = audit_of(&rows, "user.reset_mail_failed");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["target"], format!("user:{user}"));
    assert_eq!(failed[0]["org_id"], org);
    assert!(!failed[0].to_string().contains("smtp") && !failed[0].to_string().contains("acme"));
}

// --- DELETE /operator/keys/{id} -------------------------------------------------------------------

#[test]
fn revoking_a_key_stops_it_and_writes_one_row() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    let (other, _) = customer(&app.db, "Other", "o@other.test");
    let (pid, key) = printer_and_key(&app.db, org);
    let kid: i64 = app.db.with(|c| {
        c.query_row("SELECT id FROM api_keys WHERE org_id=?", [org], |r| {
            r.get(0)
        })
        .unwrap()
    });
    assert_eq!(
        app.post(
            "/jobs",
            Some(&key),
            json!({"printer_id": pid, "type": "raw_base64", "content": "QUJD"})
        )
        .0,
        200
    );
    let path = format!("/operator/keys/{kid}");
    // wrong org (a stale page), missing org_id, unknown key
    assert_eq!(send(&app, "DELETE", &path, json!({"org_id": other})).0, 409);
    assert_eq!(send(&app, "DELETE", &path, json!({})).0, 400);
    assert_eq!(
        send(
            &app,
            "DELETE",
            "/operator/keys/9999",
            json!({"org_id": org})
        )
        .0,
        404
    );
    assert_eq!(
        count(&app.db, "SELECT active FROM api_keys WHERE id=?", [kid]),
        1
    );
    let (st, v) = send(
        &app,
        "DELETE",
        &path,
        json!({"org_id": org, "reason": "leaked"}),
    );
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        count(&app.db, "SELECT active FROM api_keys WHERE id=?", [kid]),
        0
    );
    assert_eq!(
        app.post(
            "/jobs",
            Some(&key),
            json!({"printer_id": pid, "type": "raw_base64", "content": "QUJD"})
        )
        .0,
        401
    );
    let rows = audit_rows(&app);
    let rows = audit_of(&rows, "key.revoke");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["org_id"], org);
    assert_eq!(rows[0]["target"], format!("key:{kid}"));
    assert_eq!(rows[0]["reason"], "leaked");
    assert!(!rows[0].to_string().contains(&key));
}

// --- one transaction with the audit row -----------------------------------------------------------

fn break_audit(db: &Db) {
    sql(
        db,
        "CREATE TRIGGER audit_down BEFORE INSERT ON operator_audit \
         BEGIN SELECT RAISE(ABORT, 'audit down'); END",
        [],
    );
}

#[test]
fn a_failing_audit_insert_rolls_every_action_back() {
    let app = app();
    let (org, user) = customer(&app.db, "Acme", "boss@acme.test");
    let (pid, key) = printer_and_key(&app.db, org);
    let queued = enqueue(&app.db, pid, b"A", |j| j.org_id = Some(org));
    let kid: i64 = app.db.with(|c| {
        c.query_row("SELECT id FROM api_keys WHERE org_id=?", [org], |r| {
            r.get(0)
        })
        .unwrap()
    });
    let orgs = count(&app.db, "SELECT COUNT(*) FROM orgs", []);
    break_audit(&app.db);

    let (st, _) = send(
        &app,
        "PUT",
        &format!("/operator/orgs/{org}"),
        json!({"name": "Renamed", "job_quota": 1}),
    );
    assert_eq!(st, 500);
    assert_eq!(org_name(&app.db, org).as_deref(), Some("Acme"));
    assert_eq!(quota(&app.db, org), Some(100));

    let (st, _) = send(
        &app,
        "POST",
        &format!("/operator/orgs/{org}/lock"),
        json!({"reason": REASON}),
    );
    assert_eq!(st, 500);
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM org_locks", []), 0);
    assert_eq!(state_of(&app.db, queued).0, "queued");

    // an existing lock stays when its unlock cannot be written
    sql(
        &app.db,
        "INSERT INTO org_locks(org_id, org_created_at, reason, locked_at) VALUES(?,?,?,?)",
        rusqlite::params![org, 1.0, REASON, 2.0],
    );
    let (st, _) = send(
        &app,
        "POST",
        &format!("/operator/orgs/{org}/unlock"),
        json!({}),
    );
    assert_eq!(st, 500);
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM org_locks", []), 1);
    sql(&app.db, "DELETE FROM org_locks", []);

    let (st, _) = send(
        &app,
        "POST",
        "/operator/orgs",
        json!({"name": "Fresh", "email": "new@fresh.test"}),
    );
    assert_eq!(st, 500);
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM orgs", []), orgs);
    assert_eq!(
        count(
            &app.db,
            "SELECT COUNT(*) FROM users WHERE email='new@fresh.test'",
            []
        ),
        0
    );

    let (st, _) = send(
        &app,
        "POST",
        &format!("/operator/users/{user}/reset"),
        json!({"org_id": org}),
    );
    assert_eq!(st, 500);
    assert_eq!(
        count(&app.db, "SELECT COUNT(*) FROM password_resets", []),
        0
    );
    assert!(
        app.mail.all().is_empty(),
        "mail went out for a change that was rolled back"
    );

    let (st, _) = send(
        &app,
        "DELETE",
        &format!("/operator/keys/{kid}"),
        json!({"org_id": org}),
    );
    assert_eq!(st, 500);
    assert_eq!(
        count(&app.db, "SELECT active FROM api_keys WHERE id=?", [kid]),
        1
    );
    assert_eq!(
        app.post(
            "/jobs",
            Some(&key),
            json!({"printer_id": pid, "type": "raw_base64", "content": "QUJD"})
        )
        .0,
        200
    );

    let (st, _) = send(
        &app,
        "DELETE",
        &format!("/operator/orgs/{org}"),
        json!({"reason": REASON, "confirm": "Acme"}),
    );
    assert_eq!(st, 500);
    assert_eq!(org_name(&app.db, org).as_deref(), Some("Acme"));
    assert_eq!(
        count(&app.db, "SELECT COUNT(*) FROM jobs WHERE org_id=?", [org]),
        2
    );
    assert_eq!(
        count(&app.db, "SELECT COUNT(*) FROM users WHERE id=?", [user]),
        1
    );
}

#[test]
fn the_audit_log_holds_no_email_name_or_secret_after_every_action() {
    let app = app();
    let (org, user) = customer(&app.db, "Zed Holdings", "zed.boss@zedholdings.test");
    let (pid, key) = printer_and_key(&app.db, org);
    enqueue(&app.db, pid, b"A", |j| j.org_id = Some(org));
    let kid: i64 = app.db.with(|c| {
        c.query_row("SELECT id FROM api_keys WHERE org_id=?", [org], |r| {
            r.get(0)
        })
        .unwrap()
    });
    send(
        &app,
        "PUT",
        &format!("/operator/orgs/{org}"),
        json!({"name": "Zed Renamed", "plan": "pro", "reason": REASON}),
    );
    send(
        &app,
        "POST",
        &format!("/operator/orgs/{org}/lock"),
        json!({"reason": REASON}),
    );
    send(
        &app,
        "POST",
        &format!("/operator/orgs/{org}/unlock"),
        json!({}),
    );
    send(
        &app,
        "POST",
        &format!("/operator/users/{user}/reset"),
        json!({"org_id": org}),
    );
    send(
        &app,
        "DELETE",
        &format!("/operator/keys/{kid}"),
        json!({"org_id": org}),
    );
    let (_, c) = {
        let r = op(&app, "POST", "/operator/orgs")
            .json(&json!({"name": "Yolo Corp", "email": "yolo@yolocorp.test"}))
            .send();
        (r.status, r.json())
    };
    send(
        &app,
        "DELETE",
        &format!("/operator/orgs/{}", c["org_id"]),
        json!({"reason": REASON, "confirm": "Yolo Corp"}),
    );
    let all = serde_json::to_string(&audit_rows(&app)).unwrap();
    for banned in [
        "Zed", "zed", "Yolo", "yolo", "holdings", "sess_", &key, "PWHASH", "@",
    ] {
        assert!(!all.contains(banned), "{banned} in {all}");
    }
    let actions: Vec<String> = audit_rows(&app)
        .iter()
        .map(|r| r["action"].as_str().unwrap().to_string())
        .collect();
    for a in [
        "org.update",
        "org.lock",
        "org.unlock",
        "user.reset",
        "key.revoke",
        "org.create",
        "org.delete",
    ] {
        assert!(
            actions.iter().any(|x| x == a),
            "{a} missing from {actions:?}"
        );
    }
}
