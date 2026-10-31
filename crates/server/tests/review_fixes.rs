// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Regression tests for the review findings against the Rust port (see the commit messages).
mod common;

use common::*;
use printpapi_server::dispatch::{Fetcher, HttpFetcher};
use printpapi_server::store::DEFAULT_ORG;
use serde_json::json;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn utf16(s: &str, bom: &[u8], be: bool) -> Vec<u8> {
    let mut out = bom.to_vec();
    for u in s.encode_utf16() {
        out.extend_from_slice(&if be { u.to_be_bytes() } else { u.to_le_bytes() });
    }
    out
}

#[test]
fn json_bodies_with_a_bom_or_utf16_are_accepted_like_python_json_loads() {
    let app = App::new();
    let reg = register(&app.db, "w", "k", &[decl("Z", false)], DEFAULT_ORG);
    let body = serde_json::to_string(&job_body(reg.pid("Z"))).unwrap();
    let mut bom8 = vec![0xEF, 0xBB, 0xBF];
    bom8.extend_from_slice(body.as_bytes());
    let cases = [
        bom8,
        utf16(&body, &[0xFF, 0xFE], false),
        utf16(&body, &[0xFE, 0xFF], true),
        utf16(&body, &[], true),  // no BOM: detected from the NUL pattern
        utf16(&body, &[], false), // ditto
    ];
    for raw in cases {
        let r = app.call("POST", "/jobs").bearer("t").raw(&raw).send();
        assert_eq!(r.status, 200, "{raw:?} -> {}", r.text());
    }
    // still bad json when it is bad
    let r = app
        .call("POST", "/jobs")
        .bearer("t")
        .raw(b"\xEF\xBB\xBF{")
        .send();
    assert_eq!(r.status, 400);
}

#[test]
fn negative_zero_reads_as_integer_zero() {
    let app = App::new();
    let r = app
        .call("PUT", "/orgs/1")
        .bearer("t")
        .raw(br#"{"job_quota": -0}"#)
        .send();
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["job_quota"], json!(0));
}

#[test]
fn non_string_printer_and_agent_names_follow_sqlite_conversion() {
    let app = App::with(|c| c.open_enrollment = true);
    // null printer name: Python hit NOT NULL (409); here a clean 400, nothing created
    let r = app.agent(
        "POST",
        "/agent/register",
        "k1",
        Some(json!({"name": "a1", "printers": [{"name": null}]})),
    );
    assert_eq!(r.status, 400, "{}", r.text());
    // bool printer name binds as 1
    let r = app.agent(
        "POST",
        "/agent/register",
        "k2",
        Some(json!({"name": "a2", "printers": [{"name": true}]})),
    );
    assert_eq!(r.status, 200);
    assert!(r.json()["printer_ids"].get("1").is_some(), "{}", r.text());
    // numeric agent name is stored as its text
    let r = app.agent("POST", "/agent/register", "k3", Some(json!({"name": 5})));
    assert_eq!(r.status, 200, "{}", r.text());
}

#[test]
fn bool_title_is_stored_as_one() {
    let app = App::new();
    let reg = register(&app.db, "w", "k", &[decl("Z", false)], DEFAULT_ORG);
    let mut b = job_body(reg.pid("Z"));
    b["title"] = json!(true);
    let (code, v) = app.post("/jobs", Some("t"), b);
    assert_eq!(code, 200);
    let id = job_id(&v);
    let title: String = app.db.with(|c| {
        c.query_row("SELECT title FROM jobs WHERE id=?1", [id], |r| r.get(0))
            .unwrap()
    });
    assert_eq!(title, "1");
}

/// Accepts connections and never answers.
fn tarpit() -> (
    String,
    std::thread::JoinHandle<()>,
    Arc<std::sync::atomic::AtomicBool>,
) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.set_nonblocking(true).unwrap();
    let url = format!("http://{}/slow", l.local_addr().unwrap());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s = stop.clone();
    let h = std::thread::spawn(move || {
        let mut held = Vec::new();
        while !s.load(std::sync::atomic::Ordering::Relaxed) {
            if let Ok((c, _)) = l.accept() {
                held.push(c);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    (url, h, stop)
}

#[test]
fn slow_fetches_cannot_take_every_thread() {
    let (url, h, stop) = tarpit();
    let f = Arc::new(HttpFetcher::with_limits(
        Duration::from_secs(30),
        Duration::from_secs(2),
        2,
    ));
    let hs: Vec<_> = (0..2)
        .map(|_| {
            let (f, u) = (f.clone(), url.clone());
            std::thread::spawn(move || f.get(&u))
        })
        .collect();
    std::thread::sleep(Duration::from_millis(500));
    // the third is refused at once instead of waiting for a thread
    let t = Instant::now();
    let e = f.get(&url).unwrap_err();
    assert!(t.elapsed() < Duration::from_secs(1), "{e}");
    assert!(e.contains("too many concurrent"), "{e}");
    // the held two end at the global deadline (30 s per-read would have outlasted it)
    for h in hs {
        let t = Instant::now();
        assert!(h.join().unwrap().is_err());
        assert!(t.elapsed() < Duration::from_secs(5));
    }
    // and the slots come back
    assert!(!f.get(&url).unwrap_err().contains("too many concurrent"));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    h.join().unwrap();
}
