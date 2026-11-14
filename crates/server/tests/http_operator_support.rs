// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Support access over HTTP (spec A6, "PR 2 decisions"): grant, end, the payload download and the
//! titled job list, all under `/operator/` and all audited in the same transaction.
mod common;

use common::*;
use printpapi_server::proxy::TrustedProxies;
use printpapi_server::server::Config;
use printpapi_server::store::{self, Db};
use serde_json::{Value, json};

const OP: &str = "operator-token-0123456789abcdef-xyz";
const CONSOLE: &str = "10.9.0.2";
const REASON: &str = "support ticket 4711";

fn hosted_cfg(c: &mut Config) {
    c.operator_token = Some(OP.into());
    c.operator_from = TrustedProxies::parse(CONSOLE).unwrap();
}

fn app() -> App {
    App::with(hosted_cfg)
}

fn op<'a>(app: &'a App, method: &str, target: &str) -> Call<'a> {
    app.call(method, target).bearer(OP).socket(CONSOLE)
}

fn sql(db: &Db, q: &str, p: impl rusqlite::Params) {
    db.with(|c| c.execute(q, p).unwrap());
}

fn count(db: &Db, q: &str, p: impl rusqlite::Params) -> i64 {
    db.with(|c| c.query_row(q, p, |r| r.get(0)).unwrap())
}

fn created_of(db: &Db, org: i64) -> f64 {
    db.with(|c| {
        c.query_row("SELECT created_at FROM orgs WHERE id=?", [org], |r| {
            r.get(0)
        })
        .unwrap()
    })
}

/// An org with one user (as a signup makes one) and one printer: (org id, printer id).
fn customer(db: &Db, name: &str, email: &str) -> (i64, i64) {
    let s = db
        .with(|c| store::signup(c, name, email, "PWHASHSENTINEL", Some(100), None))
        .unwrap();
    let reg = register(
        db,
        &format!("pc{}", s.org_id),
        &format!("agent-key-{}", s.org_id),
        &[decl("HP", true)],
        s.org_id,
    );
    (s.org_id, reg.pid("HP"))
}

fn job(db: &Db, org: i64, printer: i64, mode: &'static str, payload: &[u8], title: &str) -> i64 {
    enqueue(db, printer, payload, |j| {
        j.org_id = Some(org);
        j.mode = mode;
        j.type_ = if mode == "pdf" {
            "pdf_base64"
        } else {
            "raw_base64"
        };
        j.title = Some(title.to_string());
    })
}

fn grant_body(app: &App, org: i64) -> Value {
    json!({"reason": REASON, "minutes": 30, "org_created_at": created_of(&app.db, org)})
}

/// Grant support on `org`; returns the grant id.
fn grant(app: &App, org: i64) -> i64 {
    let r = op(app, "POST", &format!("/operator/orgs/{org}/support"))
        .json(&grant_body(app, org))
        .send();
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["id"].as_i64().unwrap()
}

fn audit_rows(app: &App) -> Vec<Value> {
    let r = op(app, "GET", "/operator/audit?limit=200").send();
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["entries"].as_array().unwrap().clone()
}

fn audit_of(app: &App, action: &str) -> Vec<Value> {
    audit_rows(app)
        .into_iter()
        .filter(|r| r["action"] == action)
        .collect()
}

fn reads(db: &Db, grant: i64) -> i64 {
    count(db, "SELECT reads FROM support_grants WHERE id=?", [grant])
}

fn payload_of(app: &App, job: i64) -> Reply {
    op(app, "POST", &format!("/operator/jobs/{job}/payload")).send()
}

// --- granting -----------------------------------------------------------------------------------

#[test]
fn granting_answers_with_the_grant_and_audits_it() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    let before = now_s();
    let r = op(&app, "POST", &format!("/operator/orgs/{org}/support"))
        .json(&grant_body(&app, org))
        .send();
    assert_eq!(r.status, 200, "{}", r.text());
    let v = r.json();
    let id = v["id"].as_i64().unwrap();
    assert_eq!(v["reason"], REASON);
    assert!(v["granted_at"].as_f64().unwrap() >= before);
    let span = v["expires_at"].as_f64().unwrap() - v["granted_at"].as_f64().unwrap();
    assert!((span - 1800.0).abs() < 1.0, "{span}");
    assert_eq!(r.header("cache-control"), Some("no-store"));

    let rows = audit_of(&app, "support.grant");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["org_id"], org);
    assert_eq!(rows[0]["target"], format!("grant:{id}"));
    assert_eq!(rows[0]["reason"], REASON);
    assert_eq!(rows[0]["detail"], json!({"minutes": 30}));
}

fn now_s() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

#[test]
fn granting_validates_minutes_and_reason() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    let url = format!("/operator/orgs/{org}/support");
    let created = created_of(&app.db, org);
    for body in [
        json!({"reason": REASON, "minutes": 45, "org_created_at": created}),
        json!({"reason": REASON, "minutes": 0, "org_created_at": created}),
        json!({"reason": REASON, "minutes": "30", "org_created_at": created}),
        json!({"reason": REASON, "org_created_at": created}),
        json!({"reason": "short", "minutes": 30, "org_created_at": created}),
        json!({"minutes": 30, "org_created_at": created}),
        json!({"reason": "x".repeat(501), "minutes": 30, "org_created_at": created}),
        json!({"reason": 5, "minutes": 30, "org_created_at": created}),
    ] {
        let r = op(&app, "POST", &url).json(&body).send();
        assert_eq!(r.status, 400, "{body}");
    }
    for minutes in [15, 30, 60] {
        let ended = grant_with(&app, org, minutes);
        let r = op(&app, "DELETE", &format!("/operator/support/{ended}"))
            .json(&json!({}))
            .send();
        assert_eq!(r.status, 200, "{}", r.text());
    }
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM support_grants", []), 3);
}

fn grant_with(app: &App, org: i64, minutes: i64) -> i64 {
    let mut b = grant_body(app, org);
    b["minutes"] = json!(minutes);
    let r = op(app, "POST", &format!("/operator/orgs/{org}/support"))
        .json(&b)
        .send();
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["id"].as_i64().unwrap()
}

#[test]
fn granting_refuses_unknown_org_active_grant_and_stale_page() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    let url = format!("/operator/orgs/{org}/support");

    let r = op(&app, "POST", "/operator/orgs/9999/support")
        .json(&json!({"reason": REASON, "minutes": 30, "org_created_at": 1.0}))
        .send();
    assert_eq!(r.status, 404);

    // a stale page, or none named
    let stale = json!({"reason": REASON, "minutes": 30, "org_created_at": 1.0});
    assert_eq!(op(&app, "POST", &url).json(&stale).send().status, 409);
    let none = json!({"reason": REASON, "minutes": 30});
    assert_eq!(op(&app, "POST", &url).json(&none).send().status, 409);
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM support_grants", []), 0);

    grant(&app, org);
    let again = op(&app, "POST", &url).json(&grant_body(&app, org)).send();
    assert_eq!(again.status, 409, "{}", again.text());
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM support_grants", []), 1);
}

#[test]
fn an_expired_grant_does_not_block_a_new_one() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    let id = grant(&app, org);
    sql(
        &app.db,
        "UPDATE support_grants SET expires_at = expires_at - 7200 WHERE id=?",
        [id],
    );
    let second = grant(&app, org);
    assert_ne!(second, id);
}

// --- ending -------------------------------------------------------------------------------------

#[test]
fn ending_a_grant_by_id_audits_it_and_closes_access() {
    let app = app();
    let (org, pid) = customer(&app.db, "Acme", "boss@acme.test");
    let j = job(&app.db, org, pid, "raw", b"ZPL", "t");
    let id = grant(&app, org);
    assert_eq!(payload_of(&app, j).status, 200);

    let r = op(&app, "DELETE", &format!("/operator/support/{id}"))
        .json(&json!({"reason": "done"}))
        .send();
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json(), json!({"ok": true}));
    assert!(
        count(
            &app.db,
            "SELECT ended_at IS NOT NULL FROM support_grants WHERE id=?",
            [id]
        ) == 1
    );
    let rows = audit_of(&app, "support.end");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["target"], format!("grant:{id}"));
    assert_eq!(rows[0]["org_id"], org);
    assert_eq!(rows[0]["reason"], "done");
    // the access is gone at once
    assert_eq!(payload_of(&app, j).status, 403);
}

#[test]
fn ending_works_without_a_body_and_refuses_unknown_or_inactive() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    assert_eq!(
        op(&app, "DELETE", "/operator/support/9999").send().status,
        404
    );
    let id = grant(&app, org);
    let r = op(&app, "DELETE", &format!("/operator/support/{id}")).send();
    assert_eq!(r.status, 200, "{}", r.text());
    // ended: 409; expired: 409
    assert_eq!(
        op(&app, "DELETE", &format!("/operator/support/{id}"))
            .send()
            .status,
        409
    );
    let id2 = grant(&app, org);
    sql(
        &app.db,
        "UPDATE support_grants SET expires_at = expires_at - 7200 WHERE id=?",
        [id2],
    );
    assert_eq!(
        op(&app, "DELETE", &format!("/operator/support/{id2}"))
            .send()
            .status,
        409
    );
    assert_eq!(audit_of(&app, "support.end").len(), 1);
    // a reason that is not a string
    let id3 = grant(&app, org);
    let r = op(&app, "DELETE", &format!("/operator/support/{id3}"))
        .json(&json!({"reason": 7}))
        .send();
    assert_eq!(r.status, 400);
}

#[test]
fn a_grant_of_a_reused_org_id_is_not_active() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    let id = grant(&app, org);
    // the org was deleted and recreated under the same id: another generation
    sql(
        &app.db,
        "UPDATE orgs SET created_at = created_at + 100 WHERE id=?",
        [org],
    );
    assert_eq!(
        op(&app, "DELETE", &format!("/operator/support/{id}"))
            .send()
            .status,
        409
    );
}

// --- the payload --------------------------------------------------------------------------------

#[test]
fn the_payload_comes_back_byte_for_byte_with_safe_headers() {
    let app = app();
    let (org, pid) = customer(&app.db, "Acme", "boss@acme.test");
    let bytes: Vec<u8> = vec![0x25, 0x50, 0x00, 0xff, 0x0a, 0x80, 0x00];
    let pdf = job(&app.db, org, pid, "pdf", &bytes, "Invoice 77");
    let raw = job(&app.db, org, pid, "raw", b"^XA^FDhi^FS^XZ", "Label");
    let id = grant(&app, org);

    let r = payload_of(&app, pdf);
    assert_eq!(r.status, 200);
    assert_eq!(r.body, bytes);
    assert_eq!(r.header("content-type"), Some("application/octet-stream"));
    assert_eq!(
        r.header("content-disposition"),
        Some(format!("attachment; filename=\"job-{pdf}.pdf\"").as_str())
    );
    assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(
        r.header("content-security-policy"),
        Some("sandbox; default-src 'none'")
    );
    assert_eq!(r.header("cache-control"), Some("no-store"));

    let r = payload_of(&app, raw);
    assert_eq!(r.status, 200);
    assert_eq!(r.body, b"^XA^FDhi^FS^XZ");
    assert_eq!(
        r.header("content-disposition"),
        Some(format!("attachment; filename=\"job-{raw}.bin\"").as_str())
    );

    // each read: one audit row with the job id, and the counter
    let rows = audit_of(&app, "support.read_job");
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| r["target"] == format!("grant:{id}") && r["org_id"] == org)
    );
    let ids: Vec<Value> = rows.iter().map(|r| r["detail"].clone()).collect();
    assert!(ids.contains(&json!({"job_ids": [pdf]})));
    assert!(ids.contains(&json!({"job_ids": [raw]})));
    assert_eq!(reads(&app.db, id), 2);
}

#[test]
fn the_payload_needs_an_active_grant_of_the_jobs_own_org() {
    let app = app();
    let (acme, pa) = customer(&app.db, "Acme", "boss@acme.test");
    let (other, _) = customer(&app.db, "Other", "boss@other.test");
    let j = job(&app.db, acme, pa, "raw", b"A", "t");
    // no grant at all
    let r = payload_of(&app, j);
    assert_eq!(r.status, 403);
    assert!(r.body.iter().all(|b| *b != b'A') || r.json()["error"].is_string());
    // a grant on another org does not open this one
    grant(&app, other);
    assert_eq!(payload_of(&app, j).status, 403);
    assert_eq!(audit_of(&app, "support.read_job").len(), 0);
    // unknown job
    assert_eq!(payload_of(&app, 99999).status, 404);
    // expired
    let g = grant(&app, acme);
    assert_eq!(payload_of(&app, j).status, 200);
    sql(
        &app.db,
        "UPDATE support_grants SET expires_at = expires_at - 7200 WHERE id=?",
        [g],
    );
    assert_eq!(payload_of(&app, j).status, 403);
}

#[test]
fn a_scrubbed_payload_is_410_and_writes_no_row() {
    let app = app();
    let (org, pid) = customer(&app.db, "Acme", "boss@acme.test");
    let j = job(&app.db, org, pid, "raw", b"A", "t");
    let g = grant(&app, org);
    sql(&app.db, "UPDATE jobs SET payload=x'' WHERE id=?", [j]);
    let r = payload_of(&app, j);
    assert_eq!(r.status, 410, "{}", r.text());
    assert_eq!(audit_of(&app, "support.read_job").len(), 0);
    assert_eq!(reads(&app.db, g), 0);
}

#[test]
fn the_payload_and_titled_routes_refuse_root_and_a_foreign_peer() {
    let app = app();
    let (org, pid) = customer(&app.db, "Acme", "boss@acme.test");
    let j = job(&app.db, org, pid, "raw", b"A", "Secret title");
    grant(&app, org);
    let payload = format!("/operator/jobs/{j}/payload");
    let titled = format!("/operator/orgs/{org}/jobs?titles=1");
    for (m, t) in [("POST", payload.as_str()), ("GET", titled.as_str())] {
        // root (the token "t") from the console and from loopback
        for peer in [CONSOLE, "127.0.0.1"] {
            let r = app.call(m, t).bearer("t").socket(peer).send();
            assert_eq!(r.status, 401, "{m} {t} root from {peer}");
        }
        // the operator token from a socket peer that is not the console
        let r = app.call(m, t).bearer(OP).socket("203.0.113.9").send();
        assert_eq!(r.status, 401, "{m} {t} foreign peer");
        // no credential
        assert_eq!(app.call(m, t).socket(CONSOLE).send().status, 401);
    }
    for (m, t) in [
        ("POST", format!("/operator/orgs/{org}/support")),
        ("DELETE", "/operator/support/1".to_string()),
    ] {
        assert_eq!(
            app.call(m, &t)
                .bearer(OP)
                .socket("203.0.113.9")
                .send()
                .status,
            401,
            "{m} {t}"
        );
        assert_eq!(
            app.call(m, &t).bearer("t").socket(CONSOLE).send().status,
            401
        );
    }
    assert_eq!(audit_of(&app, "support.read_job").len(), 0);
    assert_eq!(audit_of(&app, "support.list_jobs").len(), 0);
}

#[test]
fn the_new_methods_are_not_accepted_elsewhere() {
    let app = app();
    let (org, pid) = customer(&app.db, "Acme", "boss@acme.test");
    let j = job(&app.db, org, pid, "raw", b"A", "t");
    grant(&app, org);
    // the payload is POST only (no prefetch or <img> may trigger it)
    for m in ["GET", "PUT", "DELETE"] {
        assert_eq!(
            op(&app, m, &format!("/operator/jobs/{j}/payload"))
                .send()
                .status,
            404,
            "{m}"
        );
    }
    assert_eq!(
        op(&app, "GET", &format!("/operator/orgs/{org}/support"))
            .send()
            .status,
        404
    );
    assert_eq!(op(&app, "GET", "/operator/support/1").send().status, 404);
    assert_eq!(op(&app, "POST", "/operator/support/1").send().status, 404);
    assert_eq!(audit_of(&app, "support.read_job").len(), 0);
}

// --- titles, opt-in -----------------------------------------------------------------------------

#[test]
fn titles_are_opt_in_and_audited_with_the_ids_returned() {
    let app = app();
    let (org, pid) = customer(&app.db, "Acme", "boss@acme.test");
    let j1 = job(&app.db, org, pid, "raw", b"A", "Invoice 77");
    let j2 = job(&app.db, org, pid, "raw", b"B", "Label 9");
    let g = grant(&app, org);

    let r = op(&app, "GET", &format!("/operator/orgs/{org}/jobs?titles=1")).send();
    assert_eq!(r.status, 200, "{}", r.text());
    let jobs = r.json()["jobs"].as_array().unwrap().clone();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0]["id"], j2);
    assert_eq!(jobs[0]["title"], "Label 9");
    assert_eq!(jobs[1]["title"], "Invoice 77");
    assert_eq!(jobs[0]["state"], "queued");

    let rows = audit_of(&app, "support.list_jobs");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["target"], format!("grant:{g}"));
    assert_eq!(rows[0]["detail"], json!({"job_ids": [j2, j1]}));
    assert_eq!(reads(&app.db, g), 1);

    // a page of one: the audit row names the one id returned, not a peeked extra row
    let r = op(
        &app,
        "GET",
        &format!("/operator/orgs/{org}/jobs?titles=1&limit=1"),
    )
    .send();
    assert_eq!(r.json()["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(r.json()["next_before_id"], j2);
    let rows = audit_of(&app, "support.list_jobs");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["detail"], json!({"job_ids": [j2]}));

    // paging works with titles on
    let r = op(
        &app,
        "GET",
        &format!("/operator/orgs/{org}/jobs?titles=1&limit=1&before_id={j2}"),
    )
    .send();
    assert_eq!(r.json()["jobs"][0]["title"], "Invoice 77");
    assert_eq!(audit_of(&app, "support.list_jobs").len(), 3);
}

#[test]
fn without_titles_1_nothing_changes_even_during_a_grant() {
    let app = app();
    let (org, pid) = customer(&app.db, "Acme", "boss@acme.test");
    job(&app.db, org, pid, "raw", b"A", "Invoice 77");
    let g = grant(&app, org);
    for q in ["", "?titles=0", "?titles=", "?titles=yes", "?limit=5"] {
        let r = op(&app, "GET", &format!("/operator/orgs/{org}/jobs{q}")).send();
        assert_eq!(r.status, 200, "{q}");
        assert!(!r.text().contains("Invoice 77"), "{q}");
        assert!(r.json()["jobs"][0].get("title").is_none(), "{q}");
    }
    assert_eq!(audit_of(&app, "support.list_jobs").len(), 0);
    assert_eq!(reads(&app.db, g), 0);
    // the org page and the overview never carry a title either
    let r = op(&app, "GET", &format!("/operator/orgs/{org}")).send();
    assert!(!r.text().contains("Invoice 77"));
    let r = op(&app, "GET", "/operator/overview").send();
    assert!(!r.text().contains("Invoice 77"));
    // no grant, no titles=1: unchanged metadata, no audit row
    let (other, op2) = customer(&app.db, "Other", "o@other.test");
    job(&app.db, other, op2, "raw", b"A", "Other title");
    let r = op(&app, "GET", &format!("/operator/orgs/{other}/jobs")).send();
    assert_eq!(r.status, 200);
    assert!(!r.text().contains("Other title"));
}

#[test]
fn titles_need_an_active_grant() {
    let app = app();
    let (org, pid) = customer(&app.db, "Acme", "boss@acme.test");
    job(&app.db, org, pid, "raw", b"A", "Invoice 77");
    let url = format!("/operator/orgs/{org}/jobs?titles=1");
    let r = op(&app, "GET", &url).send();
    assert_eq!(r.status, 403);
    assert_eq!(r.json()["error"], "no active support access");
    assert!(!r.text().contains("Invoice 77"));
    assert_eq!(
        op(&app, "GET", "/operator/orgs/9999/jobs?titles=1")
            .send()
            .status,
        404
    );
    // ended
    let g = grant(&app, org);
    op(&app, "DELETE", &format!("/operator/support/{g}")).send();
    assert_eq!(op(&app, "GET", &url).send().status, 403);
    assert_eq!(audit_of(&app, "support.list_jobs").len(), 0);
}

#[test]
fn an_empty_titled_page_writes_no_row() {
    let app = app();
    let (org, _) = customer(&app.db, "Acme", "boss@acme.test");
    grant(&app, org);
    let r = op(&app, "GET", &format!("/operator/orgs/{org}/jobs?titles=1")).send();
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["jobs"], json!([]));
    assert_eq!(audit_of(&app, "support.list_jobs").len(), 0);
}

// --- the org page and the overview --------------------------------------------------------------

#[test]
fn the_org_page_shows_the_active_grant_or_null() {
    let app = app();
    let (org, pid) = customer(&app.db, "Acme", "boss@acme.test");
    let j = job(&app.db, org, pid, "raw", b"A", "t");
    let r = op(&app, "GET", &format!("/operator/orgs/{org}")).send();
    assert_eq!(r.json()["support"], Value::Null);

    let g = grant(&app, org);
    payload_of(&app, j);
    let v = op(&app, "GET", &format!("/operator/orgs/{org}"))
        .send()
        .json();
    let s = &v["support"];
    assert_eq!(s["id"], g);
    assert_eq!(s["reason"], REASON);
    assert_eq!(s["reads"], 1);
    assert!(s["granted_at"].is_f64() && s["expires_at"].is_f64());

    op(&app, "DELETE", &format!("/operator/support/{g}")).send();
    let v = op(&app, "GET", &format!("/operator/orgs/{org}"))
        .send()
        .json();
    assert_eq!(v["support"], Value::Null);
}

#[test]
fn the_overview_reports_the_support_feature() {
    let app = app();
    let v = op(&app, "GET", "/operator/overview").send().json();
    assert_eq!(v["features"], json!(["support"]));
}

// --- what the audit rows hold -------------------------------------------------------------------

#[test]
fn audit_rows_hold_no_email_name_or_title() {
    let app = app();
    let (org, pid) = customer(&app.db, "Zebra Corp", "boss@zebra.test");
    let j = job(&app.db, org, pid, "pdf", b"%PDF", "Quarterly invoice");
    let g = grant(&app, org);
    op(&app, "GET", &format!("/operator/orgs/{org}/jobs?titles=1")).send();
    payload_of(&app, j);
    op(&app, "DELETE", &format!("/operator/support/{g}"))
        .json(&json!({"reason": "finished"}))
        .send();
    let stored: String = app.db.with(|c| {
        c.query_row(
            "SELECT group_concat(COALESCE(actor,'')||'|'||COALESCE(action,'')||'|'||\
             COALESCE(target,'')||'|'||COALESCE(reason,'')||'|'||COALESCE(detail,''), char(10)) \
             FROM operator_audit WHERE action LIKE 'support.%'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    });
    assert!(stored.contains("support.read_job"), "{stored}");
    for secret in [
        "boss@zebra.test",
        "zebra",
        "Zebra",
        "Quarterly",
        "invoice",
        "%PDF",
    ] {
        assert!(!stored.contains(secret), "{secret} in {stored}");
    }
    let api = audit_rows(&app);
    let api = serde_json::to_string(&api).unwrap();
    for secret in ["boss@zebra.test", "Zebra", "Quarterly"] {
        assert!(!api.contains(secret), "{secret}");
    }
}

// --- one transaction with the audit row ---------------------------------------------------------

fn break_audit(db: &Db) {
    sql(
        db,
        "CREATE TRIGGER audit_down BEFORE INSERT ON operator_audit \
         BEGIN SELECT RAISE(ABORT, 'audit down'); END",
        [],
    );
}

#[test]
fn a_failing_audit_insert_answers_500_and_discloses_nothing() {
    let app = app();
    let (org, pid) = customer(&app.db, "Acme", "boss@acme.test");
    let j = job(&app.db, org, pid, "raw", b"SECRETBYTES", "Secret title");
    let g = grant(&app, org);
    break_audit(&app.db);

    let r = payload_of(&app, j);
    assert_eq!(r.status, 500);
    assert!(!r.text().contains("SECRETBYTES"));
    assert_eq!(r.header("content-disposition"), None);
    let r = op(&app, "GET", &format!("/operator/orgs/{org}/jobs?titles=1")).send();
    assert_eq!(r.status, 500);
    assert!(!r.text().contains("Secret title"));
    assert_eq!(reads(&app.db, g), 0);

    let r = op(&app, "DELETE", &format!("/operator/support/{g}")).send();
    assert_eq!(r.status, 500);
    assert_eq!(
        count(
            &app.db,
            "SELECT ended_at IS NULL FROM support_grants WHERE id=?",
            [g]
        ),
        1
    );
    sql(&app.db, "DROP TRIGGER audit_down", []);
    // a failed grant leaves no row either
    op(&app, "DELETE", &format!("/operator/support/{g}")).send();
    break_audit(&app.db);
    let r = op(&app, "POST", &format!("/operator/orgs/{org}/support"))
        .json(&grant_body(&app, org))
        .send();
    assert_eq!(r.status, 500);
    assert_eq!(count(&app.db, "SELECT COUNT(*) FROM support_grants", []), 1);
}

// --- inert without the token --------------------------------------------------------------------

#[test]
fn without_a_token_every_support_path_is_a_404() {
    let app = App::new();
    for (m, p) in [
        ("POST", "/operator/orgs/1/support"),
        ("DELETE", "/operator/support/1"),
        ("POST", "/operator/jobs/1/payload"),
        ("GET", "/operator/orgs/1/jobs?titles=1"),
    ] {
        for auth in [None, Some("t"), Some(OP)] {
            let mut c = app.call(m, p);
            if let Some(a) = auth {
                c = c.bearer(a);
            }
            let r = c.send();
            assert_eq!(r.status, 404, "{m} {p} {auth:?}");
        }
    }
}
