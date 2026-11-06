// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! SQLite job store — the port of `app/store.py`.
//!
//! CONTRACT (scaffold): the types and signatures below are what `server`, `printnode` and `http`
//! code against. Implement the bodies; do not change a public signature without updating every
//! caller. The schema must stay byte-compatible with the Python server's: a DB written by either
//! must open in the other (same tables/columns, REAL epoch-second timestamps, sha256-hex key
//! hashes, JSON text in `printers.capabilities` / `jobs.options`). Additive `CREATE INDEX IF NOT
//! EXISTS` is allowed — the Python server ignores indexes it did not create.
//!
//! Multi-tenancy: `org_id: Option<i64>` — `None` is root (no filter), `Some(id)` confines the
//! query to that org, written as `(:org IS NULL OR x.org_id = :org)` exactly like the Python.
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{Map, Value};
use std::sync::{Arc, Mutex};

pub const DEFAULT_ORG: i64 = 1;
pub const DEFAULT_USER: i64 = 1;

/// The one connection, shared. Replaces the Python global `_LOCK`: every call holds the mutex for
/// the duration of one store function, so each function is one atomic unit as before.
/// ponytail: single connection; a read pool for WAL-concurrent reads only if it bottlenecks.
#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

impl Db {
    /// `connect` + `init_db`. `":memory:"` gives a private in-memory DB (tests).
    pub fn open(path: &str) -> Result<Db, StoreError> {
        let conn = connect(path)?;
        init_db(&conn)?;
        Ok(Db(Arc::new(Mutex::new(conn))))
    }

    /// Run `f` with the connection locked. A panic in an earlier holder does not wedge the store:
    /// rusqlite rolls an unfinished transaction back on drop, so the connection is still sound.
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> T) -> T {
        let guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        f(&guard)
    }
}

#[derive(Debug)]
pub enum StoreError {
    /// `unknown printer: {printer_id}` — a foreign printer reads exactly like a missing one.
    UnknownPrinter(String),
    /// `monthly job quota reached ({quota})` — the server answers 402.
    QuotaExceeded(String),
    /// Agent name already bound to a different key — the server answers 401.
    Auth(String),
    /// Bad input caught by the store (the Python `ValueError`) — the server answers 400.
    Invalid(String),
    /// A UNIQUE/NOT NULL violation (the Python `sqlite3.IntegrityError`) — callers map it to 409.
    Conflict,
    Db(rusqlite::Error),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::UnknownPrinter(m)
            | StoreError::QuotaExceeded(m)
            | StoreError::Auth(m)
            | StoreError::Invalid(m) => f.write_str(m),
            StoreError::Conflict => f.write_str("constraint violation"),
            StoreError::Db(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        match &e {
            rusqlite::Error::SqliteFailure(f, _)
                if f.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                StoreError::Conflict
            }
            _ => StoreError::Db(e),
        }
    }
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

// --- row types (field order = the Python dict's key order, which is the JSON key order) --------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Org {
    pub id: i64,
    pub name: String,
    pub event_url: Option<String>,
    pub shopify_secret: Option<String>,
    pub job_quota: Option<i64>,
    pub plan: Option<String>,
    pub created_at: f64,
}

/// `list_orgs` row: the Shopify secret is reported as a flag, never echoed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrgSummary {
    pub id: i64,
    pub name: String,
    pub event_url: Option<String>,
    pub shopify_secret_set: bool,
    pub job_quota: Option<i64>,
    pub plan: Option<String>,
    pub created_at: f64,
}

/// One printer as an agent declares it in `POST /agent/register` (already validated).
#[derive(Debug, Clone, PartialEq)]
pub struct PrinterDecl {
    pub name: String,
    /// Python truthiness of the declared `can_pdf`.
    pub can_pdf: bool,
    /// Stored as JSON text when truthy, NULL otherwise.
    pub capabilities: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Registration {
    pub computer_id: i64,
    /// printer name -> id, in declaration order.
    pub printer_ids: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CloudprntDevice {
    pub agent_id: i64,
    pub printer_id: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ClientKey {
    pub id: i64,
    pub org_id: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiKeyInfo {
    pub id: i64,
    pub org_id: i64,
    pub label: String,
    /// The raw INTEGER column (1/0), as the Python `dict(row)` reported it — not a JSON bool.
    pub active: i64,
    pub created_at: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserAuth {
    pub id: i64,
    pub org_id: i64,
    pub email: Option<String>,
    pub password_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UserInfo {
    pub id: i64,
    pub org_id: i64,
    pub email: Option<String>,
    pub created_at: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Signup {
    pub org_id: i64,
    pub user_id: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteUser {
    Deleted,
    NotFound,
    LastUser,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Session {
    pub user_id: i64,
    pub org_id: i64,
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrinterRef {
    pub id: i64,
    pub name: String,
    pub org_id: i64,
    pub agent_id: i64,
    pub can_pdf: bool,
}

/// Everything `enqueue_job` takes besides the connection (the Python kwargs).
#[derive(Debug, Clone)]
pub struct NewJob<'a> {
    /// The raw `printer_id` JSON value. Resolved with `pyfmt::as_sql_int`; the error text uses
    /// `pyfmt::py_str` of it (`unknown printer: None`, `unknown printer: abc`, ...).
    pub printer_id: &'a Value,
    pub type_: &'a str,
    pub mode: &'a str,
    pub payload: &'a [u8],
    pub user_id: i64,
    pub title: Option<String>,
    pub copies: i64,
    pub callback_url: Option<String>,
    pub options: Option<Map<String, Value>>,
    pub org_id: Option<i64>,
    pub idempotency_key: Option<String>,
    pub expire_after: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enqueued {
    pub job_id: i64,
    /// The agent the job is queued for — the server wakes its long-poll.
    pub agent_id: i64,
    /// False when an idempotent resubmit returned the existing job (nothing new to wake for).
    pub created: bool,
}

/// `claim_job` result — also the exact JSON body of `GET /agent/jobs`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClaimedJob {
    pub job_id: i64,
    pub printer_id: i64,
    pub mode: String,
    pub copies: i64,
    pub options: Option<Value>,
}

/// `claimed_job` result: the job an agent already holds.
#[derive(Debug, Clone, PartialEq)]
pub struct InFlight {
    pub job_id: i64,
    pub mode: String,
    pub copies: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    Cancelled,
    NotCancellable,
    NotFound,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingHook {
    pub job_id: i64,
    pub callback_url: String,
    pub state: String,
    pub error: Option<String>,
    pub title: Option<String>,
    pub printer_id: i64,
}

/// `get_job` row — the exact body of `GET /jobs/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobStatus {
    pub state: String,
    pub error: Option<String>,
    pub printer_id: i64,
    pub created_at: f64,
    pub finished_at: Option<f64>,
}

/// `recent_jobs` row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobRow {
    pub id: i64,
    pub printer_id: i64,
    pub printer_name: String,
    pub agent_name: String,
    pub title: Option<String>,
    pub state: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub mode: String,
    pub error: Option<String>,
    pub created_at: f64,
    pub finished_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Metrics {
    /// state -> count, only the states that occur (the Prometheus renderer fills in zeros).
    pub jobs: std::collections::BTreeMap<String, i64>,
    pub agents_total: i64,
    pub agents_online: i64,
    pub printers_total: i64,
}

/// `list_agents` row (PrintNode calls agents computers).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentInfo {
    pub id: i64,
    pub name: String,
    pub last_seen_at: Option<f64>,
    pub created_at: f64,
    pub printers: i64,
    pub online: bool,
}

/// `claim_agent_transitions` row: one liveness edge to report.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentEvent {
    pub agent_id: i64,
    pub name: String,
    pub org_id: i64,
    pub last_seen_at: Option<f64>,
    pub event_url: String,
    /// "offline" | "online"
    pub event: String,
}

/// `list_printers` row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrinterInfo {
    pub id: i64,
    pub name: String,
    pub agent_id: i64,
    pub agent_name: String,
    pub can_pdf: bool,
    pub created_at: f64,
    pub capabilities: Option<Value>,
    pub online: bool,
}

// --- functions (port of app/store.py; each one is one transaction) ----------------------------

use crate::pyfmt;
use rusqlite::{
    OptionalExtension, Row, Transaction, TransactionBehavior, named_params, params,
    params_from_iter,
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS orgs(
  id INTEGER PRIMARY KEY, name TEXT NOT NULL, event_url TEXT, shopify_secret TEXT,
  job_quota INTEGER, plan TEXT, created_at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS users(
  id INTEGER PRIMARY KEY, org_id INTEGER NOT NULL, name TEXT NOT NULL, created_at REAL NOT NULL,
  email TEXT, password_hash TEXT);
-- users_email (UNIQUE) is created with the migrations below, not here: on a pre-accounts DB the
-- column does not exist yet when this script runs. NULLs are distinct in SQLite, so the seeded
-- legacy user (no e-mail) is unconstrained.
CREATE TABLE IF NOT EXISTS sessions(
  id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL,
  token_hash TEXT NOT NULL UNIQUE, created_at REAL NOT NULL, expires_at REAL NOT NULL);
-- One live reset per user (a new request supersedes the old one), stored hashed like every other
-- credential and deleted the moment it is spent.
CREATE TABLE IF NOT EXISTS password_resets(
  id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL,
  token_hash TEXT NOT NULL UNIQUE, created_at REAL NOT NULL, expires_at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS agents(
  id INTEGER PRIMARY KEY AUTOINCREMENT, org_id INTEGER NOT NULL, name TEXT NOT NULL,
  api_key_hash TEXT NOT NULL UNIQUE, last_seen_at REAL,
  offline_notified INTEGER NOT NULL DEFAULT 0, created_at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS printers(
  id INTEGER PRIMARY KEY AUTOINCREMENT, org_id INTEGER NOT NULL, agent_id INTEGER NOT NULL,
  name TEXT NOT NULL, can_pdf INTEGER NOT NULL DEFAULT 0, state TEXT NOT NULL DEFAULT 'active',
  capabilities TEXT, created_at REAL NOT NULL, UNIQUE(agent_id, name));
-- Label templates (render settings for label_* jobs), one name per org. Additive: the Python
-- server does not know the table and ignores it. `spec` is the contract JSON of `label::spec_json`.
CREATE TABLE IF NOT EXISTS label_templates(
  id INTEGER PRIMARY KEY, org_id INTEGER NOT NULL, name TEXT NOT NULL, spec TEXT NOT NULL,
  created_at REAL NOT NULL, updated_at REAL NOT NULL, UNIQUE(org_id, name));
CREATE TABLE IF NOT EXISTS jobs(
  id INTEGER PRIMARY KEY AUTOINCREMENT, org_id INTEGER NOT NULL, user_id INTEGER NOT NULL,
  printer_id INTEGER NOT NULL, agent_id INTEGER NOT NULL, type TEXT NOT NULL, mode TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'queued', payload BLOB NOT NULL, error TEXT, title TEXT,
  copies INTEGER NOT NULL DEFAULT 1, options TEXT,
  callback_url TEXT, hook_attempts INTEGER NOT NULL DEFAULT 0, hook_delivered_at REAL,
  idempotency_key TEXT, expires_at REAL,
  retries INTEGER NOT NULL DEFAULT 0, created_at REAL NOT NULL, claimed_at REAL, finished_at REAL);
-- NULLs are distinct in SQLite, so jobs without a key are unconstrained.
CREATE UNIQUE INDEX IF NOT EXISTS jobs_idem ON jobs(org_id, idempotency_key);
CREATE TABLE IF NOT EXISTS api_keys(
  id INTEGER PRIMARY KEY AUTOINCREMENT, org_id INTEGER NOT NULL, label TEXT NOT NULL,
  key_hash TEXT NOT NULL UNIQUE, active INTEGER NOT NULL DEFAULT 1, created_at REAL NOT NULL);
";

const MIGRATIONS: &[&str] = &[
    "ALTER TABLE printers ADD COLUMN capabilities TEXT",
    "ALTER TABLE jobs ADD COLUMN title TEXT",
    "ALTER TABLE jobs ADD COLUMN copies INTEGER NOT NULL DEFAULT 1",
    "ALTER TABLE jobs ADD COLUMN options TEXT",
    "ALTER TABLE jobs ADD COLUMN callback_url TEXT",
    "ALTER TABLE jobs ADD COLUMN hook_attempts INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE jobs ADD COLUMN hook_delivered_at REAL",
    "ALTER TABLE jobs ADD COLUMN idempotency_key TEXT",
    "ALTER TABLE jobs ADD COLUMN expires_at REAL",
    "CREATE UNIQUE INDEX IF NOT EXISTS jobs_idem ON jobs(org_id, idempotency_key)",
    "ALTER TABLE orgs ADD COLUMN event_url TEXT",
    "ALTER TABLE orgs ADD COLUMN shopify_secret TEXT",
    "ALTER TABLE agents ADD COLUMN offline_notified INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE users ADD COLUMN email TEXT",
    "ALTER TABLE users ADD COLUMN password_hash TEXT",
    "CREATE UNIQUE INDEX IF NOT EXISTS users_email ON users(email)",
    "ALTER TABLE orgs ADD COLUMN job_quota INTEGER",
    "ALTER TABLE orgs ADD COLUMN plan TEXT",
];

/// Performance indexes the Python server never had. Additive and harmless to it. The columns
/// they name all exist once the migrations ran, so these are not best-effort like the migrations.
/// (`printers(agent_id, name)` is already covered by the UNIQUE autoindex, so it is not repeated.)
const INDEXES: &str = "
CREATE INDEX IF NOT EXISTS jobs_claim ON jobs(agent_id, state, created_at, id);
CREATE INDEX IF NOT EXISTS jobs_org_created ON jobs(org_id, created_at);
CREATE INDEX IF NOT EXISTS jobs_state_claimed ON jobs(state, claimed_at);
CREATE INDEX IF NOT EXISTS jobs_hooks ON jobs(id)
  WHERE callback_url IS NOT NULL AND hook_delivered_at IS NULL;
CREATE INDEX IF NOT EXISTS agents_org ON agents(org_id);
";

/// The oldest queued job of one agent that has not passed its deadline (`?1` agent, `?2` now).
/// Past its deadline the job is skipped here and failed by the reaper's `expire_jobs` — the skip
/// is what guarantees it never prints, whatever the reaper's tick is doing.
/// ponytail: served by `jobs_claim` (agent_id, state, created_at, id), so the ORDER BY needs no sort.
const CLAIM_SQL: &str = "SELECT id, printer_id, mode, copies, options FROM jobs \
     WHERE agent_id=?1 AND state='queued' AND (expires_at IS NULL OR expires_at > ?2) \
     ORDER BY created_at, id LIMIT 1";

/// Jobs one org submitted since a point in time (`?1` org, `?2` since).
/// ponytail: a COUNT over `jobs_org_created` per submit (covering index, range scan); a counter
/// column only if a single org's month ever holds millions of rows.
const USAGE_SQL: &str = "SELECT COUNT(*) FROM jobs WHERE org_id=?1 AND created_at >= ?2";

/// Terminal jobs with an undelivered callback under the attempt cap (`?1`), oldest first.
/// ponytail: `INDEXED BY` because without ANALYZE statistics the planner cannot tell that the
/// partial index `jobs_hooks` holds only the handful of undelivered rows and prefers a state scan.
const HOOKS_SQL: &str = "SELECT id, callback_url, state, error, title, printer_id FROM jobs INDEXED BY jobs_hooks WHERE callback_url IS NOT NULL AND hook_delivered_at IS NULL AND hook_attempts < ?1 AND state IN ('done','failed','cancelled') ORDER BY id";

/// Open the DB: WAL journal, 5 s busy timeout (the Python sqlite3 default).
///
/// `synchronous` stays at its default (FULL) on purpose: with WAL, NORMAL could lose the last
/// committed transactions on power loss, and a lost "done" means the label prints twice.
/// Foreign keys stay off, as in the Python server (the schema declares none).
pub fn connect(path: &str) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    conn.pragma_update(None, "foreign_keys", false)?;
    // The pragma answers with the resulting mode ("memory" for :memory:), so it is a query.
    conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))?;
    // FULL (the SQLite default the Python server ran with), stated so `touch_last_seen` has a
    // known value to return to: a job state that is not on disk after a power cut is a lost or a
    // reprinted label.
    conn.pragma_update(None, "synchronous", "FULL")?;
    Ok(conn)
}

/// An agent's liveness heartbeat, written without waiting for the disk: in WAL mode a
/// `synchronous=NORMAL` commit is on disk at the next FULL commit or checkpoint, and a power cut
/// before that loses nothing but a "last seen" a few seconds stale. Every idle long-poll ends in
/// one of these, so this keeps a fleet of waiting agents from costing an fsync each.
fn touch_last_seen(conn: &Connection, agent_id: i64, now: f64) -> Result<()> {
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    let touched = conn
        .prepare_cached("UPDATE agents SET last_seen_at=? WHERE id=?")
        .and_then(|mut s| s.execute(params![now, agent_id]));
    conn.pragma_update(None, "synchronous", "FULL")?;
    touched?;
    Ok(())
}

/// A write transaction: `BEGIN IMMEDIATE` takes the write lock up front, so a read-then-write
/// function never fails half-way on a lock upgrade. Dropping it without `commit` rolls back.
fn begin(conn: &Connection) -> Result<Transaction<'_>> {
    Ok(Transaction::new_unchecked(
        conn,
        TransactionBehavior::Immediate,
    )?)
}

/// Create the schema, run the idempotent migrations, seed DEFAULT_ORG/DEFAULT_USER.
pub fn init_db(conn: &Connection) -> Result<()> {
    let now = crate::now();
    conn.execute_batch(SCHEMA)?;
    for ddl in MIGRATIONS {
        match conn.execute(ddl, []) {
            // Python swallowed only OperationalError: column/index already there (fresh DB or
            // prior run) is idempotent, but a UNIQUE violation (users_email over duplicate
            // e-mails) or a corrupt file must stop startup, not run without the guarantee.
            Err(rusqlite::Error::SqliteFailure(f, _))
                if !matches!(
                    f.code,
                    rusqlite::ErrorCode::ConstraintViolation
                        | rusqlite::ErrorCode::DatabaseCorrupt
                        | rusqlite::ErrorCode::NotADatabase
                ) => {}
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
    }
    conn.execute_batch(INDEXES)?;
    let tx = begin(conn)?;
    let has = |sql: &str, id: i64| -> Result<bool> {
        Ok(tx.query_row(sql, [id], |_| Ok(())).optional()?.is_some())
    };
    if !has("SELECT 1 FROM orgs WHERE id=?", DEFAULT_ORG)? {
        tx.execute(
            "INSERT INTO orgs(id, name, created_at) VALUES(?,?,?)",
            params![DEFAULT_ORG, "default", now],
        )?;
    }
    if !has("SELECT 1 FROM users WHERE id=?", DEFAULT_USER)? {
        tx.execute(
            "INSERT INTO users(id, org_id, name, created_at) VALUES(?,?,?,?)",
            params![DEFAULT_USER, DEFAULT_ORG, "default", now],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// sha256 hex of a credential — how every key, session and reset token is stored.
pub fn hash_key(key: &str) -> String {
    hex(&Sha256::digest(key.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
    out
}

/// Start of the current UTC calendar month containing `now` (epoch seconds) — the quota window.
/// Calendar months (not a rolling 30 days) so the budget resets on the same date a subscription
/// bills.
pub fn month_start(now: f64) -> f64 {
    use chrono::{DateTime, Datelike};
    // A non-finite or absurd `now` cannot come from the clock; 0 keeps the guard total.
    // Python's fromtimestamp rounds to the microsecond (half-even) before taking the date.
    DateTime::from_timestamp(((now * 1e6).round_ties_even() / 1e6).floor() as i64, 0)
        .and_then(|d| d.date_naive().with_day(1))
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map_or(0.0, |d| d.and_utc().timestamp() as f64)
}

// Multi-tenancy: every org-owning query carries an `org_id`, and `None` means *root* — no filter,
// all orgs. Written as `(:org IS NULL OR x.org_id = :org)` so one statement serves both without
// string-building. Legacy DBs are entirely org_id=1, so org-1 callers see exactly what they saw
// before.

pub fn create_org(conn: &Connection, name: &str) -> Result<i64> {
    let tx = begin(conn)?;
    tx.execute(
        "INSERT INTO orgs(name, created_at) VALUES(?,?)",
        params![name, crate::now()],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(id)
}

/// Orgs for the root listing. The Shopify webhook secret is reported as a flag, never echoed —
/// it is the one credential here that has to be stored in plaintext (HMAC needs it).
pub fn list_orgs(conn: &Connection) -> Result<Vec<OrgSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, event_url, shopify_secret, job_quota, plan, created_at \
         FROM orgs ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        let secret: Option<String> = r.get(3)?;
        Ok(OrgSummary {
            id: r.get(0)?,
            name: r.get(1)?,
            event_url: r.get(2)?,
            shopify_secret_set: secret.is_some_and(|s| !s.is_empty()),
            job_quota: r.get(4)?,
            plan: r.get(5)?,
            created_at: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Full org row including secrets — for server-side use (HMAC verification), not for output.
pub fn get_org(conn: &Connection, org_id: i64) -> Result<Option<Org>> {
    Ok(conn
        .query_row(
            "SELECT id, name, event_url, shopify_secret, job_quota, plan, created_at \
             FROM orgs WHERE id=?",
            [org_id],
            |r| {
                Ok(Org {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    event_url: r.get(2)?,
                    shopify_secret: r.get(3)?,
                    job_quota: r.get(4)?,
                    plan: r.get(5)?,
                    created_at: r.get(6)?,
                })
            },
        )
        .optional()?)
}

fn set_org_field(
    conn: &Connection,
    org_id: i64,
    sql: &str,
    value: &dyn rusqlite::ToSql,
) -> Result<bool> {
    let tx = begin(conn)?;
    let n = tx.execute(sql, params![value, org_id])?;
    tx.commit()?;
    Ok(n == 1)
}

/// Where this org's agent liveness events go (None clears it). False if no such org.
pub fn set_org_event_url(conn: &Connection, org_id: i64, url: Option<&str>) -> Result<bool> {
    set_org_field(conn, org_id, "UPDATE orgs SET event_url=? WHERE id=?", &url)
}

/// The org's Shopify webhook signing secret (None clears it). False if no such org.
pub fn set_org_shopify_secret(
    conn: &Connection,
    org_id: i64,
    secret: Option<&str>,
) -> Result<bool> {
    set_org_field(
        conn,
        org_id,
        "UPDATE orgs SET shopify_secret=? WHERE id=?",
        &secret,
    )
}

/// Jobs this org may submit per calendar month (None = unlimited). False if no such org.
pub fn set_org_quota(conn: &Connection, org_id: i64, quota: Option<i64>) -> Result<bool> {
    set_org_field(
        conn,
        org_id,
        "UPDATE orgs SET job_quota=? WHERE id=?",
        &quota,
    )
}

/// Move an org onto a billing plan. The plan id is bookkeeping; its quota is what bites, and
/// both are written in one UPDATE so they can never disagree. False if no such org.
pub fn set_org_plan(
    conn: &Connection,
    org_id: i64,
    plan: &str,
    quota: Option<i64>,
) -> Result<bool> {
    let tx = begin(conn)?;
    let n = tx.execute(
        "UPDATE orgs SET plan=?, job_quota=? WHERE id=?",
        params![plan, quota, org_id],
    )?;
    tx.commit()?;
    Ok(n == 1)
}

/// Remove an org and everything that belongs to it — jobs, printers, agents, keys, accounts and
/// their sessions. False if there is no such org.
///
/// No soft delete: a tenant that cancelled and asked to be forgotten is what this is for. The
/// caller decides who may (root only) and protects DEFAULT_ORG — an agent presenting an unknown
/// key still enrolls there.
pub fn delete_org(conn: &Connection, org_id: i64) -> Result<bool> {
    let tx = begin(conn)?;
    if tx
        .query_row("SELECT 1 FROM orgs WHERE id=?", [org_id], |_| Ok(()))
        .optional()?
        .is_none()
    {
        return Ok(false);
    }
    let users = "SELECT id FROM users WHERE org_id=?";
    tx.execute(
        &format!("DELETE FROM sessions WHERE user_id IN ({users})"),
        [org_id],
    )?;
    tx.execute(
        &format!("DELETE FROM password_resets WHERE user_id IN ({users})"),
        [org_id],
    )?;
    for table in [
        "jobs",
        "printers",
        "agents",
        "api_keys",
        "users",
        "label_templates",
    ] {
        tx.execute(&format!("DELETE FROM {table} WHERE org_id=?"), [org_id])?;
    }
    tx.execute("DELETE FROM orgs WHERE id=?", [org_id])?;
    tx.commit()?;
    Ok(true)
}

fn usage(conn: &Connection, org_id: i64, since: f64) -> Result<i64> {
    Ok(conn
        .prepare_cached(USAGE_SQL)?
        .query_row(params![org_id, since], |r| r.get(0))?)
}

/// Jobs this org has submitted in the quota window (this calendar month by default).
pub fn org_usage(conn: &Connection, org_id: i64, since: Option<f64>) -> Result<i64> {
    usage(
        conn,
        org_id,
        since.unwrap_or_else(|| month_start(crate::now())),
    )
}

pub fn org_exists(conn: &Connection, org_id: i64) -> Result<bool> {
    Ok(conn
        .query_row("SELECT 1 FROM orgs WHERE id=?", [org_id], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Err(Auth) when the name is bound to another key; Err(Conflict) when the key is bound to
/// another agent (UNIQUE api_key_hash).
pub fn register_agent(
    conn: &Connection,
    name: &str,
    api_key: &str,
    printers: &[PrinterDecl],
    org_id: i64,
) -> Result<Registration> {
    let now = crate::now();
    let key_hash = hash_key(api_key);
    let tx = begin(conn)?;
    let existing: Option<(i64, String)> = tx
        .query_row(
            "SELECT id, api_key_hash FROM agents WHERE org_id=? AND name=?",
            params![org_id, name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let agent_id = match existing {
        Some((id, stored)) => {
            if !bool::from(stored.as_bytes().ct_eq(key_hash.as_bytes())) {
                return Err(StoreError::Auth(format!(
                    "agent name already registered with a different key: {}",
                    pyfmt::repr_str(name)
                )));
            }
            tx.execute(
                "UPDATE agents SET last_seen_at=? WHERE id=?",
                params![now, id],
            )?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO agents(org_id, name, api_key_hash, last_seen_at, created_at) \
                 VALUES(?,?,?,?,?)",
                params![org_id, name, key_hash, now, now],
            )?;
            tx.last_insert_rowid()
        }
    };
    let mut printer_ids = Map::new();
    for p in printers {
        let caps: Option<String> = match &p.capabilities {
            Some(c) if pyfmt::truthy(c) => Some(pyfmt::dumps(c)),
            _ => None,
        };
        let found: Option<i64> = tx
            .query_row(
                "SELECT id FROM printers WHERE agent_id=? AND name=?",
                params![agent_id, p.name],
                |r| r.get(0),
            )
            .optional()?;
        let pid = match found {
            Some(pid) => {
                tx.execute(
                    "UPDATE printers SET can_pdf=?, capabilities=? WHERE id=?",
                    params![p.can_pdf, caps, pid],
                )?;
                pid
            }
            None => {
                tx.execute(
                    "INSERT INTO printers(org_id, agent_id, name, can_pdf, capabilities, \
                     created_at) VALUES(?,?,?,?,?,?)",
                    params![org_id, agent_id, p.name, p.can_pdf, caps, now],
                )?;
                tx.last_insert_rowid()
            }
        };
        printer_ids.insert(p.name.clone(), Value::from(pid));
    }
    tx.commit()?;
    Ok(Registration {
        computer_id: agent_id,
        printer_ids,
    })
}

/// Enrol (or re-touch) a printer that polls us itself with the Star CloudPRNT protocol.
///
/// Such a printer is its own agent: one pseudo-agent row carrying exactly one raw-only printer
/// (gotcha #1 — the device has no renderer), so its jobs take the ordinary queue, quota, history
/// and dashboard path. `api_key_hash` is a random opaque value with no preimage: the device proves
/// itself with its org's client key on every request, and nothing can hand this row's "key" to the
/// agent endpoints. A poll is what keeps it online.
pub fn register_cloudprnt(conn: &Connection, org_id: i64, name: &str) -> Result<CloudprntDevice> {
    let now = crate::now();
    let tx = begin(conn)?;
    let row: Option<i64> = tx
        .query_row(
            "SELECT id FROM agents WHERE org_id=? AND name=?",
            params![org_id, name],
            |r| r.get(0),
        )
        .optional()?;
    let agent_id = match row {
        Some(id) => {
            tx.execute(
                "UPDATE agents SET last_seen_at=? WHERE id=?",
                params![now, id],
            )?;
            id
        }
        None => {
            let mut raw = [0u8; 32];
            getrandom::fill(&mut raw).map_err(|e| {
                StoreError::Invalid(format!("no system randomness for the device key: {e}"))
            })?;
            tx.execute(
                "INSERT INTO agents(org_id, name, api_key_hash, last_seen_at, created_at) \
                 VALUES(?,?,?,?,?)",
                params![org_id, name, format!("cloudprnt:{}", hex(&raw)), now, now],
            )?;
            tx.last_insert_rowid()
        }
    };
    let found: Option<i64> = tx
        .query_row(
            "SELECT id FROM printers WHERE agent_id=? AND name=?",
            params![agent_id, name],
            |r| r.get(0),
        )
        .optional()?;
    let printer_id = match found {
        Some(id) => id,
        None => {
            tx.execute(
                "INSERT INTO printers(org_id, agent_id, name, can_pdf, created_at) \
                 VALUES(?,?,?,0,?)",
                params![org_id, agent_id, name, now],
            )?;
            tx.last_insert_rowid()
        }
    };
    tx.commit()?;
    Ok(CloudprntDevice {
        agent_id,
        printer_id,
    })
}

// api_key is a high-entropy token; the sha256 lookup has no practical timing oracle, so an
// indexed equality match is fine for the authenticate_* functions.

/// The agent holding `api_key` — unless that key is an issued client key that has since been
/// revoked: revoking a key cuts off every use of it, the agent it enrolled included. (An agent
/// key that was never issued, from before issued keys were required, has no row to revoke.)
pub fn authenticate_agent(conn: &Connection, api_key: &str) -> Result<Option<i64>> {
    Ok(conn
        .prepare_cached(
            "SELECT a.id FROM agents a LEFT JOIN api_keys k ON k.key_hash = a.api_key_hash              WHERE a.api_key_hash=? AND (k.id IS NULL OR k.active=1)",
        )?
        .query_row([hash_key(api_key)], |r| r.get(0))
        .optional()?)
}

/// True if `key` is an issued client key that was revoked.
pub fn key_revoked(conn: &Connection, key: &str) -> Result<bool> {
    if key.is_empty() {
        return Ok(false);
    }
    Ok(conn
        .prepare_cached("SELECT 1 FROM api_keys WHERE key_hash=? AND active=0")?
        .query_row([hash_key(key)], |_| Ok(()))
        .optional()?
        .is_some())
}

pub fn add_api_key(conn: &Connection, label: &str, key: &str, org_id: i64) -> Result<i64> {
    let tx = begin(conn)?;
    tx.execute(
        "INSERT INTO api_keys(org_id, label, key_hash, active, created_at) VALUES(?,?,?,1,?)",
        params![org_id, label, hash_key(key), crate::now()],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(id)
}

/// Resolve a client key to its id and org — the org every request with it is confined to.
/// `None` for an empty key without touching the DB.
pub fn authenticate_client(conn: &Connection, key: &str) -> Result<Option<ClientKey>> {
    if key.is_empty() {
        return Ok(None);
    }
    Ok(conn
        .prepare_cached("SELECT id, org_id FROM api_keys WHERE key_hash=? AND active=1")?
        .query_row([hash_key(key)], |r| {
            Ok(ClientKey {
                id: r.get(0)?,
                org_id: r.get(1)?,
            })
        })
        .optional()?)
}

pub fn list_api_keys(conn: &Connection, org_id: Option<i64>) -> Result<Vec<ApiKeyInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, org_id, label, active, created_at FROM api_keys \
         WHERE (:org IS NULL OR org_id = :org) ORDER BY id",
    )?;
    let rows = stmt.query_map(named_params! {":org": org_id}, |r| {
        Ok(ApiKeyInfo {
            id: r.get(0)?,
            org_id: r.get(1)?,
            label: r.get(2)?,
            active: r.get(3)?,
            created_at: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Org-filtered like every other id lookup: another org's key is simply not found.
pub fn revoke_api_key(conn: &Connection, key_id: i64, org_id: Option<i64>) -> Result<bool> {
    let tx = begin(conn)?;
    let n = tx.execute(
        "UPDATE api_keys SET active=0 WHERE id=:kid AND (:org IS NULL OR org_id = :org)",
        named_params! {":kid": key_id, ":org": org_id},
    )?;
    tx.commit()?;
    Ok(n == 1)
}

// --- users + browser sessions -----------------------------------------------------------------
// A user belongs to exactly one org and logs in with e-mail + password; a session token is that
// login carried in the same `Authorization: Bearer` header the API keys use. Sessions are stored
// hashed (like every other credential here) and expire — logout deletes the row, the reaper
// purges the rest. Machine keys stay in api_keys: what may manage an org is decided by *which*
// table the presented credential resolves in, not by a role column.

/// Python `str.strip().lower()`: its whitespace set also holds U+001C..U+001F.
pub fn normalize_email(value: &str) -> String {
    value
        .trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        .to_lowercase()
}

/// Err(Conflict) if the e-mail is taken (globally — login is by e-mail alone). The e-mail is
/// trimmed + lowercased here too.
pub fn create_user(
    conn: &Connection,
    org_id: i64,
    email: &str,
    password_hash: &str,
) -> Result<i64> {
    let addr = normalize_email(email);
    let tx = begin(conn)?;
    tx.execute(
        "INSERT INTO users(org_id, name, email, password_hash, created_at) VALUES(?,?,?,?,?)",
        params![org_id, addr, addr, password_hash, crate::now()],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(id)
}

/// Self-serve: a new org and its first user in one transaction. Err(Conflict) if the e-mail is
/// taken — the rollback takes the half-made org with it, so a failed signup leaves nothing behind.
pub fn signup(
    conn: &Connection,
    org_name: &str,
    email: &str,
    password_hash: &str,
) -> Result<Signup> {
    let now = crate::now();
    let addr = normalize_email(email);
    let tx = begin(conn)?;
    tx.execute(
        "INSERT INTO orgs(name, created_at) VALUES(?,?)",
        params![org_name, now],
    )?;
    let org_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO users(org_id, name, email, password_hash, created_at) VALUES(?,?,?,?,?)",
        params![org_id, addr, addr, password_hash, now],
    )?;
    let user_id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(Signup { org_id, user_id })
}

/// Remove an account and everything that could still authenticate as it.
///
/// An org always keeps at least one account — deleting the last one would leave printers and keys
/// with no way back in. Jobs keep the raw user_id: the history is not joined against users, so a
/// removed account leaves its past prints readable.
pub fn delete_user(conn: &Connection, user_id: i64, org_id: Option<i64>) -> Result<DeleteUser> {
    let tx = begin(conn)?;
    let owner: Option<i64> = tx
        .query_row(
            "SELECT org_id FROM users WHERE id=:uid AND email IS NOT NULL \
             AND (:org IS NULL OR org_id = :org)",
            named_params! {":uid": user_id, ":org": org_id},
            |r| r.get(0),
        )
        .optional()?;
    let Some(owner) = owner else {
        return Ok(DeleteUser::NotFound);
    };
    let accounts: i64 = tx.query_row(
        "SELECT COUNT(*) FROM users WHERE org_id=? AND email IS NOT NULL",
        [owner],
        |r| r.get(0),
    )?;
    if accounts <= 1 {
        return Ok(DeleteUser::LastUser);
    }
    tx.execute("DELETE FROM sessions WHERE user_id=?", [user_id])?;
    tx.execute("DELETE FROM password_resets WHERE user_id=?", [user_id])?;
    tx.execute("DELETE FROM users WHERE id=?", [user_id])?;
    tx.commit()?;
    Ok(DeleteUser::Deleted)
}

/// Mint a one-shot reset token, dropping any earlier one for that user — a second "forgot my
/// password" click must not leave the first mail's link working. Returns `expires_at`.
pub fn create_password_reset(
    conn: &Connection,
    user_id: i64,
    token: &str,
    ttl_s: f64,
    now: Option<f64>,
) -> Result<f64> {
    let now = now.unwrap_or_else(crate::now);
    let expires_at = now + ttl_s;
    let tx = begin(conn)?;
    tx.execute("DELETE FROM password_resets WHERE user_id=?", [user_id])?;
    tx.execute(
        "INSERT INTO password_resets(user_id, token_hash, created_at, expires_at) VALUES(?,?,?,?)",
        params![user_id, hash_key(token), now, expires_at],
    )?;
    tx.commit()?;
    Ok(expires_at)
}

/// Spend a reset token: set the password, drop the token and log every browser out. Returns the
/// user id, or None if the token is empty, unknown, expired or already used. One transaction, so
/// a token can never be spent twice.
pub fn consume_password_reset(
    conn: &Connection,
    token: Option<&str>,
    password_hash: &str,
    now: Option<f64>,
) -> Result<Option<i64>> {
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    let now = now.unwrap_or_else(crate::now);
    let tx = begin(conn)?;
    let row: Option<(i64, i64)> = tx
        .query_row(
            "SELECT id, user_id FROM password_resets WHERE token_hash=? AND expires_at > ?",
            params![hash_key(token), now],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((reset_id, user_id)) = row else {
        return Ok(None);
    };
    tx.execute(
        "UPDATE users SET password_hash=? WHERE id=?",
        params![password_hash, user_id],
    )?;
    tx.execute("DELETE FROM password_resets WHERE id=?", [reset_id])?;
    tx.execute("DELETE FROM sessions WHERE user_id=?", [user_id])?;
    tx.commit()?;
    Ok(Some(user_id))
}

fn purge(conn: &Connection, sql: &str, now: Option<f64>) -> Result<usize> {
    let tx = begin(conn)?;
    let n = tx.execute(sql, [now.unwrap_or_else(crate::now)])?;
    tx.commit()?;
    Ok(n)
}

pub fn purge_expired_resets(conn: &Connection, now: Option<f64>) -> Result<usize> {
    purge(
        conn,
        "DELETE FROM password_resets WHERE expires_at <= ?",
        now,
    )
}

/// The login lookup — the only place the password hash leaves the DB. `None` for an empty (after
/// trim) address without touching the DB.
pub fn get_user_by_email(conn: &Connection, email: &str) -> Result<Option<UserAuth>> {
    let addr = normalize_email(email);
    if addr.is_empty() {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT id, org_id, email, password_hash FROM users WHERE email=?",
            [addr],
            |r| {
                Ok(UserAuth {
                    id: r.get(0)?,
                    org_id: r.get(1)?,
                    email: r.get(2)?,
                    password_hash: r.get(3)?,
                })
            },
        )
        .optional()?)
}

pub fn list_users(conn: &Connection, org_id: Option<i64>) -> Result<Vec<UserInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, org_id, email, created_at FROM users \
         WHERE email IS NOT NULL AND (:org IS NULL OR org_id = :org) ORDER BY id",
    )?;
    let rows = stmt.query_map(named_params! {":org": org_id}, |r| {
        Ok(UserInfo {
            id: r.get(0)?,
            org_id: r.get(1)?,
            email: r.get(2)?,
            created_at: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Changing the password logs that user's browsers out — a stolen session dies with the password
/// it was minted from. False if there is no such user.
pub fn set_user_password(conn: &Connection, user_id: i64, password_hash: &str) -> Result<bool> {
    let tx = begin(conn)?;
    let n = tx.execute(
        "UPDATE users SET password_hash=? WHERE id=?",
        params![password_hash, user_id],
    )?;
    tx.execute("DELETE FROM sessions WHERE user_id=?", [user_id])?;
    tx.commit()?;
    Ok(n == 1)
}

/// Returns `expires_at`.
pub fn create_session(
    conn: &Connection,
    user_id: i64,
    token: &str,
    ttl_s: f64,
    now: Option<f64>,
) -> Result<f64> {
    let now = now.unwrap_or_else(crate::now);
    let expires_at = now + ttl_s;
    let tx = begin(conn)?;
    tx.execute(
        "INSERT INTO sessions(user_id, token_hash, created_at, expires_at) VALUES(?,?,?,?)",
        params![user_id, hash_key(token), now, expires_at],
    )?;
    tx.commit()?;
    Ok(expires_at)
}

/// Resolve a session token; None if empty, unknown or expired.
pub fn authenticate_session(
    conn: &Connection,
    token: &str,
    now: Option<f64>,
) -> Result<Option<Session>> {
    if token.is_empty() {
        return Ok(None);
    }
    let now = now.unwrap_or_else(crate::now);
    Ok(conn
        .prepare_cached(
            "SELECT s.user_id, u.org_id, u.email FROM sessions s JOIN users u ON u.id = s.user_id \
             WHERE s.token_hash=? AND s.expires_at > ?",
        )?
        .query_row(params![hash_key(token), now], |r| {
            Ok(Session {
                user_id: r.get(0)?,
                org_id: r.get(1)?,
                email: r.get(2)?,
            })
        })
        .optional()?)
}

pub fn delete_session(conn: &Connection, token: &str) -> Result<bool> {
    let tx = begin(conn)?;
    let n = tx.execute("DELETE FROM sessions WHERE token_hash=?", [hash_key(token)])?;
    tx.commit()?;
    Ok(n == 1)
}

pub fn purge_expired_sessions(conn: &Connection, now: Option<f64>) -> Result<usize> {
    purge(conn, "DELETE FROM sessions WHERE expires_at <= ?", now)
}

/// One printer, org-filtered — another org's printer reads as missing, never as a 403.
/// `printer_id` is the raw JSON value (see `NewJob::printer_id`).
pub fn get_printer(
    conn: &Connection,
    printer_id: &Value,
    org_id: Option<i64>,
) -> Result<Option<PrinterRef>> {
    let Some(pid) = pyfmt::as_sql_int(printer_id) else {
        return Ok(None);
    };
    Ok(conn
        .query_row(
            "SELECT id, name, org_id, agent_id, can_pdf FROM printers \
             WHERE id=:pid AND (:org IS NULL OR org_id = :org)",
            named_params! {":pid": pid, ":org": org_id},
            |r| {
                Ok(PrinterRef {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    org_id: r.get(2)?,
                    agent_id: r.get(3)?,
                    can_pdf: r.get::<_, i64>(4)? != 0,
                })
            },
        )
        .optional()?)
}

/// `QuotaExceeded` once `org_id` has used up its monthly quota.
pub fn check_quota(conn: &Connection, org_id: i64, now: f64) -> Result<()> {
    let quota: Option<i64> = conn
        .query_row("SELECT job_quota FROM orgs WHERE id=?", [org_id], |r| {
            r.get(0)
        })
        .optional()?
        .flatten();
    if let Some(quota) = quota
        && usage(conn, org_id, month_start(now))? >= quota
    {
        return Err(StoreError::QuotaExceeded(format!(
            "monthly job quota reached ({quota})"
        )));
    }
    Ok(())
}

pub fn enqueue_job(conn: &Connection, job: &NewJob<'_>) -> Result<Enqueued> {
    let now = crate::now();
    let unknown = || {
        StoreError::UnknownPrinter(format!(
            "unknown printer: {}",
            pyfmt::py_str(job.printer_id)
        ))
    };
    let Some(pid) = pyfmt::as_sql_int(job.printer_id) else {
        return Err(unknown());
    };
    let tx = begin(conn)?;
    // A foreign printer is simply unknown — same error as a nonexistent one, no leak.
    let printer: Option<(i64, i64)> = tx
        .prepare_cached(
            "SELECT org_id, agent_id FROM printers WHERE id=:pid AND (:org IS NULL OR org_id = :org)",
        )?
        .query_row(named_params! {":pid": pid, ":org": job.org_id}, |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((org_id, agent_id)) = printer else {
        return Err(unknown());
    };
    if let Some(key) = &job.idempotency_key {
        // A retried submit returns the original job — the same key never prints twice in an org.
        // The lookup is safe under the write transaction; the UNIQUE index enforces it.
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM jobs WHERE org_id=? AND idempotency_key=?",
                params![org_id, key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(job_id) = existing {
            // a resubmit is the same job, so it spends no quota
            return Ok(Enqueued {
                job_id,
                agent_id,
                created: false,
            });
        }
    }
    // Checked here rather than in the handlers so every caller — POST /jobs, /orders, the Shopify
    // webhook and the PrintNode compat layer — is capped by one guard.
    check_quota(&tx, org_id, now)?;
    let options: Option<String> = job
        .options
        .as_ref()
        .filter(|o| !o.is_empty())
        .map(|o| pyfmt::dumps(&Value::Object(o.clone())));
    tx.prepare_cached(
        "INSERT INTO jobs(org_id, user_id, printer_id, agent_id, type, mode, state, payload, \
         title, copies, options, callback_url, idempotency_key, expires_at, created_at) \
         VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )?
    .execute(params![
        org_id,
        job.user_id,
        pid,
        agent_id,
        job.type_,
        job.mode,
        "queued",
        job.payload,
        job.title,
        job.copies,
        options,
        job.callback_url,
        job.idempotency_key,
        job.expire_after.map(|s| now + s as f64),
        now
    ])?;
    let job_id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(Enqueued {
        job_id,
        agent_id,
        created: true,
    })
}

pub fn claim_job(conn: &Connection, agent_id: i64, now: Option<f64>) -> Result<Option<ClaimedJob>> {
    let now = now.unwrap_or_else(crate::now);
    // Look before taking the write lock: most polls find nothing, and then the only write is the
    // liveness touch, which does not need to wait for the disk (see touch_last_seen).
    let row: Option<(i64, i64, String, i64, Option<String>)> = conn
        .prepare_cached(CLAIM_SQL)?
        .query_row(params![agent_id, now], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .optional()?;
    let Some((job_id, printer_id, mode, copies, options)) = row else {
        touch_last_seen(conn, agent_id, now)?; // the touch is the point of an empty poll
        return Ok(None);
    };
    let tx = begin(conn)?;
    tx.prepare_cached("UPDATE agents SET last_seen_at=? WHERE id=?")?
        .execute(params![now, agent_id])?;
    // State-guarded: only another process on the same file could have claimed it since the look
    // (this process holds the Db mutex), and then it is simply not ours.
    let claimed = tx
        .prepare_cached(
            "UPDATE jobs SET state='claimed', claimed_at=? WHERE id=? AND state='queued'",
        )?
        .execute(params![now, job_id])?;
    tx.commit()?;
    if claimed == 0 {
        return Ok(None);
    }
    Ok(Some(ClaimedJob {
        job_id,
        printer_id,
        mode,
        copies,
        options: parse_json(options),
    }))
}

/// JSON text back to a value; empty or unparseable text reads as absent (the Python would 500).
fn parse_json(text: Option<String>) -> Option<Value> {
    text.filter(|t| !t.is_empty())
        .and_then(|t| serde_json::from_str(&t).ok())
}

/// The job this agent already has in flight, if any.
///
/// A CloudPRNT printer is re-offered its claimed job until it confirms the result, so a poll
/// response lost on the way costs a re-download instead of burning the next job in the queue.
pub fn claimed_job(conn: &Connection, agent_id: i64) -> Result<Option<InFlight>> {
    Ok(conn
        .query_row(
            "SELECT id, mode, copies FROM jobs WHERE agent_id=? AND state='claimed' \
             ORDER BY claimed_at, id LIMIT 1",
            [agent_id],
            |r| {
                Ok(InFlight {
                    job_id: r.get(0)?,
                    mode: r.get(1)?,
                    copies: r.get(2)?,
                })
            },
        )
        .optional()?)
}

pub fn get_payload(conn: &Connection, job_id: i64, agent_id: i64) -> Result<Option<Vec<u8>>> {
    Ok(conn
        .prepare_cached("SELECT payload FROM jobs WHERE id=? AND agent_id=?")?
        .query_row(params![job_id, agent_id], |r| {
            // A payload written as TEXT by some other client still reads as its bytes.
            Ok(r.get_ref(0)?
                .as_bytes()
                .map(<[u8]>::to_vec)
                .unwrap_or_default())
        })
        .optional()?)
}

pub fn finish_job(
    conn: &Connection,
    job_id: i64,
    agent_id: i64,
    ok: bool,
    error: Option<&str>,
) -> Result<bool> {
    let state = if ok { "done" } else { "failed" };
    let tx = begin(conn)?;
    let n = tx
        .prepare_cached(
            "UPDATE jobs SET state=?, error=?, finished_at=? \
             WHERE id=? AND agent_id=? AND state='claimed'",
        )?
        .execute(params![
            state,
            if ok { None } else { error },
            crate::now(),
            job_id,
            agent_id
        ])?;
    tx.commit()?;
    Ok(n == 1)
}

/// Returns how many jobs went back to `queued`.
pub fn requeue_stale(
    conn: &Connection,
    timeout_s: f64,
    max_retries: i64,
    now: Option<f64>,
) -> Result<usize> {
    let now = now.unwrap_or_else(crate::now);
    let cutoff = now - timeout_s;
    let tx = begin(conn)?;
    tx.execute(
        "UPDATE jobs SET state='failed', error='retry limit exceeded', finished_at=? \
         WHERE state='claimed' AND claimed_at < ? AND retries >= ?",
        params![now, cutoff, max_retries],
    )?;
    let n = tx.execute(
        "UPDATE jobs SET state='queued', retries=retries+1, claimed_at=NULL \
         WHERE state='claimed' AND claimed_at < ? AND retries < ?",
        params![cutoff, max_retries],
    )?;
    tx.commit()?;
    Ok(n)
}

/// Fail every queued job whose expire_after deadline has passed. `failed` + error 'expired' —
/// terminal, so the webhook dispatcher reports it like any other outcome.
/// ponytail: no separate 'expired' state (it would have to be threaded through metrics, the
/// dashboard and every client); the error string carries the reason.
pub fn expire_jobs(conn: &Connection, now: Option<f64>) -> Result<usize> {
    let now = now.unwrap_or_else(crate::now);
    let tx = begin(conn)?;
    let n = tx.execute(
        "UPDATE jobs SET state='failed', error='expired', finished_at=:now \
         WHERE state='queued' AND expires_at IS NOT NULL AND expires_at <= :now",
        named_params! {":now": now},
    )?;
    tx.commit()?;
    Ok(n)
}

/// Cancel a job while it is still 'queued' (before any agent claims it).
/// The UPDATE is state-guarded so a claim racing the cancel can't be undone.
/// Another org's job is `NotFound` — the existence check is org-filtered too.
pub fn cancel_job(conn: &Connection, job_id: i64, org_id: Option<i64>) -> Result<CancelOutcome> {
    let tx = begin(conn)?;
    let n = tx.execute(
        "UPDATE jobs SET state='cancelled', finished_at=:now \
         WHERE id=:jid AND state='queued' AND (:org IS NULL OR org_id = :org)",
        named_params! {":now": crate::now(), ":jid": job_id, ":org": org_id},
    )?;
    if n == 1 {
        tx.commit()?;
        return Ok(CancelOutcome::Cancelled);
    }
    let exists = tx
        .query_row(
            "SELECT 1 FROM jobs WHERE id=:jid AND (:org IS NULL OR org_id = :org)",
            named_params! {":jid": job_id, ":org": org_id},
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    tx.commit()?;
    Ok(if exists {
        CancelOutcome::NotCancellable
    } else {
        CancelOutcome::NotFound
    })
}

/// Jobs in a terminal state with a callback_url, not yet delivered, under the attempt cap.
/// State-based (not transition-hooked), so every terminal path — agent report, cancel, reaper —
/// is covered without touching the mutators.
/// ponytail: the `jobs_hooks` partial index holds only the undelivered callback jobs, so the
/// per-tick scan stays small however large the history grows.
pub fn pending_webhooks(conn: &Connection, max_attempts: i64) -> Result<Vec<PendingHook>> {
    let mut stmt = conn.prepare_cached(HOOKS_SQL)?;
    let rows = stmt.query_map([max_attempts], |r| {
        Ok(PendingHook {
            job_id: r.get(0)?,
            callback_url: r.get(1)?,
            state: r.get(2)?,
            error: r.get(3)?,
            title: r.get(4)?,
            printer_id: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn mark_webhook_delivered(conn: &Connection, job_id: i64, now: Option<f64>) -> Result<()> {
    let tx = begin(conn)?;
    tx.execute(
        "UPDATE jobs SET hook_delivered_at=? WHERE id=?",
        params![now.unwrap_or_else(crate::now), job_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn bump_webhook_attempt(conn: &Connection, job_id: i64) -> Result<()> {
    let tx = begin(conn)?;
    tx.execute(
        "UPDATE jobs SET hook_attempts=hook_attempts+1 WHERE id=?",
        [job_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn get_job(conn: &Connection, job_id: i64, org_id: Option<i64>) -> Result<Option<JobStatus>> {
    Ok(conn
        .query_row(
            "SELECT state, error, printer_id, created_at, finished_at FROM jobs \
             WHERE id=:jid AND (:org IS NULL OR org_id = :org)",
            named_params! {":jid": job_id, ":org": org_id},
            |r| {
                Ok(JobStatus {
                    state: r.get(0)?,
                    error: r.get(1)?,
                    printer_id: r.get(2)?,
                    created_at: r.get(3)?,
                    finished_at: r.get(4)?,
                })
            },
        )
        .optional()?)
}

fn job_row(r: &Row<'_>) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        id: r.get(0)?,
        printer_id: r.get(1)?,
        printer_name: r.get(2)?,
        agent_name: r.get(3)?,
        title: r.get(4)?,
        state: r.get(5)?,
        type_: r.get(6)?,
        mode: r.get(7)?,
        error: r.get(8)?,
        created_at: r.get(9)?,
        finished_at: r.get(10)?,
    })
}

/// The job history, newest first. `ids` restricts it to specific job ids (the PrintNode compat
/// layer addresses jobs by id set) — still org-filtered, so a foreign id is simply absent.
/// `Some(&[])` is an empty answer without a query (`IN ()` is a syntax error).
pub fn recent_jobs(
    conn: &Connection,
    limit: i64,
    org_id: Option<i64>,
    ids: Option<&[i64]>,
) -> Result<Vec<JobRow>> {
    let mut sql = String::from(
        "SELECT j.id, j.printer_id, p.name AS printer_name, a.name AS agent_name, j.title, \
         j.state, j.type, j.mode, j.error, j.created_at, j.finished_at \
         FROM jobs j JOIN printers p ON p.id = j.printer_id \
         JOIN agents a ON a.id = j.agent_id \
         WHERE (?1 IS NULL OR j.org_id = ?1)",
    );
    let mut params: Vec<rusqlite::types::Value> = vec![org_id.into(), limit.into()];
    if let Some(ids) = ids {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let marks: Vec<String> = (0..ids.len()).map(|n| format!("?{}", n + 3)).collect();
        sql.push_str(&format!(" AND j.id IN ({})", marks.join(",")));
        params.extend(ids.iter().map(|&i| rusqlite::types::Value::from(i)));
    }
    sql.push_str(" ORDER BY j.id DESC LIMIT ?2");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), job_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Aggregate snapshot for /metrics: job counts by state, agent liveness, printer count.
pub fn metrics(
    conn: &Connection,
    online_window_s: f64,
    now: Option<f64>,
    org_id: Option<i64>,
) -> Result<Metrics> {
    let now = now.unwrap_or_else(crate::now);
    let mut m = Metrics::default();
    let mut stmt = conn.prepare(
        "SELECT state, COUNT(*) FROM jobs WHERE (:org IS NULL OR org_id = :org) GROUP BY state",
    )?;
    for row in stmt.query_map(named_params! {":org": org_id}, |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })? {
        let (state, n) = row?;
        m.jobs.insert(state, n);
    }
    let mut stmt =
        conn.prepare("SELECT last_seen_at FROM agents WHERE (:org IS NULL OR org_id = :org)")?;
    for row in stmt.query_map(named_params! {":org": org_id}, |r| {
        r.get::<_, Option<f64>>(0)
    })? {
        m.agents_total += 1;
        if row?.is_some_and(|s| now - s <= online_window_s) {
            m.agents_online += 1;
        }
    }
    m.printers_total = conn.query_row(
        "SELECT COUNT(*) FROM printers WHERE (:org IS NULL OR org_id = :org)",
        named_params! {":org": org_id},
        |r| r.get(0),
    )?;
    Ok(m)
}

/// Agents (PrintNode calls them computers) with liveness and how many printers each carries.
pub fn list_agents(
    conn: &Connection,
    online_window_s: f64,
    now: Option<f64>,
    org_id: Option<i64>,
) -> Result<Vec<AgentInfo>> {
    let now = now.unwrap_or_else(crate::now);
    let mut stmt = conn.prepare(
        "SELECT a.id, a.name, a.last_seen_at, a.created_at, \
         (SELECT COUNT(*) FROM printers p WHERE p.agent_id = a.id) AS printers \
         FROM agents a WHERE (:org IS NULL OR a.org_id = :org) ORDER BY a.id",
    )?;
    let rows = stmt.query_map(named_params! {":org": org_id}, |r| {
        let seen: Option<f64> = r.get(2)?;
        Ok(AgentInfo {
            id: r.get(0)?,
            name: r.get(1)?,
            last_seen_at: seen,
            created_at: r.get(3)?,
            printers: r.get(4)?,
            online: seen.is_some_and(|s| now - s <= online_window_s),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Agents whose liveness flipped since the last call, marked as reported in the same transaction
/// so each edge fires exactly once. Agents in an org with no event_url are marked but not
/// returned — an org that sets a URL later starts from the current state instead of replaying
/// history.
/// ponytail: full scan of agents per pass (the table is one row per machine); index it if a
/// deployment ever has thousands of agents.
pub fn claim_agent_transitions(
    conn: &Connection,
    online_window_s: f64,
    now: Option<f64>,
) -> Result<Vec<AgentEvent>> {
    let now = now.unwrap_or_else(crate::now);
    let cutoff = now - online_window_s;
    let tx = begin(conn)?;
    type Edge = (i64, String, i64, Option<f64>, i64, Option<String>);
    let rows: Vec<Edge> = {
        let mut stmt = tx.prepare(
            "SELECT a.id, a.name, a.org_id, a.last_seen_at, a.offline_notified, o.event_url \
             FROM agents a JOIN orgs o ON o.id = a.org_id ORDER BY a.id",
        )?;
        let it = stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?;
        it.collect::<rusqlite::Result<_>>()?
    };
    let mut out = Vec::new();
    for (id, name, org_id, seen, notified, event_url) in rows {
        let offline = seen.is_none_or(|s| s < cutoff);
        if offline == (notified != 0) {
            continue; // same state as last pass — no edge
        }
        tx.execute(
            "UPDATE agents SET offline_notified=? WHERE id=?",
            params![offline, id],
        )?;
        if let Some(event_url) = event_url.filter(|u| !u.is_empty()) {
            out.push(AgentEvent {
                agent_id: id,
                name,
                org_id,
                last_seen_at: seen,
                event_url,
                event: if offline { "offline" } else { "online" }.into(),
            });
        }
    }
    tx.commit()?;
    Ok(out)
}

pub fn list_printers(
    conn: &Connection,
    online_window_s: f64,
    now: Option<f64>,
    org_id: Option<i64>,
) -> Result<Vec<PrinterInfo>> {
    let now = now.unwrap_or_else(crate::now);
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name, p.agent_id, p.can_pdf, p.capabilities, p.created_at, \
         a.name AS agent_name, a.last_seen_at \
         FROM printers p JOIN agents a ON a.id = p.agent_id \
         WHERE (:org IS NULL OR p.org_id = :org) ORDER BY p.id",
    )?;
    let rows = stmt.query_map(named_params! {":org": org_id}, |r| {
        let seen: Option<f64> = r.get(7)?;
        Ok(PrinterInfo {
            id: r.get(0)?,
            name: r.get(1)?,
            agent_id: r.get(2)?,
            agent_name: r.get(6)?,
            can_pdf: r.get::<_, i64>(3)? != 0,
            created_at: r.get(5)?,
            capabilities: parse_json(r.get(4)?),
            online: seen.is_some_and(|s| now - s <= online_window_s),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// --- label templates --------------------------------------------------------------------------

/// One stored label template. `org` is `None` for root everywhere below and means the default
/// org, like every other label call: root has no org of its own to keep templates in.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelTemplate {
    pub id: i64,
    pub org_id: i64,
    pub spec: crate::label::Spec,
    pub created_at: f64,
    pub updated_at: f64,
}

impl LabelTemplate {
    /// The HTTP shape: `{id, ...contract fields, created_at, updated_at}`.
    pub fn to_json(&self) -> Value {
        let mut v = crate::label::spec_json(&self.spec);
        if let Value::Object(m) = &mut v {
            let mut out = Map::new();
            out.insert("id".into(), self.id.into());
            out.append(m);
            out.insert("created_at".into(), self.created_at.into());
            out.insert("updated_at".into(), self.updated_at.into());
            return Value::Object(out);
        }
        v
    }
}

const LABEL_COLS: &str = "id, org_id, spec, created_at, updated_at";

fn label_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<LabelTemplate> {
    let text: String = r.get(2)?;
    // The text was written by `create_label_template`; a row that no longer parses was edited by
    // hand, and is reported as a database error rather than served half-read.
    let bad = |e: String| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, e.into())
    };
    let spec = serde_json::from_str::<Value>(&text)
        .map_err(|e| bad(e.to_string()))?
        .as_object()
        .ok_or_else(|| bad("not an object".into()))
        .and_then(|m| crate::label::parse_spec(m).map_err(bad))?;
    Ok(LabelTemplate {
        id: r.get(0)?,
        org_id: r.get(1)?,
        spec,
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
    })
}

/// The org's templates, by name.
pub fn list_label_templates(conn: &Connection, org: Option<i64>) -> Result<Vec<LabelTemplate>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {LABEL_COLS} FROM label_templates WHERE org_id=? ORDER BY name COLLATE NOCASE, id"
    ))?;
    let rows = stmt.query_map([org.unwrap_or(DEFAULT_ORG)], label_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// By id; another org's template is simply not found.
pub fn get_label_template(
    conn: &Connection,
    id: i64,
    org: Option<i64>,
) -> Result<Option<LabelTemplate>> {
    Ok(conn
        .query_row(
            &format!("SELECT {LABEL_COLS} FROM label_templates WHERE id=? AND org_id=?"),
            params![id, org.unwrap_or(DEFAULT_ORG)],
            label_row,
        )
        .optional()?)
}

/// By name, within the org.
pub fn find_label_template(
    conn: &Connection,
    name: &str,
    org: Option<i64>,
) -> Result<Option<LabelTemplate>> {
    Ok(conn
        .query_row(
            &format!("SELECT {LABEL_COLS} FROM label_templates WHERE name=? AND org_id=?"),
            params![name, org.unwrap_or(DEFAULT_ORG)],
            label_row,
        )
        .optional()?)
}

/// `Err(Conflict)` when the org already has a template of that name.
pub fn create_label_template(
    conn: &Connection,
    org: Option<i64>,
    spec: &crate::label::Spec,
) -> Result<LabelTemplate> {
    let (org, now) = (org.unwrap_or(DEFAULT_ORG), crate::now());
    let tx = begin(conn)?;
    tx.execute(
        "INSERT INTO label_templates(org_id, name, spec, created_at, updated_at) VALUES(?,?,?,?,?)",
        params![
            org,
            spec.name,
            crate::label::spec_json(spec).to_string(),
            now,
            now
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(LabelTemplate {
        id,
        org_id: org,
        spec: spec.clone(),
        created_at: now,
        updated_at: now,
    })
}

/// Replace the whole template. `Ok(None)` if the org has no such id, `Err(Conflict)` if the new
/// name is another template's.
pub fn update_label_template(
    conn: &Connection,
    id: i64,
    org: Option<i64>,
    spec: &crate::label::Spec,
) -> Result<Option<LabelTemplate>> {
    let (org, now) = (org.unwrap_or(DEFAULT_ORG), crate::now());
    let tx = begin(conn)?;
    let n = tx.execute(
        "UPDATE label_templates SET name=?, spec=?, updated_at=? WHERE id=? AND org_id=?",
        params![
            spec.name,
            crate::label::spec_json(spec).to_string(),
            now,
            id,
            org
        ],
    )?;
    if n == 0 {
        return Ok(None);
    }
    let t = tx.query_row(
        &format!("SELECT {LABEL_COLS} FROM label_templates WHERE id=?"),
        [id],
        label_row,
    )?;
    tx.commit()?;
    Ok(Some(t))
}

pub fn delete_label_template(conn: &Connection, id: i64, org: Option<i64>) -> Result<bool> {
    let tx = begin(conn)?;
    let n = tx.execute(
        "DELETE FROM label_templates WHERE id=? AND org_id=?",
        params![id, org.unwrap_or(DEFAULT_ORG)],
    )?;
    tx.commit()?;
    Ok(n == 1)
}

/// True for the pseudo-agent `register_cloudprnt` makes (its key hash starts with `cloudprnt:`).
/// Such a device only understands Star commands, so templates cannot print on it.
pub fn is_cloudprnt_agent(conn: &Connection, agent_id: i64) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT substr(api_key_hash, 1, 10) = 'cloudprnt:' FROM agents WHERE id=?",
            [agent_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        == Some(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::types::FromSql;
    use serde_json::json;

    // --- helpers --------------------------------------------------------------------------

    fn db() -> Connection {
        let conn = connect(":memory:").unwrap();
        init_db(&conn).unwrap();
        conn
    }

    fn q<T: FromSql>(conn: &Connection, sql: &str, p: impl rusqlite::Params) -> T {
        conn.query_row(sql, p, |r| r.get(0)).unwrap()
    }

    fn pd(name: &str, can_pdf: bool) -> PrinterDecl {
        PrinterDecl {
            name: name.into(),
            can_pdf,
            capabilities: None,
        }
    }

    fn reg_in(
        conn: &Connection,
        name: &str,
        key: &str,
        printers: &[(&str, bool)],
        org: i64,
    ) -> Registration {
        let decls: Vec<_> = printers.iter().map(|(n, p)| pd(n, *p)).collect();
        register_agent(conn, name, key, &decls, org).unwrap()
    }

    fn reg(conn: &Connection, name: &str, key: &str, printers: &[(&str, bool)]) -> Registration {
        reg_in(conn, name, key, printers, DEFAULT_ORG)
    }

    fn id_of(r: &Registration, printer: &str) -> i64 {
        r.printer_ids[printer].as_i64().unwrap()
    }

    fn try_enq(
        conn: &Connection,
        pid: &Value,
        payload: &[u8],
        f: impl FnOnce(&mut NewJob<'_>),
    ) -> Result<Enqueued> {
        let mut j = NewJob {
            printer_id: pid,
            type_: "raw_base64",
            mode: "raw",
            payload,
            user_id: DEFAULT_USER,
            title: None,
            copies: 1,
            callback_url: None,
            options: None,
            org_id: None,
            idempotency_key: None,
            expire_after: None,
        };
        f(&mut j);
        enqueue_job(conn, &j)
    }

    fn enq_with(
        conn: &Connection,
        pid: i64,
        payload: &[u8],
        f: impl FnOnce(&mut NewJob<'_>),
    ) -> i64 {
        try_enq(conn, &json!(pid), payload, f).unwrap().job_id
    }

    fn enq(conn: &Connection, pid: i64, payload: &[u8]) -> i64 {
        enq_with(conn, pid, payload, |_| {})
    }

    fn state_of(conn: &Connection, jid: i64) -> String {
        q(conn, "SELECT state FROM jobs WHERE id=?", [jid])
    }

    fn two_orgs(conn: &Connection) -> (i64, Registration, Registration) {
        let other = create_org(conn, "acme").unwrap();
        let a = reg(conn, "pc", "ka", &[("Z", false)]);
        let b = reg_in(conn, "pc", "kb", &[("Z", false)], other);
        (other, a, b)
    }

    fn user(conn: &Connection, email: &str, org: i64) -> i64 {
        create_user(conn, org, email, "scrypt$stub").unwrap()
    }

    fn plan(conn: &Connection, sql: &str, p: impl rusqlite::Params) -> String {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let rows = stmt.query_map(p, |r| r.get::<_, String>(3)).unwrap();
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join(" | ")
    }

    fn columns(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
        rows.collect::<rusqlite::Result<_>>().unwrap()
    }

    // --- tests/test_store.py --------------------------------------------------------------

    #[test]
    fn init_seeds_default_org_and_user_and_is_idempotent() {
        let conn = db();
        init_db(&conn).unwrap(); // second call must not raise or duplicate
        let name: String = q(&conn, "SELECT name FROM orgs WHERE id=?", [DEFAULT_ORG]);
        let org: i64 = q(&conn, "SELECT org_id FROM users WHERE id=?", [DEFAULT_USER]);
        assert_eq!(name, "default");
        assert_eq!(org, DEFAULT_ORG);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM orgs", []), 1);
    }

    #[test]
    fn register_is_idempotent_and_authenticates() {
        let conn = db();
        let r1 = reg(&conn, "win-1", "key-abc", &[("Zebra", false)]);
        let r2 = reg(&conn, "win-1", "key-abc", &[("Zebra", false), ("HP", true)]);
        assert_eq!(r1.computer_id, r2.computer_id); // same agent, not duplicated
        assert_eq!(r1.printer_ids["Zebra"], r2.printer_ids["Zebra"]);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM agents", []), 1);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM printers", []), 2);
        assert_eq!(
            authenticate_agent(&conn, "key-abc").unwrap(),
            Some(r1.computer_id)
        );
        assert_eq!(authenticate_agent(&conn, "wrong").unwrap(), None);
    }

    #[test]
    fn register_rejects_existing_name_with_different_key() {
        let conn = db();
        reg(&conn, "win-1", "key-abc", &[]);
        reg(&conn, "win-1", "key-abc", &[]); // same key -> idempotent restart, ok
        match register_agent(&conn, "win-1", "other-key", &[], DEFAULT_ORG) {
            Err(StoreError::Auth(m)) => {
                assert_eq!(
                    m,
                    "agent name already registered with a different key: 'win-1'"
                )
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[test]
    fn enqueue_resolves_agent_and_rejects_unknown_printer() {
        let conn = db();
        let r = reg(&conn, "win-1", "k", &[("Zebra", false)]);
        let jid = enq(&conn, id_of(&r, "Zebra"), b"^XA^XZ");
        let (agent, state, mode): (i64, String, String) = conn
            .query_row(
                "SELECT agent_id, state, mode FROM jobs WHERE id=?",
                [jid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (agent, state.as_str(), mode.as_str()),
            (r.computer_id, "queued", "raw")
        );
        assert_eq!(get_payload(&conn, jid, agent).unwrap().unwrap(), b"^XA^XZ");
        match try_enq(&conn, &json!(9999), b"x", |_| {}) {
            Err(StoreError::UnknownPrinter(m)) => assert_eq!(m, "unknown printer: 9999"),
            other => panic!("expected UnknownPrinter, got {other:?}"),
        }
    }

    #[test]
    fn unknown_printer_text_follows_python_str_of_the_raw_value() {
        let conn = db();
        let r = reg(&conn, "w", "k", &[("Z", false)]);
        for (raw, text) in [
            (json!("abc"), "unknown printer: abc"),
            (json!(null), "unknown printer: None"),
            (json!([1]), "unknown printer: [1]"),
            (json!(1.5), "unknown printer: 1.5"),
        ] {
            match try_enq(&conn, &raw, b"x", |_| {}) {
                Err(StoreError::UnknownPrinter(m)) => assert_eq!(m, text),
                other => panic!("expected UnknownPrinter, got {other:?}"),
            }
        }
        // SQLite column affinity: "5", 5.0 and true all address a printer by its integer id
        let id = id_of(&r, "Z");
        assert_eq!(id, 1);
        for raw in [json!("1"), json!(1.0), json!(true)] {
            assert!(try_enq(&conn, &raw, b"x", |_| {}).is_ok());
            assert!(get_printer(&conn, &raw, None).unwrap().is_some());
        }
        assert!(get_printer(&conn, &json!("abc"), None).unwrap().is_none());
    }

    #[test]
    fn a_revoked_issued_key_no_longer_authenticates_its_agent() {
        let conn = db();
        let kid = add_api_key(&conn, "pc", "issued-key", DEFAULT_ORG).unwrap();
        let r = reg(&conn, "pc", "issued-key", &[]);
        reg(&conn, "legacy", "self-chosen", &[]);
        assert_eq!(
            authenticate_agent(&conn, "issued-key").unwrap(),
            Some(r.computer_id)
        );
        assert!(!key_revoked(&conn, "issued-key").unwrap());
        assert!(revoke_api_key(&conn, kid, None).unwrap());
        assert_eq!(authenticate_agent(&conn, "issued-key").unwrap(), None);
        assert!(key_revoked(&conn, "issued-key").unwrap());
        // a key that was never issued has nothing to revoke
        assert!(authenticate_agent(&conn, "self-chosen").unwrap().is_some());
        assert!(!key_revoked(&conn, "self-chosen").unwrap() && !key_revoked(&conn, "").unwrap());
    }

    #[test]
    fn an_empty_poll_touches_last_seen_without_leaving_synchronous_lowered() {
        let conn = db();
        let r = reg(&conn, "win-1", "k", &[("Z", false)]);
        assert!(
            claim_job(&conn, r.computer_id, Some(4242.0))
                .unwrap()
                .is_none()
        );
        let seen: f64 = q(
            &conn,
            "SELECT last_seen_at FROM agents WHERE id=?",
            [r.computer_id],
        );
        assert_eq!(seen, 4242.0);
        // FULL = 2: every write after the heartbeat is durable again
        assert_eq!(q::<i64>(&conn, "PRAGMA synchronous", []), 2);
        // and a claim that finds a job still claims it, touching last_seen too
        let jid = enq(&conn, id_of(&r, "Z"), b"x");
        assert_eq!(
            claim_job(&conn, r.computer_id, Some(5000.0))
                .unwrap()
                .unwrap()
                .job_id,
            jid
        );
        let seen: f64 = q(
            &conn,
            "SELECT last_seen_at FROM agents WHERE id=?",
            [r.computer_id],
        );
        assert_eq!(seen, 5000.0);
        assert_eq!(q::<i64>(&conn, "PRAGMA synchronous", []), 2);
    }

    #[test]
    fn claim_returns_one_job_then_none_and_updates_last_seen() {
        let conn = db();
        let r = reg(&conn, "win-1", "k", &[("Z", false)]);
        let (aid, pid) = (r.computer_id, id_of(&r, "Z"));
        let j1 = enq(&conn, pid, b"a");
        enq(&conn, pid, b"b");
        let c1 = claim_job(&conn, aid, None).unwrap().unwrap();
        assert_eq!((c1.job_id, c1.mode.as_str()), (j1, "raw")); // oldest first
        assert_eq!(state_of(&conn, j1), "claimed");
        assert_ne!(claim_job(&conn, aid, None).unwrap().unwrap().job_id, j1); // second job
        assert!(claim_job(&conn, aid, None).unwrap().is_none()); // nothing left
        let seen: Option<f64> = q(&conn, "SELECT last_seen_at FROM agents WHERE id=?", [aid]);
        assert!(seen.unwrap() > 0.0);
    }

    #[test]
    fn an_empty_claim_still_touches_last_seen() {
        let conn = db();
        let r = reg(&conn, "w", "k", &[]);
        conn.execute("UPDATE agents SET last_seen_at=1.0", [])
            .unwrap();
        assert!(
            claim_job(&conn, r.computer_id, Some(500.0))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            q::<f64>(&conn, "SELECT last_seen_at FROM agents", []),
            500.0
        );
    }

    #[test]
    fn payload_and_finish_are_agent_scoped() {
        let conn = db();
        let a = reg(&conn, "a", "ka", &[("PA", false)]);
        let b = reg(&conn, "b", "kb", &[("PB", false)]);
        let jid = enq(&conn, id_of(&a, "PA"), b"DATA");
        claim_job(&conn, a.computer_id, None).unwrap();
        assert_eq!(
            get_payload(&conn, jid, a.computer_id).unwrap().unwrap(),
            b"DATA"
        );
        assert!(get_payload(&conn, jid, b.computer_id).unwrap().is_none()); // foreign agent denied
        assert!(!finish_job(&conn, jid, b.computer_id, true, None).unwrap());
        assert!(finish_job(&conn, jid, a.computer_id, false, Some("boom")).unwrap());
        let (state, error): (String, Option<String>) = conn
            .query_row("SELECT state, error FROM jobs WHERE id=?", [jid], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((state.as_str(), error.as_deref()), ("failed", Some("boom")));
        assert!(!finish_job(&conn, jid, a.computer_id, true, None).unwrap()); // not 'claimed' anymore
    }

    #[test]
    fn a_successful_finish_clears_the_error() {
        let conn = db();
        let r = reg(&conn, "w", "k", &[("Z", false)]);
        let jid = enq(&conn, id_of(&r, "Z"), b"x");
        claim_job(&conn, r.computer_id, None).unwrap();
        assert!(finish_job(&conn, jid, r.computer_id, true, Some("ignored")).unwrap());
        let (error, fin): (Option<String>, Option<f64>) = conn
            .query_row(
                "SELECT error, finished_at FROM jobs WHERE id=?",
                [jid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(error.is_none() && fin.is_some());
    }

    #[test]
    fn reaper_requeues_then_fails_after_limit() {
        let conn = db();
        let r = reg(&conn, "w", "k", &[("P", false)]);
        let (aid, pid) = (r.computer_id, id_of(&r, "P"));
        let jid = enq(&conn, pid, b"x");
        claim_job(&conn, aid, None).unwrap();
        let base = crate::now();
        assert_eq!(requeue_stale(&conn, 60.0, 1, Some(base)).unwrap(), 0); // not stale yet
        assert_eq!(
            requeue_stale(&conn, 60.0, 1, Some(base + 120.0)).unwrap(),
            1
        ); // under limit
        assert_eq!(state_of(&conn, jid), "queued");
        assert_eq!(
            q::<i64>(&conn, "SELECT retries FROM jobs WHERE id=?", [jid]),
            1
        );
        claim_job(&conn, aid, None).unwrap(); // claim again, go stale again -> over the limit
        assert_eq!(
            requeue_stale(&conn, 60.0, 1, Some(base + 300.0)).unwrap(),
            0
        );
        assert_eq!(state_of(&conn, jid), "failed");
        assert_eq!(
            q::<String>(&conn, "SELECT error FROM jobs WHERE id=?", [jid]),
            "retry limit exceeded"
        );
    }

    #[test]
    fn register_stores_capabilities_and_list_printers_returns_them() {
        let conn = db();
        let caps =
            json!({"papers": ["A4", "Letter"], "bins": ["Tray 1"], "duplex": true, "color": false});
        let hp = |c: &Value| PrinterDecl {
            name: "HP".into(),
            can_pdf: true,
            capabilities: Some(c.clone()),
        };
        register_agent(&conn, "a", "k", &[hp(&caps), pd("Z", false)], DEFAULT_ORG).unwrap();
        let by_name = |conn: &Connection| -> std::collections::HashMap<String, PrinterInfo> {
            list_printers(conn, 60.0, None, None)
                .unwrap()
                .into_iter()
                .map(|p| (p.name.clone(), p))
                .collect()
        };
        let ps = by_name(&conn);
        assert_eq!(ps["HP"].capabilities, Some(caps.clone()));
        assert_eq!(ps["Z"].capabilities, None);
        // re-register updates capabilities in place
        let mut caps2 = caps.clone();
        caps2["duplex"] = json!(false);
        register_agent(&conn, "a", "k", &[hp(&caps2)], DEFAULT_ORG).unwrap();
        assert_eq!(by_name(&conn)["HP"].capabilities, Some(caps2));
        // an empty capability object is falsy: stored as NULL like the Python
        register_agent(&conn, "a", "k", &[hp(&json!({}))], DEFAULT_ORG).unwrap();
        assert_eq!(by_name(&conn)["HP"].capabilities, None);
        assert!(
            q::<Option<String>>(
                &conn,
                "SELECT capabilities FROM printers WHERE name='HP'",
                []
            )
            .is_none()
        );
    }

    #[test]
    fn printers_missing_from_a_re_registration_stay() {
        let conn = db();
        reg(&conn, "a", "k", &[("A", false), ("B", false)]);
        reg(&conn, "a", "k", &[("A", true)]);
        let ps = list_printers(&conn, 60.0, None, None).unwrap();
        assert_eq!(ps.len(), 2);
        assert!(ps[0].can_pdf && !ps[1].can_pdf);
    }

    #[test]
    fn a_key_used_by_another_agent_name_is_a_conflict() {
        let conn = db();
        reg(&conn, "a", "k", &[]);
        assert!(matches!(
            register_agent(&conn, "b", "k", &[], DEFAULT_ORG),
            Err(StoreError::Conflict)
        ));
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM agents", []), 1);
    }

    #[test]
    fn duplicate_printer_names_in_one_registration_collapse_to_one_row() {
        let conn = db();
        let r = reg(&conn, "a", "k", &[("Z", false), ("Z", true)]);
        assert_eq!(r.printer_ids.len(), 1);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM printers", []), 1);
        assert_eq!(q::<i64>(&conn, "SELECT can_pdf FROM printers", []), 1);
    }

    #[test]
    fn enqueue_and_claim_roundtrip_options() {
        let conn = db();
        let r = reg(&conn, "a", "k", &[("HP", true)]);
        let opts = json!({"duplex": "long-edge", "paper": "A4", "bin": "Tray 1"});
        let o = opts.as_object().unwrap().clone();
        let jid = enq_with(&conn, id_of(&r, "HP"), b"%PDF", |j| {
            j.type_ = "pdf_base64";
            j.mode = "pdf";
            j.options = Some(o);
        });
        let c = claim_job(&conn, r.computer_id, None).unwrap().unwrap();
        assert_eq!(c.job_id, jid);
        assert_eq!(c.options, Some(opts));
    }

    #[test]
    fn claim_without_options_is_none() {
        let conn = db();
        let r = reg(&conn, "a", "k", &[("Z", false)]);
        enq(&conn, id_of(&r, "Z"), b"x");
        assert!(
            claim_job(&conn, r.computer_id, None)
                .unwrap()
                .unwrap()
                .options
                .is_none()
        );
    }

    #[test]
    fn empty_options_are_stored_as_null_and_unparseable_options_read_as_none() {
        let conn = db();
        let r = reg(&conn, "a", "k", &[("Z", false)]);
        let pid = id_of(&r, "Z");
        let j1 = enq_with(&conn, pid, b"x", |j| j.options = Some(Map::new()));
        assert!(q::<Option<String>>(&conn, "SELECT options FROM jobs WHERE id=?", [j1]).is_none());
        let j2 = enq(&conn, pid, b"y");
        conn.execute("UPDATE jobs SET options='{not json' WHERE id=?", [j2])
            .unwrap();
        claim_job(&conn, r.computer_id, None).unwrap();
        let c = claim_job(&conn, r.computer_id, None).unwrap().unwrap();
        assert_eq!((c.job_id, c.options), (j2, None));
    }

    #[test]
    fn get_job_and_list_printers_online_flag() {
        let conn = db();
        let r = reg(&conn, "win-1", "k", &[("Z", true)]);
        let jid = enq_with(&conn, id_of(&r, "Z"), b"%PDF", |j| {
            j.type_ = "pdf_base64";
            j.mode = "pdf";
        });
        assert_eq!(get_job(&conn, jid, None).unwrap().unwrap().state, "queued");
        assert!(get_job(&conn, 123456, None).unwrap().is_none());
        claim_job(&conn, r.computer_id, None).unwrap(); // sets last_seen_at to ~now
        let ps = list_printers(&conn, 60.0, None, None).unwrap();
        assert!(ps.len() == 1 && ps[0].name == "Z" && ps[0].can_pdf);
        assert!(ps[0].online);
        assert!(!list_printers(&conn, 60.0, Some(crate::now() + 600.0), None).unwrap()[0].online);
    }

    #[test]
    fn api_keys_add_authenticate_list_revoke() {
        let conn = db();
        let kid = add_api_key(&conn, "n8n", "secret-key", DEFAULT_ORG).unwrap();
        assert_eq!(
            authenticate_client(&conn, "secret-key")
                .unwrap()
                .unwrap()
                .id,
            kid
        );
        assert!(authenticate_client(&conn, "nope").unwrap().is_none());
        assert!(authenticate_client(&conn, "").unwrap().is_none());
        let keys = list_api_keys(&conn, None).unwrap();
        assert!(keys.len() == 1 && keys[0].label == "n8n");
        let body = serde_json::to_value(&keys[0]).unwrap();
        assert!(body.get("key_hash").is_none() && body.get("key").is_none()); // secrets never returned
        assert!(revoke_api_key(&conn, kid, None).unwrap());
        assert!(authenticate_client(&conn, "secret-key").unwrap().is_none()); // revoked -> denied
        assert_eq!(list_api_keys(&conn, None).unwrap()[0].active, 0);
        assert!(!revoke_api_key(&conn, 9999, None).unwrap());
    }

    #[test]
    fn recent_jobs_newest_first_with_printer_name_and_limit() {
        let conn = db();
        let r = reg(&conn, "win-1", "k", &[("Zebra", false)]);
        let (aid, pid) = (r.computer_id, id_of(&r, "Zebra"));
        let j1 = enq(&conn, pid, b"a");
        let j2 = enq_with(&conn, pid, b"b", |j| {
            j.type_ = "pdf_base64";
            j.mode = "pdf";
        });
        claim_job(&conn, aid, None).unwrap(); // claims j1
        finish_job(&conn, j1, aid, true, None).unwrap();
        let rows = recent_jobs(&conn, 10, None, None).unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [j2, j1]); // newest first
        assert_eq!(rows[1].printer_name, "Zebra"); // joined name
        assert!(rows[1].state == "done" && rows[0].state == "queued");
        assert!(rows[0].type_ == "pdf_base64" && rows[0].mode == "pdf");
        assert!(
            serde_json::to_value(&rows[0])
                .unwrap()
                .get("payload")
                .is_none()
        ); // never leak bytes
        assert_eq!(recent_jobs(&conn, 1, None, None).unwrap().len(), 1); // limit honored
    }

    #[test]
    fn enqueue_and_recent_jobs_carry_title_and_agent_name() {
        let conn = db();
        let r = reg(&conn, "pc-1", "k", &[("Z", false)]);
        enq_with(&conn, id_of(&r, "Z"), b"x", |j| {
            j.title = Some("Versandlabel".into())
        });
        let row = &recent_jobs(&conn, 50, None, None).unwrap()[0];
        assert_eq!(row.title.as_deref(), Some("Versandlabel"));
        assert_eq!(
            (row.agent_name.as_str(), row.printer_name.as_str()),
            ("pc-1", "Z")
        );
    }

    #[test]
    fn recent_jobs_ids_filter_is_org_scoped_and_empty_ids_answer_empty() {
        let conn = db();
        let (other, a, b) = two_orgs(&conn);
        let ja = enq(&conn, id_of(&a, "Z"), b"a");
        let jb = enq(&conn, id_of(&b, "Z"), b"b");
        let jc = enq(&conn, id_of(&a, "Z"), b"c");
        let ids = |rows: Vec<JobRow>| rows.into_iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(
            ids(recent_jobs(&conn, 50, None, Some(&[ja, jb])).unwrap()),
            [jb, ja]
        );
        assert_eq!(
            ids(recent_jobs(&conn, 50, Some(other), Some(&[ja, jb, jc])).unwrap()),
            [jb]
        );
        assert!(recent_jobs(&conn, 50, None, Some(&[])).unwrap().is_empty());
        assert!(
            recent_jobs(&conn, 50, None, Some(&[424242]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn title_column_migration_is_idempotent() {
        let conn = db();
        init_db(&conn).unwrap();
        assert_eq!(
            columns(&conn, "jobs")
                .iter()
                .filter(|c| *c == "title")
                .count(),
            1
        );
    }

    #[test]
    fn enqueue_stores_copies_and_claim_returns_it() {
        let conn = db();
        let r = reg(&conn, "w", "k", &[("Z", false)]);
        let jid = enq_with(&conn, id_of(&r, "Z"), b"x", |j| j.copies = 3);
        assert_eq!(
            claim_job(&conn, r.computer_id, None)
                .unwrap()
                .unwrap()
                .copies,
            3
        );
        assert_eq!(
            q::<i64>(&conn, "SELECT copies FROM jobs WHERE id=?", [jid]),
            3
        );
    }

    #[test]
    fn enqueue_copies_defaults_to_one() {
        let conn = db();
        let r = reg(&conn, "w", "k", &[("Z", false)]);
        enq(&conn, id_of(&r, "Z"), b"x");
        assert_eq!(
            claim_job(&conn, r.computer_id, None)
                .unwrap()
                .unwrap()
                .copies,
            1
        );
    }

    #[test]
    fn cancel_queued_job_marks_cancelled() {
        let conn = db();
        let r = reg(&conn, "w", "k", &[("Z", false)]);
        let jid = enq(&conn, id_of(&r, "Z"), b"x");
        assert_eq!(
            cancel_job(&conn, jid, None).unwrap(),
            CancelOutcome::Cancelled
        );
        let (state, fin): (String, Option<f64>) = conn
            .query_row(
                "SELECT state, finished_at FROM jobs WHERE id=?",
                [jid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(state == "cancelled" && fin.is_some());
        assert!(claim_job(&conn, r.computer_id, None).unwrap().is_none()); // never claimable
    }

    #[test]
    fn cancel_claimed_job_is_refused() {
        let conn = db();
        let r = reg(&conn, "w", "k", &[("Z", false)]);
        let jid = enq(&conn, id_of(&r, "Z"), b"x");
        claim_job(&conn, r.computer_id, None).unwrap();
        assert_eq!(
            cancel_job(&conn, jid, None).unwrap(),
            CancelOutcome::NotCancellable
        );
        assert_eq!(state_of(&conn, jid), "claimed");
    }

    #[test]
    fn cancel_unknown_job_is_not_found() {
        assert_eq!(
            cancel_job(&db(), 999999, None).unwrap(),
            CancelOutcome::NotFound
        );
    }

    #[test]
    fn metrics_snapshot_counts_jobs_agents_printers() {
        let conn = db();
        let r = reg(&conn, "w", "k", &[("Z", false), ("Y", true)]);
        let (aid, pid) = (r.computer_id, id_of(&r, "Z"));
        let j1 = enq(&conn, pid, b"a");
        let j2 = enq(&conn, pid, b"b");
        claim_job(&conn, aid, None).unwrap(); // claims j1 -> claimed; last_seen ~now
        finish_job(&conn, j1, aid, true, None).unwrap(); // j1 -> done
        cancel_job(&conn, j2, None).unwrap(); // j2 -> cancelled
        let m = metrics(&conn, 60.0, None, None).unwrap();
        assert!(m.jobs["done"] == 1 && m.jobs["cancelled"] == 1);
        assert_eq!(m.jobs.get("queued").copied().unwrap_or(0), 0);
        assert!(m.agents_total == 1 && m.agents_online == 1);
        assert_eq!(m.printers_total, 2);
        // aged past the window -> offline
        assert_eq!(
            metrics(&conn, 60.0, Some(crate::now() + 600.0), None)
                .unwrap()
                .agents_online,
            0
        );
    }

    #[test]
    fn pending_webhooks_selects_only_terminal_undelivered_with_callback() {
        let conn = db();
        let r = reg(&conn, "w", "k", &[("Z", false)]);
        let (aid, pid) = (r.computer_id, id_of(&r, "Z"));
        let cb = |url: &'static str| move |j: &mut NewJob<'_>| j.callback_url = Some(url.into());
        let a = enq_with(&conn, pid, b"a", |j| {
            j.callback_url = Some("https://h/a".into());
            j.title = Some("A".into());
        });
        claim_job(&conn, aid, None).unwrap();
        finish_job(&conn, a, aid, true, None).unwrap(); // done + cb
        enq_with(&conn, pid, b"b", cb("https://h/b")); // queued + cb
        let c = enq(&conn, pid, b"c"); // failed, NO cb
        claim_job(&conn, aid, None).unwrap();
        finish_job(&conn, c, aid, false, Some("boom")).unwrap();
        let d = enq_with(&conn, pid, b"d", cb("https://h/d"));
        cancel_job(&conn, d, None).unwrap(); // cancelled + cb
        let pend = pending_webhooks(&conn, 5).unwrap();
        assert_eq!(pend.iter().map(|p| p.job_id).collect::<Vec<_>>(), [a, d]);
        assert!(pend[0].callback_url == "https://h/a" && pend[0].state == "done");
        assert!(pend[0].title.as_deref() == Some("A") && pend[0].printer_id == pid);
        assert_eq!(pend[1].state, "cancelled");
    }

    #[test]
    fn mark_webhook_delivered_removes_from_pending() {
        let conn = db();
        let r = reg(&conn, "w", "k", &[("Z", false)]);
        let a = enq_with(&conn, id_of(&r, "Z"), b"a", |j| {
            j.callback_url = Some("https://h/a".into())
        });
        claim_job(&conn, r.computer_id, None).unwrap();
        finish_job(&conn, a, r.computer_id, true, None).unwrap();
        assert_eq!(pending_webhooks(&conn, 5).unwrap().len(), 1);
        mark_webhook_delivered(&conn, a, None).unwrap();
        assert!(pending_webhooks(&conn, 5).unwrap().is_empty());
    }

    #[test]
    fn bump_webhook_attempt_caps_retries() {
        let conn = db();
        let r = reg(&conn, "w", "k", &[("Z", false)]);
        let a = enq_with(&conn, id_of(&r, "Z"), b"a", |j| {
            j.callback_url = Some("https://h/a".into())
        });
        claim_job(&conn, r.computer_id, None).unwrap();
        finish_job(&conn, a, r.computer_id, true, None).unwrap();
        for _ in 0..3 {
            bump_webhook_attempt(&conn, a).unwrap();
        }
        assert!(pending_webhooks(&conn, 3).unwrap().is_empty()); // attempts >= cap -> given up
        assert_eq!(pending_webhooks(&conn, 4).unwrap()[0].job_id, a);
    }

    #[test]
    fn webhook_columns_migration_is_idempotent() {
        let conn = db();
        init_db(&conn).unwrap();
        let cols = columns(&conn, "jobs");
        for col in ["callback_url", "hook_attempts", "hook_delivered_at"] {
            assert_eq!(cols.iter().filter(|c| *c == col).count(), 1);
        }
    }

    #[test]
    fn copies_column_migration_is_idempotent() {
        let conn = db();
        init_db(&conn).unwrap();
        assert_eq!(
            columns(&conn, "jobs")
                .iter()
                .filter(|c| *c == "copies")
                .count(),
            1
        );
    }

    // --- multi-tenancy --------------------------------------------------------------------

    #[test]
    fn create_org_and_list_orgs() {
        let conn = db();
        let oid = create_org(&conn, "acme").unwrap();
        assert_ne!(oid, DEFAULT_ORG);
        let orgs: Vec<_> = list_orgs(&conn)
            .unwrap()
            .into_iter()
            .map(|o| (o.name, o.id))
            .collect();
        assert_eq!(
            orgs,
            [
                ("default".to_string(), DEFAULT_ORG),
                ("acme".to_string(), oid)
            ]
        );
        assert!(org_exists(&conn, oid).unwrap() && !org_exists(&conn, 4242).unwrap());
    }

    #[test]
    fn api_key_resolves_to_its_org() {
        let conn = db();
        let oid = create_org(&conn, "acme").unwrap();
        let kid = add_api_key(&conn, "acme", "k-acme", oid).unwrap();
        assert_eq!(
            authenticate_client(&conn, "k-acme").unwrap(),
            Some(ClientKey {
                id: kid,
                org_id: oid
            })
        );
        assert!(authenticate_client(&conn, "nope").unwrap().is_none());
        add_api_key(&conn, "legacy", "k-legacy", DEFAULT_ORG).unwrap();
        assert_eq!(
            authenticate_client(&conn, "k-legacy")
                .unwrap()
                .unwrap()
                .org_id,
            DEFAULT_ORG
        );
    }

    #[test]
    fn list_api_keys_filters_by_org_and_reports_it() {
        let conn = db();
        let oid = create_org(&conn, "acme").unwrap();
        add_api_key(&conn, "acme", "ka", oid).unwrap();
        add_api_key(&conn, "legacy", "kd", DEFAULT_ORG).unwrap();
        let scoped = list_api_keys(&conn, Some(oid)).unwrap();
        assert!(scoped.iter().map(|k| k.label.as_str()).eq(["acme"]) && scoped[0].org_id == oid);
        assert_eq!(list_api_keys(&conn, None).unwrap().len(), 2); // unfiltered = root view
        assert!(
            serde_json::to_value(&scoped[0])
                .unwrap()
                .get("key_hash")
                .is_none()
        );
    }

    #[test]
    fn register_agent_scopes_name_and_printers_to_its_org() {
        let conn = db();
        let (other, a, b) = two_orgs(&conn);
        assert_ne!(a.computer_id, b.computer_id); // same name, different org -> two agents
        assert_ne!(a.printer_ids["Z"], b.printer_ids["Z"]);
        let org_of = |r: &Registration| {
            q::<i64>(
                &conn,
                "SELECT org_id FROM printers WHERE id=?",
                [id_of(r, "Z")],
            )
        };
        assert_eq!(org_of(&a), DEFAULT_ORG);
        assert_eq!(org_of(&b), other);
    }

    #[test]
    fn reads_and_writes_are_org_scoped_and_unfiltered_means_root() {
        let conn = db();
        let (other, a, b) = two_orgs(&conn);
        enq(&conn, id_of(&a, "Z"), b"a");
        let jb = enq(&conn, id_of(&b, "Z"), b"b");
        assert_eq!(
            q::<i64>(&conn, "SELECT org_id FROM jobs WHERE id=?", [jb]),
            other
        );

        let pids = |org| {
            list_printers(&conn, 60.0, None, org)
                .unwrap()
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(pids(Some(other)), [id_of(&b, "Z")]);
        assert_eq!(pids(None).len(), 2); // root view
        let jids = |org| {
            recent_jobs(&conn, 50, org, None)
                .unwrap()
                .iter()
                .map(|j| j.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(jids(Some(other)), [jb]);
        assert_eq!(jids(None).len(), 2);

        assert!(get_job(&conn, jb, Some(DEFAULT_ORG)).unwrap().is_none()); // cross-org read denied
        assert_eq!(
            get_job(&conn, jb, Some(other)).unwrap().unwrap().state,
            "queued"
        );
        assert!(get_job(&conn, jb, None).unwrap().is_some()); // root sees it

        assert_eq!(
            metrics(&conn, 60.0, None, Some(other))
                .unwrap()
                .printers_total,
            1
        );
        assert_eq!(
            metrics(&conn, 60.0, None, Some(other)).unwrap().jobs["queued"],
            1
        );
        assert_eq!(metrics(&conn, 60.0, None, None).unwrap().printers_total, 2);

        let foreign = try_enq(&conn, &json!(id_of(&b, "Z")), b"x", |j| {
            j.org_id = Some(DEFAULT_ORG)
        });
        assert!(matches!(foreign, Err(StoreError::UnknownPrinter(_)))); // foreign printer
        assert!(
            get_printer(&conn, &json!(id_of(&b, "Z")), Some(DEFAULT_ORG))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            get_printer(&conn, &json!(id_of(&b, "Z")), None)
                .unwrap()
                .unwrap()
                .org_id,
            other
        );
    }

    #[test]
    fn cancel_of_a_foreign_job_is_not_found_and_leaves_it_alone() {
        let conn = db();
        let (other, _a, b) = two_orgs(&conn);
        let jb = enq(&conn, id_of(&b, "Z"), b"b");
        assert_eq!(
            cancel_job(&conn, jb, Some(DEFAULT_ORG)).unwrap(),
            CancelOutcome::NotFound
        );
        assert_eq!(get_job(&conn, jb, None).unwrap().unwrap().state, "queued");
        assert_eq!(
            cancel_job(&conn, jb, Some(other)).unwrap(),
            CancelOutcome::Cancelled
        );
    }

    #[test]
    fn legacy_single_org_db_keeps_working_unchanged() {
        // A DB written before multi-tenancy has org_id=1 everywhere — org-1 callers still see it all.
        let conn = db();
        let r = reg(&conn, "old", "k", &[("Z", false)]);
        let jid = enq(&conn, id_of(&r, "Z"), b"x");
        let kid = add_api_key(&conn, "legacy", "legacy-key", DEFAULT_ORG).unwrap();
        assert_eq!(
            q::<i64>(&conn, "SELECT org_id FROM jobs WHERE id=?", [jid]),
            1
        );
        assert_eq!(
            authenticate_client(&conn, "legacy-key").unwrap(),
            Some(ClientKey { id: kid, org_id: 1 })
        );
        assert_eq!(
            get_job(&conn, jid, Some(1)).unwrap().unwrap().state,
            "queued"
        );
        assert_eq!(list_printers(&conn, 60.0, None, Some(1)).unwrap().len(), 1);
        assert_eq!(recent_jobs(&conn, 50, Some(1), None).unwrap().len(), 1);
        assert_eq!(
            claim_job(&conn, r.computer_id, None)
                .unwrap()
                .unwrap()
                .job_id,
            jid
        ); // agent path
    }

    // --- computer status + liveness events --------------------------------------------------

    #[test]
    fn list_agents_reports_liveness_printer_count_and_org_scope() {
        let conn = db();
        let (other, a, b) = two_orgs(&conn);
        let now = crate::now();
        conn.execute(
            "UPDATE agents SET last_seen_at=? WHERE id=?",
            params![now - 300.0, b.computer_id],
        )
        .unwrap();
        let scoped = list_agents(&conn, 60.0, Some(now), Some(other)).unwrap();
        assert_eq!(
            scoped.iter().map(|x| x.id).collect::<Vec<_>>(),
            [b.computer_id]
        );
        assert!(!scoped[0].online && scoped[0].printers == 1 && scoped[0].name == "pc");
        let root = list_agents(&conn, 60.0, Some(now), None).unwrap();
        assert_eq!(root.len(), 2);
        assert!(root.iter().find(|x| x.id == a.computer_id).unwrap().online);
    }

    #[test]
    fn agent_transitions_fire_once_per_liveness_edge() {
        let conn = db();
        set_org_event_url(&conn, DEFAULT_ORG, Some("https://hooks.example/x")).unwrap();
        let r = reg(&conn, "pc", "k", &[]);
        let now = crate::now();
        assert!(
            claim_agent_transitions(&conn, 60.0, Some(now))
                .unwrap()
                .is_empty()
        ); // just seen
        let late = now + 300.0;
        let evs = claim_agent_transitions(&conn, 60.0, Some(late)).unwrap();
        assert_eq!(
            evs.iter()
                .map(|e| (e.agent_id, e.event.as_str(), e.event_url.as_str()))
                .collect::<Vec<_>>(),
            [(r.computer_id, "offline", "https://hooks.example/x")]
        );
        assert!(evs[0].name == "pc" && evs[0].org_id == DEFAULT_ORG);
        assert!(
            claim_agent_transitions(&conn, 60.0, Some(late))
                .unwrap()
                .is_empty()
        ); // consumed
        reg(&conn, "pc", "k", &[]); // agent comes back
        let evs = claim_agent_transitions(&conn, 60.0, None).unwrap();
        assert_eq!(
            evs.iter().map(|e| e.event.as_str()).collect::<Vec<_>>(),
            ["online"]
        );
    }

    #[test]
    fn transitions_of_an_org_without_an_event_url_are_dropped_not_queued() {
        let conn = db();
        reg(&conn, "pc", "k", &[]);
        let late = crate::now() + 300.0;
        assert!(
            claim_agent_transitions(&conn, 60.0, Some(late))
                .unwrap()
                .is_empty()
        ); // nowhere to send
        set_org_event_url(&conn, DEFAULT_ORG, Some("https://hooks.example/x")).unwrap();
        assert!(
            claim_agent_transitions(&conn, 60.0, Some(late))
                .unwrap()
                .is_empty()
        ); // not replayed
        assert!(!set_org_event_url(&conn, 4242, Some("https://h")).unwrap()); // unknown org
    }

    // --- idempotency + expiry ---------------------------------------------------------------

    #[test]
    fn same_idempotency_key_enqueues_once_and_returns_the_first_job() {
        let conn = db();
        let r = reg(&conn, "pc", "k", &[("Z", false)]);
        let pid = json!(id_of(&r, "Z"));
        let keyed = |k: Option<&str>| {
            try_enq(&conn, &pid, b"a", |j| j.idempotency_key = k.map(Into::into)).unwrap()
        };
        let first = keyed(Some("order-42"));
        let again = keyed(Some("order-42"));
        assert!(first.created && !again.created);
        assert_eq!(
            (again.job_id, again.agent_id),
            (first.job_id, r.computer_id)
        );
        assert_eq!(recent_jobs(&conn, 50, None, None).unwrap().len(), 1); // no second print
        assert_ne!(keyed(Some("order-43")).job_id, first.job_id);
        assert_ne!(keyed(None).job_id, first.job_id); // no key -> no dedupe
    }

    #[test]
    fn idempotency_keys_are_scoped_to_the_org() {
        let conn = db();
        let (other, a, b) = two_orgs(&conn);
        let ja = enq_with(&conn, id_of(&a, "Z"), b"a", |j| {
            j.idempotency_key = Some("order-42".into())
        });
        let jb = enq_with(&conn, id_of(&b, "Z"), b"b", |j| {
            j.idempotency_key = Some("order-42".into());
            j.org_id = Some(other);
        });
        assert_ne!(ja, jb); // same key, different orgs
    }

    #[test]
    fn an_expired_job_is_never_claimed_and_the_reaper_fails_it() {
        let conn = db();
        let r = reg(&conn, "pc", "k", &[("Z", false)]);
        let (aid, pid) = (r.computer_id, id_of(&r, "Z"));
        let stale = enq_with(&conn, pid, b"a", |j| j.expire_after = Some(60));
        let fresh = enq(&conn, pid, b"b");
        let later = crate::now() + 61.0;
        assert_eq!(
            claim_job(&conn, aid, Some(later)).unwrap().unwrap().job_id,
            fresh
        ); // skips expired
        assert_eq!(expire_jobs(&conn, Some(later)).unwrap(), 1);
        let job = get_job(&conn, stale, None).unwrap().unwrap();
        assert!(job.state == "failed" && job.error.as_deref() == Some("expired"));
        assert_eq!(expire_jobs(&conn, Some(later)).unwrap(), 0); // already terminal
        assert_eq!(
            get_job(&conn, fresh, None).unwrap().unwrap().state,
            "claimed"
        ); // untouched
    }

    #[test]
    fn a_job_inside_its_window_still_prints() {
        let conn = db();
        let r = reg(&conn, "pc", "k", &[("Z", false)]);
        let jid = enq_with(&conn, id_of(&r, "Z"), b"a", |j| j.expire_after = Some(3600));
        assert_eq!(expire_jobs(&conn, None).unwrap(), 0);
        assert_eq!(
            claim_job(&conn, r.computer_id, None)
                .unwrap()
                .unwrap()
                .job_id,
            jid
        );
    }

    // --- users + sessions -------------------------------------------------------------------

    #[test]
    fn user_lookup_is_case_and_whitespace_insensitive() {
        let conn = db();
        let uid = user(&conn, "Ops@Shop.TEST", DEFAULT_ORG);
        let row = get_user_by_email(&conn, "  ops@shop.test ")
            .unwrap()
            .unwrap();
        assert!(row.id == uid && row.org_id == DEFAULT_ORG);
        assert_eq!(row.password_hash.as_deref(), Some("scrypt$stub"));
        assert!(
            get_user_by_email(&conn, "nobody@shop.test")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_second_user_with_the_same_email_is_refused() {
        let conn = db();
        user(&conn, "ops@shop.test", DEFAULT_ORG);
        let other = create_org(&conn, "other").unwrap();
        // even across orgs — login is by e-mail
        assert!(matches!(
            create_user(&conn, other, "OPS@shop.test", "h"),
            Err(StoreError::Conflict)
        ));
    }

    #[test]
    fn listing_users_is_org_scoped_and_never_carries_the_hash() {
        let conn = db();
        let other = create_org(&conn, "other").unwrap();
        user(&conn, "a@shop.test", DEFAULT_ORG);
        user(&conn, "b@other.test", other);
        let mine = list_users(&conn, Some(DEFAULT_ORG)).unwrap();
        assert!(
            mine.iter()
                .map(|u| u.email.as_deref())
                .eq([Some("a@shop.test")])
        );
        assert!(
            serde_json::to_value(&mine[0])
                .unwrap()
                .get("password_hash")
                .is_none()
        );
        assert_eq!(list_users(&conn, None).unwrap().len(), 2); // root sees every org
        assert_eq!(
            list_users(&conn, Some(other)).unwrap()[0].email.as_deref(),
            Some("b@other.test")
        );
    }

    #[test]
    fn the_seeded_legacy_user_has_no_password_and_cannot_be_found_by_email() {
        let conn = db();
        let (email, hash): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT email, password_hash FROM users WHERE id=?",
                [DEFAULT_USER],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(email.is_none() && hash.is_none());
        assert!(get_user_by_email(&conn, "").unwrap().is_none());
        assert!(get_user_by_email(&conn, "   ").unwrap().is_none());
    }

    #[test]
    fn a_session_authenticates_until_it_expires() {
        let conn = db();
        let uid = user(&conn, "ops@shop.test", DEFAULT_ORG);
        let now = crate::now();
        let expires = create_session(&conn, uid, "sess_a", 3600.0, Some(now)).unwrap();
        assert!((expires - (now + 3600.0)).abs() < 1e-6);
        let sess = authenticate_session(&conn, "sess_a", Some(now + 60.0))
            .unwrap()
            .unwrap();
        assert!(sess.user_id == uid && sess.org_id == DEFAULT_ORG);
        assert_eq!(sess.email.as_deref(), Some("ops@shop.test"));
        assert!(
            authenticate_session(&conn, "sess_a", Some(now + 3601.0))
                .unwrap()
                .is_none()
        ); // expired
        assert!(
            authenticate_session(&conn, "sess_nope", Some(now))
                .unwrap()
                .is_none()
        );
        assert!(
            authenticate_session(&conn, "", Some(now))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn logout_drops_the_session_row() {
        let conn = db();
        create_session(
            &conn,
            user(&conn, "ops@shop.test", DEFAULT_ORG),
            "sess_a",
            3600.0,
            None,
        )
        .unwrap();
        assert!(delete_session(&conn, "sess_a").unwrap());
        assert!(
            authenticate_session(&conn, "sess_a", None)
                .unwrap()
                .is_none()
        );
        assert!(!delete_session(&conn, "sess_a").unwrap());
    }

    #[test]
    fn changing_a_password_invalidates_that_users_sessions_only() {
        let conn = db();
        let (mine, theirs) = (
            user(&conn, "a@shop.test", DEFAULT_ORG),
            user(&conn, "b@shop.test", DEFAULT_ORG),
        );
        create_session(&conn, mine, "t_mine", 3600.0, None).unwrap();
        create_session(&conn, theirs, "t_theirs", 3600.0, None).unwrap();
        assert!(set_user_password(&conn, mine, "scrypt$new").unwrap());
        assert!(
            authenticate_session(&conn, "t_mine", None)
                .unwrap()
                .is_none()
        );
        assert!(
            authenticate_session(&conn, "t_theirs", None)
                .unwrap()
                .is_some()
        );
        let hash = get_user_by_email(&conn, "a@shop.test")
            .unwrap()
            .unwrap()
            .password_hash;
        assert_eq!(hash.as_deref(), Some("scrypt$new"));
        assert!(!set_user_password(&conn, 4242, "scrypt$whatever").unwrap());
    }

    #[test]
    fn the_reaper_purges_only_expired_sessions() {
        let conn = db();
        let uid = user(&conn, "ops@shop.test", DEFAULT_ORG);
        let now = crate::now();
        create_session(&conn, uid, "old", 10.0, Some(now)).unwrap();
        create_session(&conn, uid, "live", 3600.0, Some(now)).unwrap();
        assert_eq!(purge_expired_sessions(&conn, Some(now + 60.0)).unwrap(), 1);
        assert!(
            authenticate_session(&conn, "live", Some(now + 60.0))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn users_and_sessions_are_added_to_a_pre_accounts_database() {
        // A v2.2 deployment's DB: users without the account columns, no sessions table at all.
        let conn = connect(":memory:").unwrap();
        conn.execute_batch(
            "CREATE TABLE orgs(id INTEGER PRIMARY KEY, name TEXT NOT NULL, created_at REAL NOT NULL);\
             CREATE TABLE users(id INTEGER PRIMARY KEY, org_id INTEGER NOT NULL, name TEXT NOT NULL,\
              created_at REAL NOT NULL);\
             INSERT INTO orgs(id,name,created_at) VALUES(1,'default',0);\
             INSERT INTO users(id,org_id,name,created_at) VALUES(1,1,'default',0);",
        )
        .unwrap();
        init_db(&conn).unwrap();
        let uid = user(&conn, "ops@shop.test", DEFAULT_ORG);
        create_session(&conn, uid, "sess_a", 3600.0, None).unwrap();
        assert_eq!(
            authenticate_session(&conn, "sess_a", None)
                .unwrap()
                .unwrap()
                .user_id,
            uid
        );
        // INTEGER timestamps written by an older server read as floats
        assert_eq!(get_org(&conn, 1).unwrap().unwrap().created_at, 0.0);
    }

    #[test]
    fn revoking_a_key_is_org_scoped() {
        let conn = db();
        let other = create_org(&conn, "other").unwrap();
        let mine = add_api_key(&conn, "mine", "k1", DEFAULT_ORG).unwrap();
        let theirs = add_api_key(&conn, "theirs", "k2", other).unwrap();
        assert!(!revoke_api_key(&conn, theirs, Some(DEFAULT_ORG)).unwrap()); // not mine
        assert!(authenticate_client(&conn, "k2").unwrap().is_some()); // still valid
        assert!(revoke_api_key(&conn, mine, Some(DEFAULT_ORG)).unwrap());
        assert!(authenticate_client(&conn, "k1").unwrap().is_none());
        assert!(revoke_api_key(&conn, theirs, None).unwrap()); // root: no filter
    }

    // --- self-signup, user removal, password resets, quotas ---------------------------------

    #[test]
    fn signup_creates_an_org_and_its_first_user_in_one_transaction() {
        let conn = db();
        let r = signup(&conn, "Shop GmbH", "Owner@Shop.test", "scrypt$x").unwrap();
        assert!(r.org_id != DEFAULT_ORG && r.user_id != DEFAULT_USER);
        assert_eq!(get_org(&conn, r.org_id).unwrap().unwrap().name, "Shop GmbH");
        let u = get_user_by_email(&conn, "owner@shop.test")
            .unwrap()
            .unwrap();
        assert!(u.org_id == r.org_id && u.id == r.user_id);
    }

    #[test]
    fn signup_with_a_taken_email_leaves_no_orphan_org() {
        let conn = db();
        signup(&conn, "First", "owner@shop.test", "h").unwrap();
        let before = list_orgs(&conn).unwrap().len();
        assert!(matches!(
            signup(&conn, "Second", "Owner@shop.TEST", "h"),
            Err(StoreError::Conflict)
        ));
        assert_eq!(list_orgs(&conn).unwrap().len(), before); // the org went back with the failed user
    }

    #[test]
    fn delete_user_is_org_scoped_and_drops_that_users_sessions() {
        let conn = db();
        let other = create_org(&conn, "other").unwrap();
        let mine = user(&conn, "a@shop.test", DEFAULT_ORG);
        user(&conn, "b@shop.test", DEFAULT_ORG); // so `mine` is not the last one
        let theirs = user(&conn, "c@other.test", other);
        create_session(&conn, mine, "tok", 3600.0, None).unwrap();
        assert_eq!(
            delete_user(&conn, theirs, Some(DEFAULT_ORG)).unwrap(),
            DeleteUser::NotFound
        );
        assert_eq!(
            delete_user(&conn, 999999, None).unwrap(),
            DeleteUser::NotFound
        );
        assert_eq!(
            delete_user(&conn, mine, Some(DEFAULT_ORG)).unwrap(),
            DeleteUser::Deleted
        );
        assert!(get_user_by_email(&conn, "a@shop.test").unwrap().is_none());
        assert!(authenticate_session(&conn, "tok", None).unwrap().is_none()); // died with the user
    }

    #[test]
    fn an_org_cannot_delete_its_last_account() {
        let conn = db();
        let only = user(&conn, "solo@shop.test", DEFAULT_ORG);
        assert_eq!(
            delete_user(&conn, only, Some(DEFAULT_ORG)).unwrap(),
            DeleteUser::LastUser
        );
        assert_eq!(
            delete_user(&conn, only, None).unwrap(),
            DeleteUser::LastUser
        ); // root neither
        assert!(
            get_user_by_email(&conn, "solo@shop.test")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_password_reset_token_sets_the_password_once_and_kills_sessions() {
        let conn = db();
        let uid = user(&conn, "ops@shop.test", DEFAULT_ORG);
        create_session(&conn, uid, "sess_a", 3600.0, None).unwrap();
        create_password_reset(&conn, uid, "reset_a", 3600.0, None).unwrap();
        assert_eq!(
            consume_password_reset(&conn, Some("reset_a"), "scrypt$new", None).unwrap(),
            Some(uid)
        );
        let hash = get_user_by_email(&conn, "ops@shop.test")
            .unwrap()
            .unwrap()
            .password_hash;
        assert_eq!(hash.as_deref(), Some("scrypt$new"));
        assert!(
            authenticate_session(&conn, "sess_a", None)
                .unwrap()
                .is_none()
        ); // sessions die
        assert_eq!(
            consume_password_reset(&conn, Some("reset_a"), "scrypt$new", None).unwrap(),
            None
        ); // single use
    }

    #[test]
    fn an_expired_or_unknown_reset_token_does_nothing() {
        let conn = db();
        let uid = user(&conn, "ops@shop.test", DEFAULT_ORG);
        let now = crate::now();
        create_password_reset(&conn, uid, "reset_a", 60.0, Some(now)).unwrap();
        let hash = || {
            get_user_by_email(&conn, "ops@shop.test")
                .unwrap()
                .unwrap()
                .password_hash
        };
        let kept = hash();
        assert_eq!(
            consume_password_reset(&conn, Some("reset_a"), "scrypt$x", Some(now + 61.0)).unwrap(),
            None
        );
        assert_eq!(
            consume_password_reset(&conn, Some("sess_nonesuch"), "scrypt$x", None).unwrap(),
            None
        );
        assert_eq!(
            consume_password_reset(&conn, Some(""), "scrypt$x", None).unwrap(),
            None
        );
        assert_eq!(
            consume_password_reset(&conn, None, "scrypt$x", None).unwrap(),
            None
        );
        assert_eq!(hash(), kept);
        assert_eq!(purge_expired_resets(&conn, Some(now + 61.0)).unwrap(), 1);
    }

    #[test]
    fn a_new_reset_request_supersedes_the_previous_token() {
        let conn = db();
        let uid = user(&conn, "ops@shop.test", DEFAULT_ORG);
        create_password_reset(&conn, uid, "first", 3600.0, None).unwrap();
        create_password_reset(&conn, uid, "second", 3600.0, None).unwrap();
        assert_eq!(
            consume_password_reset(&conn, Some("first"), "h1", None).unwrap(),
            None
        );
        assert_eq!(
            consume_password_reset(&conn, Some("second"), "h2", None).unwrap(),
            Some(uid)
        );
    }

    #[test]
    fn a_job_quota_caps_an_orgs_prints_for_the_month() {
        let conn = db();
        let pid = id_of(&reg(&conn, "a", "k", &[("Z", false)]), "Z");
        assert!(set_org_quota(&conn, DEFAULT_ORG, Some(2)).unwrap());
        enq(&conn, pid, b"a");
        enq(&conn, pid, b"b");
        match try_enq(&conn, &json!(pid), b"c", |_| {}) {
            Err(StoreError::QuotaExceeded(m)) => assert_eq!(m, "monthly job quota reached (2)"),
            other => panic!("expected QuotaExceeded, got {other:?}"),
        }
        assert_eq!(org_usage(&conn, DEFAULT_ORG, None).unwrap(), 2);
        assert_eq!(
            get_org(&conn, DEFAULT_ORG).unwrap().unwrap().job_quota,
            Some(2)
        );
        set_org_quota(&conn, DEFAULT_ORG, None).unwrap(); // None = unlimited again
        assert!(enq(&conn, pid, b"d") > 0);
    }

    #[test]
    fn quota_counts_only_this_month_and_only_this_org() {
        let conn = db();
        let other = create_org(&conn, "other").unwrap();
        let pid = id_of(&reg(&conn, "a", "k", &[("Z", false)]), "Z");
        let opid = id_of(&reg_in(&conn, "b", "k2", &[("Z", false)], other), "Z");
        set_org_quota(&conn, DEFAULT_ORG, Some(1)).unwrap();
        enq(&conn, pid, b"a");
        enq(&conn, opid, b"b"); // another org, own budget
        enq(&conn, opid, b"c");
        assert!(matches!(
            try_enq(&conn, &json!(pid), b"d", |_| {}),
            Err(StoreError::QuotaExceeded(_))
        ));
        conn.execute(
            "UPDATE jobs SET created_at=? WHERE printer_id=?",
            params![month_start(crate::now()) - 1.0, pid],
        )
        .unwrap(); // last month's job
        assert_eq!(org_usage(&conn, DEFAULT_ORG, None).unwrap(), 0);
        assert!(enq(&conn, pid, b"e") > 0); // budget rolled over
    }

    #[test]
    fn an_idempotent_resubmit_does_not_spend_quota() {
        let conn = db();
        let pid = id_of(&reg(&conn, "a", "k", &[("Z", false)]), "Z");
        set_org_quota(&conn, DEFAULT_ORG, Some(1)).unwrap();
        let key = |j: &mut NewJob<'_>| j.idempotency_key = Some("order-7".into());
        let jid = enq_with(&conn, pid, b"a", key);
        assert_eq!(enq_with(&conn, pid, b"a", key), jid);
    }

    // --- org settings, deletion, small helpers (store-level parts of the HTTP tests) --------

    #[test]
    fn org_setters_report_whether_the_org_exists_and_list_orgs_hides_the_secret() {
        let conn = db();
        assert!(set_org_shopify_secret(&conn, DEFAULT_ORG, Some("s3cr3t")).unwrap());
        assert!(!set_org_shopify_secret(&conn, 4242, Some("x")).unwrap());
        assert_eq!(
            get_org(&conn, DEFAULT_ORG)
                .unwrap()
                .unwrap()
                .shopify_secret
                .as_deref(),
            Some("s3cr3t")
        );
        let summary = &list_orgs(&conn).unwrap()[0];
        assert!(summary.shopify_secret_set);
        assert!(
            serde_json::to_value(summary)
                .unwrap()
                .get("shopify_secret")
                .is_none()
        );
        set_org_shopify_secret(&conn, DEFAULT_ORG, None).unwrap();
        assert!(!list_orgs(&conn).unwrap()[0].shopify_secret_set);
        assert!(set_org_plan(&conn, DEFAULT_ORG, "pro", Some(500)).unwrap());
        let org = get_org(&conn, DEFAULT_ORG).unwrap().unwrap();
        assert_eq!(
            (org.plan.as_deref(), org.job_quota),
            (Some("pro"), Some(500))
        );
        assert!(!set_org_plan(&conn, 4242, "pro", None).unwrap());
        assert!(get_org(&conn, 4242).unwrap().is_none());
    }

    #[test]
    fn delete_org_removes_everything_that_belongs_to_it() {
        let conn = db();
        let gone = create_org(&conn, "leaving").unwrap();
        let r = reg_in(&conn, "pc", "agentkey", &[("HP", true)], gone);
        enq_with(&conn, id_of(&r, "HP"), b"x", |j| j.org_id = Some(gone));
        add_api_key(&conn, "k", "key", gone).unwrap();
        let uid = user(&conn, "bye@shop.test", gone);
        create_session(&conn, uid, "tok", 3600.0, None).unwrap();
        create_password_reset(&conn, uid, "reset", 3600.0, None).unwrap();
        let keep = reg(&conn, "keep", "keepkey", &[("Z", false)]);
        assert!(delete_org(&conn, gone).unwrap());
        assert!(!delete_org(&conn, gone).unwrap());
        for table in [
            "jobs",
            "printers",
            "agents",
            "api_keys",
            "sessions",
            "password_resets",
        ] {
            let left: i64 = q(
                &conn,
                &format!(
                    "SELECT COUNT(*) FROM {table} WHERE {}",
                    if table == "sessions" || table == "password_resets" {
                        format!("user_id={uid}")
                    } else {
                        format!("org_id={gone}")
                    }
                ),
                [],
            );
            assert_eq!(left, 0, "{table}");
        }
        assert!(get_user_by_email(&conn, "bye@shop.test").unwrap().is_none());
        assert_eq!(
            list_orgs(&conn)
                .unwrap()
                .iter()
                .map(|o| o.id)
                .collect::<Vec<_>>(),
            [DEFAULT_ORG]
        );
        assert_eq!(
            authenticate_agent(&conn, "keepkey").unwrap(),
            Some(keep.computer_id)
        );
    }

    #[test]
    fn hash_key_is_lowercase_sha256_hex() {
        assert_eq!(
            hash_key("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hash_key("").len(), 64);
    }

    #[test]
    fn init_db_refuses_duplicate_emails_without_the_unique_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dup.sqlite3");
        let path = path.to_str().unwrap();
        {
            let conn = connect(path).unwrap();
            init_db(&conn).unwrap();
            conn.execute_batch(
                "DROP INDEX users_email;
                 INSERT INTO users(org_id, name, email, password_hash, created_at)
                   VALUES(1,'a','dup@x.test','h',0),(1,'b','dup@x.test','h',0);",
            )
            .unwrap();
        }
        assert!(matches!(Db::open(path), Err(StoreError::Conflict)));
    }

    #[test]
    fn printer_id_padded_with_non_ascii_whitespace_is_unknown() {
        let conn = db();
        reg(&conn, "w", "k", &[("Z", false)]);
        for pad in ["\u{a0}1", "\u{3000}1", "\u{85}1", "1\u{2028}"] {
            let raw = json!(pad);
            assert!(try_enq(&conn, &raw, b"x", |_| {}).is_err(), "{pad:?}");
            assert!(get_printer(&conn, &raw, None).unwrap().is_none(), "{pad:?}");
        }
        for pad in ["\x0b1", "\x0c1", " 1 "] {
            let raw = json!(pad);
            assert!(try_enq(&conn, &raw, b"x", |_| {}).is_ok(), "{pad:?}");
            assert!(get_printer(&conn, &raw, None).unwrap().is_some(), "{pad:?}");
        }
    }

    #[test]
    fn non_scalar_printer_id_is_unknown_printer_not_a_crash() {
        // deliberate deviation: Python died with ProgrammingError/OverflowError (no response)
        let conn = db();
        reg(&conn, "w", "k", &[("Z", false)]);
        for (raw, text) in [
            (json!([1]), "unknown printer: [1]"),
            (json!({"a": 1}), "unknown printer: {'a': 1}"),
            (
                serde_json::from_str::<Value>("100000000000000000000").unwrap(),
                "unknown printer: 1e+20",
            ),
        ] {
            match try_enq(&conn, &raw, b"x", |_| {}) {
                Err(StoreError::UnknownPrinter(m)) => assert_eq!(m, text),
                other => panic!("expected UnknownPrinter, got {other:?}"),
            }
            assert!(get_printer(&conn, &raw, None).unwrap().is_none());
        }
    }

    #[test]
    fn emails_are_stripped_like_python_including_u001c_to_u001f() {
        let conn = db();
        let id = create_user(&conn, DEFAULT_ORG, "boss@x.test", "h").unwrap();
        for e in ["boss@x.test\u{1f}", "\u{1c}boss@x.test", " BOSS@x.test\n"] {
            assert_eq!(
                get_user_by_email(&conn, e).unwrap().unwrap().id,
                id,
                "{e:?}"
            );
        }
        let id2 = create_user(&conn, DEFAULT_ORG, "\u{1d}Case@Y.test\u{1f}", "h").unwrap();
        assert_eq!(
            get_user_by_email(&conn, "case@y.test").unwrap().unwrap().id,
            id2
        );
    }

    #[test]
    fn auth_error_escapes_non_printable_name_like_python_repr() {
        let conn = db();
        reg(&conn, "\u{3000}pc", "k1", &[]);
        match register_agent(&conn, "\u{3000}pc", "other", &[], DEFAULT_ORG) {
            Err(StoreError::Auth(m)) => assert_eq!(
                m,
                "agent name already registered with a different key: '\\u3000pc'"
            ),
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[test]
    fn capabilities_json_python_accepts_but_serde_rejects_read_as_none() {
        // deliberate cross-DB deviation: a Python-written NaN/Infinity capabilities object
        // (json.dumps emits it, json.loads reads it) is dropped to None, not an error.
        let conn = db();
        let r = reg(&conn, "w", "k", &[("Z", false)]);
        conn.execute(
            "UPDATE printers SET capabilities='{\"dpi\": NaN, \"max\": Infinity}' WHERE id=?",
            [id_of(&r, "Z")],
        )
        .unwrap();
        let ps = list_printers(&conn, 60.0, None, None).unwrap();
        assert_eq!(ps[0].capabilities, None);
    }

    #[test]
    fn month_start_is_the_utc_calendar_month() {
        assert_eq!(month_start(1_700_000_000.0), 1_698_796_800.0); // 2023-11-14 -> 2023-11-01
        assert_eq!(month_start(1_698_796_800.0), 1_698_796_800.0); // already the first instant
        assert_eq!(month_start(1_698_796_799.9), 1_696_118_400.0); // one tick earlier: 2023-10-01
        // sub-microsecond below the boundary rounds up into the new month, like Python
        assert_eq!(month_start(1_698_796_799.999_999_6), 1_698_796_800.0);
        assert_eq!(month_start(f64::NAN), 0.0);
    }

    // --- tests/test_cloudprnt.py (store level) ----------------------------------------------

    #[test]
    fn register_cloudprnt_upserts_one_raw_only_printer() {
        let conn = db();
        let a = register_cloudprnt(&conn, DEFAULT_ORG, "cloudprnt-mac1").unwrap();
        let b = register_cloudprnt(&conn, DEFAULT_ORG, "cloudprnt-mac1").unwrap();
        assert_eq!(a, b); // a second poll is the same device
        let printers = list_printers(&conn, 60.0, None, None).unwrap();
        assert_eq!(printers.len(), 1);
        assert_eq!(printers[0].id, a.printer_id);
        assert!(!printers[0].can_pdf); // gotcha #1: no PDF renderer in the printer
        assert!(printers[0].online); // polling is what liveness means here
        // the same MAC in another org is a different device, not a hijack
        let other =
            register_cloudprnt(&conn, create_org(&conn, "other").unwrap(), "cloudprnt-mac1")
                .unwrap();
        assert_ne!(other.printer_id, a.printer_id);
    }

    #[test]
    fn a_cloudprnt_agent_key_is_opaque_and_unauthenticatable() {
        let conn = db();
        let a = register_cloudprnt(&conn, DEFAULT_ORG, "cloudprnt-mac1").unwrap();
        let b = register_cloudprnt(&conn, DEFAULT_ORG, "cloudprnt-mac2").unwrap();
        let hash = |id| q::<String>(&conn, "SELECT api_key_hash FROM agents WHERE id=?", [id]);
        let (ha, hb) = (hash(a.agent_id), hash(b.agent_id));
        for h in [&ha, &hb] {
            let tail = h.strip_prefix("cloudprnt:").unwrap();
            assert!(tail.len() == 64 && tail.bytes().all(|c| c.is_ascii_hexdigit()));
        }
        assert_ne!(ha, hb);
        // nothing can present this row's "key": authenticate_agent hashes its input to 64 hex chars
        assert!(authenticate_agent(&conn, &ha).unwrap().is_none());
    }

    #[test]
    fn claimed_job_reports_the_job_in_flight() {
        let conn = db();
        let dev = register_cloudprnt(&conn, DEFAULT_ORG, "cloudprnt-mac1").unwrap();
        let jid = enq_with(&conn, dev.printer_id, b"ABC", |j| j.copies = 3);
        assert!(claimed_job(&conn, dev.agent_id).unwrap().is_none()); // still queued
        claim_job(&conn, dev.agent_id, None).unwrap();
        assert_eq!(
            claimed_job(&conn, dev.agent_id).unwrap(),
            Some(InFlight {
                job_id: jid,
                mode: "raw".into(),
                copies: 3
            })
        );
        finish_job(&conn, jid, dev.agent_id, true, None).unwrap();
        assert!(claimed_job(&conn, dev.agent_id).unwrap().is_none()); // terminal again
    }

    // --- indexes, concurrency ---------------------------------------------------------------

    #[test]
    fn claim_job_uses_jobs_claim_and_the_quota_count_uses_jobs_org_created() {
        let conn = db();
        let claim = plan(&conn, CLAIM_SQL, params![1, 0.0]);
        assert!(claim.contains("USING INDEX jobs_claim"), "{claim}");
        assert!(!claim.contains("TEMP B-TREE"), "{claim}"); // ORDER BY comes for free
        let usage = plan(&conn, USAGE_SQL, params![1, 0.0]);
        assert!(usage.contains("jobs_org_created"), "{usage}");
        let reaper = plan(
            &conn,
            "UPDATE jobs SET state='queued' WHERE state='claimed' AND claimed_at < ?1",
            [0.0],
        );
        assert!(reaper.contains("jobs_state_claimed"), "{reaper}");
        let hooks = plan(&conn, HOOKS_SQL, [5]);
        assert!(
            hooks.contains("jobs_hooks") && !hooks.contains("TEMP B-TREE"),
            "{hooks}"
        );
        // no printers index: the UNIQUE(agent_id, name) autoindex already serves agent_id lookups
        let by_agent = plan(&conn, "SELECT id FROM printers WHERE agent_id=?1", [1]);
        assert!(
            by_agent.contains("sqlite_autoindex_printers_1"),
            "{by_agent}"
        );
    }

    #[test]
    fn concurrent_claims_never_hand_out_a_job_twice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conc.sqlite3");
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let (aid, pid) = db.with(|c| {
            let r = reg(c, "w", "k", &[("Z", false)]);
            (r.computer_id, id_of(&r, "Z"))
        });
        const JOBS: usize = 120;
        db.with(|c| {
            for n in 0..JOBS {
                enq(c, pid, format!("job-{n}").as_bytes());
            }
        });
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let db = db.clone();
                std::thread::spawn(move || {
                    let mut mine = Vec::new();
                    while let Some(job) = db.with(|c| claim_job(c, aid, None)).unwrap() {
                        mine.push(job.job_id);
                    }
                    mine
                })
            })
            .collect();
        let mut all: Vec<i64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        assert_eq!(all.len(), JOBS);
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), JOBS, "a job was claimed twice");
    }

    // --- label templates --------------------------------------------------------------------

    fn label_spec(name: &str) -> crate::label::Spec {
        let v = json!({
            "name": name, "content": "image", "label_mm": {"w": 50, "h": 30}, "dpi": 203,
            "rotate": 0, "crop_percent": {"t": 0, "r": 0, "b": 0, "l": 0},
            "place_mm": {"x": 0, "y": 0, "w": 50, "h": 30}
        });
        crate::label::parse_spec(v.as_object().unwrap()).unwrap()
    }

    #[test]
    fn label_templates_are_scoped_to_their_org() {
        let conn = db();
        let other = create_org(&conn, "other").unwrap();
        let mine = create_label_template(&conn, None, &label_spec("a")).unwrap();
        // None is the default org, on every path
        assert_eq!(mine.org_id, DEFAULT_ORG);
        assert_eq!(
            get_label_template(&conn, mine.id, Some(DEFAULT_ORG)).unwrap(),
            Some(mine.clone())
        );
        assert_eq!(
            get_label_template(&conn, mine.id, None).unwrap(),
            Some(mine.clone())
        );
        assert_eq!(
            get_label_template(&conn, mine.id, Some(other)).unwrap(),
            None
        );
        assert_eq!(find_label_template(&conn, "a", Some(other)).unwrap(), None);
        assert_eq!(
            find_label_template(&conn, "a", None).unwrap().unwrap().id,
            mine.id
        );
        assert!(list_label_templates(&conn, Some(other)).unwrap().is_empty());
        assert!(
            update_label_template(&conn, mine.id, Some(other), &label_spec("b"))
                .unwrap()
                .is_none()
        );
        assert!(!delete_label_template(&conn, mine.id, Some(other)).unwrap());
        assert!(delete_label_template(&conn, mine.id, None).unwrap());
    }

    #[test]
    fn label_template_names_are_unique_per_org() {
        let conn = db();
        let other = create_org(&conn, "other").unwrap();
        let a = create_label_template(&conn, None, &label_spec("a")).unwrap();
        let b = create_label_template(&conn, None, &label_spec("b")).unwrap();
        assert!(matches!(
            create_label_template(&conn, None, &label_spec("a")),
            Err(StoreError::Conflict)
        ));
        assert!(matches!(
            update_label_template(&conn, b.id, None, &label_spec("a")),
            Err(StoreError::Conflict)
        ));
        create_label_template(&conn, Some(other), &label_spec("a")).unwrap();
        // renaming keeps id and creation time, and moves the update time
        let renamed = update_label_template(&conn, a.id, None, &label_spec("c"))
            .unwrap()
            .unwrap();
        assert_eq!((renamed.id, renamed.spec.name.as_str()), (a.id, "c"));
        assert_eq!(renamed.created_at, a.created_at);
        assert!(renamed.updated_at >= a.updated_at);
        let names: Vec<_> = list_label_templates(&conn, None)
            .unwrap()
            .into_iter()
            .map(|t| t.spec.name)
            .collect();
        assert_eq!(names, ["b", "c"]);
    }

    #[test]
    fn delete_org_takes_its_label_templates() {
        let conn = db();
        let other = create_org(&conn, "other").unwrap();
        create_label_template(&conn, Some(other), &label_spec("a")).unwrap();
        create_label_template(&conn, None, &label_spec("a")).unwrap();
        assert!(delete_org(&conn, other).unwrap());
        assert!(list_label_templates(&conn, Some(other)).unwrap().is_empty());
        assert_eq!(list_label_templates(&conn, None).unwrap().len(), 1);
    }

    #[test]
    fn only_a_cloudprnt_agent_counts_as_cloudprnt() {
        let conn = db();
        let dev = register_cloudprnt(&conn, DEFAULT_ORG, "cloudprnt-mac1").unwrap();
        let reg = register_agent(
            &conn,
            "pc",
            "ak",
            &[PrinterDecl {
                name: "Z".into(),
                can_pdf: false,
                capabilities: None,
            }],
            DEFAULT_ORG,
        )
        .unwrap();
        assert!(is_cloudprnt_agent(&conn, dev.agent_id).unwrap());
        assert!(!is_cloudprnt_agent(&conn, reg.computer_id).unwrap());
        assert!(!is_cloudprnt_agent(&conn, 9999).unwrap());
    }
}
