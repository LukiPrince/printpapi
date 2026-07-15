# Core Poll Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Flip printpapi from push to poll — an agent registers, long-polls the server for print jobs, downloads the bytes, prints, and reports the result.

**Architecture:** The server becomes a SQLite-backed queue + registry + payload store on `ThreadingHTTPServer`. A client `POST`s a job; the server decodes the payload to bytes at submit time and stores it. The agent (outbound only) registers its printers, long-polls `GET /agent/jobs`, downloads the payload, renders with the existing `raw_to_printer`/`pdf_to_printer`, and `POST`s the result.

**Tech Stack:** Python stdlib only — `sqlite3`, `http.server` (`ThreadingHTTPServer`), `hashlib`, `hmac`, `threading`, `time`, `urllib`. `pytest` for tests. Existing `app/dispatch.py` (`decode_payload`, `agent_mode`) is reused unchanged.

## Global Constraints

- **Stdlib only. No new dependencies.** (`pyproject.toml` declares none; keep it that way.)
- **Python 3.12+** (Dockerfile is `python:3.12-slim`).
- **Security at trust boundaries (non-negotiable):** bearer token on client endpoints; per-agent API key on agent endpoints; tokens/keys compared with `hmac.compare_digest`; agent API keys stored only as `sha256` hashes; `decode_payload` keeps `http(s)`-only URL checks and the browser User-Agent; no `shell=True`; agent temp PDF files cleaned up; an agent may only claim / download / finish its own jobs (scoped by `agent_id`).
- **Tenancy schema from day 1:** every table carries `org_id` and `created_at`; seed one default org (`id=1`) and user (`id=1`). No org/user *management* in this plan.
- **TDD:** failing test first, minimal code, commit per task.
- **Tests use real loopback servers**, matching the existing suite (no pure-mock HTTP).
- All store writes and reads go through one process-global `threading.Lock` over a single shared `sqlite3` connection (`check_same_thread=False`, WAL). `# ponytail: global lock; per-connection pool only if it bottlenecks.`

---

## File Structure

```
app/
  dispatch.py   # UNCHANGED — decode_payload, agent_mode, fetch helpers, UA gotcha
  store.py      # NEW — SQLite schema + all queries (connect, init, register, enqueue, claim, payload, finish, reaper, status, list)
  server.py     # REWRITTEN — client + agent endpoints on ThreadingHTTPServer; v0 push senders removed
agent/
  print_agent.py  # REWRITTEN — poll loop (register/poll/download/print/report); raw_to_printer/pdf_to_printer kept; inbound listener removed
tests/
  test_dispatch.py     # UNCHANGED — stays green
  test_store.py        # NEW
  test_server.py       # REWRITTEN for new endpoints
  test_integration.py  # REWRITTEN — full round-trip
agent/tests/
  test_agent.py        # REWRITTEN for the poll loop
```

The v0 push code (`send_socket`, `send_agent`, `send_cups`, `load_targets`, `resolve_target`, `PRINT_TARGETS`) is **not carried into** the rewritten `server.py` — the rewrites in Tasks 8–13 supersede it. No separate deletion task is needed.

---

## Task 1: Store — connection + schema + seed

**Files:**
- Create: `app/store.py`
- Test: `tests/test_store.py`

**Interfaces:**
- Produces:
  - `DEFAULT_ORG = 1`, `DEFAULT_USER = 1`
  - `connect(path) -> sqlite3.Connection` (WAL, `row_factory=sqlite3.Row`, `check_same_thread=False`)
  - `init_db(conn) -> None` (creates tables if absent; seeds default org + user; idempotent)

- [ ] **Step 1: Write the failing test**

```python
# tests/test_store.py
import os, tempfile
from app import store


def _db():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    conn = store.connect(path)
    store.init_db(conn)
    return conn


def test_init_seeds_default_org_and_user_and_is_idempotent():
    conn = _db()
    store.init_db(conn)  # second call must not raise or duplicate
    org = conn.execute("SELECT name FROM orgs WHERE id=?", (store.DEFAULT_ORG,)).fetchone()
    user = conn.execute("SELECT org_id FROM users WHERE id=?", (store.DEFAULT_USER,)).fetchone()
    assert org["name"] == "default"
    assert user["org_id"] == store.DEFAULT_ORG
    assert conn.execute("SELECT COUNT(*) c FROM orgs").fetchone()["c"] == 1
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_store.py -v`
Expected: FAIL with `ModuleNotFoundError`/`AttributeError` (no `app.store`).

- [ ] **Step 3: Write minimal implementation**

```python
# app/store.py
import sqlite3
import threading
import time

DEFAULT_ORG = 1
DEFAULT_USER = 1

_LOCK = threading.Lock()  # ponytail: global lock; per-connection pool only if it bottlenecks.

_SCHEMA = """
CREATE TABLE IF NOT EXISTS orgs(
  id INTEGER PRIMARY KEY, name TEXT NOT NULL, created_at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS users(
  id INTEGER PRIMARY KEY, org_id INTEGER NOT NULL, name TEXT NOT NULL, created_at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS agents(
  id INTEGER PRIMARY KEY AUTOINCREMENT, org_id INTEGER NOT NULL, name TEXT NOT NULL,
  api_key_hash TEXT NOT NULL UNIQUE, last_seen_at REAL, created_at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS printers(
  id INTEGER PRIMARY KEY AUTOINCREMENT, org_id INTEGER NOT NULL, agent_id INTEGER NOT NULL,
  name TEXT NOT NULL, can_pdf INTEGER NOT NULL DEFAULT 0, state TEXT NOT NULL DEFAULT 'active',
  created_at REAL NOT NULL, UNIQUE(agent_id, name));
CREATE TABLE IF NOT EXISTS jobs(
  id INTEGER PRIMARY KEY AUTOINCREMENT, org_id INTEGER NOT NULL, user_id INTEGER NOT NULL,
  printer_id INTEGER NOT NULL, agent_id INTEGER NOT NULL, type TEXT NOT NULL, mode TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'queued', payload BLOB NOT NULL, error TEXT,
  retries INTEGER NOT NULL DEFAULT 0, created_at REAL NOT NULL, claimed_at REAL, finished_at REAL);
"""


def connect(path):
    conn = sqlite3.connect(path, check_same_thread=False)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA journal_mode=WAL")
    return conn


def init_db(conn):
    now = time.time()
    with _LOCK:
        conn.executescript(_SCHEMA)
        if conn.execute("SELECT 1 FROM orgs WHERE id=?", (DEFAULT_ORG,)).fetchone() is None:
            conn.execute("INSERT INTO orgs(id, name, created_at) VALUES(?,?,?)",
                         (DEFAULT_ORG, "default", now))
        if conn.execute("SELECT 1 FROM users WHERE id=?", (DEFAULT_USER,)).fetchone() is None:
            conn.execute("INSERT INTO users(id, org_id, name, created_at) VALUES(?,?,?,?)",
                         (DEFAULT_USER, DEFAULT_ORG, "default", now))
        conn.commit()
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_store.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add app/store.py tests/test_store.py
git commit -m "feat(store): sqlite schema + default-org/user seed"
```
(The `poll-engine` branch already exists — do not create it.)

---

## Task 2: Store — agent register + authenticate

**Files:**
- Modify: `app/store.py`
- Test: `tests/test_store.py`

**Interfaces:**
- Consumes: `connect`, `init_db`, `DEFAULT_ORG`.
- Produces:
  - `class AuthError(Exception)`
  - `register_agent(conn, name, api_key, printers) -> {"computer_id": int, "printer_ids": {name: id}}` where `printers` is `list[{"name": str, "can_pdf": bool}]`; upsert by `(org, name)` for the agent and `(agent_id, name)` for printers; idempotent for the same name+key. **Name↔key binding:** if the name already exists with a *different* key hash, raise `AuthError` (no silent key rotation — closes name-based hijack).
  - `authenticate_agent(conn, api_key) -> int | None` (agent id, by `sha256` hash).

- [ ] **Step 1: Write the failing test**

```python
# tests/test_store.py  (append)
import pytest


def test_register_is_idempotent_and_authenticates():
    conn = _db()
    r1 = store.register_agent(conn, "win-1", "key-abc",
                              [{"name": "Zebra", "can_pdf": False}])
    r2 = store.register_agent(conn, "win-1", "key-abc",
                              [{"name": "Zebra", "can_pdf": False},
                               {"name": "HP", "can_pdf": True}])
    assert r1["computer_id"] == r2["computer_id"]            # same agent, not duplicated
    assert r1["printer_ids"]["Zebra"] == r2["printer_ids"]["Zebra"]
    assert conn.execute("SELECT COUNT(*) c FROM agents").fetchone()["c"] == 1
    assert conn.execute("SELECT COUNT(*) c FROM printers").fetchone()["c"] == 2
    assert store.authenticate_agent(conn, "key-abc") == r1["computer_id"]
    assert store.authenticate_agent(conn, "wrong") is None


def test_register_rejects_existing_name_with_different_key():
    conn = _db()
    store.register_agent(conn, "win-1", "key-abc", [])
    store.register_agent(conn, "win-1", "key-abc", [])      # same key -> idempotent restart, ok
    with pytest.raises(store.AuthError):
        store.register_agent(conn, "win-1", "other-key", [])  # name<->key binding
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_store.py::test_register_is_idempotent_and_authenticates -v`
Expected: FAIL with `AttributeError: module 'app.store' has no attribute 'register_agent'`.

- [ ] **Step 3: Write minimal implementation**

```python
# app/store.py  (add imports at top)
import hashlib
import hmac

# app/store.py  (append)
class AuthError(Exception):
    pass


def _hash_key(api_key):
    return hashlib.sha256(api_key.encode()).hexdigest()


def register_agent(conn, name, api_key, printers):
    now = time.time()
    key_hash = _hash_key(api_key)
    with _LOCK:
        row = conn.execute("SELECT id, api_key_hash FROM agents WHERE org_id=? AND name=?",
                           (DEFAULT_ORG, name)).fetchone()
        if row:
            if not hmac.compare_digest(row["api_key_hash"], key_hash):
                raise AuthError(f"agent name already registered with a different key: {name!r}")
            agent_id = row["id"]
            conn.execute("UPDATE agents SET last_seen_at=? WHERE id=?", (now, agent_id))
        else:
            cur = conn.execute(
                "INSERT INTO agents(org_id, name, api_key_hash, last_seen_at, created_at) "
                "VALUES(?,?,?,?,?)", (DEFAULT_ORG, name, key_hash, now, now))
            agent_id = cur.lastrowid
        printer_ids = {}
        for p in printers:
            can_pdf = 1 if p.get("can_pdf") else 0
            r = conn.execute("SELECT id FROM printers WHERE agent_id=? AND name=?",
                             (agent_id, p["name"])).fetchone()
            if r:
                pid = r["id"]
                conn.execute("UPDATE printers SET can_pdf=? WHERE id=?", (can_pdf, pid))
            else:
                cur = conn.execute(
                    "INSERT INTO printers(org_id, agent_id, name, can_pdf, created_at) "
                    "VALUES(?,?,?,?,?)", (DEFAULT_ORG, agent_id, p["name"], can_pdf, now))
                pid = cur.lastrowid
            printer_ids[p["name"]] = pid
        conn.commit()
    return {"computer_id": agent_id, "printer_ids": printer_ids}


def authenticate_agent(conn, api_key):
    key_hash = _hash_key(api_key)
    with _LOCK:
        row = conn.execute("SELECT id FROM agents WHERE api_key_hash=?", (key_hash,)).fetchone()
    return row["id"] if row else None
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_store.py -v`
Expected: PASS (all store tests).

- [ ] **Step 5: Commit**

```bash
git add app/store.py tests/test_store.py
git commit -m "feat(store): agent register upsert + authenticate"
```

---

## Task 3: Store — enqueue job

**Files:**
- Modify: `app/store.py`
- Test: `tests/test_store.py`

**Interfaces:**
- Consumes: `register_agent`, `DEFAULT_USER`.
- Produces:
  - `class UnknownPrinter(Exception)`
  - `enqueue_job(conn, printer_id, type_, mode, payload, user_id=DEFAULT_USER) -> int` (job id). Resolves `org_id`/`agent_id` from the printer; inserts `state='queued'`; raises `UnknownPrinter` if the printer id is unknown.

- [ ] **Step 1: Write the failing test**

```python
# tests/test_store.py  (append)  -- pytest already imported in Task 2
def test_enqueue_resolves_agent_and_rejects_unknown_printer():
    conn = _db()
    reg = store.register_agent(conn, "win-1", "k", [{"name": "Zebra", "can_pdf": False}])
    pid = reg["printer_ids"]["Zebra"]
    jid = store.enqueue_job(conn, pid, "raw_base64", "raw", b"^XA^XZ")
    row = conn.execute("SELECT agent_id, state, payload, mode FROM jobs WHERE id=?",
                       (jid,)).fetchone()
    assert row["agent_id"] == reg["computer_id"]
    assert row["state"] == "queued"
    assert bytes(row["payload"]) == b"^XA^XZ"
    assert row["mode"] == "raw"
    with pytest.raises(store.UnknownPrinter):
        store.enqueue_job(conn, 9999, "raw_base64", "raw", b"x")
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_store.py::test_enqueue_resolves_agent_and_rejects_unknown_printer -v`
Expected: FAIL with `AttributeError` (no `enqueue_job`).

- [ ] **Step 3: Write minimal implementation**

```python
# app/store.py  (append)
class UnknownPrinter(Exception):
    pass


def enqueue_job(conn, printer_id, type_, mode, payload, user_id=DEFAULT_USER):
    now = time.time()
    with _LOCK:
        p = conn.execute("SELECT org_id, agent_id FROM printers WHERE id=?",
                         (printer_id,)).fetchone()
        if p is None:
            raise UnknownPrinter(f"unknown printer: {printer_id}")
        cur = conn.execute(
            "INSERT INTO jobs(org_id, user_id, printer_id, agent_id, type, mode, state, "
            "payload, created_at) VALUES(?,?,?,?,?,?,?,?,?)",
            (p["org_id"], user_id, printer_id, p["agent_id"], type_, mode, "queued",
             sqlite3.Binary(payload), now))
        conn.commit()
        return cur.lastrowid
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_store.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add app/store.py tests/test_store.py
git commit -m "feat(store): enqueue_job + UnknownPrinter"
```

---

## Task 4: Store — claim job (atomic) + heartbeat

**Files:**
- Modify: `app/store.py`
- Test: `tests/test_store.py`

**Interfaces:**
- Consumes: `enqueue_job`, `register_agent`.
- Produces:
  - `claim_job(conn, agent_id) -> {"job_id": int, "printer_id": int, "mode": str} | None`. Atomically (under `_LOCK`) selects the oldest `queued` job for `agent_id`, marks it `claimed` with `claimed_at`, and updates the agent's `last_seen_at`. Returns `None` when none are queued.

- [ ] **Step 1: Write the failing test**

```python
# tests/test_store.py  (append)
def test_claim_returns_one_job_then_none_and_updates_last_seen():
    conn = _db()
    reg = store.register_agent(conn, "win-1", "k", [{"name": "Z", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["Z"]
    j1 = store.enqueue_job(conn, pid, "raw_base64", "raw", b"a")
    store.enqueue_job(conn, pid, "raw_base64", "raw", b"b")
    c1 = store.claim_job(conn, aid)
    assert c1["job_id"] == j1 and c1["mode"] == "raw"      # oldest first
    assert conn.execute("SELECT state FROM jobs WHERE id=?", (j1,)).fetchone()["state"] == "claimed"
    assert store.claim_job(conn, aid)["job_id"] != j1       # second job
    assert store.claim_job(conn, aid) is None               # nothing left
    assert conn.execute("SELECT last_seen_at FROM agents WHERE id=?", (aid,)).fetchone()["last_seen_at"]
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_store.py::test_claim_returns_one_job_then_none_and_updates_last_seen -v`
Expected: FAIL with `AttributeError` (no `claim_job`).

- [ ] **Step 3: Write minimal implementation**

```python
# app/store.py  (append)
def claim_job(conn, agent_id):
    now = time.time()
    with _LOCK:
        conn.execute("UPDATE agents SET last_seen_at=? WHERE id=?", (now, agent_id))
        row = conn.execute(
            "SELECT id, printer_id, mode FROM jobs WHERE agent_id=? AND state='queued' "
            "ORDER BY created_at, id LIMIT 1", (agent_id,)).fetchone()
        if row is None:
            conn.commit()
            return None
        conn.execute("UPDATE jobs SET state='claimed', claimed_at=? WHERE id=?", (now, row["id"]))
        conn.commit()
        return {"job_id": row["id"], "printer_id": row["printer_id"], "mode": row["mode"]}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_store.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add app/store.py tests/test_store.py
git commit -m "feat(store): atomic claim_job + last_seen heartbeat"
```

---

## Task 5: Store — payload fetch + finish (scoped)

**Files:**
- Modify: `app/store.py`
- Test: `tests/test_store.py`

**Interfaces:**
- Consumes: `claim_job`, `enqueue_job`, `register_agent`.
- Produces:
  - `get_payload(conn, job_id, agent_id) -> bytes | None` (only if the job belongs to that agent).
  - `finish_job(conn, job_id, agent_id, ok, error=None) -> bool`. Sets `done` (ok) or `failed` + `error` (not ok) and `finished_at`, only for a `claimed` job owned by `agent_id`; returns whether a row matched.

- [ ] **Step 1: Write the failing test**

```python
# tests/test_store.py  (append)
def test_payload_and_finish_are_agent_scoped():
    conn = _db()
    a = store.register_agent(conn, "a", "ka", [{"name": "PA", "can_pdf": False}])
    b = store.register_agent(conn, "b", "kb", [{"name": "PB", "can_pdf": False}])
    jid = store.enqueue_job(conn, a["printer_ids"]["PA"], "raw_base64", "raw", b"DATA")
    store.claim_job(conn, a["computer_id"])
    assert store.get_payload(conn, jid, a["computer_id"]) == b"DATA"
    assert store.get_payload(conn, jid, b["computer_id"]) is None        # foreign agent denied
    assert store.finish_job(conn, jid, b["computer_id"], ok=True) is False
    assert store.finish_job(conn, jid, a["computer_id"], ok=False, error="boom") is True
    row = conn.execute("SELECT state, error FROM jobs WHERE id=?", (jid,)).fetchone()
    assert row["state"] == "failed" and row["error"] == "boom"
    assert store.finish_job(conn, jid, a["computer_id"], ok=True) is False  # not 'claimed' anymore
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_store.py::test_payload_and_finish_are_agent_scoped -v`
Expected: FAIL with `AttributeError` (no `get_payload`).

- [ ] **Step 3: Write minimal implementation**

```python
# app/store.py  (append)
def get_payload(conn, job_id, agent_id):
    with _LOCK:
        row = conn.execute("SELECT payload FROM jobs WHERE id=? AND agent_id=?",
                           (job_id, agent_id)).fetchone()
    return bytes(row["payload"]) if row else None


def finish_job(conn, job_id, agent_id, ok, error=None):
    now = time.time()
    state = "done" if ok else "failed"
    with _LOCK:
        cur = conn.execute(
            "UPDATE jobs SET state=?, error=?, finished_at=? "
            "WHERE id=? AND agent_id=? AND state='claimed'",
            (state, None if ok else error, now, job_id, agent_id))
        conn.commit()
        return cur.rowcount == 1
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_store.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add app/store.py tests/test_store.py
git commit -m "feat(store): agent-scoped get_payload + finish_job"
```

---

## Task 6: Store — visibility-timeout reaper

**Files:**
- Modify: `app/store.py`
- Test: `tests/test_store.py`

**Interfaces:**
- Consumes: `claim_job`, `enqueue_job`, `register_agent`.
- Produces:
  - `requeue_stale(conn, timeout_s, max_retries, now=None) -> int`. Jobs `claimed` longer than `timeout_s`: if `retries >= max_retries` → `failed` (`error='retry limit exceeded'`); else → `queued`, `retries+1`, `claimed_at=NULL`. Returns the count requeued. `now` defaults to `time.time()` (injectable for the test).

- [ ] **Step 1: Write the failing test**

```python
# tests/test_store.py  (append)
def test_reaper_requeues_then_fails_after_limit():
    conn = _db()
    reg = store.register_agent(conn, "w", "k", [{"name": "P", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["P"]
    jid = store.enqueue_job(conn, pid, "raw_base64", "raw", b"x")
    store.claim_job(conn, aid)
    base = __import__("time").time()
    # not stale yet
    assert store.requeue_stale(conn, timeout_s=60, max_retries=1, now=base) == 0
    # stale, under retry limit -> requeued
    assert store.requeue_stale(conn, timeout_s=60, max_retries=1, now=base + 120) == 1
    assert conn.execute("SELECT state, retries FROM jobs WHERE id=?", (jid,)).fetchone()["state"] == "queued"
    # claim again, go stale again -> now over limit -> failed
    store.claim_job(conn, aid)
    assert store.requeue_stale(conn, timeout_s=60, max_retries=1, now=base + 300) == 0
    assert conn.execute("SELECT state FROM jobs WHERE id=?", (jid,)).fetchone()["state"] == "failed"
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_store.py::test_reaper_requeues_then_fails_after_limit -v`
Expected: FAIL with `AttributeError` (no `requeue_stale`).

- [ ] **Step 3: Write minimal implementation**

```python
# app/store.py  (append)
def requeue_stale(conn, timeout_s, max_retries, now=None):
    now = time.time() if now is None else now
    cutoff = now - timeout_s
    with _LOCK:
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_store.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add app/store.py tests/test_store.py
git commit -m "feat(store): visibility-timeout reaper"
```

---

## Task 7: Store — job status + printer list

**Files:**
- Modify: `app/store.py`
- Test: `tests/test_store.py`

**Interfaces:**
- Consumes: `enqueue_job`, `register_agent`, `claim_job`.
- Produces:
  - `get_job(conn, job_id) -> dict | None` with keys `state, error, printer_id, created_at, finished_at`.
  - `list_printers(conn, online_window_s, now=None) -> list[dict]` with keys `id, name, agent_id, agent_name, can_pdf, online` (bool: agent `last_seen_at` within `online_window_s`).

- [ ] **Step 1: Write the failing test**

```python
# tests/test_store.py  (append)
def test_get_job_and_list_printers_online_flag():
    conn = _db()
    reg = store.register_agent(conn, "win-1", "k", [{"name": "Z", "can_pdf": True}])
    pid = reg["printer_ids"]["Z"]
    jid = store.enqueue_job(conn, pid, "pdf_base64", "pdf", b"%PDF")
    assert store.get_job(conn, jid)["state"] == "queued"
    assert store.get_job(conn, 123456) is None
    store.claim_job(conn, reg["computer_id"])      # sets last_seen_at to ~now
    ps = store.list_printers(conn, online_window_s=60)
    assert len(ps) == 1 and ps[0]["name"] == "Z" and ps[0]["can_pdf"] is True
    assert ps[0]["online"] is True
    assert store.list_printers(conn, online_window_s=60, now=__import__("time").time() + 600)[0]["online"] is False
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_store.py::test_get_job_and_list_printers_online_flag -v`
Expected: FAIL with `AttributeError` (no `get_job`).

- [ ] **Step 3: Write minimal implementation**

```python
# app/store.py  (append)
def get_job(conn, job_id):
    with _LOCK:
        row = conn.execute(
            "SELECT state, error, printer_id, created_at, finished_at FROM jobs WHERE id=?",
            (job_id,)).fetchone()
    return dict(row) if row else None


def list_printers(conn, online_window_s, now=None):
    now = time.time() if now is None else now
    with _LOCK:
        rows = conn.execute(
            "SELECT p.id, p.name, p.agent_id, p.can_pdf, a.name AS agent_name, a.last_seen_at "
            "FROM printers p JOIN agents a ON a.id = p.agent_id ORDER BY p.id").fetchall()
    out = []
    for r in rows:
        seen = r["last_seen_at"]
        out.append({
            "id": r["id"], "name": r["name"], "agent_id": r["agent_id"],
            "agent_name": r["agent_name"], "can_pdf": bool(r["can_pdf"]),
            "online": seen is not None and (now - seen) <= online_window_s,
        })
    return out
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_store.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add app/store.py tests/test_store.py
git commit -m "feat(store): get_job + list_printers with online flag"
```

---

## Task 8: Server — client endpoints + bearer auth

**Files:**
- Modify (rewrite): `app/server.py`
- Test (rewrite): `tests/test_server.py`

**Interfaces:**
- Consumes: `app.store` (Task 1–7), `app.dispatch.decode_payload`, `app.dispatch.agent_mode`, `DispatchError`, `FetchError`, `_http_get`.
- Produces:
  - `make_handler(*, conn, token, agent_auth=store.authenticate_agent, fetch_url=None, long_poll_timeout=25.0, poll_interval=1.0, online_window_s=60) -> Handler` (a `BaseHTTPRequestHandler` subclass). **This task implements the client routes only**; agent routes are added in Tasks 9–10 but the keyword args are declared now so the signature is stable.
  - Client routes: `GET /health` → `{ok:true}`; `POST /jobs` (bearer) `{printer_id,type,content|url,json}` → decode→bytes, `agent_mode`, `enqueue_job`, `{job_id}`; `GET /jobs/{id}` (bearer) → status; `GET /printers` (bearer) → `{printers:[...]}`.

- [ ] **Step 1: Write the failing test**

```python
# tests/test_server.py   (replace the whole file)
import json, threading, urllib.request, urllib.error
from http.server import ThreadingHTTPServer
from app import store, server


def _serve(conn, token="t"):
    handler = server.make_handler(conn=conn, token=token, long_poll_timeout=0.3, poll_interval=0.05)
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd, f"http://127.0.0.1:{httpd.server_address[1]}"


def _mem():
    conn = store.connect(":memory:")
    store.init_db(conn)
    return conn


def _req(method, url, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(url, data=data, method=method)
    if token:
        r.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def test_health_and_jobs_roundtrip_via_http():
    conn = _mem()
    reg = store.register_agent(conn, "win-1", "agentkey", [{"name": "Z", "can_pdf": False}])
    pid = reg["printer_ids"]["Z"]
    httpd, base = _serve(conn)
    try:
        assert _req("GET", base + "/health")[0] == 200
        # auth required
        assert _req("POST", base + "/jobs", body={"printer_id": pid, "type": "raw_base64",
                                                   "content": "QUJD"})[0] == 401
        # submit (QUJD == base64 "ABC")
        code, raw = _req("POST", base + "/jobs", token="t",
                         body={"printer_id": pid, "type": "raw_base64", "content": "QUJD"})
        assert code == 200
        jid = json.loads(raw)["job_id"]
        code, raw = _req("GET", base + f"/jobs/{jid}", token="t")
        assert code == 200 and json.loads(raw)["state"] == "queued"
        # unknown printer -> 400
        assert _req("POST", base + "/jobs", token="t",
                    body={"printer_id": 999, "type": "raw_base64", "content": "QUJD"})[0] == 400
        # printers list
        code, raw = _req("GET", base + "/printers", token="t")
        assert code == 200 and json.loads(raw)["printers"][0]["name"] == "Z"
    finally:
        httpd.shutdown()
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_server.py -v`
Expected: FAIL (the rewritten `make_handler` signature/routes don't exist yet).

- [ ] **Step 3: Write minimal implementation**

```python
# app/server.py   (replace the whole file)
import hmac
import json
import os
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from app import store
from app.dispatch import decode_payload, agent_mode, DispatchError, FetchError, _http_get

_JOB_ID = re.compile(r"^/jobs/(\d+)$")


def make_handler(*, conn, token, agent_auth=store.authenticate_agent, fetch_url=None,
                 long_poll_timeout=25.0, poll_interval=1.0, online_window_s=60):
    fetch = fetch_url or _http_get

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, fmt, *args):
            if os.environ.get("LOG_REQUESTS"):
                super().log_message(fmt, *args)

        def _json(self, code, obj):
            payload = json.dumps(obj).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def _client_ok(self):
            return hmac.compare_digest(self.headers.get("Authorization", ""), f"Bearer {token}")

        def _read_json(self):
            length = int(self.headers.get("Content-Length", 0))
            return json.loads(self.rfile.read(length) or b"{}")

        def do_GET(self):
            if self.path == "/health":
                return self._json(200, {"ok": True})
            m = _JOB_ID.match(self.path)
            if m:
                if not self._client_ok():
                    return self._json(401, {"error": "unauthorized"})
                job = store.get_job(conn, int(m.group(1)))
                return self._json(200, job) if job else self._json(404, {"error": "not found"})
            if self.path == "/printers":
                if not self._client_ok():
                    return self._json(401, {"error": "unauthorized"})
                return self._json(200, {"printers": store.list_printers(conn, online_window_s)})
            self._json(404, {"error": "not found"})

        def do_POST(self):
            if self.path == "/jobs":
                if not self._client_ok():
                    return self._json(401, {"error": "unauthorized"})
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                try:
                    data = decode_payload(body, fetch_url=fetch)
                    mode = agent_mode(body.get("type"))
                    jid = store.enqueue_job(conn, body.get("printer_id"), body.get("type"),
                                            mode, data)
                except FetchError as e:
                    return self._json(502, {"error": f"downstream: {e}"})
                except (DispatchError, store.UnknownPrinter) as e:
                    return self._json(400, {"error": str(e)})
                return self._json(200, {"job_id": jid})
            self._json(404, {"error": "not found"})

    return Handler
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_server.py tests/test_dispatch.py -v`
Expected: PASS (`test_dispatch.py` still green — `dispatch.py` untouched).

- [ ] **Step 5: Commit**

```bash
git add app/server.py tests/test_server.py
git commit -m "feat(server): client endpoints (jobs/printers/health) + bearer auth"
```

---

## Task 9: Server — agent register / payload / result

**Files:**
- Modify: `app/server.py`
- Test: `tests/test_server.py`

**Interfaces:**
- Consumes: the `Handler` from Task 8; `store.register_agent`, `store.get_payload`, `store.finish_job`, `store.authenticate_agent`.
- Produces (added routes):
  - `POST /agent/register` (agent key) `{name, printers}` → `{computer_id, printer_ids}`; existing name presented with a different key → `401` (via `store.AuthError`)
  - `GET /agent/jobs/{id}/payload` (agent key) → `application/octet-stream`, scoped; 404 if not the agent's.
  - `POST /agent/jobs/{id}/result` (agent key) `{ok, error?}` → `{ok:true}`; 404 if not a claimed job of that agent.
  - Helper `_agent_id()` returning the authenticated agent id or `None`.

- [ ] **Step 1: Write the failing test**

```python
# tests/test_server.py   (append; reuses _serve/_mem/_req)
def _areq(method, url, key, body=None, raw=False):
    data = body if raw else (json.dumps(body).encode() if body is not None else None)
    r = urllib.request.Request(url, data=data, method=method)
    r.add_header("Authorization", f"Bearer {key}")
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def test_agent_register_payload_result():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        code, raw = _areq("POST", base + "/agent/register", "ak",
                          {"name": "win-1", "printers": [{"name": "Z", "can_pdf": False}]})
        assert code == 200
        reg = json.loads(raw)
        pid = reg["printer_ids"]["Z"]
        # same name, different key -> 401 (name<->key binding)
        assert _areq("POST", base + "/agent/register", "wrongkey",
                     {"name": "win-1", "printers": []})[0] == 401
        jid = store.enqueue_job(conn, pid, "raw_base64", "raw", b"BYTES")
        store.claim_job(conn, reg["computer_id"])
        # wrong key -> 401
        assert _areq("GET", base + f"/agent/jobs/{jid}/payload", "wrong")[0] == 401
        code, payload = _areq("GET", base + f"/agent/jobs/{jid}/payload", "ak")
        assert code == 200 and payload == b"BYTES"
        code, _ = _areq("POST", base + f"/agent/jobs/{jid}/result", "ak", {"ok": True})
        assert code == 200
        assert store.get_job(conn, jid)["state"] == "done"
    finally:
        httpd.shutdown()
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_server.py::test_agent_register_payload_result -v`
Expected: FAIL (agent routes not implemented).

- [ ] **Step 3: Write minimal implementation**

Add the agent-id helper and the payload regex near the top of `make_handler`/module, and the routes. Insert `_AGENT_PAYLOAD`/`_AGENT_RESULT` regexes at module level next to `_JOB_ID`:

```python
# app/server.py   (module level, beside _JOB_ID)
_AGENT_PAYLOAD = re.compile(r"^/agent/jobs/(\d+)/payload$")
_AGENT_RESULT = re.compile(r"^/agent/jobs/(\d+)/result$")
```

Inside `class Handler`, add the helper:

```python
        def _agent_id(self):
            auth = self.headers.get("Authorization", "")
            key = auth[7:] if auth.startswith("Bearer ") else ""
            return agent_auth(conn, key) if key else None
```

Extend `do_GET` (before the final 404) with the payload route:

```python
            mp = _AGENT_PAYLOAD.match(self.path)
            if mp:
                aid = self._agent_id()
                if aid is None:
                    return self._json(401, {"error": "unauthorized"})
                data = store.get_payload(conn, int(mp.group(1)), aid)
                if data is None:
                    return self._json(404, {"error": "not found"})
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
                return
```

Extend `do_POST` (before the final 404) with register + result:

```python
            if self.path == "/agent/register":
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                auth = self.headers.get("Authorization", "")
                key = auth[7:] if auth.startswith("Bearer ") else ""
                # First contact binds name->key; re-register requires the same key.
                try:
                    reg = store.register_agent(conn, body.get("name"), key,
                                               body.get("printers", []))
                except store.AuthError as e:
                    return self._json(401, {"error": str(e)})
                return self._json(200, reg)
            mr = _AGENT_RESULT.match(self.path)
            if mr:
                aid = self._agent_id()
                if aid is None:
                    return self._json(401, {"error": "unauthorized"})
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                ok = store.finish_job(conn, int(mr.group(1)), aid, bool(body.get("ok")),
                                      body.get("error"))
                return self._json(200, {"ok": True}) if ok else self._json(404, {"error": "not found"})
```

> Note on `/agent/register`: first contact for a name *creates* the agent and binds that name to its key (stored hashed). Re-registering the same name requires the same key; a different key raises `store.AuthError` → `401` (no silent key rotation — closes name-based hijack). Cross-name reuse of an existing key is blocked by the `UNIQUE(api_key_hash)` constraint. Every other agent route authenticates an *existing* agent via `_agent_id()`.

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_server.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add app/server.py tests/test_server.py
git commit -m "feat(server): agent register / payload / result routes"
```

---

## Task 10: Server — long-poll GET /agent/jobs

**Files:**
- Modify: `app/server.py`
- Test: `tests/test_server.py`

**Interfaces:**
- Consumes: `store.claim_job`; `long_poll_timeout`, `poll_interval` from `make_handler`.
- Produces: `GET /agent/jobs` (agent key). Loops `claim_job` every `poll_interval` until a job is found (→ `200 {job_id,printer_id,mode}`) or `long_poll_timeout` elapses (→ `204`).

- [ ] **Step 1: Write the failing test**

```python
# tests/test_server.py   (append)
import time


def test_long_poll_204_then_job():
    conn = _mem()
    reg = store.register_agent(conn, "win-1", "ak", [{"name": "Z", "can_pdf": False}])
    httpd, base = _serve(conn)  # long_poll_timeout=0.3, poll_interval=0.05
    try:
        # nothing queued -> 204 after timeout
        t0 = time.time()
        assert _areq("GET", base + "/agent/jobs", "ak")[0] == 204
        assert time.time() - t0 >= 0.25
        # enqueue, then poll returns it
        pid = reg["printer_ids"]["Z"]
        jid = store.enqueue_job(conn, pid, "raw_base64", "raw", b"x")
        code, raw = _areq("GET", base + "/agent/jobs", "ak")
        assert code == 200 and json.loads(raw)["job_id"] == jid
    finally:
        httpd.shutdown()
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_server.py::test_long_poll_204_then_job -v`
Expected: FAIL (route returns 404).

- [ ] **Step 3: Write minimal implementation**

Add `import time` at the top of `app/server.py`. In `do_GET`, before the final 404, add:

```python
            if self.path == "/agent/jobs":
                aid = self._agent_id()
                if aid is None:
                    return self._json(401, {"error": "unauthorized"})
                deadline = time.time() + long_poll_timeout
                while True:
                    job = store.claim_job(conn, aid)
                    if job is not None:
                        return self._json(200, job)
                    if time.time() >= deadline:
                        self.send_response(204)
                        self.end_headers()
                        return
                    time.sleep(poll_interval)  # ponytail: DB poll; notify-on-enqueue if latency matters
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_server.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add app/server.py tests/test_server.py
git commit -m "feat(server): long-poll GET /agent/jobs"
```

---

## Task 11: Agent — poll loop

**Files:**
- Modify (rewrite): `agent/print_agent.py`
- Test (rewrite): `agent/tests/test_agent.py`

**Interfaces:**
- Consumes: the agent HTTP API (Tasks 9–10).
- Produces:
  - `raw_to_printer(printer, data)` and `pdf_to_printer(printer, data, sumatra=...)` — **kept unchanged** from v0.
  - `register(base, key, name, printers, http_post=_post) -> dict`
  - `poll_job(base, key, http_get=_get) -> dict | None` (None on 204)
  - `download_payload(base, key, job_id, http_get_bytes=_get_bytes) -> bytes`
  - `report_result(base, key, job_id, ok, error=None, http_post=_post) -> None`
  - `print_job(mode, printer, data, raw_fn=raw_to_printer, pdf_fn=pdf_to_printer) -> None` (raises on bad mode)
  - `run_once(base, key, printer_by_id, *, http_get, http_get_bytes, http_post, raw_fn, pdf_fn) -> bool` — one poll→download→print→report cycle; returns True if a job was handled. Reports `ok=False` with the exception text on print failure.

- [ ] **Step 1: Write the failing test**

```python
# agent/tests/test_agent.py   (replace the whole file)
from agent import print_agent


class FakeHTTP:
    """Records calls; serves a scripted poll result + payload."""
    def __init__(self, poll_result, payload):
        self.poll_result, self.payload = poll_result, payload
        self.posts = []

    def get(self, url, key):            # GET /agent/jobs
        return self.poll_result

    def get_bytes(self, url, key):      # GET payload
        return self.payload

    def post(self, url, key, body):     # register / result
        self.posts.append((url, body))
        return {"ok": True}


def test_run_once_prints_and_reports_success():
    http = FakeHTTP({"job_id": 7, "printer_id": 1, "mode": "raw"}, b"ZPLDATA")
    printed = {}
    raw_fn = lambda printer, data: printed.update(printer=printer, data=data)
    pdf_fn = lambda printer, data: (_ for _ in ()).throw(AssertionError("pdf not expected"))
    handled = print_agent.run_once(
        "http://x", "k", {1: "Zebra"},
        http_get=http.get, http_get_bytes=http.get_bytes, http_post=http.post,
        raw_fn=raw_fn, pdf_fn=pdf_fn)
    assert handled is True
    assert printed == {"printer": "Zebra", "data": b"ZPLDATA"}
    url, body = http.posts[-1]
    assert url.endswith("/agent/jobs/7/result") and body == {"ok": True, "error": None}


def test_run_once_reports_failure_on_print_error():
    http = FakeHTTP({"job_id": 9, "printer_id": 1, "mode": "raw"}, b"x")
    raw_fn = lambda p, d: (_ for _ in ()).throw(RuntimeError("spooler down"))
    handled = print_agent.run_once(
        "http://x", "k", {1: "Zebra"},
        http_get=http.get, http_get_bytes=http.get_bytes, http_post=http.post,
        raw_fn=raw_fn, pdf_fn=lambda p, d: None)
    assert handled is True
    url, body = http.posts[-1]
    assert url.endswith("/agent/jobs/9/result") and body["ok"] is False
    assert "spooler down" in body["error"]


def test_run_once_no_job_returns_false():
    http = FakeHTTP(None, b"")
    assert print_agent.run_once(
        "http://x", "k", {}, http_get=http.get, http_get_bytes=http.get_bytes,
        http_post=http.post, raw_fn=lambda p, d: None, pdf_fn=lambda p, d: None) is False
    assert http.posts == []
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest agent/tests/test_agent.py -v`
Expected: FAIL (`run_once` etc. don't exist).

- [ ] **Step 3: Write minimal implementation**

```python
# agent/print_agent.py   (replace the whole file)
import configparser
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

_UA = "printpapi-agent"


def raw_to_printer(printer, data):
    import win32print  # only on Windows; injected in tests
    h = win32print.OpenPrinter(printer)
    try:
        win32print.StartDocPrinter(h, 1, ("print-agent", None, "RAW"))
        win32print.StartPagePrinter(h)
        win32print.WritePrinter(h, data)
        win32print.EndPagePrinter(h)
        win32print.EndDocPrinter(h)
    finally:
        win32print.ClosePrinter(h)


def pdf_to_printer(printer, data, sumatra="SumatraPDF.exe"):
    fd, path = tempfile.mkstemp(suffix=".pdf")
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
        subprocess.run([sumatra, "-print-to", printer, "-silent", path], check=True, timeout=60)
    finally:
        try:
            os.remove(path)
        except OSError:
            pass


def _req(url, key, *, data=None, method="GET", as_bytes=False):
    r = urllib.request.Request(url, data=data, method=method)
    r.add_header("Authorization", f"Bearer {key}")
    r.add_header("User-Agent", _UA)
    if data is not None:
        r.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(r) as resp:
            raw = resp.read()
            if resp.status == 204:
                return None
            return raw if as_bytes else (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as e:
        raise OSError(f"server returned {e.code}") from e


def _get(url, key):
    return _req(url, key, method="GET")


def _get_bytes(url, key):
    return _req(url, key, method="GET", as_bytes=True)


def _post(url, key, body):
    return _req(url, key, data=json.dumps(body).encode(), method="POST")


def register(base, key, name, printers, http_post=_post):
    return http_post(base + "/agent/register", key, {"name": name, "printers": printers})


def poll_job(base, key, http_get=_get):
    return http_get(base + "/agent/jobs", key)


def download_payload(base, key, job_id, http_get_bytes=_get_bytes):
    return http_get_bytes(base + f"/agent/jobs/{job_id}/payload", key)


def report_result(base, key, job_id, ok, error=None, http_post=_post):
    http_post(base + f"/agent/jobs/{job_id}/result", key, {"ok": ok, "error": error})


def print_job(mode, printer, data, raw_fn=raw_to_printer, pdf_fn=pdf_to_printer):
    if mode == "raw":
        raw_fn(printer, data)
    elif mode == "pdf":
        pdf_fn(printer, data)
    else:
        raise ValueError(f"bad mode: {mode}")


def run_once(base, key, printer_by_id, *, http_get=_get, http_get_bytes=_get_bytes,
             http_post=_post, raw_fn=raw_to_printer, pdf_fn=pdf_to_printer):
    job = poll_job(base, key, http_get=http_get)
    if job is None:
        return False
    job_id = job["job_id"]
    printer = printer_by_id.get(job["printer_id"])
    try:
        data = download_payload(base, key, job_id, http_get_bytes=http_get_bytes)
        if printer is None:
            raise ValueError(f"unknown printer id: {job['printer_id']}")
        print_job(job["mode"], printer, data, raw_fn=raw_fn, pdf_fn=pdf_fn)
        report_result(base, key, job_id, True, None, http_post=http_post)
    except Exception as e:
        report_result(base, key, job_id, False, str(e), http_post=http_post)
    return True


def main():
    base_dir = os.path.dirname(sys.executable) if getattr(sys, "frozen", False) \
        else os.path.dirname(os.path.abspath(__file__))
    cfg = configparser.ConfigParser()
    cfg.read(os.path.join(base_dir, "agent.ini"))
    base = cfg["agent"]["server_url"].rstrip("/")
    key = cfg["agent"]["api_key"]
    name = cfg["agent"].get("name", "agent")
    bundled = os.path.join(base_dir, "SumatraPDF.exe")
    sumatra = bundled if os.path.exists(bundled) else "SumatraPDF.exe"
    pdf_fn = lambda p, d: pdf_to_printer(p, d, sumatra=sumatra)
    printers = [{"name": n.strip(), "can_pdf": True}
                for n in cfg["agent"]["printers"].split(";") if n.strip()]
    reg = register(base, key, name, printers)
    printer_by_id = {pid: pname for pname, pid in reg["printer_ids"].items()}
    print(f"print-agent registered as computer {reg['computer_id']}, printers={printer_by_id}")
    while True:
        try:
            run_once(base, key, printer_by_id, pdf_fn=pdf_fn)
        except Exception as e:
            print(f"poll error: {e}")
            time.sleep(2)


if __name__ == "__main__":
    try:
        main()
    except Exception:
        import traceback
        logdir = os.environ.get("LOCALAPPDATA") or tempfile.gettempdir()
        with open(os.path.join(logdir, "print_agent-error.log"), "a", encoding="utf-8") as f:
            f.write(traceback.format_exc())
        raise
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest agent/tests/test_agent.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add agent/print_agent.py agent/tests/test_agent.py
git commit -m "feat(agent): poll loop (register/poll/download/print/report)"
```

---

## Task 12: Server — main() wiring + background reaper

**Files:**
- Modify: `app/server.py`
- Test: `tests/test_server.py`

**Interfaces:**
- Consumes: everything in `app/server.py` + `store`.
- Produces:
  - `create_server(conn, token, host="127.0.0.1", port=0, **handler_kwargs) -> ThreadingHTTPServer` (handler wired to `conn`/`token`).
  - `start_reaper(conn, *, timeout_s=300, max_retries=2, interval_s=30) -> threading.Thread` (daemon; loops `requeue_stale` every `interval_s`).
  - `main()` — reads `PRINTAPI_TOKEN`, `PRINT_DB` (default `printpapi.db`), `PRINT_PORT` (default `3460`); `connect`+`init_db`; starts the reaper; `serve_forever`.

- [ ] **Step 1: Write the failing test**

```python
# tests/test_server.py   (append)
def test_create_server_serves_health():
    conn = _mem()
    httpd = server.create_server(conn, "t", port=0, long_poll_timeout=0.2, poll_interval=0.05)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{httpd.server_address[1]}"
    try:
        assert _req("GET", base + "/health")[0] == 200
    finally:
        httpd.shutdown()


def test_start_reaper_thread_runs():
    conn = _mem()
    t = server.start_reaper(conn, timeout_s=0, max_retries=5, interval_s=0.05)
    assert t.is_alive()
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_server.py::test_create_server_serves_health -v`
Expected: FAIL (`create_server` undefined).

- [ ] **Step 3: Write minimal implementation**

Add `import threading` at the top of `app/server.py`, then append:

```python
# app/server.py   (append)
def create_server(conn, token, host="127.0.0.1", port=0, **handler_kwargs):
    handler = make_handler(conn=conn, token=token, **handler_kwargs)
    return ThreadingHTTPServer((host, port), handler)


def start_reaper(conn, *, timeout_s=300, max_retries=2, interval_s=30):
    def loop():
        while True:
            try:
                store.requeue_stale(conn, timeout_s, max_retries)
            except Exception:
                pass
            time.sleep(interval_s)
    t = threading.Thread(target=loop, daemon=True)
    t.start()
    return t


def main():
    token = os.environ["PRINTAPI_TOKEN"]
    db_path = os.environ.get("PRINT_DB", "printpapi.db")
    port = int(os.environ.get("PRINT_PORT", "3460"))
    conn = store.connect(db_path)
    store.init_db(conn)
    start_reaper(conn)
    httpd = create_server(conn, token, host="0.0.0.0", port=port)
    print(f"printpapi listening on :{port}")
    httpd.serve_forever()


if __name__ == "__main__":
    main()
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_server.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add app/server.py tests/test_server.py
git commit -m "feat(server): create_server, background reaper, main wiring"
```

---

## Task 13: Full round-trip integration

**Files:**
- Modify (rewrite): `tests/test_integration.py`

**Interfaces:**
- Consumes: `app.server.create_server`, `agent.print_agent.run_once`/`register`, `app.store`.
- Produces: one end-to-end test exercising the real loopback HTTP path: agent registers over HTTP → client submits a job → agent `run_once` (real HTTP poll + download) prints via an injected render fn → reports result → client sees `done`.

- [ ] **Step 1: Write the failing test**

```python
# tests/test_integration.py   (replace the whole file)
import base64, json, threading, urllib.request
from app import store, server
from agent import print_agent


def _req(method, url, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(url, data=data, method=method)
    if token:
        r.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(r) as resp:
        return resp.status, resp.read()


def test_register_submit_poll_print_result_end_to_end():
    conn = store.connect(":memory:")
    store.init_db(conn)
    httpd = server.create_server(conn, "clienttoken", port=0,
                                 long_poll_timeout=2.0, poll_interval=0.05)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{httpd.server_address[1]}"
    try:
        # 1. agent registers over real HTTP
        reg = print_agent.register(base, "agentkey", "win-1",
                                   [{"name": "Zebra", "can_pdf": False}])
        printer_by_id = {pid: name for name, pid in reg["printer_ids"].items()}
        pid = reg["printer_ids"]["Zebra"]

        # 2. client submits a job (base64 "HELLO")
        code, raw = _req("POST", base + "/jobs", token="clienttoken",
                         body={"printer_id": pid, "type": "raw_base64",
                               "content": base64.b64encode(b"HELLO").decode()})
        assert code == 200
        jid = json.loads(raw)["job_id"]

        # 3. agent runs one real poll cycle; capture what got "printed"
        printed = {}
        handled = print_agent.run_once(
            base, "agentkey", printer_by_id,
            raw_fn=lambda p, d: printed.update(p=p, d=d),
            pdf_fn=lambda p, d: None)
        assert handled is True
        assert printed == {"p": "Zebra", "d": b"HELLO"}

        # 4. client sees it done
        code, raw = _req("GET", base + f"/jobs/{jid}", token="clienttoken")
        assert json.loads(raw)["state"] == "done"
    finally:
        httpd.shutdown()
```

- [ ] **Step 2: Run test to verify it fails (if implemented out of order) or passes**

Run: `python -m pytest tests/test_integration.py -v`
Expected: PASS (all prior tasks complete). If it fails, the failure points at the integration gap to fix.

- [ ] **Step 3: Run the whole suite**

Run: `python -m pytest -q`
Expected: all tests pass (`test_dispatch.py`, `test_store.py`, `test_server.py`, `test_integration.py`, `agent/tests/test_agent.py`). No references remain to `PRINT_TARGETS`, `send_agent`, `send_socket`, `send_cups`, `load_targets`, or `resolve_target`.

- [ ] **Step 4: Update docs**

Update `README.md` and `HANDOFF.md` "what works" to describe the poll model (agent registers + long-polls; client `POST /jobs`); note `agent.ini` now needs `server_url`, `api_key`, `name`, `printers`; and that env vars are `PRINTAPI_TOKEN`, `PRINT_DB`, `PRINT_PORT`.

- [ ] **Step 5: Commit**

```bash
git add tests/test_integration.py README.md HANDOFF.md
git commit -m "test: end-to-end poll round-trip; docs for poll model"
```

---

## Self-Review

**1. Spec coverage**

| Spec item | Task |
|-----------|------|
| Agent registers + auto-lists printers | 2 (store), 9 (route), 11 (agent), 13 (e2e) |
| Job queue, `queued→claimed→done\|failed` | 3, 4, 5 |
| Long-poll `GET /agent/jobs` (~25s, 204) | 10 |
| Payload as separate octet-stream download | 9 (route), 11 (agent) |
| Result reporting | 5 (store), 9 (route), 11 (agent) |
| Server holds bytes (`decode_payload` at submit) | 8 |
| Tenancy columns + seed default org/user | 1 |
| SQLite WAL + global write lock | 1 |
| Atomic claim (no double-claim) | 4 |
| Visibility-timeout reaper + bounded retries | 6, 12 |
| Online/offline via `last_seen_at` | 4 (heartbeat), 7 (flag) |
| Client endpoints: `POST /jobs`, `GET /jobs/{id}`, `GET /printers`, `/health` | 8 |
| Agent endpoints: register/jobs/payload/result | 9, 10 |
| Bearer (client) + per-agent key (agent), `hmac.compare_digest`, hashed keys | 1, 2, 8, 9 |
| Agent registration binds name↔key (no name-based hijack) | 2 (store `AuthError`), 9 (route → 401) |
| Agent scoped to own jobs | 5, 9 |
| `http(s)`-only + browser UA kept | inherited (dispatch.py unchanged), 8 |
| No `shell=True`, temp cleanup | 11 (agent kept) |
| Push code removed | 8 (rewrite supersedes), 13 (verified) |
| Scope cut: raw+pdf Windows only; content types kept | 11; 8 |
| Tests: store atomicity/reaper/upsert; full round-trip; long-poll; auth | 4,6,2; 13; 10; 8,9 |

No uncovered spec items.

**2. Placeholder scan:** No "TBD"/"add error handling"/"similar to Task N". Every code step shows complete code. Pass.

**3. Type consistency:** `register_agent`→`{computer_id, printer_ids}` used identically in Tasks 2, 9, 11, 13. `claim_job`→`{job_id, printer_id, mode}` consumed in Tasks 10, 11. `finish_job(conn, job_id, agent_id, ok, error)` matches its call in Task 9. `run_once(...)` keyword signature matches Tasks 11 and 13. `make_handler` kwargs (`long_poll_timeout`, `poll_interval`, `online_window_s`) declared in Task 8, used in 10/12. Consistent.
