// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The setup window's checks against the real server over loopback HTTP, with the real agent
//! client: no mocks, nothing printed.
use printpapi_agent::client::{Client, Http};
use printpapi_agent::config::HttpSettings;
use printpapi_agent::pair::{self, PairError};
use printpapi_agent::setup::{
    ConnectError, agent_seen_after, check_connection, pair_checked, reach,
};
use printpapi_server::dispatch::HttpFetcher;
use printpapi_server::http::{spawn_test_server, spawn_test_server_with};
use printpapi_server::mail::SmtpMailer;
use printpapi_server::server::{AppState, Config, Req, Resp, handle};
use printpapi_server::store::Db;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// A running server, its base URL and a fresh agent key (issued with the root token, like the
/// dashboard's Keys page does).
fn server_with_key() -> (String, String, Client) {
    let state = Arc::new(AppState::new(
        Config::new("root-token"),
        Db::open(":memory:").unwrap(),
        Arc::new(SmtpMailer),
        Arc::new(HttpFetcher::new(Duration::from_secs(1))),
    ));
    let base = format!("http://{}", spawn_test_server(state));
    let http = Client::new(&HttpSettings::default());
    let issued = http
        .post(
            &format!("{base}/apikeys"),
            "root-token",
            &json!({"label": "setup test"}),
        )
        .unwrap()
        .unwrap();
    let key = issued["key"].as_str().expect("the issued key").to_string();
    (base, key, http)
}

fn register(http: &Client, base: &str, key: &str, name: &str) {
    http.post(
        &format!("{base}/agent/register"),
        key,
        &json!({"name": name, "printers": [{"name": "Zebra", "can_pdf": false}]}),
    )
    .unwrap();
}

#[test]
fn a_valid_key_passes_and_a_wrong_one_is_bad_key() {
    let (base, key, http) = server_with_key();
    assert_eq!(check_connection(&http, &base, &key, "shop-pc"), Ok(false));
    assert_eq!(
        check_connection(&http, &base, "not-a-key", "shop-pc"),
        Err(ConnectError::BadKey)
    );
}

/// A key issued with these rights, the way the dashboard's Keys page does it.
fn key_with_scope(base: &str, http: &Client, scope: &str) -> String {
    let issued = http
        .post(
            &format!("{base}/apikeys"),
            "root-token",
            &json!({"label": scope, "scope": scope}),
        )
        .unwrap()
        .unwrap();
    issued["key"].as_str().expect("the issued key").to_string()
}

#[test]
fn an_agent_key_passes_every_setup_step() {
    let (base, _, http) = server_with_key();
    let key = key_with_scope(&base, &http, "agent");
    assert_eq!(check_connection(&http, &base, &key, "shop-pc"), Ok(false));
    register(&http, &base, &key, "shop-pc");
    assert_eq!(check_connection(&http, &base, &key, "shop-pc"), Ok(true));
    let baseline = http
        .get(&format!("{base}/computers"), &key)
        .unwrap()
        .unwrap()["computers"][0]["last_seen_at"]
        .as_f64()
        .unwrap();
    assert_eq!(
        agent_seen_after(&http, &base, &key, "shop-pc", baseline),
        Ok(false)
    );
}

#[test]
fn a_key_for_something_else_is_not_an_agent_key() {
    let (base, full, http) = server_with_key();
    for scope in ["print", "cloudprnt", "read"] {
        let key = key_with_scope(&base, &http, scope);
        assert_eq!(
            check_connection(&http, &base, &key, "shop-pc"),
            Err(ConnectError::NotAgentKey),
            "{scope}"
        );
    }
    // a full key is unchanged, a wrong key is still a wrong key
    assert_eq!(check_connection(&http, &base, &full, "shop-pc"), Ok(false));
    assert_eq!(
        check_connection(&http, &base, "not-a-key", "shop-pc"),
        Err(ConnectError::BadKey)
    );
}

#[test]
fn the_check_registers_nothing() {
    let (base, key, http) = server_with_key();
    check_connection(&http, &base, &key, "shop-pc").unwrap();
    let list = http
        .get(&format!("{base}/computers"), &key)
        .unwrap()
        .unwrap();
    assert_eq!(list["computers"], json!([]));
}

#[test]
fn a_registered_name_is_reported_as_taken() {
    let (base, key, http) = server_with_key();
    register(&http, &base, &key, "shop-pc");
    assert_eq!(check_connection(&http, &base, &key, "shop-pc"), Ok(true));
    assert_eq!(check_connection(&http, &base, &key, "other"), Ok(false));
}

#[test]
fn a_second_register_moves_last_seen_past_the_baseline() {
    let (base, key, http) = server_with_key();
    register(&http, &base, &key, "shop-pc");
    let baseline = http
        .get(&format!("{base}/computers"), &key)
        .unwrap()
        .unwrap()["computers"][0]["last_seen_at"]
        .as_f64()
        .expect("last_seen_at after register");
    assert_eq!(
        agent_seen_after(&http, &base, &key, "shop-pc", baseline),
        Ok(false)
    );
    // last_seen_at has a resolution of its own: wait until a new stamp can differ
    std::thread::sleep(Duration::from_millis(1100));
    register(&http, &base, &key, "shop-pc");
    assert_eq!(
        agent_seen_after(&http, &base, &key, "shop-pc", baseline),
        Ok(true)
    );
}

#[test]
fn a_dead_server_is_unreachable_in_words() {
    let http = Client::new(&HttpSettings::default());
    let e = check_connection(&http, "http://127.0.0.1:1", "k", "pc").unwrap_err();
    assert!(matches!(e, ConnectError::Unreachable(_)));
    assert!(e.to_string().starts_with("Could not reach the server:"));
}

// --- pairing with the real client ---------------------------------------------------------------

fn fresh_state() -> Arc<AppState> {
    let mut cfg = Config::new("root-token");
    // a poll without a job ends after a moment, not after the server's 25 seconds
    cfg.long_poll_timeout_s = 0.3;
    Arc::new(AppState::new(
        cfg,
        Db::open(":memory:").unwrap(),
        Arc::new(SmtpMailer),
        Arc::new(HttpFetcher::new(Duration::from_secs(1))),
    ))
}

fn new_code(http: &Client, base: &str, body: serde_json::Value) -> String {
    http.post(&format!("{base}/pairing"), "root-token", &body)
        .unwrap()
        .unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn a_code_is_redeemed_with_the_real_client_and_the_key_registers() {
    let state = fresh_state();
    let base = format!("http://{}", spawn_test_server(state));
    let http = Client::new(&HttpSettings::default());
    reach(&http, &base).expect("the whole server is reachable");
    let code = new_code(&http, &base, json!({}));
    let got = pair::redeem(&http, &base, &code, "shop-pc").ok().unwrap();
    assert_eq!(
        (got.name.as_str(), got.org.as_str(), got.replaces),
        ("shop-pc", "default", false)
    );
    assert_eq!(got.created_by, None);
    // the key is an agent key: it passes every setup step
    assert_eq!(
        check_connection(&http, &base, &got.key, "shop-pc"),
        Ok(false)
    );
    register(&http, &base, &got.key, "shop-pc");
    assert_eq!(
        check_connection(&http, &base, &got.key, "shop-pc"),
        Ok(true)
    );
    // the code is spent
    assert_eq!(
        pair::redeem(&http, &base, &code, "other").err(),
        Some(PairError::BadCode)
    );
    // a wrong name is the server's 400, a taken one the 409
    let code = new_code(&http, &base, json!({}));
    assert_eq!(
        pair::redeem(&http, &base, &code, "shop-pc").err(),
        Some(PairError::NameTaken)
    );
    assert_eq!(
        pair::redeem(&http, &base, &code, "cloudprnt-x").err(),
        Some(PairError::BadName)
    );
    assert!(pair::redeem(&http, &base, &code, "second-pc").is_ok());
}

#[test]
fn a_reconnect_keeps_the_computer_id() {
    let state = fresh_state();
    let base = format!("http://{}", spawn_test_server(state));
    let http = Client::new(&HttpSettings::default());
    let old = key_with_scope(&base, &http, "agent");
    register(&http, &base, &old, "shop-pc");
    let before = http
        .get(&format!("{base}/computers"), &old)
        .unwrap()
        .unwrap()["computers"][0]["id"]
        .clone();
    let code = new_code(&http, &base, json!({"computer_id": before}));
    let got = pair::redeem(&http, &base, &code, "ignored").ok().unwrap();
    assert!(got.replaces);
    assert_eq!(got.name, "shop-pc");
    let reg = http
        .post(
            &format!("{base}/agent/register"),
            &got.key,
            &json!({"name": got.name, "printers": [{"name": "Zebra"}]}),
        )
        .unwrap()
        .unwrap();
    assert_eq!(reg["computer_id"], before);
    // the first poll takes the device over
    assert_eq!(
        http.get(&format!("{base}/agent/jobs"), &old).unwrap(),
        None,
        "the old one still polls"
    );
    assert_eq!(
        http.get(&format!("{base}/agent/jobs"), &got.key).unwrap(),
        None
    );
    assert!(
        http.get(&format!("{base}/agent/jobs"), &old).is_err(),
        "and now it is refused"
    );
}

#[test]
fn a_server_before_pairing_codes_is_an_old_server() {
    fn old(state: &AppState, req: &Req) -> Resp {
        if req.path() == "/agent/pair" {
            return Resp::json(404, &json!({"error": "not found"}));
        }
        handle(state, req)
    }
    let base = format!("http://{}", spawn_test_server_with(fresh_state(), old));
    let http = Client::new(&HttpSettings::default());
    assert_eq!(
        pair::redeem(&http, &base, "7K3M-Q2ZD-9XHT", "pc").err(),
        Some(PairError::OldServer)
    );
}

#[test]
fn a_server_that_publishes_only_the_agent_routes_fails_reach() {
    fn agent_only(state: &AppState, req: &Req) -> Resp {
        if req.path().starts_with("/agent/") || req.path() == "/health" {
            handle(state, req)
        } else {
            Resp::json(404, &json!({"error": "not found"}))
        }
    }
    let base = format!(
        "http://{}",
        spawn_test_server_with(fresh_state(), agent_only)
    );
    let http = Client::new(&HttpSettings::default());
    assert_eq!(
        reach(&http, &base),
        Err(PairError::Unreachable(
            "only a part of this server can be reached from here".into()
        ))
    );
}

/// What the window calls: `pair_checked` is the check and then the spend, in that order. A server
/// that cannot serve the setup is found out with the code still good.
#[test]
fn a_code_is_spent_only_after_the_server_passed_the_reach_check() {
    fn agent_only(state: &AppState, req: &Req) -> Resp {
        if req.path().starts_with("/agent/") || req.path() == "/health" {
            handle(state, req)
        } else {
            Resp::json(404, &json!({"error": "not found"}))
        }
    }
    let state = fresh_state();
    let code = printpapi_server::auth::new_pairing_code();
    state
        .db
        .with(|c| printpapi_server::store::create_pairing(c, 1, None, None, &code, None))
        .unwrap();
    let base = format!(
        "http://{}",
        spawn_test_server_with(state.clone(), agent_only)
    );
    let http = Client::new(&HttpSettings::default());
    assert_eq!(
        pair_checked(&http, &base, &code, "pc").err(),
        Some(PairError::Unreachable(
            "only a part of this server can be reached from here".into()
        ))
    );
    // the code was not spent: the same code redeems directly
    let got = pair::redeem(&http, &base, &code, "pc").expect("still good");
    assert_eq!(got.name, "pc");

    // on a server that serves everything the same call spends the code
    let (base, _, http) = server_with_key();
    let made = new_code(&http, &base, json!({}));
    let got = pair_checked(&http, &base, &made, "shop-pc").expect("paired");
    assert_eq!(got.name, "shop-pc");
    assert_eq!(
        pair_checked(&http, &base, &made, "shop-pc").err(),
        Some(PairError::BadCode)
    );
}

#[test]
fn an_empty_bearer_gets_401_on_computers_and_the_health_check_answers() {
    let base = format!("http://{}", spawn_test_server(fresh_state()));
    let http = Client::new(&HttpSettings::default());
    let err = http.get(&format!("{base}/computers"), "").unwrap_err();
    assert_eq!(printpapi_agent::client::http_status(&err), Some(401));
    assert!(http.get(&format!("{base}/health"), "").is_ok());
    assert_eq!(reach(&http, &base), Ok(()));
    // nothing there at all
    assert!(matches!(
        reach(&http, "http://127.0.0.1:1"),
        Err(PairError::Unreachable(_))
    ));
}

/// An install that fails after its register call (a copy, the user, the last-seen check) leaves the
/// old agent working: the Reconnect key registered but did not take the device over. Real client,
/// real server, real store.
#[test]
fn a_reconnect_key_that_registered_and_then_failed_leaves_the_old_agent_working() {
    let base = format!("http://{}", spawn_test_server(fresh_state()));
    let http = Client::new(&HttpSettings::default());
    let old = key_with_scope(&base, &http, "full");
    register(&http, &base, &old, "shop-pc");
    let computers = http
        .get(&format!("{base}/computers"), "root-token")
        .unwrap()
        .unwrap();
    let id = computers["computers"][0]["id"]
        .as_i64()
        .expect("the device");
    let code = new_code(&http, &base, json!({"computer_id": id}));

    let got = pair::redeem(&http, &base, &code, "whatever").expect("redeemed");
    assert_eq!((got.name.as_str(), got.replaces), ("shop-pc", true));
    let body = json!({"name": got.name, "printers": [{"name": "Zebra", "can_pdf": false}]});
    let registered = http
        .post(&format!("{base}/agent/register"), &got.key, &body)
        .unwrap()
        .unwrap();
    assert_eq!(registered["computer_id"], id, "the device keeps its id");

    // ... and here the install fails. The old agent has not noticed anything:
    assert!(
        http.get(&format!("{base}/agent/jobs"), &old).is_ok(),
        "the old key still polls"
    );
    // "Try again" with the same key registers the same way
    let again = http
        .post(&format!("{base}/agent/register"), &got.key, &body)
        .unwrap()
        .unwrap();
    assert_eq!(again["printer_ids"], registered["printer_ids"]);
    assert!(http.get(&format!("{base}/agent/jobs"), &old).is_ok());
    // a key under another name is refused: it would be a second device, not this one
    let other = json!({"name": "new-pc", "printers": []});
    assert!(
        http.post(&format!("{base}/agent/register"), &got.key, &other)
            .is_err()
    );

    // the new agent's first poll takes the device over, and the old key is gone
    assert!(http.get(&format!("{base}/agent/jobs"), &got.key).is_ok());
    let err = http.get(&format!("{base}/agent/jobs"), &old).unwrap_err();
    assert_eq!(printpapi_agent::client::http_status(&err), Some(401));
}
