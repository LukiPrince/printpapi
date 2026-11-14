// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The operator API: the gate (token + socket peer), hosted-mode plumbing, the audit log and the
//! overview. Everything is inert without `PRINTAPI_OPERATOR_TOKEN`.
mod common;

use common::*;
use printpapi_server::dispatch::HttpFetcher;
use printpapi_server::http::spawn_test_server;
use printpapi_server::mail::SmtpMailer;
use printpapi_server::proxy::TrustedProxies;
use printpapi_server::server::{AppState, Config};
use printpapi_server::store::{self, AuditRow, Db};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Duration;

const OP: &str = "operator-token-0123456789abcdef-xyz";
const CONSOLE: &str = "10.9.0.2";

fn hosted_cfg(c: &mut Config) {
    c.operator_token = Some(OP.into());
    c.operator_from = TrustedProxies::parse(CONSOLE).unwrap();
}

fn hosted() -> App {
    App::with(hosted_cfg)
}

/// A call as the console makes it: the operator token from the console's address.
fn op<'a>(app: &'a App, method: &str, target: &str) -> Call<'a> {
    app.call(method, target).bearer(OP).socket(CONSOLE)
}

fn audit(db: &Db, action: &str, org: Option<i64>) -> i64 {
    db.with(|c| {
        let tx = store::begin_tx(c).unwrap();
        let id = store::insert_audit(
            &tx,
            &AuditRow {
                action,
                org_id: org,
                org_created_at: org.map(|_| 1_700_000_000.0),
                target: Some("t:1"),
                reason: Some("because"),
                detail: Some(r#"{"quota":5}"#),
            },
        )
        .unwrap();
        tx.commit().unwrap();
        id
    })
}

// --- the gate -----------------------------------------------------------------------------------

#[test]
fn without_a_token_every_operator_path_is_a_404() {
    let app = App::new();
    for (m, p) in [
        ("GET", "/operator/overview"),
        ("GET", "/operator/audit"),
        ("GET", "/operator/nothing"),
        ("POST", "/operator/orgs"),
        ("DELETE", "/operator/orgs/1"),
    ] {
        for auth in [None, Some("t"), Some(OP)] {
            let mut c = app.call(m, p);
            if let Some(a) = auth {
                c = c.bearer(a);
            }
            let r = c.send();
            assert_eq!(r.status, 404, "{m} {p} {auth:?}");
            assert_eq!(r.header("cache-control"), Some("no-store"), "{m} {p}");
        }
    }
}

#[test]
fn the_token_works_from_a_listed_peer_and_nowhere_else() {
    let app = hosted();
    let r = op(&app, "GET", "/operator/overview").send();
    assert_eq!(r.status, 200, "{}", r.text());
    // another socket peer: the token is an unknown credential
    let r = op(&app, "GET", "/operator/overview")
        .socket("127.0.0.1")
        .send();
    assert_eq!(r.status, 401);
    // no credential, a wrong one
    assert_eq!(
        app.call("GET", "/operator/overview")
            .socket(CONSOLE)
            .send()
            .status,
        401
    );
    let r = app
        .call("GET", "/operator/overview")
        .bearer("not-the-token-not-the-token-not-the-token")
        .socket(CONSOLE)
        .send();
    assert_eq!(r.status, 401);
}

#[test]
fn root_is_refused_on_operator_paths() {
    let app = hosted();
    for peer in [CONSOLE, "127.0.0.1"] {
        let r = app
            .call("GET", "/operator/overview")
            .bearer("t")
            .socket(peer)
            .send();
        assert_eq!(r.status, 401, "{peer}");
    }
}

#[test]
fn a_header_naming_the_console_does_not_make_the_peer_the_console() {
    // the resolved client (X-Forwarded-For behind a trusted proxy) is the console's address, the
    // socket peer is not
    let app = hosted();
    let r = op(&app, "GET", "/operator/overview")
        .peer(CONSOLE)
        .socket("203.0.113.9")
        .send();
    assert_eq!(r.status, 401);
}

fn raw(addr: std::net::SocketAddr, extra: &str) -> String {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(
        s,
        "GET /operator/overview HTTP/1.1\r\nHost: t\r\n{extra}Connection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

fn wire_state(tweak: impl FnOnce(&mut Config)) -> Arc<AppState> {
    let mut cfg = Config::new("t");
    cfg.long_poll_timeout_s = 0.3;
    cfg.poll_interval_s = 0.05;
    tweak(&mut cfg);
    Arc::new(AppState::new(
        cfg,
        Db::open(":memory:").unwrap(),
        Arc::new(SmtpMailer),
        Arc::new(HttpFetcher::new(Duration::from_secs(1))),
    ))
}

#[test]
fn over_the_wire_the_gate_reads_the_socket_peer_not_x_forwarded_for() {
    let auth = format!("Authorization: Bearer {OP}\r\n");
    // a trusted proxy (the loopback peer) forwards a header naming the console: still refused
    let addr = spawn_test_server(wire_state(|c| {
        c.operator_token = Some(OP.into());
        c.operator_from = TrustedProxies::parse(CONSOLE).unwrap();
        c.trusted_proxies = TrustedProxies::parse("127.0.0.1").unwrap();
    }));
    let r = raw(addr, &format!("{auth}X-Forwarded-For: {CONSOLE}\r\n"));
    assert!(r.starts_with("HTTP/1.1 401"), "{r}");
    assert!(r.to_lowercase().contains("cache-control: no-store"), "{r}");
    // the real socket peer in the list: accepted, and never cached
    let addr = spawn_test_server(wire_state(|c| {
        c.operator_token = Some(OP.into());
        c.operator_from = TrustedProxies::parse("127.0.0.1").unwrap();
    }));
    let r = raw(addr, &auth);
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    assert!(r.to_lowercase().contains("cache-control: no-store"), "{r}");
}

#[test]
fn every_operator_response_is_no_store() {
    let app = hosted();
    for (m, p, peer, tok) in [
        ("GET", "/operator/overview", CONSOLE, Some(OP)),
        ("GET", "/operator/audit", CONSOLE, Some(OP)),
        ("GET", "/operator/nothing", CONSOLE, Some(OP)),
        ("GET", "/operator/overview", "127.0.0.1", Some(OP)),
        ("GET", "/operator/overview", CONSOLE, None),
        ("POST", "/operator/audit", CONSOLE, Some(OP)),
        ("HEAD", "/operator/audit", CONSOLE, Some(OP)),
    ] {
        let mut c = app.call(m, p).socket(peer);
        if let Some(t) = tok {
            c = c.bearer(t);
        }
        let r = c.send();
        assert_eq!(
            r.header("cache-control"),
            Some("no-store"),
            "{m} {p} {peer}"
        );
    }
}

#[test]
fn an_unknown_operator_path_is_a_404_behind_the_gate() {
    let app = hosted();
    assert_eq!(op(&app, "GET", "/operator/nothing").send().status, 404);
    assert_eq!(op(&app, "GET", "/operator/").send().status, 404);
    // and a 401 before it, so the gate does not tell which paths exist
    let r = app.call("GET", "/operator/nothing").socket(CONSOLE).send();
    assert_eq!(r.status, 401);
}

// --- the operator token is no credential anywhere else ------------------------------------------

/// Every route of the docs/api.md table, concrete.
fn routes() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GET", "/"),
        ("GET", "/health"),
        ("GET", "/metrics"),
        ("POST", "/jobs"),
        ("GET", "/labels"),
        ("POST", "/labels"),
        ("GET", "/labels/1"),
        ("PUT", "/labels/1"),
        ("DELETE", "/labels/1"),
        ("POST", "/orders"),
        ("POST", "/integrations/shopify/orders"),
        ("GET", "/jobs"),
        ("GET", "/jobs/1"),
        ("DELETE", "/jobs/1"),
        ("GET", "/printers"),
        ("GET", "/computers"),
        ("POST", "/login"),
        ("POST", "/signup"),
        ("POST", "/password/reset"),
        ("POST", "/password/reset/confirm"),
        ("POST", "/logout"),
        ("GET", "/me"),
        ("PUT", "/me/password"),
        ("POST", "/users"),
        ("GET", "/users"),
        ("DELETE", "/users/1"),
        ("POST", "/orgs/1/users"),
        ("POST", "/orgs"),
        ("GET", "/orgs"),
        ("GET", "/orgs/1"),
        ("PUT", "/orgs/1"),
        ("DELETE", "/orgs/1"),
        ("GET", "/plans"),
        ("POST", "/billing/webhook"),
        ("POST", "/apikeys"),
        ("GET", "/apikeys"),
        ("DELETE", "/apikeys/1"),
        ("POST", "/agent/register"),
        ("GET", "/agent/jobs"),
        ("GET", "/agent/jobs/1/payload"),
        ("POST", "/agent/jobs/1/result"),
        ("POST", "/cloudprnt"),
        ("GET", "/cloudprnt"),
        ("DELETE", "/cloudprnt"),
    ]
}

/// A server where the operator token happens to also be a valid client key and agent key: if any
/// route resolved it as a credential, it would answer differently from an anonymous request.
fn hosted_with_lookalike_keys(open: bool) -> App {
    let app = App::with(|c| {
        hosted_cfg(c);
        c.open_enrollment = open;
    });
    add_key(&app.db, "k", OP, store::DEFAULT_ORG);
    register(&app.db, "pc", OP, &[decl("Z", false)], store::DEFAULT_ORG);
    app
}

#[test]
fn the_operator_token_is_no_credential_on_any_other_route() {
    let app = hosted_with_lookalike_keys(false);
    for (m, p) in routes() {
        let body = json!({"name": "x", "printers": [], "email": "a@b.test", "password": PW});
        let anon = app.call(m, p).json(&body).send().status;
        for (how, c) in [
            ("bearer", app.call(m, p).bearer(OP)),
            ("basic", app.call(m, p).basic(OP)),
            (
                "bearer from console",
                app.call(m, p).bearer(OP).socket(CONSOLE),
            ),
        ] {
            let r = c.json(&body).send();
            assert_eq!(r.status, anon, "{m} {p} as {how}: {}", r.text());
        }
    }
    // and the path key of a CloudPRNT URL
    let anon = app.call("POST", "/cloudprnt/zzz").json(&json!({})).send();
    let r = app
        .call("POST", &format!("/cloudprnt/{OP}"))
        .json(&json!({"printerMAC": "00:11:22:33:44:55"}))
        .send();
    assert_eq!(r.status, anon.status, "{}", r.text());
}

#[test]
fn enrollment_refuses_the_operator_token_even_with_open_enrollment() {
    let app = App::with(|c| {
        hosted_cfg(c);
        c.open_enrollment = true;
    });
    let body = json!({"name": "evil", "printers": [{"name": "P", "can_pdf": false}]});
    for c in [
        app.call("POST", "/agent/register").bearer(OP),
        app.call("POST", "/agent/register").basic(OP),
    ] {
        let r = c.json(&body).send();
        assert_eq!(r.status, 401, "{}", r.text());
    }
    assert_eq!(app.get("/computers", Some("t")).1, json!({"computers": []}));
    // any other value still enrolls with open enrollment: that rule is unchanged
    let r = app
        .call("POST", "/agent/register")
        .bearer("whatever")
        .json(&body)
        .send();
    assert_eq!(r.status, 200, "{}", r.text());
}

#[test]
fn the_long_poll_refuses_the_operator_token_too() {
    let state = wire_state(hosted_cfg);
    state.db.with(|c| {
        store::register_agent(c, "pc", OP, &[decl("Z", false)], 1).unwrap();
    });
    let addr = spawn_test_server(state);
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(
        s,
        "GET /agent/jobs HTTP/1.1\r\nHost: t\r\nAuthorization: Bearer {OP}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let out = String::from_utf8_lossy(&out);
    assert!(out.starts_with("HTTP/1.1 401"), "{out}");
}

// --- audit ----------------------------------------------------------------------------------------

#[test]
fn the_audit_log_lists_newest_first_with_every_column() {
    let app = hosted();
    let a = audit(&app.db, "org.update", Some(2));
    let b = audit(&app.db, "org.lock", Some(3));
    let (st, v) = {
        let r = op(&app, "GET", "/operator/audit").send();
        (r.status, r.json())
    };
    assert_eq!(st, 200, "{v}");
    let rows = v["entries"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], b);
    assert_eq!(rows[1]["id"], a);
    let r = &rows[0];
    assert_eq!(r["actor"], "operator");
    assert_eq!(r["action"], "org.lock");
    assert_eq!(r["org_id"], 3);
    assert_eq!(r["org_created_at"], 1_700_000_000.0);
    assert_eq!(r["target"], "t:1");
    assert_eq!(r["reason"], "because");
    assert_eq!(r["detail"], json!({"quota": 5}));
    assert!(r["at"].as_f64().unwrap() > 1.0e9);
    assert!(v["next_before_id"].is_null());
}

#[test]
fn audit_pages_by_before_id() {
    let app = hosted();
    let ids: Vec<i64> = (0..5)
        .map(|_| audit(&app.db, "org.update", Some(2)))
        .collect();
    let (_, v) = app.get_op("/operator/audit?limit=2");
    let got: Vec<i64> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_i64().unwrap())
        .collect();
    assert_eq!(got, vec![ids[4], ids[3]]);
    assert_eq!(v["next_before_id"], ids[3]);
    let (_, v) = app.get_op(&format!("/operator/audit?limit=2&before_id={}", ids[3]));
    let got: Vec<i64> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_i64().unwrap())
        .collect();
    assert_eq!(got, vec![ids[2], ids[1]]);
    let (_, v) = app.get_op(&format!("/operator/audit?limit=2&before_id={}", ids[1]));
    assert_eq!(v["entries"].as_array().unwrap().len(), 1);
    assert!(v["next_before_id"].is_null());
}

#[test]
fn audit_filters_by_org_and_action_and_caps_the_limit() {
    let app = hosted();
    audit(&app.db, "org.update", Some(2));
    audit(&app.db, "org.lock", Some(2));
    audit(&app.db, "org.update", Some(3));
    let (_, v) = app.get_op("/operator/audit?org=2");
    assert_eq!(v["entries"].as_array().unwrap().len(), 2);
    let (_, v) = app.get_op("/operator/audit?action=org.update");
    assert_eq!(v["entries"].as_array().unwrap().len(), 2);
    let (_, v) = app.get_op("/operator/audit?org=2&action=org.update");
    assert_eq!(v["entries"].as_array().unwrap().len(), 1);
    for _ in 0..205 {
        audit(&app.db, "org.update", Some(4));
    }
    let (_, v) = app.get_op("/operator/audit?limit=1000");
    assert_eq!(v["entries"].as_array().unwrap().len(), 200);
    assert!(v["next_before_id"].is_i64());
}

#[test]
fn audit_rejects_bad_parameters() {
    let app = hosted();
    for q in ["limit=abc", "limit=0", "limit=-1", "before_id=x", "org=x"] {
        let (st, _) = app.get_op(&format!("/operator/audit?{q}"));
        assert_eq!(st, 400, "{q}");
    }
}

#[test]
fn the_audit_log_is_read_only_here() {
    let app = hosted();
    for m in ["POST", "PUT", "DELETE"] {
        assert_eq!(op(&app, m, "/operator/audit").send().status, 404, "{m}");
    }
}

// --- overview -------------------------------------------------------------------------------------

trait OpGet {
    fn get_op(&self, target: &str) -> (u16, Value);
}

impl OpGet for App {
    fn get_op(&self, target: &str) -> (u16, Value) {
        let r = op(self, "GET", target).send();
        (r.status, r.json())
    }
}

#[test]
fn overview_counts_orgs_jobs_and_failures() {
    let app = hosted();
    let (st, v) = app.get_op("/operator/overview");
    assert_eq!(st, 200);
    // the seeded default org only; it is not a signup
    assert_eq!(v["orgs"], 1);
    assert_eq!(v["signups_7d"], 0);
    assert_eq!(v["jobs_this_month"], 0);
    assert_eq!(v["features"], json!([]));
    assert_eq!(
        v["jobs_today"],
        json!({"queued": 0, "claimed": 0, "done": 0, "failed": 0, "cancelled": 0})
    );
}

#[test]
fn overview_reflects_data() {
    let app = hosted();
    let org = create_org(&app.db, "Shop");
    let reg = register(&app.db, "pc", "ak", &[decl("Z", false)], org);
    let pid = reg.pid("Z");
    let done = enqueue(&app.db, pid, b"secret-payload", |j| {
        j.org_id = Some(org);
        j.title = Some("secret-title".into());
    });
    let failed = enqueue(&app.db, pid, b"x", |j| j.org_id = Some(org));
    let _queued = enqueue(&app.db, pid, b"x", |j| j.org_id = Some(org));
    claim(&app.db, reg.computer_id);
    finish(&app.db, done, reg.computer_id, true, None);
    claim(&app.db, reg.computer_id);
    finish(&app.db, failed, reg.computer_id, false, Some("boom"));
    let (_, v) = app.get_op("/operator/overview");
    assert_eq!(v["orgs"], 2);
    assert_eq!(v["signups_7d"], 1);
    assert_eq!(v["orgs_active_7d"], 1);
    assert_eq!(v["jobs_this_month"], 3);
    assert_eq!(v["failed_jobs_30d"], 1);
    assert_eq!(
        v["jobs_today"],
        json!({"queued": 1, "claimed": 0, "done": 1, "failed": 1, "cancelled": 0})
    );
    let text = v.to_string();
    assert!(
        !text.contains("secret-title") && !text.contains("secret-payload"),
        "{text}"
    );
}

#[test]
fn overview_reports_the_server() {
    let app = hosted();
    let (_, v) = app.get_op("/operator/overview");
    assert_eq!(v["server"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(v["server"]["uptime_s"].as_f64().unwrap() >= 0.0);
    assert!(v["db"]["size_bytes"].as_i64().unwrap() > 0);
    assert_eq!(v["db"]["wal_bytes"], 0);
    assert!(v["retention"]["last_pass_at"].is_null());
}

#[test]
fn overview_reports_the_wal_of_a_file_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.db");
    let db = Db::open(path.to_str().unwrap()).unwrap();
    create_org(&db, "Shop");
    let app = App::on(db, hosted_cfg);
    let (_, v) = app.get_op("/operator/overview");
    let wal = std::fs::metadata(dir.path().join("p.db-wal")).map_or(0, |m| m.len());
    assert!(wal > 0, "the write left a WAL");
    assert_eq!(v["db"]["wal_bytes"], wal);
    assert!(v["db"]["size_bytes"].as_i64().unwrap() > 0);
}

#[test]
fn overview_reports_the_last_retention_pass() {
    let app = App::with(|c| {
        hosted_cfg(c);
        c.job_retention_days = Some(7);
    });
    let now = printpapi_server::now();
    printpapi_server::server::retention_pass(&app.state, now).expect("a pass ran");
    let (_, v) = app.get_op("/operator/overview");
    let at = v["retention"]["last_pass_at"].as_f64().unwrap();
    assert!((at - now).abs() < 1.0);
    for k in ["abandoned", "scrubbed", "deleted"] {
        assert_eq!(v["retention"][k], 0, "{k}");
    }
}

#[test]
fn overview_is_cached_for_ten_seconds() {
    let app = hosted();
    let (_, first) = app.get_op("/operator/overview");
    create_org(&app.db, "Late");
    let (_, second) = app.get_op("/operator/overview");
    assert_eq!(first["orgs"], 1);
    assert_eq!(second, first);
}

// --- read: orgs and their jobs ----------------------------------------------------------------------

const DAY: f64 = 86_400.0;

fn now() -> f64 {
    printpapi_server::now()
}

fn sql(db: &Db, q: &str, p: impl rusqlite::Params) {
    db.with(|c| c.execute(q, p).unwrap());
}

/// A self-signup the way /signup makes one: org, first user, accepted terms. Returns the org id.
fn signup(db: &Db, name: &str, email: &str, terms: Option<&str>) -> i64 {
    db.with(|c| store::signup(c, name, email, "PWHASHSENTINEL", None, terms))
        .unwrap()
        .org_id
}

fn age_org(db: &Db, org: i64, days: f64) {
    sql(
        db,
        "UPDATE orgs SET created_at=? WHERE id=?",
        rusqlite::params![now() - days * DAY, org],
    );
}

/// A job of `org` on its printer (made on first use), created `ago` seconds ago.
fn job_at(app: &App, org: i64, ago: f64, tweak: impl FnOnce(&mut store::NewJob<'_>)) -> i64 {
    let existing = app
        .db
        .with(|c| {
            c.query_row(
                "SELECT id FROM printers WHERE org_id=? LIMIT 1",
                [org],
                |r| r.get::<_, i64>(0),
            )
        })
        .ok();
    let pid = match existing {
        Some(p) => p,
        None => register(
            &app.db,
            &format!("pc{org}"),
            &format!("agent-key-{org}"),
            &[decl("Z", false)],
            org,
        )
        .pid("Z"),
    };
    let id = enqueue(&app.db, pid, b"PAYLOADSENTINEL", |j| {
        j.org_id = Some(org);
        tweak(j)
    });
    sql(
        &app.db,
        "UPDATE jobs SET created_at=? WHERE id=?",
        rusqlite::params![now() - ago, id],
    );
    id
}

fn org_row(v: &Value, id: i64) -> &Value {
    v["orgs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"] == id)
        .unwrap_or_else(|| panic!("org {id} not in {v}"))
}

fn names(v: &Value) -> Vec<String> {
    v["orgs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap().to_string())
        .collect()
}

fn ids_of(v: &Value, key: &str) -> Vec<i64> {
    v[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_i64().unwrap())
        .collect()
}

#[test]
fn org_read_endpoints_need_the_gate_and_are_get_only() {
    let app = hosted();
    for t in [
        "/operator/orgs",
        "/operator/orgs/1",
        "/operator/orgs/1/jobs",
    ] {
        assert_eq!(app.call("GET", t).socket(CONSOLE).send().status, 401, "{t}");
        assert_eq!(
            app.call("GET", t).bearer("t").socket(CONSOLE).send().status,
            401,
            "{t}"
        );
        // /operator/orgs/{id} has writes of its own now; the list and the jobs stay read-only
        if t != "/operator/orgs/1" {
            assert_eq!(op(&app, "DELETE", t).send().status, 404, "DELETE {t}");
        }
    }
    let r = op(&app, "GET", "/operator/orgs").send();
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.header("cache-control"), Some("no-store"));
}

#[test]
fn org_list_has_every_field_with_its_numbers() {
    let app = hosted();
    let acme = signup(&app.db, "Acme", "Boss@Acme.test", Some("2026-11"));
    age_org(&app.db, acme, 30.0);
    sql(
        &app.db,
        "UPDATE terms_acceptances SET accepted_at=? WHERE org_id=?",
        rusqlite::params![1_700_000_000.0, acme],
    );
    sql(
        &app.db,
        "UPDATE orgs SET plan='pro', job_quota=100 WHERE id=?",
        [acme],
    );
    let month = store::month_start(now());
    // one failed job today, one queued job from last month, one done job this month
    let failed = job_at(&app, acme, 60.0, |_| {});
    let queued = job_at(&app, acme, 5.0 * DAY, |_| {});
    let done = job_at(&app, acme, 120.0, |_| {});
    sql(
        &app.db,
        "UPDATE jobs SET state='failed' WHERE id=?",
        [failed],
    );
    sql(&app.db, "UPDATE jobs SET state='done' WHERE id=?", [done]);
    sql(
        &app.db,
        "UPDATE jobs SET created_at=? WHERE id=?",
        rusqlite::params![month - 10.0, queued],
    );
    // two agents: one seen just now, one long ago
    let a2 = register(&app.db, "fresh", "k2", &[decl("L", true)], acme);
    let seen = now() - 30.0;
    sql(
        &app.db,
        "UPDATE agents SET last_seen_at=? WHERE id=?",
        rusqlite::params![seen, a2.computer_id],
    );
    sql(
        &app.db,
        "UPDATE agents SET last_seen_at=? WHERE org_id=? AND id!=?",
        rusqlite::params![now() - 3.0 * DAY, acme, a2.computer_id],
    );

    let (st, v) = app.get_op("/operator/orgs");
    assert_eq!(st, 200, "{v}");
    let o = org_row(&v, acme);
    assert_eq!(o["name"], "Acme");
    assert_eq!(o["plan"], "pro");
    assert_eq!(o["job_quota"], 100);
    assert_eq!(o["jobs_this_month"], 2, "{o}");
    assert_eq!(o["queued_jobs"], 1, "{o}");
    assert_eq!(o["failed_today"], 1);
    assert_eq!(o["agents_total"], 2);
    assert_eq!(o["agents_online"], 1);
    assert_eq!(o["printers"], 2);
    assert!((o["newest_agent_seen_at"].as_f64().unwrap() - seen).abs() < 1.0);
    assert!(o["last_job_at"].as_f64().unwrap() > now() - 200.0);
    assert_eq!(o["locked"], false);
    assert_eq!(o["first_user_email"], "boss@acme.test");
    assert_eq!(o["terms_version"], "2026-11");
    assert_eq!(o["terms_accepted_at"], 1_700_000_000.0);
    assert!(o["created_at"].as_f64().unwrap() < now() - 29.0 * DAY);
    assert_eq!(o["status"], "active");
    assert!(v["next_after"].is_null());
}

#[test]
fn org_status_is_locked_new_idle_or_active_and_filters() {
    let app = hosted();
    let fresh = signup(&app.db, "Fresh", "f@x.test", None);
    let idle = signup(&app.db, "Idle", "i@x.test", None);
    let active = signup(&app.db, "Active", "a@x.test", None);
    let quiet = signup(&app.db, "OldQuiet", "q@x.test", None);
    let locked = signup(&app.db, "Locked", "l@x.test", None);
    for o in [store::DEFAULT_ORG, idle, active, quiet, locked] {
        age_org(&app.db, o, 40.0);
    }
    job_at(&app, idle, 20.0 * DAY, |_| {});
    job_at(&app, active, 2.0 * DAY, |_| {});
    // a locked org is locked even when it is new and busy
    age_org(&app.db, locked, 1.0);
    job_at(&app, locked, 60.0, |_| {});
    sql(
        &app.db,
        "INSERT INTO org_locks(org_id, org_created_at, reason, locked_at) \n         SELECT id, created_at, ?, ? FROM orgs WHERE id=?",
        rusqlite::params!["unpaid invoice", now(), locked],
    );
    let (st, v) = app.get_op("/operator/orgs");
    assert_eq!(st, 200, "{v}");
    assert_eq!(org_row(&v, fresh)["status"], "new");
    assert_eq!(org_row(&v, idle)["status"], "idle");
    assert_eq!(org_row(&v, active)["status"], "active");
    assert_eq!(org_row(&v, quiet)["status"], "idle");
    assert_eq!(org_row(&v, locked)["status"], "locked");
    assert_eq!(org_row(&v, locked)["locked"], true);
    for (s, want) in [
        ("locked", vec!["Locked"]),
        ("new", vec!["Fresh"]),
        ("active", vec!["Active"]),
    ] {
        let (st, v) = app.get_op(&format!("/operator/orgs?status={s}"));
        assert_eq!(st, 200, "{v}");
        assert_eq!(names(&v), want, "{s}");
    }
    let (_, v) = app.get_op("/operator/orgs?status=idle");
    let got = names(&v);
    assert!(got.contains(&"Idle".to_string()) && got.contains(&"OldQuiet".to_string()));
    assert!(!got.contains(&"Active".to_string()));
    assert_eq!(app.get_op("/operator/orgs?status=bogus").0, 400);
}

#[test]
fn org_search_matches_name_or_email_case_insensitively() {
    let app = hosted();
    signup(&app.db, "Mueller Druck", "chef@mueller.test", None);
    signup(&app.db, "Beta", "Anna@Gamma.test", None);
    signup(&app.db, "Delta 100%", "d@x.test", None);
    let q = |s: &str| {
        let (st, v) = app.get_op(&format!("/operator/orgs?q={s}"));
        assert_eq!(st, 200, "q={s}: {v}");
        names(&v)
    };
    assert_eq!(q("mueller%20dr"), vec!["Mueller Druck"]);
    assert_eq!(q("BETA"), vec!["Beta"]);
    assert_eq!(q("anna@gamma"), vec!["Beta"]);
    assert_eq!(q("CHEF@MUELLER"), vec!["Mueller Druck"]);
    // % and _ are literal
    assert_eq!(q("100%25"), vec!["Delta 100%"]);
    assert_eq!(q("%25"), vec!["Delta 100%"]);
    assert!(q("_").is_empty());
    assert!(q("nothing-like-this").is_empty());
}

#[test]
fn org_list_pages_by_id() {
    let app = hosted();
    for i in 0..5 {
        signup(&app.db, &format!("O{i}"), &format!("o{i}@x.test"), None);
    }
    // 6 orgs in all, the seeded default one included
    let (st, v) = app.get_op("/operator/orgs?limit=4");
    assert_eq!(st, 200, "{v}");
    let first = ids_of(&v, "orgs");
    assert_eq!(first.len(), 4);
    assert!(first.windows(2).all(|w| w[0] < w[1]), "ascending by id");
    let next = v["next_after"].as_i64().unwrap();
    assert_eq!(next, *first.last().unwrap());
    let (_, v) = app.get_op(&format!("/operator/orgs?limit=4&after={next}"));
    let rest = ids_of(&v, "orgs");
    assert_eq!(rest.len(), 2);
    assert!(rest[0] > next);
    assert!(v["next_after"].is_null());
    // the filter pages too
    let (_, v) = app.get_op("/operator/orgs?q=x.test&status=new&limit=2");
    assert_eq!(v["orgs"].as_array().unwrap().len(), 2);
    assert!(v["next_after"].is_i64());
    for bad in ["limit=0", "limit=x", "after=x"] {
        assert_eq!(app.get_op(&format!("/operator/orgs?{bad}")).0, 400, "{bad}");
    }
    for i in 0..205 {
        signup(&app.db, "Bulk", &format!("bulk{i}@b.test"), None);
    }
    let (_, v) = app.get_op("/operator/orgs?limit=1000");
    assert_eq!(v["orgs"].as_array().unwrap().len(), 200);
    assert!(v["next_after"].is_i64());
}

#[test]
fn org_detail_has_usage_users_agents_printers_keys_terms_and_lock() {
    let app = hosted();
    let acme = signup(&app.db, "Acme", "boss@acme.test", Some("v1"));
    sql(
        &app.db,
        "INSERT INTO terms_acceptances(org_id, user_id, version, accepted_at) VALUES(?,1,'v2',?)",
        rusqlite::params![acme, now() + 1.0],
    );
    let month = store::month_start(now());
    let j1 = job_at(&app, acme, 1.0, |_| {});
    let j2 = job_at(&app, acme, 2.0, |_| {});
    let j0 = job_at(&app, acme, 3.0, |_| {});
    sql(&app.db, "UPDATE jobs SET state='failed' WHERE id=?", [j1]);
    // last month does not count; the first day of this month does
    sql(
        &app.db,
        "UPDATE jobs SET created_at=? WHERE id=?",
        rusqlite::params![month - 5.0, j0],
    );
    sql(
        &app.db,
        "UPDATE jobs SET created_at=? WHERE id=?",
        rusqlite::params![month + 5.0, j2],
    );
    add_key(&app.db, "shop", "KEYSECRET-1234567890", acme);
    sql(
        &app.db,
        "UPDATE agents SET last_seen_at=? WHERE org_id=?",
        rusqlite::params![now() - 3600.0, acme],
    );
    let (st, v) = app.get_op(&format!("/operator/orgs/{acme}"));
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["name"], "Acme");
    assert_eq!(v["status"], "new");
    assert_eq!(v["jobs_this_month"], 2);
    assert!(v["lock"].is_null());
    let usage = v["usage_by_day"].as_array().unwrap();
    assert_eq!(usage[0]["jobs"], 1, "{usage:?}");
    assert!(usage[0]["date"].as_str().unwrap().ends_with("-01"));
    let total: i64 = usage.iter().map(|d| d["jobs"].as_i64().unwrap()).sum();
    let failed: i64 = usage.iter().map(|d| d["failed"].as_i64().unwrap()).sum();
    assert_eq!((total, failed), (2, 1));
    // every day of the month so far, none after today
    assert_eq!(usage.len(), ((now() - month) / DAY).floor() as usize + 1);
    let u = &v["users"][0];
    assert_eq!(u["email"], "boss@acme.test");
    assert!(u["id"].is_i64() && u["created_at"].is_f64());
    assert_eq!(v["users"].as_array().unwrap().len(), 1);
    let a = &v["agents"][0];
    assert_eq!(a["name"], format!("pc{acme}"));
    assert_eq!(a["online"], false);
    assert!(a.get("last_seen_at").is_some());
    let p = &v["printers"][0];
    assert_eq!(p["name"], "Z");
    assert_eq!(p["can_pdf"], false);
    assert_eq!(p["state"], "active");
    assert_eq!(p["agent_id"], a["id"]);
    let k = &v["keys"][0];
    assert_eq!(k["label"], "shop");
    assert_eq!(k["active"], true);
    assert!(k["created_at"].is_f64());
    let terms: Vec<&str> = v["terms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["version"].as_str().unwrap())
        .collect();
    assert_eq!(terms, vec!["v1", "v2"]);
    assert!(v["terms"][0]["accepted_at"].is_f64());
    assert_eq!(v["terms_version"], "v2");
    // a lock shows up with its reason
    sql(
        &app.db,
        "INSERT INTO org_locks(org_id, org_created_at, reason, locked_at) \n         SELECT id, created_at, ?, ? FROM orgs WHERE id=?",
        rusqlite::params!["unpaid invoice", 1_700_000_500.0, acme],
    );
    let (_, v) = app.get_op(&format!("/operator/orgs/{acme}"));
    assert_eq!(
        v["lock"],
        json!({"reason": "unpaid invoice", "locked_at": 1_700_000_500.0})
    );
    assert_eq!(v["status"], "locked");
}

#[test]
fn org_detail_and_jobs_of_an_unknown_org_are_404() {
    let app = hosted();
    // the seeded default org exists, so a 404 below is the org and not the route
    assert_eq!(app.get_op("/operator/orgs/1").0, 200);
    assert_eq!(app.get_op("/operator/orgs/1/jobs").0, 200);
    for t in [
        "/operator/orgs/999",
        "/operator/orgs/999/jobs",
        "/operator/orgs/x",
        "/operator/orgs/x/jobs",
    ] {
        assert_eq!(app.get_op(t).0, 404, "{t}");
    }
}

#[test]
fn org_jobs_are_metadata_newest_first_and_paged() {
    let app = hosted();
    let acme = signup(&app.db, "Acme", "boss@acme.test", None);
    let other = signup(&app.db, "Other", "o@x.test", None);
    let ids: Vec<i64> = (0..5)
        .map(|i| {
            job_at(&app, acme, 100.0 - i as f64, |j| {
                j.title = Some("TITLESENTINEL".into());
                j.callback_url = Some("https://cb.example/CALLBACKSENTINEL".into());
                j.idempotency_key = Some(format!("IDEMSENTINEL{i}"));
                j.copies = 2;
            })
        })
        .collect();
    job_at(&app, other, 1.0, |_| {});
    sql(
        &app.db,
        "UPDATE jobs SET state='failed', error='printer on fire', finished_at=5.5, claimed_at=4.5 WHERE id=?",
        [ids[4]],
    );
    let (st, v) = app.get_op(&format!("/operator/orgs/{acme}/jobs?limit=2"));
    assert_eq!(st, 200, "{v}");
    assert_eq!(ids_of(&v, "jobs"), vec![ids[4], ids[3]]);
    assert_eq!(v["next_before_id"], ids[3]);
    let j = &v["jobs"][0];
    assert_eq!(j["state"], "failed");
    assert_eq!(j["error"], "printer on fire");
    assert_eq!(j["finished_at"], 5.5);
    assert_eq!(j["claimed_at"], 4.5);
    assert_eq!(j["type"], "raw_base64");
    assert_eq!(j["mode"], "raw");
    assert_eq!(j["copies"], 2);
    assert_eq!(j["printer_name"], "Z");
    assert!(j["printer_id"].is_i64() && j["created_at"].is_f64());
    // exactly these fields: no title, payload, callback or idempotency key
    let mut keys: Vec<&String> = j.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "claimed_at",
            "copies",
            "created_at",
            "error",
            "finished_at",
            "id",
            "mode",
            "printer_id",
            "printer_name",
            "state",
            "type"
        ]
    );
    let (_, v) = app.get_op(&format!(
        "/operator/orgs/{acme}/jobs?limit=2&before_id={}",
        ids[3]
    ));
    assert_eq!(ids_of(&v, "jobs"), vec![ids[2], ids[1]]);
    let (_, v) = app.get_op(&format!("/operator/orgs/{acme}/jobs?before_id={}", ids[1]));
    assert_eq!(v["jobs"].as_array().unwrap().len(), 1);
    assert!(v["next_before_id"].is_null());
    // only this org's jobs, and bad parameters
    let (_, v) = app.get_op(&format!("/operator/orgs/{acme}/jobs"));
    assert_eq!(v["jobs"].as_array().unwrap().len(), 5);
    for bad in ["limit=0", "limit=x", "before_id=x"] {
        assert_eq!(
            app.get_op(&format!("/operator/orgs/{acme}/jobs?{bad}")).0,
            400,
            "{bad}"
        );
    }
}

#[test]
fn org_jobs_limit_is_capped_at_200() {
    let app = hosted();
    let acme = signup(&app.db, "Acme", "boss@acme.test", None);
    for _ in 0..205 {
        job_at(&app, acme, 1.0, |_| {});
    }
    let (st, v) = app.get_op(&format!("/operator/orgs/{acme}/jobs?limit=1000"));
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["jobs"].as_array().unwrap().len(), 200);
    assert!(v["next_before_id"].is_i64());
}

#[test]
fn no_operator_read_answer_carries_a_secret_or_content() {
    let app = hosted();
    let acme = signup(&app.db, "Acme", "boss@acme.test", Some("v1"));
    sql(
        &app.db,
        "UPDATE orgs SET shopify_secret='SHOPIFYSENTINEL', event_url='https://ev.example/EVENTSENTINEL' WHERE id=?",
        [acme],
    );
    add_key(&app.db, "shop", "KEYSECRET-1234567890", acme);
    register(
        &app.db,
        "pc",
        "AGENTKEYSECRET-123",
        &[decl("Z", false)],
        acme,
    );
    sql(
        &app.db,
        "UPDATE users SET password_hash='PWHASHSENTINEL' WHERE org_id=?",
        [acme],
    );
    job_at(&app, acme, 10.0, |j| {
        j.title = Some("TITLESENTINEL".into());
        j.callback_url = Some("https://cb.example/CALLBACKSENTINEL".into());
        j.idempotency_key = Some("IDEMSENTINEL".into());
    });
    let mut forbidden: Vec<String> = [
        "SHOPIFYSENTINEL",
        "EVENTSENTINEL",
        "KEYSECRET-1234567890",
        "AGENTKEYSECRET-123",
        "PWHASHSENTINEL",
        "TITLESENTINEL",
        "CALLBACKSENTINEL",
        "IDEMSENTINEL",
        "PAYLOADSENTINEL",
        "password_hash",
        "key_hash",
        "api_key_hash",
        "payload",
        "callback_url",
        "idempotency_key",
        "shopify_secret",
        "title",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    forbidden.push(store::hash_key("KEYSECRET-1234567890"));
    forbidden.push(store::hash_key("AGENTKEYSECRET-123"));
    for t in [
        "/operator/orgs".to_string(),
        "/operator/orgs?q=acme".to_string(),
        format!("/operator/orgs/{acme}"),
        format!("/operator/orgs/{acme}/jobs"),
    ] {
        let r = op(&app, "GET", &t).send();
        assert_eq!(r.status, 200, "{t}");
        let body = r.text();
        assert!(body.contains("Acme") || t.ends_with("/jobs"), "{t}: {body}");
        for f in &forbidden {
            assert!(!body.contains(f.as_str()), "{t} leaks {f}: {body}");
        }
    }
}

#[test]
fn org_read_queries_use_indexes() {
    let db = store::connect(":memory:").unwrap();
    store::init_db(&db).unwrap();
    for (name, plan) in [
        ("org list", store::explain_operator_org_list(&db).unwrap()),
        ("org jobs", store::explain_operator_org_jobs(&db).unwrap()),
    ] {
        assert!(!plan.is_empty(), "{name}");
        for line in &plan {
            for t in [
                "jobs",
                "users",
                "agents",
                "printers",
                "terms_acceptances",
                "org_locks",
            ] {
                assert!(
                    !line.starts_with(&format!("SCAN {t}")),
                    "{name} scans {t}: {plan:#?}"
                );
            }
            assert!(!line.contains("TEMP B-TREE"), "{name} sorts: {plan:#?}");
        }
    }
}
