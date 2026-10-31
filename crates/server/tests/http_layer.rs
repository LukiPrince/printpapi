// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The hyper layer, driven over real sockets with fake handlers (and the real store for the
//! long-poll). Raw TCP on purpose: the wire format is what is under test.
use bytes::Bytes;
use printpapi_server::dispatch::HttpFetcher;
use printpapi_server::http::{MAX_BODY, spawn_test_server_with};
use printpapi_server::mail::SmtpMailer;
use printpapi_server::server::{AppState, Config, Req, Resp, RespBody};
use printpapi_server::store::{self, Db, NewJob, PrinterDecl};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn state_with(cfg: Config) -> Arc<AppState> {
    Arc::new(AppState::new(
        cfg,
        Db::open(":memory:").unwrap(),
        Arc::new(SmtpMailer),
        Arc::new(HttpFetcher::new(Duration::from_secs(1))),
    ))
}

fn fake(_: &AppState, req: &Req) -> Resp {
    match req.path() {
        "/ok" => Resp::json(200, &json!({"ok": true, "peer": req.peer_ip})),
        "/hello" => Resp {
            status: 200,
            headers: vec![("Content-Type", "text/plain".into())],
            body: RespBody::Bytes(Bytes::from_static(b"hello")),
        },
        "/head-hdr" => Resp {
            status: 200,
            headers: vec![("Content-Length", "5".into())],
            body: RespBody::Empty,
        },
        "/nocontent" => Resp::empty(204),
        "/empty" => Resp::empty(200),
        "/repeat" => Resp {
            status: 200,
            headers: vec![("Content-Type", "application/octet-stream".into())],
            body: RespBody::Repeat(Bytes::from_static(b"abc"), 4),
        },
        "/panic" => panic!("boom"),
        "/body" => Resp::json(
            if req.body_error { 400 } else { 200 },
            &json!({"len": req.body.len(), "body_error": req.body_error}),
        ),
        _ => Resp::json(404, &json!({"error": "not found"})),
    }
}

fn server() -> SocketAddr {
    spawn_test_server_with(state_with(Config::new("t")), fake)
}

struct Reply {
    head: String,
    body: Vec<u8>,
}

impl Reply {
    fn status(&self) -> &str {
        self.head.lines().next().unwrap_or("")
    }
    fn header(&self, name: &str) -> Option<String> {
        let prefix = format!("{}:", name.to_lowercase());
        self.head
            .lines()
            .skip(1)
            .find(|l| l.to_lowercase().starts_with(&prefix))
            .map(|l| l[prefix.len()..].trim().to_string())
    }
}

/// Read one response off a keep-alive connection. `head_only` = the request was HEAD.
fn read_reply(s: &mut TcpStream, head_only: bool) -> Reply {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        let n = s.read(&mut byte).expect("read head");
        assert!(
            n == 1,
            "connection closed mid-head: {:?}",
            String::from_utf8_lossy(&buf)
        );
        buf.push(byte[0]);
    }
    let head = String::from_utf8(buf).unwrap();
    let mut r = Reply {
        head,
        body: Vec::new(),
    };
    let no_body = head_only || r.status().contains(" 204 ");
    if !no_body {
        let n: usize = r
            .header("content-length")
            .expect("content-length")
            .parse()
            .unwrap();
        r.body = vec![0; n];
        s.read_exact(&mut r.body).expect("read body");
    }
    r
}

fn connect(addr: SocketAddr) -> TcpStream {
    let s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s
}

fn get(addr: SocketAddr, path: &str, extra: &str) -> Reply {
    let mut s = connect(addr);
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: t\r\n{extra}Connection: close\r\n\r\n"
    )
    .unwrap();
    read_reply(&mut s, false)
}

#[test]
fn content_length_is_set_and_peer_ip_reaches_the_handler() {
    let r = get(server(), "/ok", "");
    assert_eq!(r.status(), "HTTP/1.1 200 OK");
    assert_eq!(
        r.header("content-length")
            .unwrap()
            .parse::<usize>()
            .unwrap(),
        r.body.len()
    );
    let v: Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(v, json!({"ok": true, "peer": "127.0.0.1"}));
    assert_eq!(r.header("content-type").unwrap(), "application/json");
}

#[test]
fn empty_body_gets_content_length_zero() {
    let r = get(server(), "/empty", "");
    assert_eq!(r.header("content-length").unwrap(), "0");
    assert!(r.body.is_empty());
}

#[test]
fn head_has_no_body_but_keeps_content_length() {
    let addr = server();
    let mut s = connect(addr);
    write!(s, "HEAD /hello HTTP/1.1\r\nHost: t\r\n\r\n").unwrap();
    let r = read_reply(&mut s, true);
    assert_eq!(r.status(), "HTTP/1.1 200 OK");
    assert_eq!(r.header("content-length").unwrap(), "5");
    // the connection stays in sync: the next response is not preceded by stray body bytes
    write!(
        s,
        "GET /ok HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    assert_eq!(read_reply(&mut s, false).status(), "HTTP/1.1 200 OK");
    // a handler-supplied Content-Length (Python HEAD answers) wins and is not doubled
    let mut s = connect(addr);
    write!(
        s,
        "HEAD /head-hdr HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let r = read_reply(&mut s, true);
    assert_eq!(
        r.head.to_lowercase().matches("content-length").count(),
        1,
        "{}",
        r.head
    );
    assert_eq!(r.header("content-length").unwrap(), "5");
}

#[test]
fn no_content_has_no_body_and_no_content_length() {
    let r = get(server(), "/nocontent", "");
    assert_eq!(r.status(), "HTTP/1.1 204 No Content");
    assert!(r.header("content-length").is_none(), "{}", r.head);
    assert!(r.body.is_empty());
}

#[test]
fn repeat_streams_n_copies_with_the_right_length() {
    let r = get(server(), "/repeat", "");
    assert_eq!(r.header("content-length").unwrap(), "12");
    assert_eq!(r.body, b"abcabcabcabc");
}

#[test]
fn declared_oversized_content_length_is_not_read_and_closes() {
    let mut s = connect(server());
    write!(
        s,
        "POST /body HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n\r\n",
        MAX_BODY + 1
    )
    .unwrap();
    let r = read_reply(&mut s, false);
    assert_eq!(r.status(), "HTTP/1.1 400 Bad Request");
    assert_eq!(r.header("connection").unwrap().to_lowercase(), "close");
    assert_eq!(
        serde_json::from_slice::<Value>(&r.body).unwrap()["body_error"],
        true
    );
    let mut rest = Vec::new();
    assert_eq!(
        s.read_to_end(&mut rest).unwrap_or(0),
        0,
        "server closes the connection"
    );
}

#[test]
fn small_body_arrives_whole() {
    let mut s = connect(server());
    write!(
        s,
        "POST /body HTTP/1.1\r\nHost: t\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"
    )
    .unwrap();
    let r = read_reply(&mut s, false);
    assert_eq!(
        serde_json::from_slice::<Value>(&r.body).unwrap(),
        json!({"len": 5, "body_error": false})
    );
}

#[test]
fn bad_content_length_rejected() {
    let addr = server();
    for cl in ["-1", "999999999999", "nan"] {
        let mut s = connect(addr);
        write!(
            s,
            "POST /body HTTP/1.1\r\nHost: t\r\nContent-Length: {cl}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        let text = String::from_utf8_lossy(&out);
        let first = text.lines().next().unwrap_or("");
        assert!(
            first.contains(" 400 "),
            "Content-Length {cl}: got {first:?}"
        );
    }
    // server still alive afterwards
    assert_eq!(get(addr, "/ok", "").status(), "HTTP/1.1 200 OK");
}

#[test]
fn keep_alive_serves_two_requests_on_one_connection() {
    let mut s = connect(server());
    write!(s, "GET /ok HTTP/1.1\r\nHost: t\r\n\r\n").unwrap();
    assert_eq!(read_reply(&mut s, false).status(), "HTTP/1.1 200 OK");
    write!(
        s,
        "GET /hello HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let r = read_reply(&mut s, false);
    assert_eq!(r.body, b"hello");
}

#[test]
fn panicking_handler_gives_500_and_the_server_keeps_running() {
    let addr = server();
    let r = get(addr, "/panic", "");
    assert_eq!(r.status(), "HTTP/1.1 500 Internal Server Error");
    assert_eq!(
        serde_json::from_slice::<Value>(&r.body).unwrap(),
        json!({"error": "internal error"})
    );
    assert_eq!(get(addr, "/ok", "").status(), "HTTP/1.1 200 OK");
}

// ---- long-poll with the real store ----

fn poll_state(timeout: f64, interval: f64) -> (Arc<AppState>, SocketAddr, i64) {
    let mut cfg = Config::new("t");
    cfg.long_poll_timeout_s = timeout;
    cfg.poll_interval_s = interval;
    let state = state_with(cfg);
    let aid = state.db.with(|c| {
        let printer = PrinterDecl {
            name: "P1".into(),
            can_pdf: true,
            capabilities: None,
        };
        let reg = store::register_agent(c, "pc", "agent-key", &[printer], 1).unwrap();
        reg.computer_id
    });
    let addr = spawn_test_server_with(state.clone(), fake);
    (state, addr, aid)
}

fn enqueue(state: &AppState) -> store::Enqueued {
    state.db.with(|c| {
        let printer_id = c
            .query_row("SELECT id FROM printers LIMIT 1", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap();
        let pid = json!(printer_id);
        store::enqueue_job(
            c,
            &NewJob {
                printer_id: &pid,
                type_: "pdf_base64",
                mode: "pdf",
                payload: b"%PDF",
                user_id: 1,
                title: None,
                copies: 1,
                callback_url: None,
                options: None,
                org_id: Some(1),
                idempotency_key: None,
                expire_after: None,
            },
        )
        .unwrap()
    })
}

const AGENT: &str = "Authorization: Bearer agent-key\r\n";

#[test]
fn long_poll_rejects_missing_and_wrong_key() {
    let (_s, addr, _) = poll_state(0.3, 0.05);
    for extra in [
        "",
        "Authorization: Bearer nope\r\n",
        "Authorization: Basic abc\r\n",
    ] {
        let r = get(addr, "/agent/jobs", extra);
        assert_eq!(r.status(), "HTTP/1.1 401 Unauthorized", "{extra:?}");
        assert_eq!(
            serde_json::from_slice::<Value>(&r.body).unwrap(),
            json!({"error": "unauthorized"})
        );
    }
}

#[test]
fn long_poll_times_out_with_204_and_marks_the_agent_seen() {
    let (state, addr, aid) = poll_state(0.3, 0.05);
    let t0 = Instant::now();
    let r = get(addr, "/agent/jobs", AGENT);
    let took = t0.elapsed();
    assert_eq!(r.status(), "HTTP/1.1 204 No Content");
    assert!(took >= Duration::from_millis(250), "{took:?}");
    let seen: f64 = state
        .db
        .with(|c| {
            c.query_row("SELECT last_seen_at FROM agents WHERE id=?", [aid], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert!(printpapi_server::now() - seen < 5.0);
    // the poll is over, so the agent no longer holds a notifier
    assert_eq!(state.waker.tracked(), 0);
}

#[test]
fn a_waiting_agent_keeps_its_notifier_until_its_poll_ends() {
    let (state, addr, _) = poll_state(1.0, 10.0);
    let poll = std::thread::spawn(move || get(addr, "/agent/jobs", AGENT).status().to_string());
    let t0 = Instant::now();
    while state.waker.tracked() == 0 && t0.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(state.waker.tracked(), 1, "registered while waiting");
    assert_eq!(poll.join().unwrap(), "HTTP/1.1 204 No Content");
    assert_eq!(state.waker.tracked(), 0, "released afterwards");
}

#[test]
fn long_poll_returns_a_queued_job_immediately() {
    let (state, addr, _) = poll_state(5.0, 10.0);
    let e = enqueue(&state);
    let t0 = Instant::now();
    let r = get(addr, "/agent/jobs", AGENT);
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert_eq!(r.status(), "HTTP/1.1 200 OK");
    let v: Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(v["job_id"], e.job_id);
    assert_eq!(v["mode"], "pdf");
}

#[test]
fn long_poll_is_woken_by_an_enqueue_during_the_wait() {
    let (state, addr, aid) = poll_state(10.0, 10.0);
    let waiter = std::thread::spawn(move || get(addr, "/agent/jobs", AGENT));
    std::thread::sleep(Duration::from_millis(400)); // the poll is parked by now
    let e = enqueue(&state);
    assert_eq!(e.agent_id, aid);
    let t0 = Instant::now();
    state.waker.wake(e.agent_id);
    let r = waiter.join().unwrap();
    assert!(
        t0.elapsed() < Duration::from_millis(100),
        "{:?}",
        t0.elapsed()
    );
    assert_eq!(r.status(), "HTTP/1.1 200 OK");
    assert_eq!(
        serde_json::from_slice::<Value>(&r.body).unwrap()["job_id"],
        e.job_id
    );
}

#[test]
fn a_key_revoked_while_its_agent_waits_claims_nothing() {
    // the agent's key is an issued one (it has an api_keys row) and the poll is already waiting
    let (state, addr, _) = poll_state(3.0, 10.0);
    let kid = state
        .db
        .with(|c| store::add_api_key(c, "pc", "agent-key", 1))
        .unwrap();
    let poll = std::thread::spawn(move || get(addr, "/agent/jobs", AGENT));
    let t0 = Instant::now();
    while state.waker.tracked() == 0 && t0.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(10));
    }
    // revoke, queue a job, wake the poll as DELETE /apikeys and POST /jobs would
    state
        .db
        .with(|c| store::revoke_api_key(c, kid, None))
        .unwrap();
    let job = enqueue(&state);
    state.waker.wake_all();
    let r = poll.join().unwrap();
    assert_eq!(r.status(), "HTTP/1.1 401 Unauthorized");
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "answered on the wake-up, not the timeout"
    );
    let st: String = state.db.with(|c| {
        c.query_row("SELECT state FROM jobs WHERE id=?", [job.job_id], |r| {
            r.get(0)
        })
        .unwrap()
    });
    assert_eq!(st, "queued", "the revoked agent did not claim it");
}
