// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! `GET /support-access`: what a customer (or root) sees of the support access to their org
//! (spec A6, "PR 2 decisions").
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

fn created_of(db: &Db, org: i64) -> f64 {
    db.with(|c| {
        c.query_row("SELECT created_at FROM orgs WHERE id=?", [org], |r| {
            r.get(0)
        })
        .unwrap()
    })
}

/// An org with one printer: (org id, printer id).
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

fn job(db: &Db, org: i64, printer: i64, title: &str) -> i64 {
    enqueue(db, printer, b"%PDF-x", |j| {
        j.org_id = Some(org);
        j.title = Some(title.to_string());
    })
}

fn grant(app: &App, org: i64, minutes: i64) -> i64 {
    let body = json!({
        "reason": REASON,
        "minutes": minutes,
        "org_created_at": created_of(&app.db, org),
    });
    let r = op(app, "POST", &format!("/operator/orgs/{org}/support"))
        .json(&body)
        .send();
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["id"].as_i64().unwrap()
}

fn grants_of(app: &App, path: &str, token: &str) -> Vec<Value> {
    let (code, v) = app.get(path, Some(token));
    assert_eq!(code, 200, "{v}");
    v["grants"].as_array().unwrap().clone()
}

// --- the gate -----------------------------------------------------------------------------------

#[test]
fn without_a_credential_or_with_a_key_it_is_a_401() {
    let app = app();
    add_key(&app.db, "k", "client-key-1", store::DEFAULT_ORG);
    assert_eq!(app.call("GET", "/support-access").send().status, 401);
    assert_eq!(app.get("/support-access", Some("client-key-1")).0, 401);
    assert_eq!(app.get("/support-access", Some("nope")).0, 401);
}

#[test]
fn a_session_sees_its_own_org_and_not_another() {
    let app = app();
    let (a, _) = customer(&app.db, "Acme", "boss@acme.test");
    let (b, _) = customer(&app.db, "Beta", "boss@beta.test");
    let sess = app.account(a, "me@acme.test");
    let g = grant(&app, a, 30);
    grant(&app, b, 30);

    let own = grants_of(&app, "/support-access", &sess);
    assert_eq!(own.len(), 1);
    assert_eq!(own[0]["id"], g);
    let asked = format!("/support-access?org={a}");
    assert_eq!(grants_of(&app, &asked, &sess).len(), 1);
    let foreign = format!("/support-access?org={b}");
    assert_eq!(app.get(&foreign, Some(&sess)).0, 403);
    assert_eq!(app.get("/support-access?org=99999", Some(&sess)).0, 403);
}

#[test]
fn root_reads_the_default_org_or_the_one_asked_for() {
    let app = app();
    let (a, _) = customer(&app.db, "Acme", "boss@acme.test");
    let d = grant(&app, store::DEFAULT_ORG, 15);
    let g = grant(&app, a, 60);

    let default = grants_of(&app, "/support-access", "t");
    assert_eq!(default.len(), 1);
    assert_eq!(default[0]["id"], d);
    let asked = grants_of(&app, &format!("/support-access?org={a}"), "t");
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0]["id"], g);
    assert_eq!(app.get("/support-access?org=99999", Some("t")).0, 404);
}

#[test]
fn it_answers_no_store() {
    let app = app();
    let r = app.call("GET", "/support-access").bearer("t").send();
    assert_eq!(r.status, 200);
    assert_eq!(r.header("cache-control"), Some("no-store"));
}

// --- enabled ------------------------------------------------------------------------------------

#[test]
fn enabled_follows_the_operator_token() {
    let on = app();
    let (code, v) = on.get("/support-access", Some("t"));
    assert_eq!((code, v["enabled"].clone()), (200, json!(true)));
    assert_eq!(v["grants"], json!([]));

    let off = App::new();
    let (code, v) = off.get("/support-access", Some("t"));
    assert_eq!(code, 200);
    assert_eq!(v, json!({"enabled": false, "grants": []}));
    // still behind the gate
    assert_eq!(off.call("GET", "/support-access").send().status, 401);
}

#[test]
fn with_the_token_unset_old_grants_are_not_listed() {
    let db = mem();
    let on = App::on(db.clone(), hosted_cfg);
    let (a, _) = customer(&db, "Acme", "boss@acme.test");
    grant(&on, a, 30);
    let off = App::on(db, |_| {});
    let (code, v) = off.get(&format!("/support-access?org={a}"), Some("t"));
    assert_eq!((code, v), (200, json!({"enabled": false, "grants": []})));
}

// --- the fields ---------------------------------------------------------------------------------

#[test]
fn a_grant_lists_its_fields_newest_first() {
    let app = app();
    let (a, _) = customer(&app.db, "Acme", "boss@acme.test");
    let first = grant(&app, a, 15);
    let r = op(&app, "DELETE", &format!("/operator/support/{first}")).send();
    assert_eq!(r.status, 200, "{}", r.text());
    let second = grant(&app, a, 30);

    let gs = grants_of(&app, &format!("/support-access?org={a}"), "t");
    assert_eq!(gs.len(), 2);
    assert_eq!(gs[0]["id"], second);
    assert_eq!(gs[1]["id"], first);
    let g = &gs[0];
    assert_eq!(g["reason"], REASON);
    assert!(g["granted_at"].is_f64());
    assert!(g["expires_at"].as_f64().unwrap() > g["granted_at"].as_f64().unwrap());
    assert_eq!(g["ended_at"], Value::Null);
    assert_eq!(g["active"], true);
    assert_eq!(g["listed_job_ids"], json!([]));
    assert_eq!(g["opened_job_ids"], json!([]));
    assert!(gs[1]["ended_at"].is_f64());
}

#[test]
fn an_ended_or_expired_grant_is_not_active() {
    let app = app();
    let (a, _) = customer(&app.db, "Acme", "boss@acme.test");
    let ended = grant(&app, a, 30);
    op(&app, "DELETE", &format!("/operator/support/{ended}")).send();
    let expired = grant(&app, a, 30);
    app.db.with(|c| {
        c.execute(
            "UPDATE support_grants SET expires_at = ? WHERE id = ?",
            rusqlite::params![1.0, expired],
        )
        .unwrap()
    });
    let live = grant(&app, a, 30);

    let gs = grants_of(&app, &format!("/support-access?org={a}"), "t");
    let active = |id: i64| gs.iter().find(|g| g["id"] == id).unwrap()["active"].clone();
    assert_eq!(active(ended), false);
    assert_eq!(active(expired), false);
    assert_eq!(active(live), true);
}

#[test]
fn job_ids_are_deduplicated_and_ascending() {
    let app = app();
    let (a, p) = customer(&app.db, "Acme", "boss@acme.test");
    let ids: Vec<i64> = (0..3)
        .map(|i| job(&app.db, a, p, &format!("t{i}")))
        .collect();
    let g = grant(&app, a, 30);
    // list twice (the same ids both times), open the last and the first, the last again
    for _ in 0..2 {
        let r = op(&app, "GET", &format!("/operator/orgs/{a}/jobs?titles=1")).send();
        assert_eq!(r.status, 200, "{}", r.text());
    }
    for id in [ids[2], ids[0], ids[2]] {
        let r = op(&app, "POST", &format!("/operator/jobs/{id}/payload")).send();
        assert_eq!(r.status, 200);
    }
    let mut want = ids.clone();
    want.sort();
    let gs = grants_of(&app, &format!("/support-access?org={a}"), "t");
    assert_eq!(gs[0]["id"], g);
    assert_eq!(gs[0]["listed_job_ids"], json!(want));
    assert_eq!(gs[0]["opened_job_ids"], json!([ids[0], ids[2]]));
}

#[test]
fn a_grant_of_an_earlier_generation_of_the_org_is_not_shown() {
    let app = app();
    let (a, _) = customer(&app.db, "Acme", "boss@acme.test");
    let sess = app.account(a, "me@acme.test");
    grant(&app, a, 30);
    assert_eq!(grants_of(&app, "/support-access", &sess).len(), 1);
    app.db.with(|c| {
        c.execute(
            "UPDATE orgs SET created_at = created_at + 5 WHERE id = ?",
            [a],
        )
        .unwrap()
    });
    assert_eq!(grants_of(&app, "/support-access", &sess).len(), 0);
}

#[test]
fn at_most_fifty_grants_are_listed() {
    let app = app();
    let (a, _) = customer(&app.db, "Acme", "boss@acme.test");
    let created = created_of(&app.db, a);
    app.db.with(|c| {
        for _ in 0..55 {
            c.execute(
                "INSERT INTO support_grants(org_id, org_created_at, reason, granted_at, expires_at, ended_at) \
                 VALUES(?, ?, 'old ticket', 1.0, 2.0, 2.0)",
                rusqlite::params![a, created],
            )
            .unwrap();
        }
    });
    let asked = format!("/support-access?org={a}");
    assert_eq!(grants_of(&app, &asked, "t").len(), 50);
}
