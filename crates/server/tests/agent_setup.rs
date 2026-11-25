// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The setup window's checks against the real server over loopback HTTP, with the real agent
//! client: no mocks, nothing printed.
use printpapi_agent::client::{Client, Http};
use printpapi_agent::config::HttpSettings;
use printpapi_agent::setup::{ConnectError, agent_seen_after, check_connection};
use printpapi_server::dispatch::HttpFetcher;
use printpapi_server::http::spawn_test_server;
use printpapi_server::mail::SmtpMailer;
use printpapi_server::server::{AppState, Config};
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
