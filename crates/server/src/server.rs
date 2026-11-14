// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Request routing and the background passes — the port of `app/server.py`'s handler.
//!
//! `handle` is synchronous on purpose: it mirrors the Python handler 1:1 (same route order, same
//! status codes, same error texts) and runs on tokio's blocking pool, called from `http`. The one
//! route that waits — the agent long-poll `GET /agent/jobs` — lives in `http` as async code so an
//! idle agent costs no thread.
//!
//! The contract types below are what `http` and the integration tests code against.
use crate::auth::LoginLimiter;
use crate::billing::Plan;
use crate::dispatch::Fetcher;
use crate::mail::Mailer;
use crate::store::{Db, Metrics};
use bytes::Bytes;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

/// Everything `make_handler(...)` took as keyword arguments.
#[derive(Debug, Clone)]
pub struct Config {
    /// The bootstrap PRINTAPI_TOKEN (root). Never empty — `main` refuses to start without it.
    pub token: String,
    pub long_poll_timeout_s: f64,
    /// Safety-net recheck while a long-poll waits; the wake-up on enqueue is the normal path.
    pub poll_interval_s: f64,
    pub online_window_s: f64,
    pub session_ttl_s: f64,
    pub max_login_fails: u32,
    pub login_window_s: f64,
    pub signup_open: bool,
    /// PUBLIC_URL without a trailing slash; "" = reset mails carry the bare token only.
    pub public_url: String,
    pub reset_ttl_s: f64,
    pub reset_enabled: bool,
    pub plans: Vec<Plan>,
    pub billing_secret: Option<String>,
    /// LOG_REQUESTS set: one access-log line per request on stderr.
    pub log_requests: bool,
    /// Where payload URLs, job callbacks and org event URLs may point (PRINTAPI_PRIVATE_URLS).
    pub destinations: crate::dispatch::Destinations,
    /// PRINTAPI_OPEN_ENROLLMENT=1: any Bearer value may enroll a new agent into the default org
    /// (the v2 rule). Off: a new agent needs an issued client key (or the root token).
    pub open_enrollment: bool,
    /// PRINTAPI_TRUSTED_PROXIES: peers whose `X-Forwarded-For` names the real client. Empty: nobody.
    pub trusted_proxies: crate::proxy::TrustedProxies,
    /// PRINTAPI_SIGNUP_QUOTA: the monthly `job_quota` of every org `POST /signup` creates.
    /// None: no quota. Root-created orgs are never touched.
    pub signup_quota: Option<i64>,
    /// PRINTAPI_JOB_RETENTION_DAYS: finished jobs lose their content after this many days and the
    /// rows go with the month. None: keep for ever.
    pub job_retention_days: Option<i64>,
    /// PRINTAPI_TERMS_VERSION and its links: set, `POST /signup` needs `accept_terms` = version.
    pub terms: Option<Terms>,
    /// PRINTAPI_FETCHES_PER_ORG: payload downloads one org may have in flight; 0 = no cap. None:
    /// `MAX_FETCHES_PER_ORG` with open signup, else no cap (see `Config::fetch_cap`).
    pub fetches_per_org: Option<usize>,
    /// PRINTAPI_OPERATOR_TOKEN: the credential of the `/operator/` API. Never a `Principal`: only
    /// `operator_gate` reads it. None: every `/operator/` path is a 404.
    pub operator_token: Option<String>,
    /// PRINTAPI_OPERATOR_FROM: the socket peers the operator token is accepted from.
    pub operator_from: crate::proxy::TrustedProxies,
}

/// The terms a signup must accept (`PRINTAPI_TERMS_*`). The URLs are `http(s)` (checked at startup).
#[derive(Clone, Debug, PartialEq)]
pub struct Terms {
    pub version: String,
    pub terms_url: String,
    pub dpa_url: Option<String>,
    pub privacy_url: Option<String>,
}

impl Config {
    /// The Python defaults (`make_handler`'s keyword defaults) with the given root token.
    pub fn new(token: &str) -> Config {
        Config {
            token: token.to_string(),
            long_poll_timeout_s: 25.0,
            poll_interval_s: 5.0,
            online_window_s: 60.0,
            session_ttl_s: crate::auth::SESSION_TTL_S,
            max_login_fails: 10,
            login_window_s: 900.0,
            signup_open: false,
            public_url: String::new(),
            reset_ttl_s: 3600.0,
            reset_enabled: false,
            plans: Vec::new(),
            billing_secret: None,
            log_requests: false,
            destinations: crate::dispatch::Destinations::Any,
            open_enrollment: false,
            trusted_proxies: crate::proxy::TrustedProxies::default(),
            signup_quota: None,
            job_retention_days: None,
            terms: None,
            fetches_per_org: None,
            operator_token: None,
            operator_from: crate::proxy::TrustedProxies::default(),
        }
    }
}

/// Wakes the long-polls of one agent when a job is queued for it (or of all agents when the
/// reaper requeues). Replaces the Python 1 s DB poll per waiting agent.
#[derive(Default)]
pub struct Waker {
    agents: Mutex<HashMap<i64, Arc<Notify>>>,
}

impl Waker {
    /// The notifier an agent's long-poll waits on (created on first use).
    pub fn for_agent(&self, agent_id: i64) -> Arc<Notify> {
        let mut map = self.agents.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(agent_id).or_default().clone()
    }

    pub fn wake(&self, agent_id: i64) {
        let map = self.agents.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = map.get(&agent_id) {
            n.notify_waiters();
        }
    }

    pub fn wake_all(&self) {
        let map = self.agents.lock().unwrap_or_else(|e| e.into_inner());
        for n in map.values() {
            n.notify_waiters();
        }
    }

    /// How many agents have a notifier right now (tests).
    pub fn tracked(&self) -> usize {
        self.agents.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// One long-poll's hold on its agent's notifier. Dropping it removes the notifier once no other
/// poll of that agent holds it, so a fleet that registered once and went away leaves nothing.
pub struct WaitSlot<'a> {
    waker: &'a Waker,
    agent_id: i64,
    notify: Option<Arc<Notify>>,
}

impl<'a> WaitSlot<'a> {
    pub fn new(waker: &'a Waker, agent_id: i64) -> Self {
        WaitSlot {
            waker,
            agent_id,
            notify: Some(waker.for_agent(agent_id)),
        }
    }

    pub fn notify(&self) -> &Notify {
        self.notify.as_deref().expect("set until drop")
    }
}

impl Drop for WaitSlot<'_> {
    fn drop(&mut self) {
        let mut map = self.waker.agents.lock().unwrap_or_else(|e| e.into_inner());
        drop(self.notify.take());
        // Under the map lock no other poll can pick the entry up between the check and removal.
        if map
            .get(&self.agent_id)
            .is_some_and(|n| Arc::strong_count(n) == 1)
        {
            map.remove(&self.agent_id);
        }
    }
}

pub struct AppState {
    pub cfg: Config,
    pub db: Db,
    pub limiter: LoginLimiter,
    pub mailer: Arc<dyn Mailer>,
    pub fetcher: Arc<dyn Fetcher>,
    pub waker: Waker,
    /// Verified against when no user matches an e-mail, so a wrong address and a wrong password
    /// cost the same scrypt work.
    pub dummy_hash: String,
    /// Long-poll claims waiting for the one DB connection. A claim is pure DB work, so more than
    /// a few blocking threads queueing on the mutex buy nothing; the rest wait as cheap tasks
    /// (200 idle agents otherwise grew the blocking pool to 130+ threads).
    pub poll_slots: Arc<tokio::sync::Semaphore>,
    /// Image renders (`label_uri`, `label_base64`) running at once. A decode can take a few
    /// hundred MB, and the fetcher's slots are free again by the time the render starts.
    pub render_slots: RenderSlots,
    /// When the retention purge last ran (the reaper ticks far more often than once an hour).
    last_retention: Mutex<Option<f64>>,
    /// Payload fetches running now, per org (see `MAX_FETCHES_PER_ORG`).
    org_fetches: Mutex<std::collections::HashMap<i64, usize>>,
    /// Idempotency keys whose payload is being fetched now, per org (see `KeyGuard`).
    keys_in_flight: Mutex<std::collections::HashSet<(i64, String)>>,
    /// A retention WAL truncate was blocked by a reader; the next pass retries it even if it
    /// changes no rows.
    wal_truncate_pending: std::sync::atomic::AtomicBool,
    /// When the process started (the overview's uptime).
    started: std::time::Instant,
    /// The last retention pass that ran: when, and what it did (the operator overview).
    last_purge: Mutex<Option<(f64, store::Purged)>>,
    /// The operator overview, kept for `operator::OVERVIEW_TTL`.
    overview_cache: Mutex<Option<(std::time::Instant, Value)>>,
}

/// Concurrent image renders (see `AppState::render_slots`). ponytail: one global cap, not per
/// org, and a full house answers 503 instead of queueing. A render is worth up to ~0.9 GB at the
/// size limits, so 4 bound the worst case to a few GB; a typical label takes milliseconds.
pub const RENDER_SLOTS: usize = 4;

/// A counter of running renders, shaped like the fetcher's slots (`dispatch::HttpFetcher::slot`).
pub struct RenderSlots {
    in_flight: std::sync::atomic::AtomicUsize,
    max: usize,
}

/// Holds one render slot; dropping it gives the slot back.
pub struct RenderSlot<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for RenderSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl RenderSlots {
    pub fn new(max: usize) -> Self {
        RenderSlots {
            in_flight: std::sync::atomic::AtomicUsize::new(0),
            max,
        }
    }

    /// Whether a slot is free right now, without taking it.
    pub fn has_free(&self) -> bool {
        self.in_flight.load(std::sync::atomic::Ordering::SeqCst) < self.max
    }

    /// A slot, or `None` when `max` renders are already running.
    pub fn try_enter(&self) -> Option<RenderSlot<'_>> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.in_flight.fetch_add(1, SeqCst) >= self.max {
            self.in_flight.fetch_sub(1, SeqCst);
            return None;
        }
        Some(RenderSlot(&self.in_flight))
    }
}

/// Payload downloads one org may have in flight at once where the cap applies (open signup, or
/// PRINTAPI_FETCHES_PER_ORG): the hosted default. ponytail: this stops one org from holding every
/// slot of the global `OUTBOUND_IN_FLIGHT` cap; an attacker with many orgs still shares that
/// global 256, and the cap does not queue, it answers 503.
pub const MAX_FETCHES_PER_ORG: usize = 4;

impl Config {
    /// Downloads one org may have in flight; 0 = no cap. Unset: the hosted default with open
    /// signup, none on a self-hosted box (its one org would stall on four hung upstream hosts).
    pub fn fetch_cap(&self) -> usize {
        self.fetches_per_org.unwrap_or(if self.signup_open {
            MAX_FETCHES_PER_ORG
        } else {
            0
        })
    }
}

/// Holds one of an org's download slots; dropping it (success, error or panic unwind) gives it back.
struct FetchGuard<'a> {
    state: &'a AppState,
    org_id: i64,
}

impl Drop for FetchGuard<'_> {
    fn drop(&mut self) {
        let mut map = self
            .state
            .org_fetches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(n) = map.get_mut(&self.org_id) {
            *n -= 1;
            if *n == 0 {
                map.remove(&self.org_id);
            }
        }
    }
}

impl AppState {
    /// A download slot for `org_id`, or `None` when it already has `Config::fetch_cap` running.
    /// Without a cap nothing is counted.
    fn try_fetch_slot(&self, org_id: i64) -> Option<Option<FetchGuard<'_>>> {
        let cap = self.cfg.fetch_cap();
        if cap == 0 {
            return Some(None);
        }
        let mut map = self
            .org_fetches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let n = map.entry(org_id).or_insert(0);
        if *n >= cap {
            return None;
        }
        *n += 1;
        Some(Some(FetchGuard {
            state: self,
            org_id,
        }))
    }

    /// Marks `(org_id, key)` as being fetched, or `None` when another request already does.
    fn try_key_in_flight(&self, org_id: i64, key: &str) -> Option<KeyGuard<'_>> {
        let mut set = self
            .keys_in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !set.insert((org_id, key.to_string())) {
            return None;
        }
        Some(KeyGuard {
            state: self,
            org_id,
            key: key.to_string(),
        })
    }
}

/// Holds an idempotency key as "being fetched"; dropping it (success, error or panic unwind)
/// releases it. Two requests with the same new key must not both fetch the payload (a `*_uri_post`
/// would hit the downstream service twice); `enqueue_job` only dedupes at the end.
struct KeyGuard<'a> {
    state: &'a AppState,
    org_id: i64,
    key: String,
}

impl Drop for KeyGuard<'_> {
    fn drop(&mut self) {
        self.state
            .keys_in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(self.org_id, std::mem::take(&mut self.key)));
    }
}

/// Concurrent long-poll DB calls (see `AppState::poll_slots`).
pub const POLL_SLOTS: usize = 4;

impl AppState {
    pub fn new(
        cfg: Config,
        db: Db,
        mailer: Arc<dyn Mailer>,
        fetcher: Arc<dyn Fetcher>,
    ) -> AppState {
        let limiter = LoginLimiter::new(cfg.max_login_fails, cfg.login_window_s);
        let dummy_hash = crate::auth::hash_password(Some("no such user, no such password"))
            .expect("the dummy password is long enough");
        AppState {
            cfg,
            db,
            limiter,
            mailer,
            fetcher,
            waker: Waker::default(),
            dummy_hash,
            poll_slots: Arc::new(tokio::sync::Semaphore::new(POLL_SLOTS)),
            render_slots: RenderSlots::new(RENDER_SLOTS),
            last_retention: Mutex::new(None),
            org_fetches: Mutex::new(std::collections::HashMap::new()),
            keys_in_flight: Mutex::new(std::collections::HashSet::new()),
            wal_truncate_pending: std::sync::atomic::AtomicBool::new(false),
            started: std::time::Instant::now(),
            last_purge: Mutex::new(None),
            overview_cache: Mutex::new(None),
        }
    }
}

/// The signup throttle key of a client address: IPv6 by /64 (`proxy::throttle_key`). Anything that
/// is not an IP (a test's placeholder) is used as given.
fn signup_key(peer_ip: &str) -> String {
    peer_ip
        .parse()
        .map_or_else(|_| peer_ip.to_string(), crate::proxy::throttle_key)
}

/// One request, fully read. `target` is the raw request-target (path + `?query`), exactly what
/// the Python handler saw as `self.path`.
#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub target: String,
    pub headers: http::HeaderMap,
    pub body: Bytes,
    /// The body could not be read (declared Content-Length over `http::MAX_BODY`, or the stream
    /// broke off). Every route that reads a body answers its own 400 for this, exactly where the
    /// Python `_read_body` raised ValueError ("bad json", "bad content-length", ...).
    pub body_error: bool,
    /// The client's IP: the socket peer, or the address `X-Forwarded-For` names when the peer is a
    /// trusted proxy (`proxy::client_ip`). The signup throttle key and the access log use it.
    pub peer_ip: String,
    /// The raw socket peer, never `X-Forwarded-For`. Only the operator gate reads it.
    pub socket_peer: String,
}

impl Req {
    /// A header as text ("" when absent or not valid UTF-8).
    pub fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }

    /// The path without the query string.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }
}

#[derive(Debug, Clone)]
pub enum RespBody {
    Empty,
    Bytes(Bytes),
    /// The same bytes written `n` times (CloudPRNT copies) without materialising n copies.
    Repeat(Bytes, usize),
}

#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: RespBody,
}

impl Resp {
    pub fn json(status: u16, v: &Value) -> Resp {
        // json.dumps formatting, byte for byte (see pyfmt::dumps)
        let body = crate::pyfmt::dumps(v).into_bytes();
        Resp {
            status,
            headers: vec![("Content-Type", "application/json".into())],
            body: RespBody::Bytes(body.into()),
        }
    }

    pub fn empty(status: u16) -> Resp {
        Resp {
            status,
            headers: Vec::new(),
            body: RespBody::Empty,
        }
    }
}

// --- routing ------------------------------------------------------------------------------------

use crate::pyfmt::{as_sql_int, py_str, repr, repr_str, truthy};
use crate::store::{self, StoreError};
use crate::{auth, billing, cloudprnt, dispatch, label, orders, packing_slip, printnode, web};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use serde_json::{Map, json};
use sha2::Sha256;

type Obj = Map<String, Value>;

const LABEL_NAME_TAKEN: &str = "label template name already in use";
/// A handler outcome: a response, or a store failure nobody handled (answered 500).
type R = Result<Resp, StoreError>;
/// A precondition that already holds the response to send when it fails.
type Gate<T> = Result<T, Resp>;

/// Unwrap a `Gate`, or return its response from the enclosing handler.
macro_rules! gate {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(resp) => return Ok(resp),
        }
    };
}

mod operator;
pub use operator::is_operator_token;

/// The answer of every submit path while the operator has locked the org.
const ORG_LOCKED: &str = "org locked";

fn jerr(status: u16, msg: impl AsRef<str>) -> Resp {
    Resp::json(status, &json!({ "error": msg.as_ref() }))
}

fn unauthorized() -> Resp {
    jerr(401, "unauthorized")
}

fn not_found() -> Resp {
    jerr(404, "not found")
}

fn bad_json() -> Resp {
    jerr(400, "bad json")
}

fn ok_true() -> Resp {
    Resp::json(200, &json!({ "ok": true }))
}

/// A response body from any row type (they all serialize to the Python dicts' key order).
fn val<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).expect("row types always serialize")
}

fn html(status: u16, body: &str, head: bool) -> Resp {
    let mut r = Resp {
        status,
        headers: vec![("Content-Type", "text/html; charset=utf-8".into())],
        body: RespBody::Empty,
    };
    if head {
        r.headers.push(("Content-Length", body.len().to_string()));
    } else {
        r.body = RespBody::Bytes(Bytes::copy_from_slice(body.as_bytes()));
    }
    r
}

/// `json.loads(bytes)`: the encoding is detected like `json.detect_encoding` (UTF-8 with or
/// without BOM, UTF-16/32 by BOM or by the NUL pattern of the first bytes), and `-0` reads as the
/// integer 0 (serde_json makes it the float -0.0).
pub fn json_from_bytes(raw: &[u8]) -> Result<Value, serde_json::Error> {
    let b = raw;
    let text: Option<String> =
        if b.starts_with(&[0xFF, 0xFE, 0, 0]) || b.starts_with(&[0, 0, 0xFE, 0xFF]) {
            utf32(&b[4..], b[0] == 0)
        } else if b.starts_with(&[0xFF, 0xFE]) {
            utf16(&b[2..], false)
        } else if b.starts_with(&[0xFE, 0xFF]) {
            utf16(&b[2..], true)
        } else if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
            return from_utf8_json(&b[3..]);
        } else if b.len() >= 4 && b[0] == 0 {
            if b[1] == 0 {
                utf32(b, true)
            } else {
                utf16(b, true)
            }
        } else if b.len() >= 4 && b[1] == 0 {
            if b[2] == 0 && b[3] == 0 {
                utf32(b, false)
            } else {
                utf16(b, false)
            }
        } else if b.len() == 2 && b[0] == 0 {
            utf16(b, true)
        } else if b.len() == 2 && b[1] == 0 {
            utf16(b, false)
        } else {
            return from_utf8_json(b);
        };
    match text {
        Some(t) => from_utf8_json(t.as_bytes()),
        None => serde_json::from_slice(b"\xFF"), // undecodable: a syntax error, like Python's
    }
}

fn from_utf8_json(b: &[u8]) -> Result<Value, serde_json::Error> {
    let mut v: Value = serde_json::from_slice(b)?;
    neg_zero_to_int(&mut v);
    Ok(v)
}

/// serde_json parses `-0` as the float -0.0; Python's `json.loads` gives the int 0.
// ponytail: a literal `-0.0` is flattened to 0 as well (Python keeps the float).
pub fn neg_zero_to_int(v: &mut Value) {
    match v {
        Value::Number(n) if n.as_f64().is_some_and(|f| f == 0.0 && f.is_sign_negative()) => {
            *v = json!(0);
        }
        Value::Array(a) => a.iter_mut().for_each(neg_zero_to_int),
        Value::Object(o) => o.values_mut().for_each(neg_zero_to_int),
        _ => {}
    }
}

fn utf16(b: &[u8], be: bool) -> Option<String> {
    if !b.len().is_multiple_of(2) {
        return None;
    }
    let units = b.chunks(2).map(|c| {
        if be {
            u16::from_be_bytes([c[0], c[1]])
        } else {
            u16::from_le_bytes([c[0], c[1]])
        }
    });
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()
}

fn utf32(b: &[u8], be: bool) -> Option<String> {
    if !b.len().is_multiple_of(4) {
        return None;
    }
    b.chunks(4)
        .map(|c| {
            let a = [c[0], c[1], c[2], c[3]];
            char::from_u32(if be {
                u32::from_be_bytes(a)
            } else {
                u32::from_le_bytes(a)
            })
        })
        .collect()
}

/// A string-valued field; anything else reads as absent (Python crashed on the other types).
fn str_field<'a>(m: &'a Obj, key: &str) -> Option<&'a str> {
    m.get(key).and_then(Value::as_str)
}

/// The text a JSON value binds as into a TEXT column: strings as-is, null as NULL, other JSON as
/// its Python `str()`.
fn sql_text(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        // SQLite stores a bound Python bool as the integer 1/0
        Some(Value::Bool(b)) => Some(u8::from(*b).to_string()),
        Some(v) => Some(py_str(v)),
    }
}

/// `value == i` the way Python compares a JSON value with an int (`1 == 1.0 == True`).
fn py_eq_int(v: &Value, i: i64) -> bool {
    match v {
        Value::Bool(b) => i64::from(*b) == i,
        Value::Number(n) => n.as_i64() == Some(i) || n.as_f64() == Some(i as f64),
        _ => false,
    }
}

/// The id in `<prefix><digits><suffix>` — the regex routes. `\d+` that does not fit an i64 is
/// not a route (the Python crashed binding it), so it falls through to the 404.
/// A `job_quota` field: a non-negative integer, or null for unlimited. Shared by the root
/// `PUT /orgs/{id}` and the operator edit, so both reject the same values with the same text.
pub(crate) fn parse_job_quota(v: &Value) -> Result<Option<i64>, &'static str> {
    match v {
        Value::Null => Ok(None),
        Value::Number(n) if n.as_i64().is_some_and(|q| q >= 0) => Ok(n.as_i64()),
        _ => Err("job_quota must be a non-negative integer or null"),
    }
}

fn id_of(target: &str, prefix: &str, suffix: &str) -> Option<i64> {
    let d = target.strip_prefix(prefix)?.strip_suffix(suffix)?;
    if d.is_empty() || !d.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    d.parse().ok()
}

/// `parse_qs(urlparse(target).query)` flattened: percent-decoded, `+` as space, blank values
/// dropped. Lookups take the first value.
fn query_params(target: &str) -> Vec<(String, String)> {
    let Some((_, q)) = target.split_once('?') else {
        return Vec::new();
    };
    let q = q.split('#').next().unwrap_or("");
    let unq = |s: &str| web::percent_decode(&s.replace('+', " "));
    q.split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (!v.is_empty()).then(|| (unq(k), unq(v)))
        })
        .collect()
}

fn qfirst<'a>(q: &'a [(String, String)], key: &str) -> Option<&'a str> {
    q.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// `^/cloudprnt(?:/([A-Za-z0-9_-]+))?$`: Some(None) = the bare path, Some(Some(key)) = key in path.
fn cloudprnt_route(path: &str) -> Option<Option<&str>> {
    let rest = path.strip_prefix("/cloudprnt")?;
    if rest.is_empty() {
        return Some(None);
    }
    let key = rest.strip_prefix('/')?;
    let ok = !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    ok.then_some(Some(key))
}

/// `^/(computers|printers|printjobs)/([\d,\- ]+)(/printers|/states)?$` — a collection addressed by
/// id set, optionally with a sub-resource.
struct PnSet<'a> {
    kind: &'static str,
    set: &'a str,
    sub: &'a str,
}

fn pn_set(path: &str) -> Option<PnSet<'_>> {
    let (kind, rest) = path.strip_prefix('/')?.split_once('/')?;
    let kind = ["computers", "printers", "printjobs"]
        .into_iter()
        .find(|k| *k == kind)?;
    let (set, sub) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let set_ok = !set.is_empty()
        && set
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ',' | '-' | ' '));
    (set_ok && matches!(sub, "" | "/printers" | "/states")).then_some(PnSet { kind, set, sub })
}

const PN_COLLECTIONS: [&str; 4] = ["/whoami", "/computers", "/printers", "/printjobs"];

/// Who is calling, resolved from the one Bearer header.
///
/// kind `root`: the bootstrap token — no org filter, may act in and manage every org.
/// kind `session`: a browser login — one org, and may manage it (keys, users, org settings).
/// kind `key`: a machine credential — one org, print and read only. A leaked integration key must
/// not be able to issue itself a successor.
/// Which table the credential resolves in *is* the permission — no role column.
struct Principal {
    kind: &'static str,
    org_id: Option<i64>,
    user_id: Option<i64>,
    email: Option<String>,
    manage: bool,
}

impl Principal {
    /// True if this principal may not touch `org_id` (root may touch every one).
    fn foreign_org(&self, org_id: i64) -> bool {
        self.org_id.is_some_and(|o| o != org_id)
    }
}

/// What `gate_printer` decided: the job already exists, or go on (holding the download slot and
/// the in-flight key when the payload is fetched).
enum Gate2<'a> {
    Existing(store::Enqueued),
    Go(Held<'a>),
}

/// What a fetching request holds until its job is queued (or it fails).
#[derive(Default)]
struct Held<'a> {
    slot: Option<FetchGuard<'a>>,
    _key: Option<KeyGuard<'a>>,
}

/// What `label_payload` made: an existing job (idempotent resubmit), or the job's mode, validated
/// options and rendered bytes.
enum Labeled<'a> {
    Existing(store::Enqueued),
    Ready(&'static str, Option<Obj>, Vec<u8>, Held<'a>),
}

/// The job types whose payload the server fetches itself (`label_uri` has its own path).
fn fetches_payload(type_v: &Value) -> bool {
    matches!(
        type_v.as_str(),
        Some("raw_uri" | "pdf_uri" | "raw_uri_post" | "pdf_uri_post")
    )
}

/// Why a job submit failed — the callers map these onto their own status codes and shapes.
enum SubmitErr {
    Fetch(String),
    Quota(String),
    /// The org is locked by the operator: 403, whichever way the job came in.
    Locked,
    Invalid(String),
    /// Too many renders or downloads running; the client may retry.
    Busy(String),
    /// The same new idempotency key is being fetched by another request.
    InProgress(String),
    Store(StoreError),
}

impl From<dispatch::DispatchError> for SubmitErr {
    fn from(e: dispatch::DispatchError) -> Self {
        match e {
            dispatch::DispatchError::Invalid(m) => SubmitErr::Invalid(m),
            dispatch::DispatchError::Fetch(m) => SubmitErr::Fetch(m),
        }
    }
}

struct Ctx<'a> {
    s: &'a AppState,
    req: &'a Req,
    /// The request carried the operator token (Authorization or a CloudPRNT path key) on a route
    /// that is not `/operator/`. `handle` has already removed it, so every resolver sees no
    /// credential; only `enrollment_org` still needs to know, because open enrollment would
    /// otherwise take a request without a key.
    operator_cred: bool,
}

/// The org `key` may enroll an agent into, or why it may not (see `Ctx::agent_register`): the
/// root token -> the default org; a revoked issued key -> nothing; an issued key -> its org; a
/// non-empty key an agent already holds, or anything at all with open enrollment -> the default
/// org. An empty key is never "already held": a v2 agent registered without one cannot poll
/// anyway, and must not let a headerless request rewrite its printers.
fn enrollment_org(
    c: &rusqlite::Connection,
    key: &str,
    root: bool,
    open: bool,
    operator: bool,
) -> Result<Result<i64, &'static str>, StoreError> {
    // The operator token enrolls nothing, not even with open enrollment.
    if operator {
        return Ok(Err(
            "unknown agent key: issue one with POST /apikeys (the dashboard's Keys page)",
        ));
    }
    if root {
        return Ok(Ok(store::DEFAULT_ORG));
    }
    if store::key_revoked(c, key)? {
        return Ok(Err("agent key revoked"));
    }
    if let Some(row) = store::authenticate_client(c, key)? {
        return Ok(Ok(row.org_id));
    }
    if open || (!key.is_empty() && store::authenticate_agent(c, key)?.is_some()) {
        return Ok(Ok(store::DEFAULT_ORG));
    }
    Ok(Err(
        "unknown agent key: issue one with POST /apikeys (the dashboard's Keys page)",
    ))
}

/// Route one request (every method and path except the `GET /agent/jobs` long-poll).
pub fn handle(state: &AppState, req: &Req) -> Resp {
    let path = req.path();
    if path == "/operator" || path.starts_with("/operator/") {
        return Ctx {
            s: state,
            req,
            operator_cred: false,
        }
        .operator();
    }
    // The operator token is no credential anywhere else: take it out of the request before any
    // session, key, Basic, CloudPRNT or agent lookup can see it.
    if let Some(token) = state.cfg.operator_token.as_deref()
        && let Some(clean) = operator::without_operator_credential(req, token)
    {
        return route(state, &clean, true);
    }
    route(state, req, false)
}

fn route(state: &AppState, req: &Req, operator_cred: bool) -> Resp {
    let cx = Ctx {
        s: state,
        req,
        operator_cred,
    };
    let r = match req.method.as_str() {
        "GET" => cx.get(),
        "HEAD" => Ok(cx.head()),
        "POST" => cx.post(),
        "PUT" => cx.put(),
        "DELETE" => cx.delete(),
        // deliberate: the Python sent the stdlib's HTML 501 here
        _ => Ok(jerr(501, "unsupported method")),
    };
    r.unwrap_or_else(|e| {
        eprintln!("internal error: {e}");
        jerr(500, "internal error")
    })
}

impl Ctx<'_> {
    fn db<T>(&self, f: impl FnOnce(&rusqlite::Connection) -> T) -> T {
        self.s.db.with(f)
    }

    fn cfg(&self) -> &Config {
        &self.s.cfg
    }

    fn target(&self) -> &str {
        &self.req.target
    }

    // --- auth -----------------------------------------------------------------------------------

    fn presented_key(&self) -> &str {
        self.req
            .header("authorization")
            .strip_prefix("Bearer ")
            .unwrap_or("")
    }

    /// The bootstrap token is the root/admin credential (orgs, keys) and the only credential that
    /// spans orgs.
    fn admin_ok(&self) -> bool {
        auth::ct_eq(self.presented_key(), &self.cfg().token)
    }

    fn principal(&self) -> Result<Option<Principal>, StoreError> {
        let key = self.presented_key();
        if self.admin_ok() {
            return Ok(Some(Principal {
                kind: "root",
                org_id: None,
                user_id: None,
                email: None,
                manage: true,
            }));
        }
        if key.starts_with("sess_") {
            // never looked up as a machine key
            let s = self.db(|c| store::authenticate_session(c, key, None))?;
            return Ok(s.map(|s| Principal {
                kind: "session",
                org_id: Some(s.org_id),
                user_id: Some(s.user_id),
                email: s.email,
                manage: true,
            }));
        }
        let row = self.db(|c| store::authenticate_client(c, key))?;
        Ok(row.map(|r| Principal {
            kind: "key",
            org_id: Some(r.org_id),
            user_id: None,
            email: None,
            manage: false,
        }))
    }

    /// The print/read gate: Err(401) or the org to filter by (None = root, no filter).
    fn client_org(&self) -> Result<Gate<Option<i64>>, StoreError> {
        Ok(self.principal()?.map(|p| p.org_id).ok_or_else(unauthorized))
    }

    /// The gate for key/user/org administration: root or a browser session, never a machine key.
    fn manager(&self) -> Result<Gate<Principal>, StoreError> {
        Ok(match self.principal()? {
            Some(p) if p.manage => Ok(p),
            _ => Err(unauthorized()),
        })
    }

    /// `GET /support-access`: the support grants of one org, for the dashboard card. A session
    /// sees its own org, root any (default: the default org). Always `no-store`: it is a log.
    fn support_access(&self) -> R {
        let p = gate!(self.manager()?);
        let asked = qfirst(&query_params(self.target()), "org").map(str::parse::<i64>);
        let oid = match (asked, p.org_id) {
            (Some(Ok(o)), _) => o,
            (Some(Err(_)), _) => return Ok(jerr(400, "org must be a number")),
            (None, own) => own.unwrap_or(store::DEFAULT_ORG),
        };
        if p.foreign_org(oid) {
            return Ok(jerr(403, "forbidden"));
        }
        if self.db(|c| store::get_org(c, oid))?.is_none() {
            return Ok(not_found());
        }
        let enabled = self.cfg().operator_token.is_some();
        let grants = if enabled {
            self.db(|c| store::support_grants_of_org(c, oid))?
        } else {
            Vec::new()
        };
        let list: Vec<Value> = grants
            .iter()
            .map(|g| {
                json!({
                    "id": g.id, "reason": g.reason, "granted_at": g.granted_at,
                    "expires_at": g.expires_at, "ended_at": g.ended_at, "active": g.active,
                    "listed_job_ids": g.listed_job_ids, "opened_job_ids": g.opened_job_ids,
                })
            })
            .collect();
        let mut r = Resp::json(200, &json!({ "enabled": enabled, "grants": list }));
        r.headers.push(("Cache-Control", "no-store".into()));
        Ok(r)
    }

    fn admin(&self) -> Gate<()> {
        if self.admin_ok() {
            Ok(())
        } else {
            Err(unauthorized())
        }
    }

    fn agent_id(&self) -> Result<Option<i64>, StoreError> {
        let key = self.presented_key();
        if key.is_empty() {
            return Ok(None);
        }
        self.db(|c| store::authenticate_agent(c, key))
    }

    // --- bodies ---------------------------------------------------------------------------------

    /// The request body as JSON; an empty body reads as `{}`. None = unreadable or not JSON.
    fn parse_body(&self) -> Option<Value> {
        if self.req.body_error {
            return None;
        }
        let raw: &[u8] = if self.req.body.is_empty() {
            b"{}"
        } else {
            &self.req.body
        };
        json_from_bytes(raw).ok()
    }

    /// A JSON object body. Python crashed on any other JSON value; that is a clean 400 here.
    fn read_obj(&self) -> Gate<Obj> {
        match self.parse_body() {
            Some(Value::Object(m)) => Ok(m),
            _ => Err(bad_json()),
        }
    }

    // --- accounts -------------------------------------------------------------------------------

    fn create_user(&self, org_id: i64, body: &Obj) -> R {
        let email = str_field(body, "email")
            .map(store::normalize_email)
            .unwrap_or_default();
        if !email.contains('@') {
            return Ok(jerr(400, "email required"));
        }
        let pw_hash = match auth::hash_password(str_field(body, "password")) {
            Ok(h) => h,
            Err(e) => return Ok(jerr(400, e)),
        };
        match self.db(|c| store::create_user(c, org_id, &email, &pw_hash)) {
            Ok(uid) => Ok(Resp::json(
                200,
                &json!({ "id": uid, "email": email, "org_id": org_id }),
            )),
            Err(StoreError::Conflict) => Ok(jerr(409, "email already registered")),
            Err(e) => Err(e),
        }
    }

    fn login(&self) -> R {
        let body = gate!(self.read_obj());
        let email = str_field(&body, "email")
            .map(store::normalize_email)
            .unwrap_or_default();
        // namespaced: the limiter's map is shared with `signup:` and `reset:` keys
        let slot = format!("login:{email}");
        if !self.s.limiter.allow(&slot, None) {
            return Ok(jerr(429, "too many failed logins, try again later"));
        }
        let user = self.db(|c| store::get_user_by_email(c, &email))?;
        // Verify even when there is no such user (against a dummy hash): same work, same timing,
        // and one shared "invalid credentials" answer for both cases.
        let stored = match &user {
            Some(u) => u.password_hash.as_deref(),
            None => Some(self.s.dummy_hash.as_str()),
        };
        let ok = auth::verify_password(str_field(&body, "password"), stored);
        let Some(user) = user.filter(|_| ok) else {
            self.s.limiter.fail(&slot, None);
            return Ok(jerr(401, "invalid credentials"));
        };
        self.s.limiter.succeed(&slot);
        let tok = auth::new_session_token();
        let expires_at =
            self.db(|c| store::create_session(c, user.id, &tok, self.cfg().session_ttl_s, None))?;
        Ok(Resp::json(
            200,
            &json!({ "token": tok, "expires_at": expires_at, "org_id": user.org_id,
                     "user_id": user.id, "email": user.email }),
        ))
    }

    /// Self-serve: a new org, its first user, and a session in one call — the hosted deployment's
    /// front door. Off by default (`signup_open`).
    fn signup(&self) -> R {
        if !self.cfg().signup_open {
            return Ok(jerr(403, "signup is disabled on this server"));
        }
        let body = gate!(self.read_obj());
        // Throttled by client address, not by e-mail: an org sprayer picks a fresh address every
        // time, so counting addresses would bound nothing.
        // The address is the client `http.rs` resolved (X-Forwarded-For behind a trusted proxy).
        let slot = format!("signup:{}", signup_key(&self.req.peer_ip));
        if !self.s.limiter.allow(&slot, None) {
            return Ok(jerr(429, "too many signups, try again later"));
        }
        let email = str_field(&body, "email")
            .map(store::normalize_email)
            .unwrap_or_default();
        if !email.contains('@') {
            return Ok(jerr(400, "email required"));
        }
        let terms_version = self.cfg().terms.as_ref().map(|t| t.version.as_str());
        if let Some(current) = terms_version {
            match body.get("accept_terms") {
                Some(Value::String(v)) if v == current => {}
                Some(Value::String(_)) => return Ok(jerr(409, "terms changed, reload")),
                _ => return Ok(jerr(400, "accept_terms required")),
            }
        }
        // A bad password must not cost a slot: only the cheap length check runs before it.
        if let Err(e) = auth::validate_password(str_field(&body, "password")) {
            return Ok(jerr(400, e));
        }
        // The slot is taken atomically, before the slow hash: parallel requests from one address
        // must not all pass the peek above and then count afterwards. A successful signup spends
        // its slot too, which is the cap.
        if !self.s.limiter.try_take(&slot, None) {
            return Ok(jerr(429, "too many signups, try again later"));
        }
        let pw_hash = match auth::hash_password(str_field(&body, "password")) {
            Ok(h) => h,
            Err(e) => return Ok(jerr(400, e)),
        };
        let name = str_field(&body, "org_name")
            .map(|n| printnode::py_trim(n).to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| email.split_once('@').map_or("", |(_, d)| d).to_string());
        let quota = self.cfg().signup_quota;
        let new = match self.db(|c| store::signup(c, &name, &email, &pw_hash, quota, terms_version))
        {
            Ok(n) => n,
            Err(StoreError::Conflict) => return Ok(jerr(409, "email already registered")),
            Err(e) => return Err(e),
        };
        let tok = auth::new_session_token();
        let expires_at = self
            .db(|c| store::create_session(c, new.user_id, &tok, self.cfg().session_ttl_s, None))?;
        Ok(Resp::json(
            200,
            &json!({ "token": tok, "expires_at": expires_at, "org_id": new.org_id,
                     "user_id": new.user_id, "email": email }),
        ))
    }

    /// The body of a reset mail: `lead`, the link and the token. The link is built from PUBLIC_URL
    /// only. Deriving it from the Host header would let anyone who can send this server a request
    /// mail a *valid* token pointing at their own host; without the env var the mail carries the
    /// bare token instead.
    fn reset_mail_text(&self, lead: &str, tok: &str) -> String {
        let public_url = &self.cfg().public_url;
        let link = if public_url.is_empty() {
            String::new()
        } else {
            format!("\nOpen {public_url}/?reset={tok}\n")
        };
        format!(
            "{lead}\n\
             {link}\ntoken: {tok}\n\n\
             It is valid for {} minutes and works once. \
             If this was not you, ignore this message.\n",
            (self.cfg().reset_ttl_s / 60.0).floor() as i64
        )
    }

    /// Always 200, whether or not the address exists — the answer must not enumerate accounts. A
    /// mail goes out only for a real one.
    fn password_reset(&self) -> R {
        let body = gate!(self.read_obj());
        let email = str_field(&body, "email")
            .map(store::normalize_email)
            .unwrap_or_default();
        let slot = format!("reset:{email}");
        if !self.s.limiter.allow(&slot, None) {
            return Ok(jerr(429, "too many reset requests, try again later"));
        }
        // counted for every address, so a 429 leaks nothing either
        self.s.limiter.fail(&slot, None);
        if let Some(user) = self.db(|c| store::get_user_by_email(c, &email))? {
            let tok = auth::new_session_token();
            let ttl = self.cfg().reset_ttl_s;
            self.db(|c| store::create_password_reset(c, user.id, &tok, ttl, None))?;
            let text = self.reset_mail_text(
                &format!("Someone asked to reset the printpapi password for {email}."),
                &tok,
            );
            // a broken mail server must not answer differently
            if let Err(e) = self
                .s
                .mailer
                .send(&email, "Reset your printpapi password", &text)
            {
                eprintln!("password reset mail to {email} failed: {e}");
            }
        }
        Ok(ok_true())
    }

    fn password_reset_confirm(&self) -> R {
        let body = gate!(self.read_obj());
        let pw_hash = match auth::hash_password(str_field(&body, "password")) {
            Ok(h) => h,
            Err(e) => return Ok(jerr(400, e)),
        };
        let token = str_field(&body, "token");
        if self
            .db(|c| store::consume_password_reset(c, token, &pw_hash, None))?
            .is_none()
        {
            return Ok(jerr(400, "invalid or expired token"));
        }
        Ok(ok_true())
    }

    // --- jobs -----------------------------------------------------------------------------------

    /// The POST /jobs core: validate, fetch the payload, enqueue. The PrintNode compat layer
    /// submits through here too, so there is one validation path.
    fn submit_job(
        &self,
        body: &Obj,
        org: Option<i64>,
        user_id: Option<i64>,
    ) -> Result<store::Enqueued, SubmitErr> {
        let null = Value::Null;
        let type_v = body.get("type").unwrap_or(&null);
        let mut mode = dispatch::agent_mode(type_v);
        // Validate before decode_payload: a bad field must 400 without first fetching a URL (and
        // a retried submit must not re-fetch it either).
        let copies = dispatch::parse_copies(body)?;
        let callback_url = dispatch::parse_callback_url(body)?;
        let label = dispatch::parse_label_job(body)?;
        // A label job's mode is its printer's, so its options are checked once that is known.
        let mut options = if label.is_none() {
            dispatch::parse_options(body, mode)?
        } else {
            None
        };
        let idempotency_key = dispatch::parse_idempotency_key(body)?;
        let expire_after = dispatch::parse_expire_after(body)?;
        // the key stays marked until the job is queued, so a second request cannot fetch meanwhile
        let mut _held = Held::default();
        let data = match &label {
            None => {
                // The gate runs only for types that fetch; the rest keep their order of checks.
                if fetches_payload(type_v) {
                    // a bad url still 400s before the printer is looked at
                    dispatch::checked_http_url(body.get("url"))?;
                    let printer_v = body.get("printer_id").unwrap_or(&null);
                    let printer = self
                        .db(|c| store::get_printer(c, printer_v, org))
                        .map_err(SubmitErr::Store)?
                        .ok_or_else(|| {
                            SubmitErr::Invalid(format!("unknown printer: {}", py_str(printer_v)))
                        })?;
                    match self.gate_printer(&printer, idempotency_key.as_deref(), true)? {
                        Gate2::Existing(e) => return Ok(e),
                        Gate2::Go(held) => _held = held,
                    }
                }
                dispatch::decode_payload(body, &*self.s.fetcher)?
            }
            Some(l) => match self.label_payload(body, l, org, idempotency_key.as_deref())? {
                Labeled::Existing(e) => return Ok(e),
                Labeled::Ready(m, o, d, h) => {
                    (mode, options) = (m, o);
                    _held = h;
                    d
                }
            },
        };
        let job = store::NewJob {
            printer_id: body.get("printer_id").unwrap_or(&null),
            type_: type_v.as_str().unwrap_or(""),
            mode,
            payload: &data,
            user_id: user_id.unwrap_or(store::DEFAULT_USER),
            title: sql_text(body.get("title")),
            copies,
            callback_url,
            options,
            org_id: org,
            idempotency_key,
            expire_after,
        };
        self.enqueue(&job)
    }

    /// The checks before any outbound fetch, cheapest first (the printer is already resolved):
    /// an existing job under the idempotency key (returned as it is, nothing fetched), the quota,
    /// and the org's download cap when `fetch`. `enqueue_job` repeats the printer, key and quota
    /// checks inside its transaction; this only stops the expensive work early.
    fn gate_printer(
        &self,
        printer: &store::PrinterRef,
        key: Option<&str>,
        fetch: bool,
    ) -> Result<Gate2<'_>, SubmitErr> {
        // Marked before the lookup: a request that finished in between has queued its job by now,
        // so the lookup below finds it; one that is still fetching holds the key.
        let key_guard = match key.filter(|_| fetch) {
            Some(k) => Some(self.s.try_key_in_flight(printer.org_id, k).ok_or_else(|| {
                SubmitErr::InProgress(
                    "a request with this idempotency_key is still in progress, retry shortly"
                        .into(),
                )
            })?),
            None => None,
        };
        // One closure, so a retry cannot see "no job yet" and then a quota the original used up.
        let checked = self.db(|c| {
            if self.cfg().operator_token.is_some() {
                store::check_not_locked(c, printer.org_id)?;
            }
            if let Some(key) = key
                && let Some(e) = store::find_job_by_key(c, printer.org_id, key)?
            {
                return Ok(Some(e));
            }
            store::check_quota(c, printer.org_id, crate::now()).map(|()| None)
        });
        match checked {
            Ok(Some(e)) => return Ok(Gate2::Existing(e)),
            Ok(None) => {}
            Err(StoreError::QuotaExceeded(m)) => return Err(SubmitErr::Quota(m)),
            Err(StoreError::Locked) => return Err(SubmitErr::Locked),
            Err(e) => return Err(SubmitErr::Store(e)),
        }
        if !fetch {
            return Ok(Gate2::Go(Held::default()));
        }
        match self.s.try_fetch_slot(printer.org_id) {
            Some(slot) => Ok(Gate2::Go(Held {
                slot,
                _key: key_guard,
            })),
            None => Err(SubmitErr::Busy(
                "too many downloads in flight for this org, try again later".into(),
            )),
        }
    }

    /// A `label_*` job: the printer, then the template, then the payload, each before the next so
    /// that an unknown printer or template never causes a fetch. The printer decides the output:
    /// PDF where it takes PDF, ZPL otherwise (gotcha #1: never a PDF to a label printer). Returns
    /// the job's mode, its validated options and the rendered bytes.
    fn label_payload(
        &self,
        body: &Obj,
        job: &dispatch::LabelJob,
        org: Option<i64>,
        key: Option<&str>,
    ) -> Result<Labeled<'_>, SubmitErr> {
        let null = Value::Null;
        let printer_v = body.get("printer_id").unwrap_or(&null);
        let printer = self
            .db(|c| store::get_printer(c, printer_v, org))
            .map_err(SubmitErr::Store)?
            .ok_or_else(|| SubmitErr::Invalid(format!("unknown printer: {}", py_str(printer_v))))?;
        if self
            .db(|c| store::is_cloudprnt_agent(c, printer.agent_id))
            .map_err(SubmitErr::Store)?
        {
            return Err(SubmitErr::Invalid(
                "CloudPRNT printers cannot print label templates — send raw Star commands".into(),
            ));
        }
        let found = self
            .db(|c| match &job.template {
                dispatch::TemplateRef::Name(n) => store::find_label_template(c, n, org),
                dispatch::TemplateRef::Id(i) => store::get_label_template(c, *i, org),
            })
            .map_err(SubmitErr::Store)?;
        let Some(template) = found else {
            return Err(SubmitErr::Invalid(format!(
                "unknown label template: {}",
                job.template
            )));
        };
        let spec = template.spec;
        let (want, what) = match job.text {
            Some(_) => (label::Content::Text, "text"),
            None => (label::Content::Image, "image"),
        };
        if spec.content != want {
            return Err(SubmitErr::Invalid(format!(
                "label template {} does not take {what} jobs",
                spec.name
            )));
        }
        let (mode, out) = if printer.can_pdf {
            ("pdf", label::Output::Pdf)
        } else {
            ("raw", label::Output::Zpl)
        };
        let options = dispatch::parse_options(body, mode)?;
        // Fetching and rendering cost a thread and a render slot, so an existing key, a spent
        // quota and a full download cap stop before them, not at enqueue_job.
        // only `label_uri` fetches; `label_base64` decodes what it was sent
        let fetches = body.get("type").and_then(Value::as_str) == Some("label_uri");
        let mut held = match self.gate_printer(&printer, key, fetches)? {
            Gate2::Existing(e) => return Ok(Labeled::Existing(e)),
            Gate2::Go(held) => held,
        };
        let rendered = match &job.text {
            Some(text) => label::render_text(&spec, text, out),
            None => {
                // ponytail: a check, not a reservation: a slot can still be taken between here and
                // the render, which then answers the same 503 after the fetch.
                if fetches && !self.s.render_slots.has_free() {
                    return Err(SubmitErr::Busy(
                        "label rendering is busy, try again later".into(),
                    ));
                }
                let img = dispatch::decode_payload(body, &*self.s.fetcher)?;
                // the download slot is free again; the key stays held until the job is queued
                held.slot = None;
                let Some(_slot) = self.s.render_slots.try_enter() else {
                    return Err(SubmitErr::Busy(
                        "label rendering is busy, try again later".into(),
                    ));
                };
                label::render_image(&spec, &img, out)
            }
        };
        let data = rendered.map_err(|e| SubmitErr::Invalid(format!("label: {e}")))?;
        Ok(Labeled::Ready(mode, options, data, held))
    }

    fn enqueue(&self, job: &store::NewJob<'_>) -> Result<store::Enqueued, SubmitErr> {
        let locks = self.cfg().operator_token.is_some();
        match self.db(|c| store::enqueue_job_with(c, job, locks)) {
            Ok(e) => {
                if e.created {
                    self.s.waker.wake(e.agent_id);
                }
                Ok(e)
            }
            Err(StoreError::QuotaExceeded(m)) => Err(SubmitErr::Quota(m)),
            Err(StoreError::Locked) => Err(SubmitErr::Locked),
            Err(StoreError::UnknownPrinter(m)) => Err(SubmitErr::Invalid(m)),
            Err(e) => Err(SubmitErr::Store(e)),
        }
    }

    /// A template body: the contract JSON, or the 400 for why it is not one.
    fn label_spec(&self) -> Gate<label::Spec> {
        let body = self.read_obj()?;
        label::parse_spec(&body).map_err(|m| jerr(400, m))
    }

    fn post_labels(&self) -> R {
        let org = gate!(self.client_org()?);
        let spec = gate!(self.label_spec());
        match self.db(|c| store::create_label_template(c, org, &spec)) {
            Ok(t) => Ok(Resp::json(200, &t.to_json())),
            Err(StoreError::Conflict) => Ok(jerr(409, LABEL_NAME_TAKEN)),
            Err(e) => Err(e),
        }
    }

    fn put_label(&self, id: i64) -> R {
        let org = gate!(self.client_org()?);
        let spec = gate!(self.label_spec());
        match self.db(|c| store::update_label_template(c, id, org, &spec)) {
            Ok(Some(t)) => Ok(Resp::json(200, &t.to_json())),
            Ok(None) => Ok(not_found()),
            Err(StoreError::Conflict) => Ok(jerr(409, LABEL_NAME_TAKEN)),
            Err(e) => Err(e),
        }
    }

    fn post_jobs(&self) -> R {
        let Some(p) = self.principal()? else {
            return Ok(unauthorized());
        };
        let body = gate!(self.read_obj());
        match self.submit_job(&body, p.org_id, p.user_id) {
            Ok(e) => Ok(Resp::json(200, &json!({ "job_id": e.job_id }))),
            Err(SubmitErr::Fetch(m)) => Ok(jerr(502, format!("downstream: {m}"))),
            Err(SubmitErr::Quota(m)) => Ok(jerr(402, m)),
            Err(SubmitErr::Locked) => Ok(jerr(403, ORG_LOCKED)),
            Err(SubmitErr::Invalid(m)) => Ok(jerr(400, m)),
            Err(SubmitErr::Busy(m)) => Ok(jerr(503, m)),
            Err(SubmitErr::InProgress(m)) => Ok(jerr(409, m)),
            Err(SubmitErr::Store(e)) => Err(e),
        }
    }

    /// Render a store order as a packing slip and queue it as a pdf job. `opts` carries the same
    /// job knobs POST /jobs takes (printer_id, copies, title, expire_after, ...).
    #[allow(clippy::too_many_arguments)]
    fn enqueue_order(
        &self,
        payload: &Value,
        fmt: &Value,
        opts: &Obj,
        org: Option<i64>,
        idem: Option<String>,
        shop: Option<&str>,
        user_id: Option<i64>,
    ) -> R {
        let prep = (|| {
            let order = orders::normalize_order(payload, fmt).map_err(|e| e.to_string())?;
            let copies = dispatch::parse_copies(opts).map_err(|e| e.to_string())?;
            let callback_url = dispatch::parse_callback_url(opts).map_err(|e| e.to_string())?;
            let expire_after = dispatch::parse_expire_after(opts).map_err(|e| e.to_string())?;
            let idem = match idem {
                Some(i) => Some(i),
                None => dispatch::parse_idempotency_key(opts).map_err(|e| e.to_string())?,
            };
            Ok::<_, String>((order, copies, callback_url, expire_after, idem))
        })();
        let (mut order, copies, callback_url, expire_after, idem) = match prep {
            Ok(t) => t,
            Err(m) => return Ok(jerr(400, m)),
        };
        if let Some(shop) = shop.filter(|s| !s.is_empty())
            && order.shop.as_deref().is_none_or(str::is_empty)
        {
            order.shop = Some(shop.to_string());
        }
        let null = Value::Null;
        let printer_v = opts.get("printer_id").unwrap_or(&null);
        let Some(printer) = self.db(|c| store::get_printer(c, printer_v, org))? else {
            return Ok(jerr(400, format!("unknown printer: {}", py_str(printer_v))));
        };
        if !printer.can_pdf {
            // gotcha #1: a label printer form-feeds blanks on a PDF. Refuse before printing.
            return Ok(jerr(
                400,
                "printer is raw-only; a packing slip needs a PDF-capable printer",
            ));
        }
        let title = match opts.get("title") {
            Some(t) if truthy(t) => py_str(t),
            _ => format!("Packing slip {}", order.number),
        };
        let pdf = packing_slip::render_packing_slip(&order);
        let printer_id = json!(printer.id);
        let job = store::NewJob {
            printer_id: &printer_id,
            type_: "order",
            mode: "pdf",
            payload: &pdf,
            user_id: user_id.unwrap_or(store::DEFAULT_USER),
            title: Some(title),
            copies,
            callback_url,
            options: None,
            org_id: org,
            idempotency_key: idem,
            expire_after,
        };
        match self.enqueue(&job) {
            Ok(e) => Ok(Resp::json(200, &json!({ "job_id": e.job_id }))),
            Err(SubmitErr::Quota(m)) => Ok(jerr(402, m)),
            Err(SubmitErr::Locked) => Ok(jerr(403, ORG_LOCKED)),
            Err(
                SubmitErr::Invalid(m)
                | SubmitErr::Fetch(m)
                | SubmitErr::Busy(m)
                | SubmitErr::InProgress(m),
            ) => Ok(jerr(400, m)),
            Err(SubmitErr::Store(e)) => Err(e),
        }
    }

    fn post_orders(&self) -> R {
        let Some(p) = self.principal()? else {
            return Ok(unauthorized());
        };
        let body = gate!(self.read_obj());
        let null = Value::Null;
        self.enqueue_order(
            body.get("order").unwrap_or(&null),
            body.get("format").unwrap_or(&null),
            &body,
            p.org_id,
            None,
            None,
            p.user_id,
        )
    }

    /// Shopify's order webhook. It cannot send an Authorization header, so the org comes from
    /// `key` in the URL and *authenticity* from Shopify's HMAC over the raw body — the key alone is
    /// never enough to print here.
    fn shopify_webhook(&self) -> R {
        let q = query_params(self.target());
        if self.req.body_error {
            return Ok(jerr(400, "bad content-length"));
        }
        let raw = &self.req.body;
        let key = qfirst(&q, "key").unwrap_or("");
        // an issued client key only — root has no org
        let Some(row) = self.db(|c| store::authenticate_client(c, key))? else {
            return Ok(unauthorized());
        };
        let org = row.org_id;
        let secret = self
            .db(|c| store::get_org(c, org))?
            .and_then(|o| o.shopify_secret)
            .filter(|s| !s.is_empty());
        let Some(secret) = secret else {
            return Ok(jerr(
                400,
                "shopify_secret not configured for this org (PUT /orgs/{id})",
            ));
        };
        let digest = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map(|mut m| {
            m.update(raw);
            B64.encode(m.finalize().into_bytes())
        });
        match digest {
            Ok(d) if auth::ct_eq(&d, self.req.header("x-shopify-hmac-sha256")) => {}
            _ => return Ok(jerr(401, "bad signature")),
        }
        let payload: Option<Value> = json_from_bytes(if raw.is_empty() { b"{}" } else { raw }).ok();
        let pid_text = qfirst(&q, "printer_id").unwrap_or("");
        let (Some(payload), Some(pid)) = (payload, printnode::py_int(pid_text)) else {
            return Ok(jerr(400, "bad json or missing printer_id"));
        };
        let printer_id = i64::try_from(pid).map_or_else(
            |_| Value::String(printnode::py_trim(pid_text).into()),
            |n| json!(n),
        );
        // Shopify retries a webhook it considers undelivered, so dedupe on the order id: a
        // redelivery returns the original job instead of printing a second slip.
        let sid = payload.as_object().and_then(|o| {
            let id = o.get("id").filter(|v| truthy(v));
            id.or_else(|| o.get("name"))
                .filter(|v| truthy(v))
                .map(py_str)
        });
        let mut opts = Obj::new();
        opts.insert("printer_id".into(), printer_id);
        let shop = Some(self.req.header("x-shopify-shop-domain"));
        self.enqueue_order(
            &payload,
            &json!("shopify"),
            &opts,
            Some(org),
            sid.map(|s| format!("shopify-{s}")),
            shop,
            None,
        )
    }

    /// The payment provider's callback: "this org is on that plan now". Signed with a shared secret
    /// over the raw body — the body names an org, so an unsigned one would let anybody grant
    /// themselves the top plan.
    fn billing_webhook(&self) -> R {
        if self.req.body_error {
            return Ok(jerr(400, "bad content-length"));
        }
        let raw = &self.req.body;
        let cfg = self.cfg();
        let secret = cfg.billing_secret.as_deref().filter(|s| !s.is_empty());
        let Some(secret) = secret.filter(|_| !cfg.plans.is_empty()) else {
            return Ok(jerr(503, "billing is not configured on this server"));
        };
        if !billing::verify(secret, raw, self.req.header("x-signature")) {
            return Ok(jerr(401, "bad signature"));
        }
        let parsed: Result<Value, serde_json::Error> =
            json_from_bytes(if raw.is_empty() { b"{}" } else { raw });
        let ev = match parsed {
            Err(e) => return Ok(jerr(400, json_error_text(&e))),
            Ok(v) => match billing::parse_event(&v, &cfg.plans) {
                Ok(ev) => ev,
                Err(e) => return Ok(jerr(400, e.0)),
            },
        };
        let org_id = match ev.org_id {
            Some(o) => Some(o),
            // named by its owner's address instead
            None => self
                .db(|c| store::get_user_by_email(c, &ev.email))?
                .map(|u| u.org_id),
        };
        let plan = &ev.plan;
        let applied = match org_id {
            Some(o) => self.db(|c| store::set_org_plan(c, o, &plan.id, plan.jobs))?,
            None => false,
        };
        match org_id.filter(|_| applied) {
            Some(o) => Ok(Resp::json(
                200,
                &json!({ "ok": true, "org_id": o, "plan": plan.id, "job_quota": plan.jobs }),
            )),
            None => Ok(jerr(404, "unknown org")),
        }
    }

    // --- PrintNode-compatible layer -------------------------------------------------------------
    // Selected by auth scheme, not by URL: PrintNode carries the API key as the HTTP Basic
    // username, so a Basic header means "answer in their shapes" while Bearer keeps ours. The
    // shapes themselves live in printnode.rs; these methods are only routing + auth.

    fn is_basic(&self) -> bool {
        self.req.header("authorization").starts_with("Basic ")
    }

    fn pn_error(&self, code: u16, name: &str, message: impl AsRef<str>) -> Resp {
        Resp::json(code, &json!({ "code": name, "message": message.as_ref() }))
    }

    /// Same credentials as the Bearer API — any issued client key, or the root token. The inner
    /// Option is the org (None = root).
    fn pn_org(&self) -> Result<Option<Option<i64>>, StoreError> {
        let key = printnode::basic_key(self.req.header("authorization"));
        if !key.is_empty() && auth::ct_eq(&key, &self.cfg().token) {
            return Ok(Some(None));
        }
        Ok(self
            .db(|c| store::authenticate_client(c, &key))?
            .map(|r| Some(r.org_id)))
    }

    fn pn_printers(
        &self,
        org: Option<i64>,
        ids: Option<&[i64]>,
        agent_ids: Option<&[i64]>,
    ) -> Result<Vec<Value>, StoreError> {
        let win = self.cfg().online_window_s;
        let comps: HashMap<i64, Value> = self
            .db(|c| store::list_agents(c, win, None, org))?
            .iter()
            .map(|a| (a.id, printnode::computer(a)))
            .collect();
        Ok(self
            .db(|c| store::list_printers(c, win, None, org))?
            .iter()
            .filter(|p| {
                ids.is_none_or(|ids| ids.contains(&p.id))
                    && agent_ids.is_none_or(|ids| ids.contains(&p.agent_id))
            })
            .map(|p| printnode::printer(p, comps.get(&p.agent_id)))
            .collect())
    }

    fn pn_jobs(
        &self,
        org: Option<i64>,
        ids: Option<&[i64]>,
    ) -> Result<Vec<store::JobRow>, StoreError> {
        if let Some(ids) = ids.filter(|i| !i.is_empty()) {
            return self.db(|c| store::recent_jobs(c, ids.len() as i64, org, Some(ids)));
        }
        // `?limit=` is how their clients page the job list; silently capping every caller at our
        // own default would be a surprising truncation. Junk falls back to the default.
        let q = query_params(self.target());
        let limit = printnode::py_int(qfirst(&q, "limit").unwrap_or("50"))
            .map_or(50, |n| n.clamp(1, 500) as i64);
        self.db(|c| store::recent_jobs(c, limit, org, None))
    }

    /// PrintNode-shaped GETs. None if the path is not part of the compat surface.
    fn printnode_get(&self) -> Result<Option<Resp>, StoreError> {
        let path = self.req.path();
        let m = pn_set(path);
        if !PN_COLLECTIONS.contains(&path) && m.is_none() {
            return Ok(None);
        }
        let kind = m
            .as_ref()
            .map_or_else(|| path.trim_start_matches('/'), |m| m.kind);
        let sub = m.as_ref().map_or("", |m| m.sub);
        if (sub == "/printers" && kind != "computers") || (sub == "/states" && kind != "printjobs")
        {
            return Ok(None); // e.g. /printers/1/states — not a route of theirs
        }
        let Some(org) = self.pn_org()? else {
            return Ok(Some(self.pn_error(401, "Unauthorized", "invalid API key")));
        };
        let ids = match m.as_ref().map(|m| printnode::parse_set(m.set)).transpose() {
            Ok(ids) => ids,
            Err(e) => return Ok(Some(self.pn_error(400, "BadRequest", e.0))),
        };
        let win = self.cfg().online_window_s;
        let body: Value = if path == "/whoami" {
            let agents = self.db(|c| store::list_agents(c, win, None, org))?;
            let metrics = self.db(|c| store::metrics(c, win, None, org))?;
            let online: Vec<String> = agents
                .iter()
                .filter(|a| a.online)
                .map(|a| a.name.clone())
                .collect();
            printnode::whoami(org, &metrics, &online)
        } else if kind == "computers" && sub != "/printers" {
            let agents = self.db(|c| store::list_agents(c, win, None, org))?;
            Value::Array(
                agents
                    .iter()
                    .filter(|a| ids.as_ref().is_none_or(|ids| ids.contains(&a.id)))
                    .map(printnode::computer)
                    .collect(),
            )
        } else if kind == "printers" || sub == "/printers" {
            let by_printer = if kind == "printers" {
                ids.as_deref()
            } else {
                None
            };
            let by_agent = if kind == "computers" {
                ids.as_deref()
            } else {
                None
            };
            Value::Array(self.pn_printers(org, by_printer, by_agent)?)
        } else if sub == "/states" {
            Value::Array(
                self.pn_jobs(org, ids.as_deref())?
                    .iter()
                    .map(printnode::printjob_states)
                    .collect(),
            )
        } else {
            let pmap: HashMap<i64, Value> = {
                let printers = self.db(|c| store::list_printers(c, win, None, org))?;
                let comps: HashMap<i64, Value> = self
                    .db(|c| store::list_agents(c, win, None, org))?
                    .iter()
                    .map(|a| (a.id, printnode::computer(a)))
                    .collect();
                printers
                    .iter()
                    .map(|p| (p.id, printnode::printer(p, comps.get(&p.agent_id))))
                    .collect()
            };
            Value::Array(
                self.pn_jobs(org, ids.as_deref())?
                    .iter()
                    .map(|j| printnode::printjob(j, pmap.get(&j.printer_id)))
                    .collect(),
            )
        };
        Ok(Some(Resp::json(200, &body)))
    }

    fn printnode_post(&self) -> Result<Option<Resp>, StoreError> {
        if self.req.path() != "/printjobs" {
            return Ok(None);
        }
        let Some(org) = self.pn_org()? else {
            return Ok(Some(self.pn_error(401, "Unauthorized", "invalid API key")));
        };
        let Some(body) = self.parse_body() else {
            return Ok(Some(self.pn_error(
                400,
                "BadRequest",
                "malformed json body",
            )));
        };
        let map = match printnode::job_body(&body) {
            Ok(m) => m,
            Err(e) => return Ok(Some(self.pn_error(400, "BadRequest", e.0))),
        };
        Ok(Some(match self.submit_job(&map, org, None) {
            // they answer a create with the bare print job id
            Ok(e) => Resp::json(201, &json!(e.job_id)),
            Err(SubmitErr::Fetch(m)) => self.pn_error(502, "DownstreamError", m),
            Err(SubmitErr::Quota(m)) => self.pn_error(402, "QuotaExceeded", m),
            Err(SubmitErr::Locked) => self.pn_error(403, "Forbidden", ORG_LOCKED),
            Err(SubmitErr::Invalid(m)) => self.pn_error(400, "BadRequest", m),
            Err(SubmitErr::Busy(m)) => self.pn_error(503, "ServiceUnavailable", m),
            // ponytail: unreachable today, this layer takes no idempotency key
            Err(SubmitErr::InProgress(m)) => self.pn_error(409, "Conflict", m),
            Err(SubmitErr::Store(e)) => return Err(e),
        }))
    }

    /// Their DELETE /printjobs/{set} drops queued jobs; ours cancels them (a printed job stays in
    /// the history) and answers with the number affected, as they do.
    fn printnode_delete(&self) -> Result<Option<Resp>, StoreError> {
        let Some(m) = pn_set(self.req.path()).filter(|m| m.kind == "printjobs" && m.sub.is_empty())
        else {
            return Ok(None);
        };
        let Some(org) = self.pn_org()? else {
            return Ok(Some(self.pn_error(401, "Unauthorized", "invalid API key")));
        };
        let ids = match printnode::parse_set(m.set) {
            Ok(ids) => ids,
            Err(e) => return Ok(Some(self.pn_error(400, "BadRequest", e.0))),
        };
        let mut n = 0;
        for id in ids {
            if self.db(|c| store::cancel_job(c, id, org))? == store::CancelOutcome::Cancelled {
                n += 1;
            }
        }
        Ok(Some(Resp::json(200, &json!(n))))
    }

    // --- Star CloudPRNT -------------------------------------------------------------------------
    // The printer itself is the agent here: it POSTs its status on a timer, GETs the job data when
    // we offer one, and DELETEs with the result once it has printed. That maps onto the ordinary
    // queue — poll = claim_job, GET = get_payload, DELETE = finish_job — so these jobs carry the
    // same quota, history and dashboard as an agent's. Shapes live in cloudprnt.rs.

    /// Resolve a request to its enrolled device, or the 401/400 to answer. Every request
    /// re-touches the device, which is its liveness.
    fn cloudprnt_device(
        &self,
        path_key: Option<&str>,
        mac: &str,
    ) -> Result<Gate<store::CloudprntDevice>, StoreError> {
        let key = path_key.map_or_else(
            || printnode::basic_key(self.req.header("authorization")),
            str::to_string,
        );
        // An issued client key only: the root token belongs to no org, so it has no place to
        // enrol a printer into.
        let Some(row) = self.db(|c| store::authenticate_client(c, &key))? else {
            return Ok(Err(unauthorized()));
        };
        if printnode::py_trim(mac).is_empty() {
            return Ok(Err(jerr(400, "mac required")));
        }
        Ok(Ok(self.db(|c| {
            store::register_cloudprnt(c, row.org_id, &cloudprnt::device_name(mac))
        })?))
    }

    fn cloudprnt_poll(&self, path_key: Option<&str>) -> R {
        let Some(body) = self.parse_body() else {
            return Ok(bad_json());
        };
        let empty = Obj::new();
        let body = body.as_object().unwrap_or(&empty);
        let dev =
            gate!(self.cloudprnt_device(path_key, str_field(body, "printerMAC").unwrap_or(""))?);
        let mut job: Option<(i64, String)> = None;
        if !body.get("printingInProgress").is_some_and(truthy) {
            // A printer that is mid-print gets offered nothing — it has not confirmed the job it
            // holds yet, and re-offering it invites a second copy. Otherwise re-offer what it
            // already holds before claiming anything new: a poll response lost on the way must not
            // burn the next job in the queue.
            job = match self.db(|c| store::claimed_job(c, dev.agent_id))? {
                Some(j) => Some((j.job_id, j.mode)),
                None => self
                    .db(|c| store::claim_job(c, dev.agent_id, None))?
                    .map(|j| (j.job_id, j.mode)),
            };
        }
        if let Some((id, mode)) = &job
            && mode != "raw"
        {
            // gotcha #1: no renderer in the printer, so a PDF would come out as blank feed.
            // Fail it here — the dashboard then says why — instead of handing it over.
            // ponytail: one such job per poll; the next poll takes the next one.
            let msg = "CloudPRNT printers cannot render PDF — send raw Star commands";
            self.db(|c| store::finish_job(c, *id, dev.agent_id, false, Some(msg)))?;
            job = None;
        }
        let media = cloudprnt::media_type(Some(self.req.header("accept")));
        Ok(Resp::json(
            200,
            &cloudprnt::poll_response(job.map(|(id, _)| id), media),
        ))
    }

    fn cloudprnt_payload(&self, path_key: Option<&str>) -> R {
        let q = query_params(self.target());
        let dev = gate!(self.cloudprnt_device(path_key, qfirst(&q, "mac").unwrap_or(""))?);
        let media = qfirst(&q, "type")
            .unwrap_or_else(|| cloudprnt::media_type(Some(self.req.header("accept"))));
        if !cloudprnt::MEDIA_TYPES.contains(&media) {
            // We hand over the submitted bytes unchanged, so a type we cannot honestly label them
            // with is their 415, not a silent mislabel.
            return Ok(jerr(415, format!("cannot serve {media}")));
        }
        let job = self.db(|c| store::claimed_job(c, dev.agent_id))?;
        let token = qfirst(&q, "token").unwrap_or("");
        let job = match job {
            Some(j) if token.is_empty() || token == j.job_id.to_string() => j,
            _ => return Ok(jerr(404, "no job")), // their "no data available"
        };
        let data = self
            .db(|c| store::get_payload(c, job.job_id, dev.agent_id))?
            .unwrap_or_default();
        // nothing in this protocol counts copies — repeat the stream (written in passes, so 100
        // copies of a 32 MB job is not 3 GB of RAM)
        Ok(Resp {
            status: 200,
            headers: vec![("Content-Type", media.to_string())],
            body: RespBody::Repeat(data.into(), job.copies.max(0) as usize),
        })
    }

    fn cloudprnt_confirm(&self, path_key: Option<&str>) -> R {
        let q = query_params(self.target());
        let dev = gate!(self.cloudprnt_device(path_key, qfirst(&q, "mac").unwrap_or(""))?);
        if let Some(job) = self.db(|c| store::claimed_job(c, dev.agent_id))? {
            let code = qfirst(&q, "code").unwrap_or("");
            let ok = cloudprnt::job_ok(Some(code));
            let err =
                (!ok).then(|| format!("printer reported {}", cloudprnt::status_text(Some(code))));
            self.db(|c| store::finish_job(c, job.job_id, dev.agent_id, ok, err.as_deref()))?;
        }
        // ponytail: no `deleteMethod` — the printer confirms with DELETE, which is the default.
        // Offer the GET form if a deployment ever sits behind a proxy that blocks DELETE.
        Ok(Resp::empty(200)) // their DELETE answer: 200, no body
    }

    // --- dashboard ------------------------------------------------------------------------------

    /// Serve the static dashboard bundle. None if the path isn't one of ours. HEAD carries the
    /// same headers (Content-Length included) and no body.
    fn dashboard(&self, head: bool) -> Option<Resp> {
        let target = self.target();
        let Some(f) = web::lookup(target) else {
            // bundle not built yet
            let is_root = matches!(target, "/" | "/index.html");
            return (is_root && !web::has_index()).then(|| html(503, web::NO_DASHBOARD_HTML, head));
        };
        // decided from the resolved file, not the raw URL (see web::lookup)
        let cache = if f.immutable {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        let mut headers = vec![
            ("Content-Type", f.content_type.to_string()),
            ("Cache-Control", cache.to_string()),
        ];
        if head {
            headers.push(("Content-Length", f.data.len().to_string()));
        }
        let body = if head {
            RespBody::Empty
        } else {
            RespBody::Bytes(Bytes::from_static(f.data))
        };
        Some(Resp {
            status: 200,
            headers,
            body,
        })
    }

    // --- methods --------------------------------------------------------------------------------

    fn head(&self) -> Resp {
        // Uptime checks and the browser's own probes HEAD the dashboard and /health.
        if self.target() == "/health" {
            return Resp::empty(200);
        }
        self.dashboard(true).unwrap_or_else(|| Resp::empty(404))
    }

    fn get(&self) -> R {
        let t = self.target();
        if self.is_basic()
            && let Some(r) = self.printnode_get()?
        {
            return Ok(r);
        }
        if let Some(key) = cloudprnt_route(self.req.path()) {
            return self.cloudprnt_payload(key);
        }
        if self.req.path() == "/support-access" {
            // the target carries a query (`?org=`), so it is matched by path, not in the table below
            return self.support_access();
        }
        match t {
            "/" | "/index.html" => return Ok(self.dashboard(false).unwrap_or_else(not_found)),
            // Unauthenticated on purpose: the sign-in screen reads it to decide whether to offer
            // "create an account" and "forgot password" at all. Both are booleans about the
            // server's configuration, not about any account.
            "/health" => {
                let mut h = json!({ "ok": true, "signup": if self.cfg().signup_open { "open" } else { "closed" },
                                    "password_reset": self.cfg().reset_enabled });
                // Only when consent is required, so a server without it answers as before.
                if let Some(t) = &self.cfg().terms {
                    h["terms"] = json!({ "version": t.version, "terms_url": t.terms_url,
                                         "dpa_url": t.dpa_url, "privacy_url": t.privacy_url });
                }
                return Ok(Resp::json(200, &h));
            }
            "/metrics" => {
                let org = gate!(self.client_org()?);
                let m = self.db(|c| store::metrics(c, self.cfg().online_window_s, None, org))?;
                return Ok(Resp {
                    status: 200,
                    headers: vec![(
                        "Content-Type",
                        "text/plain; version=0.0.4; charset=utf-8".into(),
                    )],
                    body: RespBody::Bytes(prometheus(&m).into()),
                });
            }
            "/jobs" => {
                let org = gate!(self.client_org()?);
                let jobs = self.db(|c| store::recent_jobs(c, 50, org, None))?;
                return Ok(Resp::json(200, &json!({ "jobs": val(&jobs) })));
            }
            "/labels" => {
                let org = gate!(self.client_org()?);
                let all = self.db(|c| store::list_label_templates(c, org))?;
                let templates: Vec<Value> = all.iter().map(store::LabelTemplate::to_json).collect();
                return Ok(Resp::json(200, &json!({ "templates": templates })));
            }
            _ => {}
        }
        if let Some(id) = id_of(t, "/labels/", "") {
            let org = gate!(self.client_org()?);
            return Ok(match self.db(|c| store::get_label_template(c, id, org))? {
                Some(l) => Resp::json(200, &l.to_json()),
                None => not_found(),
            });
        }
        if let Some(id) = id_of(t, "/jobs/", "") {
            let org = gate!(self.client_org()?);
            return Ok(match self.db(|c| store::get_job(c, id, org))? {
                Some(j) => Resp::json(200, &val(&j)),
                None => not_found(),
            });
        }
        match t {
            "/printers" => {
                let org = gate!(self.client_org()?);
                let v =
                    self.db(|c| store::list_printers(c, self.cfg().online_window_s, None, org))?;
                return Ok(Resp::json(200, &json!({ "printers": val(&v) })));
            }
            "/computers" => {
                let org = gate!(self.client_org()?);
                let v =
                    self.db(|c| store::list_agents(c, self.cfg().online_window_s, None, org))?;
                return Ok(Resp::json(200, &json!({ "computers": val(&v) })));
            }
            "/me" => {
                let Some(p) = self.principal()? else {
                    return Ok(unauthorized());
                };
                let mut me = json!({ "kind": p.kind, "org_id": p.org_id });
                if p.kind == "session" {
                    me["email"] = json!(p.email);
                    me["user_id"] = json!(p.user_id);
                }
                return Ok(Resp::json(200, &me));
            }
            "/plans" => {
                // The catalogue, with each checkout link already pointed at the caller's own org so
                // the provider hands that id back in its webhook. [] = billing not configured.
                let Some(p) = self.principal()? else {
                    return Ok(unauthorized());
                };
                let org = match p.org_id {
                    Some(o) => self.db(|c| store::get_org(c, o))?,
                    None => None,
                };
                let plans: Vec<Value> = self
                    .cfg()
                    .plans
                    .iter()
                    .map(|pl| {
                        let mut v = val(pl);
                        v["checkout_url"] = billing::checkout_url(pl, p.org_id);
                        v
                    })
                    .collect();
                return Ok(Resp::json(
                    200,
                    &json!({ "plans": plans, "current": org.and_then(|o| o.plan) }),
                ));
            }
            "/apikeys" => {
                let p = gate!(self.manager()?);
                let keys = self.db(|c| store::list_api_keys(c, p.org_id))?;
                return Ok(Resp::json(200, &json!({ "keys": val(&keys) })));
            }
            "/users" => {
                let p = gate!(self.manager()?);
                let users = self.db(|c| store::list_users(c, p.org_id))?;
                return Ok(Resp::json(200, &json!({ "users": val(&users) })));
            }
            "/orgs" => {
                gate!(self.admin());
                let orgs = self.db(store::list_orgs)?;
                return Ok(Resp::json(200, &json!({ "orgs": val(&orgs) })));
            }
            _ => {}
        }
        if let Some(oid) = id_of(t, "/orgs/", "") {
            // One org's own settings, readable by a session so the dashboard can show them — the
            // root-only GET /orgs above stays the cross-org listing.
            let p = gate!(self.manager()?);
            let org = if p.foreign_org(oid) {
                None
            } else {
                self.db(|c| store::get_org(c, oid))?
            };
            let Some(org) = org else {
                return Ok(not_found());
            };
            let usage = self.db(|c| store::org_usage(c, oid, None))?;
            return Ok(Resp::json(
                200,
                &json!({
                    "id": org.id, "name": org.name, "event_url": org.event_url,
                    // never echo the secret
                    "shopify_secret_set": org.shopify_secret.as_deref().is_some_and(|s| !s.is_empty()),
                    "job_quota": org.job_quota, "plan": org.plan,
                    "jobs_this_month": usage, "created_at": org.created_at,
                }),
            ));
        }
        if let Some(jid) = id_of(t, "/agent/jobs/", "/payload") {
            let Some(aid) = self.agent_id()? else {
                return Ok(unauthorized());
            };
            return Ok(match self.db(|c| store::get_payload(c, jid, aid))? {
                None => not_found(),
                Some(data) => Resp {
                    status: 200,
                    headers: vec![("Content-Type", "application/octet-stream".into())],
                    body: RespBody::Bytes(data.into()),
                },
            });
        }
        if t == "/agent/jobs" {
            // The real route is the async long-poll in `http`; this is its single non-waiting claim.
            let Some(aid) = self.agent_id()? else {
                return Ok(unauthorized());
            };
            return Ok(match self.db(|c| store::claim_job(c, aid, None))? {
                Some(job) => Resp::json(200, &val(&job)),
                None => Resp::empty(204),
            });
        }
        // Last: the dashboard bundle (/_next/..., /devices/, ...). After the API routes, so a stray
        // file in the bundle can never shadow an endpoint.
        Ok(self.dashboard(false).unwrap_or_else(not_found))
    }

    fn post(&self) -> R {
        let t = self.target();
        if self.is_basic()
            && let Some(r) = self.printnode_post()?
        {
            return Ok(r);
        }
        if let Some(key) = cloudprnt_route(self.req.path()) {
            return self.cloudprnt_poll(key);
        }
        match t {
            "/jobs" => return self.post_jobs(),
            "/orders" => return self.post_orders(),
            "/labels" => return self.post_labels(),
            "/billing/webhook" => return self.billing_webhook(),
            "/login" => return self.login(),
            "/signup" => return self.signup(),
            "/password/reset" => return self.password_reset(),
            "/password/reset/confirm" => return self.password_reset_confirm(),
            _ => {}
        }
        if self.req.path() == "/integrations/shopify/orders" {
            return self.shopify_webhook();
        }
        match t {
            "/logout" => {
                if self.principal()?.is_none_or(|p| p.kind != "session") {
                    return Ok(unauthorized());
                }
                self.db(|c| store::delete_session(c, self.presented_key()))?;
                return Ok(ok_true());
            }
            "/users" => {
                let p = gate!(self.manager()?);
                let Some(org_id) = p.org_id else {
                    // root belongs to no org — it must name one
                    return Ok(jerr(400, "root has no org; use POST /orgs/{id}/users"));
                };
                let body = gate!(self.read_obj());
                return self.create_user(org_id, &body);
            }
            "/apikeys" => return self.post_apikeys(),
            "/orgs" => {
                gate!(self.admin());
                let body = gate!(self.read_obj());
                let name = str_field(&body, "name")
                    .map(|n| printnode::py_trim(n).to_string())
                    .unwrap_or_default();
                if name.is_empty() {
                    return Ok(jerr(400, "name required"));
                }
                let id = self.db(|c| store::create_org(c, &name))?;
                return Ok(Resp::json(200, &json!({ "id": id, "name": name })));
            }
            "/agent/register" => return self.agent_register(),
            _ => {}
        }
        if let Some(oid) = id_of(t, "/orgs/", "/users") {
            // seeding an org's first user is root's job
            gate!(self.admin());
            let body = gate!(self.read_obj());
            if !self.db(|c| store::org_exists(c, oid))? {
                return Ok(not_found());
            }
            return self.create_user(oid, &body);
        }
        if let Some(jid) = id_of(t, "/agent/jobs/", "/result") {
            let Some(aid) = self.agent_id()? else {
                return Ok(unauthorized());
            };
            let body = gate!(self.read_obj());
            let ok = body.get("ok").is_some_and(truthy);
            let error = sql_text(body.get("error"));
            return Ok(
                if self.db(|c| store::finish_job(c, jid, aid, ok, error.as_deref()))? {
                    ok_true()
                } else {
                    not_found()
                },
            );
        }
        Ok(not_found())
    }

    fn post_apikeys(&self) -> R {
        let p = gate!(self.manager()?);
        let body = gate!(self.read_obj());
        // A truthy non-string label crashed the Python on .strip() before any key was issued —
        // refuse it the same way (no key) instead of quietly issuing one labelled "client".
        if body
            .get("label")
            .is_some_and(|l| truthy(l) && !l.is_string())
        {
            return Ok(Resp::json(400, &json!({"error": "label must be a string"})));
        }
        let label = str_field(&body, "label")
            .map(printnode::py_trim)
            .filter(|l| !l.is_empty())
            .unwrap_or("client")
            .to_string();
        // The org id is echoed exactly as the caller wrote it (a string "2" stays "2"); the DB
        // gets the integer SQLite would compare it as.
        let org_v = match body.get("org_id").filter(|v| truthy(v)) {
            Some(v) => v.clone(),
            None => json!(p.org_id.unwrap_or(store::DEFAULT_ORG)),
        };
        if p.org_id.is_some_and(|o| !py_eq_int(&org_v, o)) {
            return Ok(jerr(400, "org_id not allowed"));
        }
        let org_id = match as_sql_int(&org_v) {
            Some(o) if self.db(|c| store::org_exists(c, o))? => o,
            _ => return Ok(jerr(400, "unknown org")),
        };
        let key = auth::token_urlsafe(32);
        let kid = self.db(|c| store::add_api_key(c, &label, &key, org_id))?;
        Ok(Resp::json(
            200,
            &json!({ "id": kid, "label": label, "org_id": org_v, "key": key }),
        ))
    }

    fn agent_register(&self) -> R {
        let body = gate!(self.read_obj());
        let key = self.presented_key();
        // Who may enroll an agent, and into which org: an issued client key -> that key's org;
        // the root token, or a key an agent already holds (machines set up before issued keys
        // were required) -> the default org. Any other value only with PRINTAPI_OPEN_ENROLLMENT
        // (the v2 rule): otherwise whoever can reach /agent/register could plant agents and
        // printers in the default org. A revoked key enrolls nothing — and polls nothing either
        // (store::authenticate_agent).
        // ponytail: the agent key doubles as its org's client key; issue one key per agent to
        // keep the blast radius of a lost machine to that machine.
        let (root, open) = (self.admin_ok(), self.cfg().open_enrollment);
        // Checked here so a refused key answers 401 before any body validation, and again below
        // in the same lock hold as the registration, so a revocation in between cannot slip by.
        if let Err(m) = self.db(|c| enrollment_org(c, key, root, open, self.operator_cred))? {
            return Ok(jerr(401, m));
        }
        let mut printers = Vec::new();
        match body.get("printers") {
            None => {}
            Some(Value::Array(items)) => {
                for p in items {
                    let Some(name) = p.get("name") else {
                        return Ok(jerr(
                            400,
                            format!("printer entry missing 'name': {}", repr(p)),
                        ));
                    };
                    // Python hit NOT NULL (409, rolled back) on a null name; a clean 400 here
                    if name.is_null() {
                        return Ok(jerr(
                            400,
                            format!("printer entry has a null 'name': {}", repr(p)),
                        ));
                    }
                    printers.push(store::PrinterDecl {
                        name: sql_text(Some(name)).unwrap_or_default(),
                        can_pdf: p.get("can_pdf").is_some_and(truthy),
                        capabilities: p.get("capabilities").filter(|c| truthy(c)).cloned(),
                    });
                }
            }
            Some(_) => return Ok(jerr(400, "printers must be a list")),
        }
        // deliberate: Python hit a NOT NULL violation (409) for a missing name
        // (a number or bool is stored as its text, as SQLite converts it)
        let name = match body.get("name") {
            Some(v @ (Value::String(_) | Value::Number(_) | Value::Bool(_))) => {
                sql_text(Some(v)).unwrap_or_default()
            }
            _ => return Ok(jerr(400, "name required")),
        };
        let name = name.as_str();
        // First contact binds name->key; re-register requires the same key.
        let registered = self.db(|c| -> Result<_, StoreError> {
            Ok(
                match enrollment_org(c, key, root, open, self.operator_cred)? {
                    Err(m) => Err(m),
                    Ok(org) => Ok(store::register_agent(c, name, key, &printers, org)),
                },
            )
        })?;
        match registered {
            Err(m) => Ok(jerr(401, m)),
            Ok(Ok(reg)) => Ok(Resp::json(200, &val(&reg))),
            Ok(Err(StoreError::Auth(m))) => Ok(jerr(401, m)),
            Ok(Err(StoreError::Invalid(m))) => Ok(jerr(400, m)),
            Ok(Err(StoreError::Conflict)) => Ok(jerr(409, "agent key already in use")),
            Ok(Err(e)) => Err(e),
        }
    }

    fn put(&self) -> R {
        let t = self.target();
        if t == "/me/password" {
            let Some(p) = self.principal()?.filter(|p| p.kind == "session") else {
                return Ok(unauthorized());
            };
            let body = gate!(self.read_obj());
            let email = p.email.as_deref().unwrap_or("");
            let user = self.db(|c| store::get_user_by_email(c, email))?;
            let stored = user.as_ref().and_then(|u| u.password_hash.as_deref());
            if !auth::verify_password(str_field(&body, "current"), stored) {
                return Ok(jerr(401, "current password does not match"));
            }
            let pw_hash = match auth::hash_password(str_field(&body, "new")) {
                Ok(h) => h,
                Err(e) => return Ok(jerr(400, e)),
            };
            // Drops every session of this user, including the one making the call — a stolen
            // session dies with the password it was minted from.
            self.db(|c| store::set_user_password(c, p.user_id.unwrap_or(0), &pw_hash))?;
            return Ok(ok_true());
        }
        if let Some(oid) = id_of(t, "/orgs/", "") {
            return self.put_org(oid);
        }
        if let Some(id) = id_of(t, "/labels/", "") {
            return self.put_label(id);
        }
        Ok(not_found())
    }

    /// Field by field, in the order the Python applied them — not transactional: a later field's
    /// rejection leaves the earlier ones written.
    fn put_org(&self, oid: i64) -> R {
        let p = gate!(self.manager()?);
        let body = gate!(self.read_obj());
        let mut applied = Obj::new();
        if p.foreign_org(oid) {
            return Ok(not_found());
        }
        if let Some(ev) = body.get("event_url") {
            // Same http(s) check as a job's callback_url; null/"" clears the URL.
            let mut probe = Obj::new();
            probe.insert("callback_url".into(), ev.clone());
            let url = match dispatch::parse_callback_url(&probe) {
                Ok(u) => u,
                Err(e) => {
                    return Ok(jerr(
                        400,
                        e.to_string().replace("callback_url", "event_url"),
                    ));
                }
            };
            if !self.db(|c| store::set_org_event_url(c, oid, url.as_deref()))? {
                return Ok(not_found());
            }
            applied.insert("event_url".into(), json!(url));
        }
        if let Some(quota) = body.get("job_quota") {
            // The one org field a session may *not* set: a tenant that could raise its own cap has
            // no cap. Only the operator's bootstrap token writes it.
            if p.kind != "root" {
                return Ok(jerr(403, "job_quota is set by the server operator"));
            }
            let quota_n = match parse_job_quota(quota) {
                Ok(q) => q,
                Err(msg) => return Ok(jerr(400, msg)),
            };
            if !self.db(|c| store::set_org_quota(c, oid, quota_n))? {
                return Ok(not_found());
            }
            applied.insert("job_quota".into(), json!(quota_n));
        }
        if let Some(plan_v) = body.get("plan") {
            // Same rule as the quota it grants: the operator (or a signed billing event) moves an
            // org between plans, never the org itself.
            if p.kind != "root" {
                return Ok(jerr(403, "plan is set by billing, not by the org"));
            }
            let Some(plan) = billing::find(&self.cfg().plans, plan_v) else {
                return Ok(jerr(400, format!("unknown plan: {}", repr(plan_v))));
            };
            if !self.db(|c| store::set_org_plan(c, oid, &plan.id, plan.jobs))? {
                return Ok(not_found());
            }
            applied.insert("plan".into(), json!(plan.id));
            applied.insert("job_quota".into(), json!(plan.jobs));
        }
        if let Some(secret) = body.get("shopify_secret") {
            // null clears it
            let secret = match secret {
                Value::Null => None,
                Value::String(s) if !printnode::py_trim(s).is_empty() => Some(s.as_str()),
                _ => return Ok(jerr(400, "shopify_secret must be a string or null")),
            };
            if !self.db(|c| store::set_org_shopify_secret(c, oid, secret))? {
                return Ok(not_found());
            }
            // never echo the secret
            applied.insert("shopify_secret_set".into(), json!(secret.is_some()));
        }
        if applied.is_empty() {
            if !self.db(|c| store::org_exists(c, oid))? {
                return Ok(not_found());
            }
            return Ok(jerr(400, "nothing to update"));
        }
        let mut out = Obj::new();
        out.insert("ok".into(), json!(true));
        out.extend(applied);
        Ok(Resp::json(200, &Value::Object(out)))
    }

    fn delete(&self) -> R {
        let t = self.target();
        if self.is_basic()
            && let Some(r) = self.printnode_delete()?
        {
            return Ok(r);
        }
        if let Some(key) = cloudprnt_route(self.req.path()) {
            return self.cloudprnt_confirm(key);
        }
        if let Some(id) = id_of(t, "/apikeys/", "") {
            let p = gate!(self.manager()?);
            let revoked = self.db(|c| store::revoke_api_key(c, id, p.org_id))?;
            if revoked {
                // a long-poll of an agent enrolled with this key re-checks it and ends with 401
                self.s.waker.wake_all();
            }
            return Ok(if revoked { ok_true() } else { not_found() });
        }
        if let Some(uid) = id_of(t, "/users/", "") {
            let p = gate!(self.manager()?);
            if p.user_id == Some(uid) {
                // Removing yourself would sign you out mid-request and, in a two-admin org, is the
                // mistake nobody can undo from the dashboard. Someone else does it.
                return Ok(jerr(400, "cannot remove your own account"));
            }
            return Ok(match self.db(|c| store::delete_user(c, uid, p.org_id))? {
                store::DeleteUser::Deleted => ok_true(),
                store::DeleteUser::LastUser => jerr(400, "an org must keep at least one account"),
                store::DeleteUser::NotFound => not_found(),
            });
        }
        if let Some(oid) = id_of(t, "/orgs/", "") {
            // Root only, and never DEFAULT_ORG: an org holds other people's print history, and the
            // default one is where an agent with an unknown key still lands.
            gate!(self.admin());
            if oid == store::DEFAULT_ORG {
                return Ok(jerr(400, "the default org cannot be removed"));
            }
            return Ok(if self.db(|c| store::delete_org(c, oid))? {
                // secure_delete zeroed the pages; this clears the WAL frames too. A reader can
                // block it: logged, never a failed request (the org is gone either way).
                self.db(|c| store::truncate_wal_logged(c, "delete org"));
                ok_true()
            } else {
                not_found()
            });
        }
        if let Some(id) = id_of(t, "/labels/", "") {
            let org = gate!(self.client_org()?);
            return Ok(if self.db(|c| store::delete_label_template(c, id, org))? {
                ok_true()
            } else {
                not_found()
            });
        }
        if let Some(jid) = id_of(t, "/jobs/", "") {
            let org = gate!(self.client_org()?);
            return Ok(match self.db(|c| store::cancel_job(c, jid, org))? {
                store::CancelOutcome::Cancelled => {
                    Resp::json(200, &json!({ "ok": true, "state": "cancelled" }))
                }
                store::CancelOutcome::NotFound => not_found(),
                store::CancelOutcome::NotCancellable => {
                    jerr(409, "job not cancellable (already claimed or finished)")
                }
            });
        }
        Ok(not_found())
    }
}

/// Stand-in for Python's `JSONDecodeError` text, which the billing webhook echoed.
/// ponytail: the position is serde's, so it can differ from Python's by a character or two, and
/// every syntax error reads "Expecting value" — a caller cannot act on the difference.
fn json_error_text(e: &serde_json::Error) -> String {
    let col = e.column().max(1);
    let offset = e.column().saturating_sub(1);
    format!(
        "Expecting value: line {} column {} (char {})",
        e.line().max(1),
        col,
        offset
    )
}

// --- Prometheus ---------------------------------------------------------------------------------

const JOB_STATES: [&str; 5] = ["queued", "claimed", "done", "failed", "cancelled"];

/// A `store::metrics` snapshot as Prometheus text (v0.0.4), byte-identical to the Python.
pub fn prometheus(m: &Metrics) -> String {
    let mut out = vec![
        "# HELP printpapi_jobs Jobs by state.".to_string(),
        "# TYPE printpapi_jobs gauge".to_string(),
    ];
    // emit all states (incl. 0) for stable series
    for s in JOB_STATES {
        out.push(format!(
            "printpapi_jobs{{state=\"{s}\"}} {}",
            m.jobs.get(s).copied().unwrap_or(0)
        ));
    }
    for (name, help, val) in [
        (
            "printpapi_agents_online",
            "Agents seen within the online window.",
            m.agents_online,
        ),
        (
            "printpapi_agents_total",
            "Registered agents.",
            m.agents_total,
        ),
        (
            "printpapi_printers_total",
            "Registered printers.",
            m.printers_total,
        ),
    ] {
        out.push(format!("# HELP {name} {help}"));
        out.push(format!("# TYPE {name} gauge"));
        out.push(format!("{name} {val}"));
    }
    out.join("\n") + "\n"
}

// --- background passes --------------------------------------------------------------------------

/// One delivery pass: POST each pending terminal job to its callback_url (outside the DB lock).
/// `post` Ok -> mark delivered; any error -> bump the attempt count (retried next pass until the
/// cap, then given up). One bad URL never aborts the pass.
pub fn deliver_webhooks(
    db: &Db,
    post: &dyn Fn(&str, &Value) -> Result<(), String>,
    max_attempts: i64,
) {
    let jobs = match db.with(|c| store::pending_webhooks(c, max_attempts)) {
        Ok(j) => j,
        Err(e) => return eprintln!("webhook dispatcher error: {e}"),
    };
    for job in jobs {
        let payload = json!({ "job_id": job.job_id, "state": job.state, "error": job.error,
                              "title": job.title, "printer_id": job.printer_id });
        // only a POST failure counts as a delivery failure
        let res = match post(&job.callback_url, &payload) {
            Err(e) => {
                eprintln!("webhook {} -> {} failed: {e}", job.job_id, job.callback_url);
                db.with(|c| store::bump_webhook_attempt(c, job.job_id))
            }
            // marked outside the post handling: a mark-time DB error must not look like a POST failure
            Ok(()) => db.with(|c| store::mark_webhook_delivered(c, job.job_id, None)),
        };
        if let Err(e) = res {
            return eprintln!("webhook dispatcher error: {e}");
        }
    }
}

/// One pass of agent liveness events: POST `computer_offline` / `computer_online` to the
/// event_url of the agent's org (fleet operators watch customer-site machines with these).
/// ponytail: at-most-once — the edge is consumed when it is claimed, so a failed POST is logged,
/// not retried. Add an attempt counter like the job hooks if these ever need a guarantee.
pub fn deliver_agent_events(
    db: &Db,
    post: &dyn Fn(&str, &Value) -> Result<(), String>,
    online_window_s: f64,
    now: Option<f64>,
) {
    let events = match db.with(|c| store::claim_agent_transitions(c, online_window_s, now)) {
        Ok(e) => e,
        Err(e) => return eprintln!("webhook dispatcher error: {e}"),
    };
    for ev in events {
        let event = format!("computer_{}", ev.event);
        let payload = json!({ "event": event, "computer_id": ev.agent_id, "name": ev.name,
                              "org_id": ev.org_id, "last_seen_at": ev.last_seen_at });
        if let Err(e) = post(&ev.event_url, &payload) {
            eprintln!(
                "agent event {event} for {} -> {} failed: {e}",
                repr_str(&ev.name),
                ev.event_url
            );
        }
    }
}

/// How often the retention purge may run.
const RETENTION_EVERY_S: f64 = 3600.0;

/// The job retention purge (`store::purge_jobs`), at most once an hour and only when
/// PRINTAPI_JOB_RETENTION_DAYS is set. `None`: not due, not configured, or the purge failed
/// (logged; the next tick after the hour tries again).
pub fn retention_pass(state: &AppState, now: f64) -> Option<store::Purged> {
    let days = state.cfg.job_retention_days?;
    {
        let mut last = state
            .last_retention
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| now - t < RETENTION_EVERY_S) {
            return None;
        }
        *last = Some(now);
    }
    use std::sync::atomic::Ordering::Relaxed;
    let retry = state.wal_truncate_pending.load(Relaxed);
    match state.db.with(|c| {
        let mut p = store::purge_jobs(c, days, Some(now))?;
        // A pass that changed nothing does not truncate itself, so an earlier blocked truncate
        // is retried here.
        if retry && !p.wal_busy && p.abandoned + p.scrubbed + p.deleted == 0 {
            p.wal_busy = !store::truncate_wal_logged(c, "retention");
        }
        Ok::<_, StoreError>(p)
    }) {
        Ok(p) => {
            state.wal_truncate_pending.store(p.wal_busy, Relaxed);
            *state.last_purge.lock().unwrap_or_else(|e| e.into_inner()) = Some((now, p.clone()));
            Some(p)
        }
        Err(e) => {
            eprintln!("retention error: {e}");
            None
        }
    }
}

/// One reaper pass: expire, requeue stale (waking every long-poll if anything was requeued),
/// purge expired sessions and resets. Errors are logged to stderr, never propagated.
pub fn reaper_pass(state: &AppState, timeout_s: f64, max_retries: i64) {
    let run = || -> store::Result<()> {
        state.db.with(|c| store::expire_jobs(c, None))?;
        if state.db.with(|c| {
            store::requeue_stale(
                c,
                timeout_s,
                max_retries,
                None,
                state.cfg.operator_token.is_some(),
            )
        })? > 0
        {
            state.waker.wake_all();
        }
        state.db.with(|c| store::purge_expired_sessions(c, None))?;
        state.db.with(|c| store::purge_expired_resets(c, None))?;
        retention_pass(state, crate::now());
        Ok(())
    };
    if let Err(e) = run() {
        eprintln!("reaper error: {e}");
    }
}
