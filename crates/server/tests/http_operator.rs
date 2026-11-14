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
