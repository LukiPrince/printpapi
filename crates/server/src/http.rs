// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! hyper glue: accept loop, body limit, request/response conversion, the async agent long-poll,
//! access log, background loops, graceful shutdown.
//!
//! Every request except the long-poll is read fully and routed through the (synchronous)
//! `server::handle` on tokio's blocking pool: the handler does SQLite, scrypt and outbound HTTP.
use crate::dispatch::{Fetcher, HttpFetcher};
use crate::server::{self, AppState, Req, Resp, RespBody};
use crate::store;
use bytes::Bytes;
use http_body_util::{BodyExt, Limited};
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::header::{AUTHORIZATION, CONTENT_LENGTH, HeaderName, HeaderValue};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::json;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::task::JoinSet;
use tokio::time::Instant;

/// ponytail: flat 32 MB body cap (the Python `_MAX_BODY`); make env-tunable if someone needs bigger.
pub const MAX_BODY: usize = 32 * 1024 * 1024;

/// How long a client may take to send its request head (slow-loris guard).
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// In-flight requests get this long to finish once shutdown starts.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// `server::handle`'s shape; tests inject fakes.
pub type Handler = fn(&AppState, &Req) -> Resp;

struct Ctx {
    state: Arc<AppState>,
    handler: Handler,
    max_body: usize,
    body_timeout: Duration,
}

/// How long a client may take to send a request body. Without it a client trickling bytes keeps
/// its buffer (up to MAX_BODY) and its connection forever — the Python server had no limit
/// either. 32 MB in 120 s still allows a ~2 Mbit/s uplink.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(120);

/// Serve HTTP/1.1 on `listener` until `shutdown` resolves. Every request except `GET /agent/jobs`
/// is read fully (capped at MAX_BODY) and routed through `server::handle` on the blocking pool.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<AppState>,
    shutdown: impl std::future::Future<Output = ()>,
) -> std::io::Result<()> {
    serve_with(listener, state, server::handle, shutdown).await
}

pub async fn serve_with(
    listener: tokio::net::TcpListener,
    state: Arc<AppState>,
    handler: Handler,
    shutdown: impl std::future::Future<Output = ()>,
) -> std::io::Result<()> {
    serve_limited(
        listener,
        state,
        handler,
        MAX_BODY,
        BODY_READ_TIMEOUT,
        shutdown,
    )
    .await
}

async fn serve_limited(
    listener: tokio::net::TcpListener,
    state: Arc<AppState>,
    handler: Handler,
    max_body: usize,
    body_timeout: Duration,
    shutdown: impl std::future::Future<Output = ()>,
) -> std::io::Result<()> {
    let ctx = Arc::new(Ctx {
        state,
        handler,
        max_body,
        body_timeout,
    });
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let mut conns = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            // reap finished connection tasks so the set does not grow with every request
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(a) => a,
                    Err(e) => {
                        // fd exhaustion and friends: back off instead of spinning
                        eprintln!("accept error: {e}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let _ = stream.set_nodelay(true);
                conns.spawn(run_connection(stream, peer, ctx.clone(), stop_rx.clone()));
            }
        }
    }
    drop(listener);
    let _ = stop_tx.send(true);
    // ponytail: in-flight requests (incl. long-polls) get SHUTDOWN_GRACE, then they are dropped.
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, async {
        while conns.join_next().await.is_some() {}
    })
    .await;
    Ok(())
}

async fn run_connection(
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    ctx: Arc<Ctx>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let svc = service_fn(move |req| respond(ctx.clone(), peer, req));
    let conn = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT)
        .serve_connection(TokioIo::new(stream), svc);
    tokio::pin!(conn);
    tokio::select! {
        _ = conn.as_mut() => {}
        _ = async { let _ = stop.wait_for(|s| *s).await; } => {
            // finish the request in flight, close idle keep-alive connections right away
            conn.as_mut().graceful_shutdown();
            let _ = conn.await;
        }
    }
}

async fn respond(
    ctx: Arc<Ctx>,
    peer: SocketAddr,
    req: Request<Incoming>,
) -> Result<Response<OutBody>, Infallible> {
    let (parts, body) = req.into_parts();
    let method = parts.method.as_str().to_string();
    let target = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| parts.uri.to_string());
    let version = format!("{:?}", parts.version);
    let client = client_addr(&ctx.state.cfg.trusted_proxies, peer, &parts.headers);

    let mut close = false;
    let resp = if method == "GET" && target == "/agent/jobs" {
        // Python compared self.path, so "/agent/jobs?x=1" is not the long-poll.
        let key = bearer(&parts.headers);
        // The operator token is no agent key either: an empty key answers 401 at once.
        let key = if server::is_operator_token(&ctx.state.cfg, key) {
            ""
        } else {
            key
        };
        long_poll(&ctx.state, key).await
    } else {
        let declared = parts
            .headers
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());
        let (body, body_error) = if declared.is_some_and(|n| n > ctx.max_body as u64) {
            (Bytes::new(), true)
        } else {
            // ponytail: the body is buffered before routing, so aggregate memory is bounded only
            // by connections x MAX_BODY (as in the Python, which read most bodies before auth
            // too). A public deployment caps connections and body sizes in its reverse proxy.
            let read = Limited::new(body, ctx.max_body).collect();
            match tokio::time::timeout(ctx.body_timeout, read).await {
                Ok(Ok(c)) => (c.to_bytes(), false),
                Ok(Err(_)) | Err(_) => (Bytes::new(), true),
            }
        };
        // Unread or half-read body: the connection is no longer in sync, so it cannot be reused.
        close = body_error;
        let req = Req {
            method: method.clone(),
            target: target.clone(),
            headers: parts.headers,
            body,
            body_error,
            peer_ip: client.clone(),
            socket_peer: peer.ip().to_string(),
        };
        let (st, handler) = (ctx.state.clone(), ctx.handler);
        match tokio::task::spawn_blocking(move || handler(&st, &req)).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("handler failed: {e}");
                internal_error()
            }
        }
    };
    if ctx.state.cfg.log_requests {
        eprintln!(
            "{}",
            access_log_line(
                &client,
                &log_timestamp(),
                &method,
                &target,
                &version,
                resp.status
            )
        );
    }
    Ok(build_response(resp, method == "HEAD", close))
}

/// The client's address as text: the socket peer, or what `X-Forwarded-For` says when the peer is
/// a trusted proxy. A header value that is not text counts as garbage, never as absent.
fn client_addr(
    trusted: &crate::proxy::TrustedProxies,
    peer: SocketAddr,
    headers: &hyper::HeaderMap,
) -> String {
    if trusted.is_empty() {
        return peer.ip().to_string();
    }
    let lines: Vec<String> = headers
        .get_all("x-forwarded-for")
        .iter()
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .collect();
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    crate::proxy::client_ip(peer.ip(), &lines, trusted).to_string()
}

fn internal_error() -> Resp {
    Resp::json(500, &json!({"error": "internal error"}))
}

/// The Bearer token (Python `auth[7:] if auth.startswith("Bearer ")`); "" when absent or not text.
fn bearer(headers: &hyper::HeaderMap) -> &str {
    headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
}

/// `GET /agent/jobs`: hold the request until a job is queued for the agent or the timeout passes.
/// Waits on the agent's `Waker` (enqueue wakes it) with a timed recheck as the safety net, so an
/// idle agent costs a task, not a thread.
async fn long_poll(state: &Arc<AppState>, key: &str) -> Resp {
    let unauthorized = || Resp::json(401, &json!({"error": "unauthorized"}));
    if key.is_empty() {
        return unauthorized();
    }
    let key = key.to_string();
    let k = key.clone();
    let aid = match poll_db(state, move |c| store::authenticate_agent(c, &k)).await {
        Ok(Ok(Some(id))) => id,
        Ok(Ok(None)) => return unauthorized(),
        Ok(Err(e)) => {
            eprintln!("agent auth error: {e}");
            return internal_error();
        }
        Err(_) => return internal_error(),
    };
    let timeout =
        Duration::try_from_secs_f64(state.cfg.long_poll_timeout_s).unwrap_or(Duration::ZERO);
    // at least 10 ms, so a poll_interval of 0 cannot spin
    let interval = Duration::try_from_secs_f64(state.cfg.poll_interval_s)
        .unwrap_or(Duration::from_secs(5))
        .max(Duration::from_millis(10));
    let deadline = Instant::now() + timeout;
    // Dropped on every way out, a client hanging up included: the waker map then holds only the
    // agents that are waiting right now, not every agent that ever polled.
    let slot = server::WaitSlot::new(&state.waker, aid);
    loop {
        // Register for the wake-up BEFORE claiming, so an enqueue between the claim and the wait
        // is not lost.
        let notified = slot.notify().notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let k = key.clone();
        // The key is re-checked with every claim, under the same lock: one revoked while this
        // poll waits claims nothing (the revocation also wakes the poll, see DELETE /apikeys).
        let claimed = poll_db(state, move |c| -> store::Result<Option<_>> {
            if store::authenticate_agent(c, &k)? != Some(aid) {
                return Ok(None);
            }
            store::claim_job(c, aid, None).map(Some)
        })
        .await;
        match claimed {
            Ok(Ok(None)) => return unauthorized(),
            Ok(Ok(Some(Some(job)))) => {
                let v = serde_json::to_value(&job).expect("a ClaimedJob always serializes");
                return Resp::json(200, &v);
            }
            Ok(Ok(Some(None))) => {}
            Ok(Err(e)) => {
                eprintln!("claim error: {e}");
                return internal_error();
            }
            Err(_) => return internal_error(),
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Resp::empty(204);
        }
        tokio::select! {
            _ = &mut notified => {}
            _ = tokio::time::sleep(interval.min(left)) => {}
        }
    }
}

/// A long-poll DB call on the blocking pool, at most `server::POLL_SLOTS` at a time. The permit
/// moves into the blocking task, so a client hanging up mid-call does not free its slot early.
async fn poll_db<T: Send + 'static>(
    state: &Arc<AppState>,
    f: impl FnOnce(&rusqlite::Connection) -> T + Send + 'static,
) -> Result<T, ()> {
    let permit = state
        .poll_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ())?;
    let st = state.clone();
    tokio::task::spawn_blocking(move || {
        let _slot = permit;
        st.db.with(f)
    })
    .await
    .map_err(|_| ())
}

/// Response body: empty, one buffer, or the same buffer `left` times (CloudPRNT copies).
enum OutBody {
    Empty,
    Full(Option<Bytes>),
    Repeat { chunk: Bytes, left: usize },
}

impl Body for OutBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let frame = match self.get_mut() {
            OutBody::Empty => None,
            OutBody::Full(b) => b.take().filter(|b| !b.is_empty()),
            OutBody::Repeat { chunk, left } => {
                if *left == 0 || chunk.is_empty() {
                    None
                } else {
                    *left -= 1;
                    Some(chunk.clone())
                }
            }
        };
        Poll::Ready(frame.map(|b| Ok(Frame::data(b))))
    }

    fn is_end_stream(&self) -> bool {
        match self {
            OutBody::Empty => true,
            OutBody::Full(b) => b.as_ref().is_none_or(|b| b.is_empty()),
            OutBody::Repeat { chunk, left } => *left == 0 || chunk.is_empty(),
        }
    }

    fn size_hint(&self) -> SizeHint {
        let n = match self {
            OutBody::Empty => 0,
            OutBody::Full(b) => b.as_ref().map_or(0, |b| b.len() as u64),
            OutBody::Repeat { chunk, left } => (chunk.len() as u64).saturating_mul(*left as u64),
        };
        SizeHint::with_exact(n)
    }
}

/// `Resp` -> hyper response. Content-Length is always set (except 1xx/204, which carry no body);
/// a HEAD answer keeps its headers (incl. the handler's own Content-Length) and drops the body.
fn build_response(resp: Resp, head: bool, close: bool) -> Response<OutBody> {
    let Ok(status) = StatusCode::from_u16(resp.status) else {
        return build_response(internal_error(), head, close);
    };
    let bodyless = status == StatusCode::NO_CONTENT || status.is_informational();
    let (body, len) = match resp.body {
        _ if bodyless => (OutBody::Empty, 0),
        RespBody::Empty => (OutBody::Empty, 0),
        RespBody::Bytes(b) => {
            let n = b.len();
            (OutBody::Full(Some(b)), n as u64)
        }
        RespBody::Repeat(b, n) => {
            let total = (b.len() as u64).saturating_mul(n as u64);
            (OutBody::Repeat { chunk: b, left: n }, total)
        }
    };
    let mut out = Response::new(if head { OutBody::Empty } else { body });
    *out.status_mut() = status;
    let headers = out.headers_mut();
    let mut has_len = false;
    for (name, value) in resp.headers {
        let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) else {
            continue;
        };
        if n == CONTENT_LENGTH {
            if bodyless {
                continue;
            }
            has_len = true;
        }
        headers.append(n, v);
    }
    if !bodyless && !has_len {
        headers.insert(CONTENT_LENGTH, HeaderValue::from(len));
    }
    if close {
        headers.insert(hyper::header::CONNECTION, HeaderValue::from_static("close"));
    }
    out
}

/// `BaseHTTPRequestHandler.log_message` format: `<ip> - - [date] "<request line>" <status> -`.
fn access_log_line(
    ip: &str,
    when: &str,
    method: &str,
    target: &str,
    version: &str,
    status: u16,
) -> String {
    // The query of an `/operator/` request can name a customer (`q=` searches e-mail addresses),
    // so it stays out of the log.
    let target = if target.starts_with("/operator/") {
        target.split('?').next().unwrap_or(target)
    } else {
        target
    };
    format!("{ip} - - [{when}] \"{method} {target} {version}\" {status} -")
}

/// `31/Oct/2026 01:52:18`, local time.
fn log_timestamp() -> String {
    chrono::Local::now().format("%d/%b/%Y %H:%M:%S").to_string()
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

/// Start the reaper (every 30 s: timeout 300 s, 2 retries) and the webhook dispatcher (every 5 s,
/// 5 attempts, 10 s POST timeout) on their own threads, like the Python daemon threads.
pub fn start_background(state: Arc<AppState>) {
    let s = state.clone();
    std::thread::spawn(move || {
        loop {
            if let Err(p) = catch_unwind(AssertUnwindSafe(|| server::reaper_pass(&s, 300.0, 2))) {
                eprintln!("reaper error: {}", panic_text(&*p));
            }
            std::thread::sleep(Duration::from_secs(30));
        }
    });
    // ponytail: one thread, sequential delivery — a slow callback delays the ones behind it (bounded
    // by the 10 s timeout x attempt cap). A worker pool / async delivery only if hook volume grows.
    std::thread::spawn(move || {
        // background sender: shorter timeout than the 30 s payload fetcher
        // 15 s overall: a callback that trickles its answer must not stall the one dispatcher.
        let fetcher = HttpFetcher::with_limits_to(
            Duration::from_secs(10),
            Duration::from_secs(15),
            4,
            state.cfg.destinations,
        );
        let post = |url: &str, body: &serde_json::Value| fetcher.post_json(url, body).map(|_| ());
        loop {
            let pass = catch_unwind(AssertUnwindSafe(|| {
                server::deliver_webhooks(&state.db, &post, 5);
                server::deliver_agent_events(&state.db, &post, state.cfg.online_window_s, None);
            }));
            if let Err(p) = pass {
                eprintln!("webhook dispatcher error: {}", panic_text(&*p));
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

/// For tests: bind 127.0.0.1:0, serve on a dedicated runtime thread, return the address. The
/// server lives until the process exits (like the Python tests' daemon threads).
pub fn spawn_test_server(state: Arc<AppState>) -> SocketAddr {
    spawn_test_server_with(state, server::handle)
}

pub fn spawn_test_server_with(state: Arc<AppState>, handler: Handler) -> SocketAddr {
    spawn_test_server_limited(state, handler, MAX_BODY, BODY_READ_TIMEOUT)
}

fn spawn_test_server_limited(
    state: Arc<AppState>,
    handler: Handler,
    max_body: usize,
    body_timeout: Duration,
) -> SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("test runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            tx.send(listener.local_addr().expect("addr"))
                .expect("report addr");
            let _ = serve_limited(
                listener,
                state,
                handler,
                max_body,
                body_timeout,
                std::future::pending(),
            )
            .await;
        });
    });
    rx.recv().expect("test server started")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::Config;
    use std::io::{Read, Write};

    #[test]
    fn access_log_line_matches_basehttp_format() {
        assert_eq!(
            access_log_line(
                "10.0.0.7",
                "31/Oct/2026 01:52:18",
                "GET",
                "/health",
                "HTTP/1.1",
                200
            ),
            "10.0.0.7 - - [31/Oct/2026 01:52:18] \"GET /health HTTP/1.1\" 200 -"
        );
    }

    #[test]
    fn operator_paths_are_logged_without_their_query_string() {
        let line = access_log_line(
            "10.0.0.7",
            "31/Oct/2026 01:52:18",
            "GET",
            "/operator/orgs?q=alice%40shop.test&status=active",
            "HTTP/1.1",
            200,
        );
        assert_eq!(
            line,
            "10.0.0.7 - - [31/Oct/2026 01:52:18] \"GET /operator/orgs HTTP/1.1\" 200 -"
        );
        // every other path keeps what it always logged
        let line = access_log_line("10.0.0.7", "t", "GET", "/jobs?limit=5", "HTTP/1.1", 200);
        assert!(line.contains("/jobs?limit=5"), "{line}");
    }

    #[test]
    fn log_timestamp_has_python_shape() {
        let t = log_timestamp(); // 31/Oct/2026 01:52:18
        let b = t.as_bytes();
        assert_eq!(b.len(), 20, "{t}");
        assert_eq!(
            (b[2], b[6], b[11], b[14], b[17]),
            (b'/', b'/', b' ', b':', b':')
        );
    }

    fn state() -> Arc<AppState> {
        Arc::new(AppState::new(
            Config::new("t"),
            store::Db::open(":memory:").unwrap(),
            Arc::new(crate::mail::SmtpMailer),
            Arc::new(HttpFetcher::new(Duration::from_secs(1))),
        ))
    }

    /// Echoes what the transport saw of the body.
    fn echo(_: &AppState, req: &Req) -> Resp {
        Resp::json(
            if req.body_error { 400 } else { 200 },
            &json!({"len": req.body.len(), "body_error": req.body_error}),
        )
    }

    fn roundtrip(addr: SocketAddr, raw: &[u8]) -> String {
        let mut s = std::net::TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(raw).unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn oversized_chunked_body_sets_body_error_and_closes() {
        let addr = spawn_test_server_limited(state(), echo, 1000, BODY_READ_TIMEOUT);
        let mut raw = b"POST /x HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for _ in 0..3 {
            raw.extend_from_slice(format!("{:x}\r\n", 600).as_bytes());
            raw.extend_from_slice(&[b'a'; 600]);
            raw.extend_from_slice(b"\r\n");
        }
        raw.extend_from_slice(b"0\r\n\r\n");
        let r = roundtrip(addr, &raw);
        assert!(r.starts_with("HTTP/1.1 400"), "{r}");
        assert!(r.to_lowercase().contains("connection: close"), "{r}");
        assert!(r.contains("\"body_error\": true"), "{r}");
    }

    #[test]
    fn a_body_that_trickles_past_the_deadline_is_cut_off() {
        let addr = spawn_test_server_limited(state(), echo, 1000, Duration::from_millis(300));
        let mut s = std::net::TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        s.write_all(
            b"POST /x HTTP/1.1
Host: t
Content-Length: 10

ab",
        )
        .unwrap();
        let t0 = std::time::Instant::now();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out); // the rest of the body never comes
        let r = String::from_utf8_lossy(&out);
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "waited {:?}",
            t0.elapsed()
        );
        assert!(r.starts_with("HTTP/1.1 400"), "{r}");
        assert!(r.contains("\"body_error\": true"), "{r}");
    }

    #[test]
    fn chunked_body_within_limit_is_read() {
        let addr = spawn_test_server_limited(state(), echo, 1000, BODY_READ_TIMEOUT);
        let raw = b"POST /x HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        let r = roundtrip(addr, raw);
        assert!(r.starts_with("HTTP/1.1 200"), "{r}");
        assert!(r.contains("\"len\": 5"), "{r}");
    }
}
