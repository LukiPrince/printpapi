# printpapi — self-hosted PrintNode alternative. MIT License (see LICENSE).
import hashlib
import hmac
import json
import sqlite3
import threading
import time

DEFAULT_ORG = 1
DEFAULT_USER = 1

_LOCK = threading.Lock()  # ponytail: global lock; per-connection pool only if it bottlenecks.

_SCHEMA = """
CREATE TABLE IF NOT EXISTS orgs(
  id INTEGER PRIMARY KEY, name TEXT NOT NULL, event_url TEXT, shopify_secret TEXT,
  created_at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS users(
  id INTEGER PRIMARY KEY, org_id INTEGER NOT NULL, name TEXT NOT NULL, created_at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS agents(
  id INTEGER PRIMARY KEY AUTOINCREMENT, org_id INTEGER NOT NULL, name TEXT NOT NULL,
  api_key_hash TEXT NOT NULL UNIQUE, last_seen_at REAL,
  offline_notified INTEGER NOT NULL DEFAULT 0, created_at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS printers(
  id INTEGER PRIMARY KEY AUTOINCREMENT, org_id INTEGER NOT NULL, agent_id INTEGER NOT NULL,
  name TEXT NOT NULL, can_pdf INTEGER NOT NULL DEFAULT 0, state TEXT NOT NULL DEFAULT 'active',
  capabilities TEXT, created_at REAL NOT NULL, UNIQUE(agent_id, name));
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
"""


def connect(path):
    conn = sqlite3.connect(path, check_same_thread=False)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA journal_mode=WAL")
    return conn


def init_db(conn):
    now = time.time()
    with _LOCK:
        try:
            conn.executescript(_SCHEMA)
            for ddl in ("ALTER TABLE printers ADD COLUMN capabilities TEXT",
                        "ALTER TABLE jobs ADD COLUMN title TEXT",
                        "ALTER TABLE jobs ADD COLUMN copies INTEGER NOT NULL DEFAULT 1",
                        "ALTER TABLE jobs ADD COLUMN options TEXT",
                        "ALTER TABLE jobs ADD COLUMN callback_url TEXT",
                        "ALTER TABLE jobs ADD COLUMN hook_attempts INTEGER NOT NULL DEFAULT 0",
                        "ALTER TABLE jobs ADD COLUMN hook_delivered_at REAL",
                        "ALTER TABLE jobs ADD COLUMN idempotency_key TEXT",
                        "ALTER TABLE jobs ADD COLUMN expires_at REAL",
                        "CREATE UNIQUE INDEX IF NOT EXISTS jobs_idem "
                        "ON jobs(org_id, idempotency_key)",
                        "ALTER TABLE orgs ADD COLUMN event_url TEXT",
                        "ALTER TABLE orgs ADD COLUMN shopify_secret TEXT",
                        "ALTER TABLE agents ADD COLUMN offline_notified "
                        "INTEGER NOT NULL DEFAULT 0"):
                try:
                    conn.execute(ddl)
                except sqlite3.OperationalError:
                    pass  # column already exists (fresh DB or prior run) — idempotent
            if conn.execute("SELECT 1 FROM orgs WHERE id=?", (DEFAULT_ORG,)).fetchone() is None:
                conn.execute("INSERT INTO orgs(id, name, created_at) VALUES(?,?,?)",
                             (DEFAULT_ORG, "default", now))
            if conn.execute("SELECT 1 FROM users WHERE id=?", (DEFAULT_USER,)).fetchone() is None:
                conn.execute("INSERT INTO users(id, org_id, name, created_at) VALUES(?,?,?,?)",
                             (DEFAULT_USER, DEFAULT_ORG, "default", now))
            conn.commit()
        except Exception:
            conn.rollback()
            raise


class AuthError(Exception):
    pass


# Multi-tenancy: every org-owning query carries an `org_id`, and `org_id=None` means *root* —
# no filter, all orgs. Written as `(:org IS NULL OR x.org_id = :org)` so one statement serves
# both without string-building. Legacy DBs are entirely org_id=1, so org-1 callers see exactly
# what they saw before.


def create_org(conn, name):
    now = time.time()
    with _LOCK:
        try:
            cur = conn.execute("INSERT INTO orgs(name, created_at) VALUES(?,?)", (name, now))
            conn.commit()
            return cur.lastrowid
        except Exception:
            conn.rollback()
            raise


def list_orgs(conn):
    """Orgs for the root listing. The Shopify webhook secret is reported as a flag, never echoed —
    it is the one credential here that has to be stored in plaintext (HMAC needs it)."""
    with _LOCK:
        rows = conn.execute("SELECT id, name, event_url, shopify_secret, created_at "
                            "FROM orgs ORDER BY id").fetchall()
    return [{"id": r["id"], "name": r["name"], "event_url": r["event_url"],
             "shopify_secret_set": bool(r["shopify_secret"]), "created_at": r["created_at"]}
            for r in rows]


def get_org(conn, org_id):
    """Full org row including secrets — for server-side use (HMAC verification), not for output."""
    with _LOCK:
        row = conn.execute("SELECT id, name, event_url, shopify_secret, created_at FROM orgs "
                           "WHERE id=?", (org_id,)).fetchone()
    return dict(row) if row else None


def _set_org_field(conn, org_id, column, value):
    # `column` is one of this module's own literals, never client input.
    with _LOCK:
        try:
            cur = conn.execute(f"UPDATE orgs SET {column}=? WHERE id=?", (value, org_id))
            conn.commit()
            return cur.rowcount == 1
        except Exception:
            conn.rollback()
            raise


def set_org_event_url(conn, org_id, url):
    """Where this org's agent liveness events go (None clears it). False if no such org."""
    return _set_org_field(conn, org_id, "event_url", url)


def set_org_shopify_secret(conn, org_id, secret):
    """The org's Shopify webhook signing secret (None clears it). False if no such org."""
    return _set_org_field(conn, org_id, "shopify_secret", secret)


def org_exists(conn, org_id):
    with _LOCK:
        return conn.execute("SELECT 1 FROM orgs WHERE id=?", (org_id,)).fetchone() is not None


def _hash_key(api_key):
    return hashlib.sha256(api_key.encode()).hexdigest()


def register_agent(conn, name, api_key, printers, org_id=DEFAULT_ORG):
    now = time.time()
    key_hash = _hash_key(api_key)
    for p in printers:
        if "name" not in p:
            raise ValueError(f"printer entry missing 'name': {p!r}")
    with _LOCK:
        try:
            row = conn.execute("SELECT id, api_key_hash FROM agents WHERE org_id=? AND name=?",
                               (org_id, name)).fetchone()
            if row:
                if not hmac.compare_digest(row["api_key_hash"], key_hash):
                    raise AuthError(f"agent name already registered with a different key: {name!r}")
                agent_id = row["id"]
                conn.execute("UPDATE agents SET last_seen_at=? WHERE id=?", (now, agent_id))
            else:
                cur = conn.execute(
                    "INSERT INTO agents(org_id, name, api_key_hash, last_seen_at, created_at) "
                    "VALUES(?,?,?,?,?)", (org_id, name, key_hash, now, now))
                agent_id = cur.lastrowid
            printer_ids = {}
            for p in printers:
                can_pdf = 1 if p.get("can_pdf") else 0
                caps = json.dumps(p["capabilities"]) if p.get("capabilities") else None
                r = conn.execute("SELECT id FROM printers WHERE agent_id=? AND name=?",
                                 (agent_id, p["name"])).fetchone()
                if r:
                    pid = r["id"]
                    conn.execute("UPDATE printers SET can_pdf=?, capabilities=? WHERE id=?",
                                 (can_pdf, caps, pid))
                else:
                    cur = conn.execute(
                        "INSERT INTO printers(org_id, agent_id, name, can_pdf, capabilities, "
                        "created_at) VALUES(?,?,?,?,?,?)",
                        (org_id, agent_id, p["name"], can_pdf, caps, now))
                    pid = cur.lastrowid
                printer_ids[p["name"]] = pid
            conn.commit()
        except Exception:
            conn.rollback()
            raise
    return {"computer_id": agent_id, "printer_ids": printer_ids}


def authenticate_agent(conn, api_key):
    key_hash = _hash_key(api_key)
    # api_key is a high-entropy token; sha256 lookup has no practical timing oracle, so an indexed equality match is fine here.
    with _LOCK:
        row = conn.execute("SELECT id FROM agents WHERE api_key_hash=?", (key_hash,)).fetchone()
    return row["id"] if row else None


def add_api_key(conn, label, key, org_id=DEFAULT_ORG):
    now = time.time()
    key_hash = _hash_key(key)
    with _LOCK:
        try:
            cur = conn.execute(
                "INSERT INTO api_keys(org_id, label, key_hash, active, created_at) "
                "VALUES(?,?,?,1,?)", (org_id, label, key_hash, now))
            conn.commit()
            return cur.lastrowid
        except Exception:
            conn.rollback()
            raise


def authenticate_client(conn, key):
    """Resolve a client key to {'id', 'org_id'} — the org every request with it is confined to."""
    if not key:
        return None
    key_hash = _hash_key(key)
    # High-entropy key; sha256 lookup has no practical timing oracle (see authenticate_agent).
    with _LOCK:
        row = conn.execute("SELECT id, org_id FROM api_keys WHERE key_hash=? AND active=1",
                           (key_hash,)).fetchone()
    return dict(row) if row else None


def list_api_keys(conn, org_id=None):
    with _LOCK:
        rows = conn.execute(
            "SELECT id, org_id, label, active, created_at FROM api_keys "
            "WHERE (:org IS NULL OR org_id = :org) ORDER BY id", {"org": org_id}).fetchall()
    return [dict(r) for r in rows]


def revoke_api_key(conn, key_id):
    with _LOCK:
        try:
            cur = conn.execute("UPDATE api_keys SET active=0 WHERE id=?", (key_id,))
            conn.commit()
            return cur.rowcount == 1
        except Exception:
            conn.rollback()
            raise


class UnknownPrinter(Exception):
    pass


def get_printer(conn, printer_id, org_id=None):
    """One printer, org-filtered — another org's printer reads as missing, never as a 403."""
    with _LOCK:
        row = conn.execute("SELECT id, name, org_id, agent_id, can_pdf FROM printers "
                           "WHERE id=:pid AND (:org IS NULL OR org_id = :org)",
                           {"pid": printer_id, "org": org_id}).fetchone()
    return dict(row, can_pdf=bool(row["can_pdf"])) if row else None


def enqueue_job(conn, printer_id, type_, mode, payload, user_id=DEFAULT_USER, title=None, copies=1,
                callback_url=None, options=None, org_id=None, idempotency_key=None,
                expire_after=None):
    now = time.time()
    with _LOCK:
        try:
            # A foreign printer is simply unknown — same error as a nonexistent one, no leak.
            p = conn.execute("SELECT org_id, agent_id FROM printers "
                             "WHERE id=:pid AND (:org IS NULL OR org_id = :org)",
                             {"pid": printer_id, "org": org_id}).fetchone()
            if p is None:
                raise UnknownPrinter(f"unknown printer: {printer_id}")
            if idempotency_key is not None:
                # A retried submit returns the original job — the same key never prints twice in an
                # org. The lookup is safe under the global write lock; the UNIQUE index enforces it.
                row = conn.execute("SELECT id FROM jobs WHERE org_id=? AND idempotency_key=?",
                                   (p["org_id"], idempotency_key)).fetchone()
                if row:
                    conn.commit()
                    return row["id"]
            cur = conn.execute(
                "INSERT INTO jobs(org_id, user_id, printer_id, agent_id, type, mode, state, "
                "payload, title, copies, options, callback_url, idempotency_key, expires_at, "
                "created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (p["org_id"], user_id, printer_id, p["agent_id"], type_, mode, "queued",
                 sqlite3.Binary(payload), title, copies,
                 json.dumps(options) if options else None, callback_url, idempotency_key,
                 None if expire_after is None else now + expire_after, now))
            conn.commit()
            return cur.lastrowid
        except Exception:
            conn.rollback()
            raise


def claim_job(conn, agent_id, now=None):
    now = time.time() if now is None else now
    with _LOCK:
        try:
            conn.execute("UPDATE agents SET last_seen_at=? WHERE id=?", (now, agent_id))
            # ponytail: no index on jobs(agent_id, state); add one if claim throughput ever demands it.
            # Past its deadline the job is skipped here and failed by the reaper's expire_jobs —
            # the skip is what guarantees it never prints, whatever the reaper's tick is doing.
            row = conn.execute(
                "SELECT id, printer_id, mode, copies, options FROM jobs "
                "WHERE agent_id=? AND state='queued' AND (expires_at IS NULL OR expires_at > ?) "
                "ORDER BY created_at, id LIMIT 1", (agent_id, now)).fetchone()
            if row is None:
                conn.commit()
                return None
            conn.execute("UPDATE jobs SET state='claimed', claimed_at=? WHERE id=?", (now, row["id"]))
            conn.commit()
            return {"job_id": row["id"], "printer_id": row["printer_id"], "mode": row["mode"],
                    "copies": row["copies"],
                    "options": json.loads(row["options"]) if row["options"] else None}
        except Exception:
            conn.rollback()
            raise


def get_payload(conn, job_id, agent_id):
    with _LOCK:
        row = conn.execute("SELECT payload FROM jobs WHERE id=? AND agent_id=?",
                           (job_id, agent_id)).fetchone()
    return bytes(row["payload"]) if row else None


def finish_job(conn, job_id, agent_id, ok, error=None):
    now = time.time()
    state = "done" if ok else "failed"
    with _LOCK:
        try:
            cur = conn.execute(
                "UPDATE jobs SET state=?, error=?, finished_at=? "
                "WHERE id=? AND agent_id=? AND state='claimed'",
                (state, None if ok else error, now, job_id, agent_id))
            conn.commit()
            return cur.rowcount == 1
        except Exception:
            conn.rollback()
            raise


def requeue_stale(conn, timeout_s, max_retries, now=None):
    now = time.time() if now is None else now
    cutoff = now - timeout_s
    with _LOCK:
        try:
            conn.execute(
                "UPDATE jobs SET state='failed', error='retry limit exceeded', finished_at=? "
                "WHERE state='claimed' AND claimed_at < ? AND retries >= ?",
                (now, cutoff, max_retries))
            cur = conn.execute(
                "UPDATE jobs SET state='queued', retries=retries+1, claimed_at=NULL "
                "WHERE state='claimed' AND claimed_at < ? AND retries < ?",
                (cutoff, max_retries))
            conn.commit()
            return cur.rowcount
        except Exception:
            conn.rollback()
            raise


def expire_jobs(conn, now=None):
    """Fail every queued job whose expire_after deadline has passed. `failed` + error 'expired' —
    terminal, so the webhook dispatcher reports it like any other outcome.
    # ponytail: no separate 'expired' state (it would have to be threaded through metrics, the
    # dashboard and every client); the error string carries the reason."""
    now = time.time() if now is None else now
    with _LOCK:
        try:
            cur = conn.execute(
                "UPDATE jobs SET state='failed', error='expired', finished_at=:now "
                "WHERE state='queued' AND expires_at IS NOT NULL AND expires_at <= :now",
                {"now": now})
            conn.commit()
            return cur.rowcount
        except Exception:
            conn.rollback()
            raise


def cancel_job(conn, job_id, org_id=None):
    """Cancel a job while it is still 'queued' (before any agent claims it).
    Returns 'cancelled' | 'not_cancellable' (already claimed/finished) | 'not_found'.
    The UPDATE is state-guarded so a claim racing the cancel can't be undone.
    Another org's job is 'not_found' — the existence check is org-filtered too."""
    now = time.time()
    with _LOCK:
        try:
            cur = conn.execute(
                "UPDATE jobs SET state='cancelled', finished_at=:now "
                "WHERE id=:jid AND state='queued' AND (:org IS NULL OR org_id = :org)",
                {"now": now, "jid": job_id, "org": org_id})
            if cur.rowcount == 1:
                conn.commit()
                return "cancelled"
            row = conn.execute("SELECT 1 FROM jobs WHERE id=:jid AND (:org IS NULL OR org_id = :org)",
                               {"jid": job_id, "org": org_id}).fetchone()
            conn.commit()
            return "not_found" if row is None else "not_cancellable"
        except Exception:
            conn.rollback()
            raise


_TERMINAL = ("done", "failed", "cancelled")


def pending_webhooks(conn, max_attempts):
    """Jobs in a terminal state with a callback_url, not yet delivered, under the attempt cap.
    State-based (not transition-hooked), so every terminal path — agent report, cancel, reaper —
    is covered without touching the mutators."""
    # ponytail: unindexed full scan under _LOCK every dispatch tick (like metrics/requeue_stale);
    # add a partial index on (callback_url, hook_delivered_at) if the jobs table ever gets large.
    q = ("SELECT id, callback_url, state, error, title, printer_id FROM jobs "
         "WHERE callback_url IS NOT NULL AND hook_delivered_at IS NULL AND hook_attempts < ? "
         f"AND state IN ({','.join('?' * len(_TERMINAL))}) ORDER BY id")
    with _LOCK:
        rows = conn.execute(q, (max_attempts, *_TERMINAL)).fetchall()
    return [{"job_id": r["id"], "callback_url": r["callback_url"], "state": r["state"],
             "error": r["error"], "title": r["title"], "printer_id": r["printer_id"]} for r in rows]


def mark_webhook_delivered(conn, job_id, now=None):
    now = time.time() if now is None else now
    with _LOCK:
        try:
            conn.execute("UPDATE jobs SET hook_delivered_at=? WHERE id=?", (now, job_id))
            conn.commit()
        except Exception:
            conn.rollback()
            raise


def bump_webhook_attempt(conn, job_id):
    with _LOCK:
        try:
            conn.execute("UPDATE jobs SET hook_attempts=hook_attempts+1 WHERE id=?", (job_id,))
            conn.commit()
        except Exception:
            conn.rollback()
            raise


def get_job(conn, job_id, org_id=None):
    with _LOCK:
        row = conn.execute(
            "SELECT state, error, printer_id, created_at, finished_at FROM jobs "
            "WHERE id=:jid AND (:org IS NULL OR org_id = :org)",
            {"jid": job_id, "org": org_id}).fetchone()
    return dict(row) if row else None


def recent_jobs(conn, limit=50, org_id=None):
    with _LOCK:
        rows = conn.execute(
            "SELECT j.id, j.printer_id, p.name AS printer_name, a.name AS agent_name, j.title, "
            "j.state, j.type, j.mode, j.error, j.created_at, j.finished_at "
            "FROM jobs j JOIN printers p ON p.id = j.printer_id "
            "JOIN agents a ON a.id = j.agent_id "
            "WHERE (:org IS NULL OR j.org_id = :org) "
            "ORDER BY j.id DESC LIMIT :limit", {"limit": limit, "org": org_id}).fetchall()
    return [dict(r) for r in rows]


def metrics(conn, online_window_s, now=None, org_id=None):
    """Aggregate snapshot for /metrics: job counts by state, agent liveness, printer count."""
    now = time.time() if now is None else now
    org = {"org": org_id}
    with _LOCK:
        jobrows = conn.execute("SELECT state, COUNT(*) c FROM jobs "
                               "WHERE (:org IS NULL OR org_id = :org) GROUP BY state",
                               org).fetchall()
        seen = [r["last_seen_at"] for r in conn.execute(
            "SELECT last_seen_at FROM agents WHERE (:org IS NULL OR org_id = :org)", org).fetchall()]
        printers = conn.execute("SELECT COUNT(*) c FROM printers "
                                "WHERE (:org IS NULL OR org_id = :org)", org).fetchone()["c"]
    online = sum(1 for s in seen if s is not None and (now - s) <= online_window_s)
    return {"jobs": {r["state"]: r["c"] for r in jobrows},
            "agents_total": len(seen), "agents_online": online, "printers_total": printers}


def list_agents(conn, online_window_s, now=None, org_id=None):
    """Agents (PrintNode calls them computers) with liveness and how many printers each carries."""
    now = time.time() if now is None else now
    with _LOCK:
        rows = conn.execute(
            "SELECT a.id, a.name, a.last_seen_at, a.created_at, "
            "(SELECT COUNT(*) FROM printers p WHERE p.agent_id = a.id) AS printers "
            "FROM agents a WHERE (:org IS NULL OR a.org_id = :org) ORDER BY a.id",
            {"org": org_id}).fetchall()
    return [{"id": r["id"], "name": r["name"], "last_seen_at": r["last_seen_at"],
             "created_at": r["created_at"], "printers": r["printers"],
             "online": r["last_seen_at"] is not None and (now - r["last_seen_at"]) <= online_window_s}
            for r in rows]


def claim_agent_transitions(conn, online_window_s, now=None):
    """Agents whose liveness flipped since the last call, marked as reported under the same lock so
    each edge fires exactly once. Agents in an org with no event_url are marked but not returned —
    an org that sets a URL later starts from the current state instead of replaying history."""
    # ponytail: full scan of agents per pass (the table is one row per machine); index it if a
    # deployment ever has thousands of agents.
    now = time.time() if now is None else now
    cutoff = now - online_window_s
    out = []
    with _LOCK:
        try:
            rows = conn.execute(
                "SELECT a.id, a.name, a.org_id, a.last_seen_at, a.offline_notified, o.event_url "
                "FROM agents a JOIN orgs o ON o.id = a.org_id ORDER BY a.id").fetchall()
            for r in rows:
                offline = r["last_seen_at"] is None or r["last_seen_at"] < cutoff
                if offline == bool(r["offline_notified"]):
                    continue                      # same state as last pass — no edge
                conn.execute("UPDATE agents SET offline_notified=? WHERE id=?",
                             (1 if offline else 0, r["id"]))
                if r["event_url"]:
                    out.append({"agent_id": r["id"], "name": r["name"], "org_id": r["org_id"],
                                "last_seen_at": r["last_seen_at"], "event_url": r["event_url"],
                                "event": "offline" if offline else "online"})
            conn.commit()
        except Exception:
            conn.rollback()
            raise
    return out


def list_printers(conn, online_window_s, now=None, org_id=None):
    now = time.time() if now is None else now
    with _LOCK:
        rows = conn.execute(
            "SELECT p.id, p.name, p.agent_id, p.can_pdf, p.capabilities, "
            "a.name AS agent_name, a.last_seen_at "
            "FROM printers p JOIN agents a ON a.id = p.agent_id "
            "WHERE (:org IS NULL OR p.org_id = :org) ORDER BY p.id", {"org": org_id}).fetchall()
    out = []
    for r in rows:
        seen = r["last_seen_at"]
        out.append({
            "id": r["id"], "name": r["name"], "agent_id": r["agent_id"],
            "agent_name": r["agent_name"], "can_pdf": bool(r["can_pdf"]),
            "capabilities": json.loads(r["capabilities"]) if r["capabilities"] else None,
            "online": seen is not None and (now - seen) <= online_window_s,
        })
    return out
