// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Smoke test of the built binary: environment handling and a real request over TCP.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::Duration;

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_printpapi"));
    for k in [
        "PRINTAPI_TOKEN",
        "PRINT_DB",
        "PRINT_PORT",
        "PRINTAPI_PLANS",
        "PRINTAPI_SIGNUP",
        "LOG_REQUESTS",
    ] {
        c.env_remove(k);
    }
    c
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn missing_token_exits_1() {
    let out = bin().output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("PRINTAPI_TOKEN is required"));
}

#[test]
fn blank_token_exits_1() {
    let out = bin().env("PRINTAPI_TOKEN", "  ").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("PRINTAPI_TOKEN must not be empty"));
}

#[test]
fn bad_port_exits_1() {
    let out = bin()
        .env("PRINTAPI_TOKEN", "t")
        .env("PRINT_PORT", "abc")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn bad_plans_exit_1() {
    let out = bin()
        .env("PRINTAPI_TOKEN", "t")
        .env("PRINTAPI_PLANS", "[]")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("non-empty JSON array"));
}

#[test]
fn version_flag_prints_the_version() {
    let out = bin().arg("--version").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("printpapi {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn serves_health_over_tcp() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let mut child = bin()
        .env("PRINTAPI_TOKEN", "t")
        .env("PRINT_DB", dir.path().join("p.db"))
        .env("PRINT_PORT", port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let result = std::panic::catch_unwind(|| {
        assert!(line.contains(&format!("listening on :{port}")), "{line:?}");
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(
            s,
            "GET /health HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    });
    child.kill().unwrap();
    let _ = child.wait();
    result.unwrap();
}
