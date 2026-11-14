// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Live settings at runtime (spec A5, PR 3): the effective values are one snapshot per request,
//! read by the signup handler, `GET /health`, the operator org create and the retention pass
//! only, and only with the operator token set.
mod common;

use common::*;
use printpapi_server::dispatch::{Destinations, Fetcher, HttpFetcher};
use printpapi_server::proxy::TrustedProxies;
use printpapi_server::server::{AppState, Config, Terms, retention_pass};
use printpapi_server::settings::{Change, Parsed, Source};
use printpapi_server::store::{self, DEFAULT_ORG, Db};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const OP: &str = "operator-token-0123456789abcdef-xyz";
const CONSOLE: &str = "10.9.0.2";

/// Hosted the way `main` sets it up: the token, its peer and the private-URL deny.
fn hosted_cfg(c: &mut Config) {
    c.operator_token = Some(OP.into());
    c.operator_from = TrustedProxies::parse(CONSOLE).unwrap();
    c.destinations = Destinations::PublicOnly;
    c.fetches_per_org = Some(4);
}

fn terms(version: &str) -> Terms {
    Terms {
        version: version.into(),
        terms_url: "https://x.test/terms".into(),
        dpa_url: None,
        privacy_url: None,
    }
}

/// Write override rows straight into the table, as an earlier run of the operator API left them.
fn seed(db: &Db, rows: &[(&str, &str)]) {
    db.with(|c| {
        for (k, v) in rows {
            c.execute(
                "INSERT OR REPLACE INTO settings(key, value, updated_at) VALUES(?,?,1.0)",
                rusqlite::params![k, v],
            )
            .unwrap();
        }
    });
}

fn rows(db: &Db) -> Vec<(String, String)> {
    db.with(|c| store::settings_rows(c).unwrap())
}

fn change(set: Vec<Parsed>, unset: Vec<&'static str>) -> Change {
    Change { set, unset }
}

fn signup(app: &App, body: Value) -> (u16, Value) {
    app.post("/signup", None, body)
}

fn good() -> Value {
    json!({"email": "a@b.test", "password": PW})
}

fn health(app: &App) -> Value {
    app.get("/health", None).1
}

/// `POST /operator/orgs` as the console sends it (mail must be configured for it).
fn operator_create(app: &App, email: &str) -> (u16, Value) {
    let r = app
        .call("POST", "/operator/orgs")
        .bearer(OP)
        .socket(CONSOLE)
        .json(&json!({"name": "Fresh", "email": email}))
        .send();
    (r.status, r.json())
}

fn mail_cfg(c: &mut Config) {
    hosted_cfg(c);
    c.reset_enabled = true;
    c.public_url = "https://print.example".into();
}

fn org_quota(db: &Db, org: i64) -> Option<i64> {
    db.with(|c| store::get_org(c, org))
        .unwrap()
        .unwrap()
        .job_quota
}

// --- the token gates the table ------------------------------------------------------------------

#[test]
fn a_database_open_with_the_token_opens_signup_and_health_says_so() {
    let db = mem();
    seed(&db, &[("signup", "open")]);
    let app = App::on(db, hosted_cfg);
    assert_eq!(health(&app)["signup"], "open");
    let (st, v) = signup(&app, good());
    assert_eq!(st, 200, "{v}");
    assert_eq!(app.state.live().signup.source, Source::Db);
}

#[test]
fn without_the_token_the_database_changes_nothing_and_the_table_is_left_alone() {
    let db = mem();
    seed(
        &db,
        &[
            ("signup", "open"),
            ("signup_quota", "5"),
            ("job_retention_days", "7"),
            (
                "terms",
                r#"{"version":"v9","terms_url":"https://x.test/t"}"#,
            ),
        ],
    );
    let before = rows(&db);
    let app = App::on(db.clone(), |_| {});
    let h = health(&app);
    assert_eq!(h["signup"], "closed");
    assert!(h.get("terms").is_none(), "{h}");
    assert_eq!(signup(&app, good()).0, 403);
    assert_eq!(app.state.live().job_retention_days.value, None);
    assert_eq!(app.state.live().signup_quota.value, None);
    assert_eq!(rows(&db), before);
}

// --- live changes leave the security defaults alone -------------------------------------------------

#[test]
fn opening_and_closing_signup_live_moves_no_security_default() {
    let db = mem();
    let mail = Arc::new(Mailbox::default());
    let mut cfg = Config::new("t");
    hosted_cfg(&mut cfg);
    let fetcher = Arc::new(HttpFetcher::with_limits_to(
        Duration::from_secs(1),
        Duration::from_secs(5),
        8,
        cfg.destinations,
    ));
    let state = AppState::new(cfg, db.clone(), mail.clone(), fetcher.clone());
    let app = App {
        state,
        db: db.clone(),
        mail,
        fetch: Arc::new(FakeFetcher::default()),
    };
    add_key(&app.db, "one", "k1", DEFAULT_ORG);
    let pid = register(&app.db, "pc", "ak", &[decl("P", true)], DEFAULT_ORG).pid("P");
    let cap = app.state.cfg.fetch_cap();
    assert_eq!(app.state.cfg.destinations, Destinations::PublicOnly);

    let refused = |app: &App| {
        let (st, v) = app.post(
            "/jobs",
            Some("k1"),
            json!({"printer_id": pid, "type": "raw_uri", "url": "http://127.0.0.1/"}),
        );
        assert_ne!(st, 200, "{v}");
        let e = fetcher.post_json("http://10.0.0.1/", &json!({"event": "x"}));
        assert!(e.unwrap_err().contains("not allowed"));
    };
    refused(&app);
    assert_eq!(signup(&app, good()).0, 403);
    app.state
        .apply_settings(&change(vec![Parsed::Signup(true)], vec![]), None)
        .unwrap();
    assert_eq!(health(&app)["signup"], "open");
    assert_eq!(signup(&app, good()).0, 200);
    refused(&app);
    app.state
        .apply_settings(&change(vec![Parsed::Signup(false)], vec![]), None)
        .unwrap();
    assert_eq!(health(&app)["signup"], "closed");
    assert_eq!(
        signup(&app, json!({"email": "c@d.test", "password": PW})).0,
        403
    );
    refused(&app);
    assert_eq!(app.state.cfg.fetch_cap(), cap);
    assert_eq!(app.state.cfg.destinations, Destinations::PublicOnly);
    assert!(!app.state.cfg.signup_open);
}

#[test]
fn a_live_change_is_written_and_swapped_in_one_critical_section() {
    let app = App::with(hosted_cfg);
    let live = app
        .state
        .apply_settings(
            &change(vec![Parsed::SignupQuota(3)], vec![]),
            Some("because"),
        )
        .unwrap();
    // the returned snapshot is the rebuilt one and is what the state serves from now on
    assert_eq!(live.signup_quota.value, Some(3));
    assert_eq!(app.state.live().signup_quota.value, Some(3));
    assert_eq!(
        rows(&app.db),
        [("signup_quota".to_string(), "3".to_string())]
    );
    app.state
        .apply_settings(&change(vec![], vec!["signup_quota"]), None)
        .unwrap();
    assert_eq!(app.state.live().signup_quota.value, None);
    assert!(rows(&app.db).is_empty());
}

#[test]
fn a_snapshot_keeps_its_values_when_the_live_values_are_swapped() {
    let app = App::with(hosted_cfg);
    let before = app.state.live();
    assert!(!before.signup.value);
    app.state
        .apply_settings(&change(vec![Parsed::Signup(true)], vec![]), None)
        .unwrap();
    assert!(!before.signup.value, "a request in flight keeps its view");
    assert!(app.state.live().signup.value);
    assert!(!Arc::ptr_eq(&before, &app.state.live()));
}

// --- the readers ----------------------------------------------------------------------------------

#[test]
fn a_live_signup_quota_applies_to_the_next_signup_and_to_operator_org_create() {
    let db = mem();
    seed(&db, &[("signup", "open")]);
    let app = App::on(db, |c| {
        mail_cfg(c);
        c.signup_quota = Some(25);
    });
    let (_, v) = signup(&app, good());
    assert_eq!(org_quota(&app.db, v["org_id"].as_i64().unwrap()), Some(25));
    app.state
        .apply_settings(&change(vec![Parsed::SignupQuota(7)], vec![]), None)
        .unwrap();
    let (st, v) = signup(&app, json!({"email": "c@d.test", "password": PW}));
    assert_eq!(st, 200, "{v}");
    assert_eq!(org_quota(&app.db, v["org_id"].as_i64().unwrap()), Some(7));
    let (st, v) = operator_create(&app, "e@f.test");
    assert_eq!(st, 200, "{v}");
    assert_eq!(org_quota(&app.db, v["org_id"].as_i64().unwrap()), Some(7));
    // unset: back to the env value
    app.state
        .apply_settings(&change(vec![], vec!["signup_quota"]), None)
        .unwrap();
    let (_, v) = operator_create(&app, "g@h.test");
    assert_eq!(org_quota(&app.db, v["org_id"].as_i64().unwrap()), Some(25));
}

#[test]
fn live_terms_change_what_health_shows_and_what_signup_requires() {
    let db = mem();
    seed(&db, &[("signup", "open")]);
    let app = App::on(db, hosted_cfg);
    assert!(health(&app).get("terms").is_none());
    assert_eq!(signup(&app, good()).0, 200);
    app.state
        .apply_settings(&change(vec![Parsed::Terms(terms("v2"))], vec![]), None)
        .unwrap();
    assert_eq!(health(&app)["terms"]["version"], "v2");
    let body = |v: Option<&str>, mail: &str| {
        let mut b = json!({"email": mail, "password": PW});
        if let Some(v) = v {
            b["accept_terms"] = json!(v);
        }
        b
    };
    assert_eq!(signup(&app, body(None, "c@d.test")).0, 400);
    assert_eq!(signup(&app, body(Some("v1"), "c@d.test")).0, 409);
    assert_eq!(signup(&app, body(Some("v2"), "c@d.test")).0, 200);
    app.state
        .apply_settings(&change(vec![], vec!["terms"]), None)
        .unwrap();
    assert!(health(&app).get("terms").is_none());
}

#[test]
fn a_live_job_retention_is_used_by_the_next_retention_pass() {
    let day = 86_400.0;
    let now = store::month_start(printpapi_server::now()) + 25.0 * day;
    let app = App::with(hosted_cfg);
    let pid = agent_z(&app.db).pid("Z");
    let old = enqueue(&app.db, pid, b"secret", |j| j.title = Some("T".into()));
    app.db.with(|c| {
        c.execute(
            "UPDATE jobs SET state='done', created_at=?1, finished_at=?1 WHERE id=?2",
            rusqlite::params![now - 10.0 * day, old],
        )
        .unwrap()
    });
    // env keeps for ever: a pass does nothing
    assert!(retention_pass(&app.state, now).is_none());
    app.state
        .apply_settings(
            &change(vec![Parsed::JobRetentionDays(7)], vec![]),
            Some("keep a week"),
        )
        .unwrap();
    assert_eq!(retention_pass(&app.state, now).unwrap().scrubbed, 1);
}

// --- restart: the startup rules hold ------------------------------------------------------------------

#[test]
fn an_explicit_env_closed_beats_a_database_open_across_a_restart() {
    let db = mem();
    seed(&db, &[("signup", "open")]);
    let app = App::on(db.clone(), |c| {
        hosted_cfg(c);
        c.env_flags.signup_set = true;
        c.env_flags.signup_explicit_closed = true;
    });
    assert_eq!(health(&app)["signup"], "closed");
    assert_eq!(signup(&app, good()).0, 403);
    assert_eq!(app.state.live().signup.source, Source::EnvLocked);
    // the same DB with a plain (unset) env is open again: unset is not a lock
    let app = App::on(db, hosted_cfg);
    assert_eq!(health(&app)["signup"], "open");
}

#[test]
fn a_database_open_is_ignored_once_private_urls_are_allowed() {
    let db = mem();
    seed(&db, &[("signup", "open"), ("signup_quota", "4")]);
    let app = App::on(db, |c| {
        hosted_cfg(c);
        c.destinations = Destinations::Any;
    });
    assert_eq!(health(&app)["signup"], "closed");
    assert_eq!(signup(&app, good()).0, 403);
    // only the open row is dropped, the others still apply
    assert_eq!(app.state.live().signup_quota.value, Some(4));
}

#[test]
fn a_row_that_no_longer_validates_is_ignored_at_startup() {
    let db = mem();
    seed(&db, &[("signup_quota", "-3"), ("job_retention_days", "0")]);
    let app = App::on(db, |c| {
        hosted_cfg(c);
        c.signup_quota = Some(9);
    });
    assert_eq!(app.state.live().signup_quota.value, Some(9));
    assert_eq!(app.state.live().signup_quota.source, Source::Default);
    assert_eq!(app.state.live().job_retention_days.value, None);
}
