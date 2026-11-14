// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Shared helpers of the HTTP-level tests: an `AppState` over an in-memory DB with a capturing
//! mailer and a canned fetcher, hand-built requests, and the store shortcuts the Python tests used.
#![allow(dead_code)]
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use bytes::Bytes;
use printpapi_server::dispatch::Fetcher;
use printpapi_server::mail::Mailer;
use printpapi_server::server::{AppState, Config, Req, Resp, RespBody, handle};
use printpapi_server::store::{self, Db, NewJob, PrinterDecl};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub const PW: &str = "hunter2hunter2";

#[derive(Debug, Clone)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub body: String,
}

/// Stands in for the SMTP mailer: keeps what would have been sent.
#[derive(Default)]
pub struct Mailbox {
    pub sent: Mutex<Vec<Mail>>,
    /// Fail every send (a broken mail server).
    pub fail: Mutex<bool>,
}

impl Mailer for Mailbox {
    fn send(&self, to: &str, subject: &str, body: &str) -> Result<bool, String> {
        if *self.fail.lock().unwrap() {
            return Err("smtp down".into());
        }
        self.sent.lock().unwrap().push(Mail {
            to: to.into(),
            subject: subject.into(),
            body: body.into(),
        });
        Ok(true)
    }
}

impl Mailbox {
    pub fn all(&self) -> Vec<Mail> {
        self.sent.lock().unwrap().clone()
    }
}

/// A fetcher that answers every GET with `body` (or `Err`) and records the URLs.
#[derive(Default)]
pub struct FakeFetcher {
    pub gets: Mutex<Vec<String>>,
    pub reply: Mutex<Option<Result<Vec<u8>, String>>>,
}

impl Fetcher for FakeFetcher {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        self.gets.lock().unwrap().push(url.to_string());
        self.reply
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Ok(b"FETCHED".to_vec()))
    }

    fn post_json(&self, url: &str, _body: &Value) -> Result<Vec<u8>, String> {
        self.get(url)
    }
}

pub struct App {
    pub state: AppState,
    pub db: Db,
    pub mail: Arc<Mailbox>,
    pub fetch: Arc<FakeFetcher>,
}

/// The reply, decoded the way the Python tests read it.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The body as JSON; an empty body reads as `{}` (`json.loads(body or b"{}")`).
    pub fn json(&self) -> Value {
        if self.body.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&self.body).expect("a JSON body")
        }
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

pub fn mem() -> Db {
    Db::open(":memory:").unwrap()
}

impl App {
    pub fn new() -> App {
        App::with(|_| {})
    }

    /// Defaults (root token "t") plus whatever `tweak` changes.
    pub fn with(tweak: impl FnOnce(&mut Config)) -> App {
        App::on(mem(), tweak)
    }

    /// A second server over an existing DB (the Python tests started several handlers on one conn).
    pub fn on(db: Db, tweak: impl FnOnce(&mut Config)) -> App {
        let mut cfg = Config::new("t");
        tweak(&mut cfg);
        let mail = Arc::new(Mailbox::default());
        let fetch = Arc::new(FakeFetcher::default());
        let state = AppState::new(cfg, db.clone(), mail.clone(), fetch.clone());
        App {
            state,
            db,
            mail,
            fetch,
        }
    }

    pub fn call(&self, method: &str, target: &str) -> Call<'_> {
        Call {
            app: self,
            req: Req {
                method: method.into(),
                target: target.into(),
                headers: http::HeaderMap::new(),
                body: Bytes::new(),
                body_error: false,
                peer_ip: "127.0.0.1".into(),
                socket_peer: "127.0.0.1".into(),
            },
        }
    }

    /// `_req(method, url, token, body)`: a Bearer call answered as (status, JSON).
    pub fn req(
        &self,
        method: &str,
        target: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut c = self.call(method, target);
        if let Some(t) = token {
            c = c.bearer(t);
        }
        if let Some(b) = body {
            c = c.json(&b);
        }
        let r = c.send();
        (r.status, r.json())
    }

    pub fn get(&self, target: &str, token: Option<&str>) -> (u16, Value) {
        self.req("GET", target, token, None)
    }

    pub fn post(&self, target: &str, token: Option<&str>, body: Value) -> (u16, Value) {
        self.req("POST", target, token, Some(body))
    }

    pub fn put(&self, target: &str, token: Option<&str>, body: Value) -> (u16, Value) {
        self.req("PUT", target, token, Some(body))
    }

    pub fn delete(&self, target: &str, token: Option<&str>) -> (u16, Value) {
        self.req("DELETE", target, token, None)
    }

    /// A PrintNode-style call: the API key is the HTTP Basic username. Answered as (status, JSON),
    /// an empty body reading as null.
    pub fn pn(&self, method: &str, target: &str, key: &str, body: Option<Value>) -> (u16, Value) {
        let mut c = self.call(method, target).basic(key);
        if let Some(b) = body {
            c = c.json(&b);
        }
        let r = c.send();
        let v = if r.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&r.body).unwrap()
        };
        (r.status, v)
    }

    /// The agent's own calls (Bearer with the agent key).
    pub fn agent(&self, method: &str, target: &str, key: &str, body: Option<Value>) -> Reply {
        let mut c = self.call(method, target).bearer(key);
        if let Some(b) = body {
            c = c.json(&b);
        }
        c.send()
    }

    /// Root creates the org's first user, that user logs in. Returns the session token.
    pub fn account(&self, org_id: i64, email: &str) -> String {
        let (code, _) = self.post(
            &format!("/orgs/{org_id}/users"),
            Some("t"),
            json!({"email": email, "password": PW}),
        );
        assert_eq!(code, 200);
        let (code, body) = self.login(email, PW);
        assert_eq!(code, 200);
        body["token"].as_str().unwrap().to_string()
    }

    pub fn default_account(&self) -> String {
        self.account(store::DEFAULT_ORG, "ops@shop.test")
    }

    pub fn login(&self, email: &str, password: &str) -> (u16, Value) {
        self.post(
            "/login",
            None,
            json!({"email": email, "password": password}),
        )
    }
}

pub struct Call<'a> {
    app: &'a App,
    req: Req,
}

impl Call<'_> {
    pub fn header(mut self, name: &'static str, value: &str) -> Self {
        self.req.headers.insert(
            name,
            http::HeaderValue::from_bytes(value.as_bytes()).unwrap(),
        );
        self
    }

    pub fn bearer(self, token: &str) -> Self {
        self.header("authorization", &format!("Bearer {token}"))
    }

    pub fn basic(self, key: &str) -> Self {
        self.header(
            "authorization",
            &format!("Basic {}", B64.encode(format!("{key}:"))),
        )
    }

    pub fn json(mut self, v: &Value) -> Self {
        self.req.body = Bytes::from(serde_json::to_vec(v).unwrap());
        self
    }

    pub fn raw(mut self, body: &[u8]) -> Self {
        self.req.body = Bytes::copy_from_slice(body);
        self
    }

    pub fn body_error(mut self) -> Self {
        self.req.body_error = true;
        self
    }

    pub fn peer(mut self, ip: &str) -> Self {
        self.req.peer_ip = ip.into();
        self
    }

    /// The raw socket peer (what the operator gate checks), apart from the resolved `peer_ip`.
    pub fn socket(mut self, ip: &str) -> Self {
        self.req.socket_peer = ip.into();
        self
    }

    pub fn send(self) -> Reply {
        let Resp {
            status,
            headers,
            body,
        } = handle(&self.app.state, &self.req);
        let body = match body {
            RespBody::Empty => Vec::new(),
            RespBody::Bytes(b) => b.to_vec(),
            RespBody::Repeat(b, n) => b.repeat(n),
        };
        Reply {
            status,
            headers,
            body,
        }
    }
}

// --- store shortcuts --------------------------------------------------------------------------

pub fn decl(name: &str, can_pdf: bool) -> PrinterDecl {
    PrinterDecl {
        name: name.into(),
        can_pdf,
        capabilities: None,
    }
}

pub struct Registered {
    pub computer_id: i64,
    pub printer_ids: std::collections::HashMap<String, i64>,
}

impl Registered {
    pub fn pid(&self, name: &str) -> i64 {
        self.printer_ids[name]
    }
}

pub fn register(
    db: &Db,
    name: &str,
    key: &str,
    printers: &[PrinterDecl],
    org_id: i64,
) -> Registered {
    let reg = db
        .with(|c| store::register_agent(c, name, key, printers, org_id))
        .unwrap();
    Registered {
        computer_id: reg.computer_id,
        printer_ids: reg
            .printer_ids
            .iter()
            .map(|(k, v)| (k.clone(), v.as_i64().unwrap()))
            .collect(),
    }
}

/// One agent in the default org with printer "Z" (raw-only).
pub fn agent_z(db: &Db) -> Registered {
    register(db, "pc", "ak", &[decl("Z", false)], store::DEFAULT_ORG)
}

/// Enqueue a job straight into the store; `tweak` adjusts the defaults.
pub fn enqueue(
    db: &Db,
    printer_id: i64,
    payload: &[u8],
    tweak: impl FnOnce(&mut NewJob<'_>),
) -> i64 {
    let pid = json!(printer_id);
    let mut job = NewJob {
        printer_id: &pid,
        type_: "raw_base64",
        mode: "raw",
        payload,
        user_id: store::DEFAULT_USER,
        title: None,
        copies: 1,
        callback_url: None,
        options: None,
        org_id: None,
        idempotency_key: None,
        expire_after: None,
    };
    tweak(&mut job);
    db.with(|c| store::enqueue_job(c, &job)).unwrap().job_id
}

pub fn claim(db: &Db, agent_id: i64) -> Option<store::ClaimedJob> {
    db.with(|c| store::claim_job(c, agent_id, None)).unwrap()
}

pub fn finish(db: &Db, job_id: i64, agent_id: i64, ok: bool, error: Option<&str>) {
    assert!(
        db.with(|c| store::finish_job(c, job_id, agent_id, ok, error))
            .unwrap()
    );
}

pub fn job_state(db: &Db, job_id: i64) -> String {
    db.with(|c| store::get_job(c, job_id, None))
        .unwrap()
        .unwrap()
        .state
}

pub fn job_error(db: &Db, job_id: i64) -> Option<String> {
    db.with(|c| store::get_job(c, job_id, None))
        .unwrap()
        .unwrap()
        .error
}

pub fn create_org(db: &Db, name: &str) -> i64 {
    db.with(|c| store::create_org(c, name)).unwrap()
}

pub fn add_key(db: &Db, label: &str, key: &str, org_id: i64) {
    db.with(|c| store::add_api_key(c, label, key, org_id))
        .unwrap();
}

/// A single integer from a SQL query (the tests' `conn.execute(...).fetchone()`).
pub fn sql_i64(db: &Db, sql: &str, arg: i64) -> Option<i64> {
    db.with(|c| c.query_row(sql, [arg], |r| r.get(0)).unwrap())
}

pub fn sql_f64(db: &Db, sql: &str, arg: i64) -> Option<f64> {
    db.with(|c| c.query_row(sql, [arg], |r| r.get(0)).unwrap())
}

pub fn job_body(printer_id: i64) -> Value {
    json!({"printer_id": printer_id, "type": "raw_base64", "content": "QUJD"})
}

/// The job id out of a `POST /jobs` answer.
pub fn job_id(v: &Value) -> i64 {
    v["job_id"].as_i64().unwrap()
}
