// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The client address behind a trusted proxy, over real loopback sockets: the signup throttle
//! must see the `X-Forwarded-For` client when (and only when) the peer is a trusted proxy.
use printpapi_server::dispatch::HttpFetcher;
use printpapi_server::http::spawn_test_server;
use printpapi_server::mail::SmtpMailer;
use printpapi_server::proxy::TrustedProxies;
use printpapi_server::server::{AppState, Config};
use printpapi_server::store::Db;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

/// A signup server where each client address gets exactly one signup.
fn server(trusted: &str) -> SocketAddr {
    let mut cfg = Config::new("t");
    cfg.signup_open = true;
    cfg.max_login_fails = 1;
    cfg.trusted_proxies = TrustedProxies::parse(trusted).unwrap();
    spawn_test_server(Arc::new(AppState::new(
        cfg,
        Db::open(":memory:").unwrap(),
        Arc::new(SmtpMailer),
        Arc::new(HttpFetcher::new(Duration::from_secs(1))),
    )))
}

/// POST /signup with a fresh e-mail each time and the given `X-Forwarded-For` lines; the status.
fn signup(addr: SocketAddr, n: u32, xff: &[&str]) -> u16 {
    let body = format!(r#"{{"email":"u{n}@shop.test","password":"correct horse battery"}}"#);
    let extra: String = xff
        .iter()
        .map(|v| format!("X-Forwarded-For: {v}\r\n"))
        .collect();
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "POST /signup HTTP/1.1\r\nHost: t\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[test]
fn behind_a_trusted_proxy_each_forwarded_client_has_its_own_signup_slot() {
    let addr = server("127.0.0.1");
    assert_eq!(signup(addr, 1, &["1.1.1.1"]), 200);
    assert_eq!(signup(addr, 2, &["1.1.1.1"]), 429);
    assert_eq!(signup(addr, 3, &["2.2.2.2"]), 200);
    // a client's own header further left does not buy a fresh slot: the proxy's entry is the
    // rightmost untrusted one
    assert_eq!(signup(addr, 4, &["9.9.9.9, 1.1.1.1"]), 429);
}

#[test]
fn an_ipv6_client_is_throttled_by_its_slash_64() {
    let addr = server("127.0.0.1");
    assert_eq!(signup(addr, 1, &["2001:db8:1:2::1"]), 200);
    assert_eq!(signup(addr, 2, &["[2001:db8:1:2:ffff::9]"]), 429);
    assert_eq!(signup(addr, 3, &["2001:db8:1:3::1"]), 200);
}

#[test]
fn without_the_setting_a_spoofed_header_does_not_escape_the_shared_throttle() {
    let addr = server("");
    assert_eq!(signup(addr, 1, &["1.1.1.1"]), 200);
    assert_eq!(signup(addr, 2, &["2.2.2.2"]), 429);
    assert_eq!(signup(addr, 3, &[]), 429);
}

#[test]
fn a_peer_that_is_not_trusted_cannot_choose_its_address() {
    // trusts some other proxy; the loopback caller is a stranger
    let addr = server("10.0.0.1");
    assert_eq!(signup(addr, 1, &["1.1.1.1"]), 200);
    assert_eq!(signup(addr, 2, &["2.2.2.2"]), 429);
}
