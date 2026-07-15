# PrintNode-clone WebUI + direct :9100 socket — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give printpapi a PrintNode-shaped web UI (sidebar nav, print-from-browser, devices, history, API-key management, downloads) and let the agent print straight to a network printer by IP (`socket://host:port`).

**Architecture:** Two independent parts. **Part A** (Tasks 1-3) extends the agent so a printer entry can be a raw TCP socket target; the agent opens the socket — poll model stays intact, works behind NAT. **Part B** (Tasks 4-8) adds a `pdf_uri` content type, a `jobs.title` column + `agent_name` in history, and replaces the inline dashboard with a static multi-view SPA served from `app/dashboard.html`.

**Tech Stack:** Python stdlib only (`http.server`, `sqlite3`, `socket`, `urllib`, `pathlib`); pytest; plain HTML/CSS/JS (no framework, no build step).

## Global Constraints

- **Server: Python stdlib only.** No web framework, no new dependencies (CLAUDE.md).
- **Agent: cross-platform**, stdlib `socket` for the network path.
- **Tests: pytest**, no fixtures/frameworks beyond it. Real loopback `ThreadingHTTPServer` + real SQLite (`:memory:` or tempfile), injected fns — no mocks. Run `python -m pytest` from repo root.
- **Security stays** (CLAUDE.md trust-boundary rules): bearer/API-key auth on every endpoint, `hmac.compare_digest`, `http(s)`-only URL fetch, no `shell=True`, temp-file cleanup. Do not regress any of these.
- **Gotcha #1:** never send PDF bytes to a raw/label/socket printer. A `socket://` printer is forced `can_pdf=False`; `pdf` mode to a socket printer raises.
- **No license headers** — existing source files carry none; match the repo.
- TDD, frequent commits. Branch: `feat/printnode-clone-webui` (already checked out).

---

## Part A — Direct network-printer output (agent)

### Task 1: `parse_printers` gains a `target` field

**Files:**
- Modify: `agent/print_agent.py:57-66` (`parse_printers`)
- Test: `agent/tests/test_agent.py`

**Interfaces:**
- Produces: `parse_printers(spec) -> [{"name": str, "can_pdf": bool, "target": str}]`. `target` defaults to `name`; a `socket://host:port` target forces `can_pdf=False`.

- [ ] **Step 1: Update the existing parse test for the new `target` key**

In `agent/tests/test_agent.py`, replace `test_parse_printers_pdf_is_opt_in_default_raw` with:

```python
def test_parse_printers_pdf_is_opt_in_default_raw():
    ps = print_agent.parse_printers("Zebra GK420d; HP LaserJet|pdf ; Office|PDF; ")
    # default is raw-only so a label printer is never auto-sent a PDF (gotcha #1);
    # a document printer opts into PDF with a '|pdf' tag (case-insensitive).
    assert ps == [
        {"name": "Zebra GK420d", "can_pdf": False, "target": "Zebra GK420d"},
        {"name": "HP LaserJet", "can_pdf": True, "target": "HP LaserJet"},
        {"name": "Office", "can_pdf": True, "target": "Office"},
    ]
```

- [ ] **Step 2: Add the failing socket-grammar tests**

Append to `agent/tests/test_agent.py`:

```python
def test_parse_printers_socket_target():
    ps = print_agent.parse_printers("Bixolon ; netz = socket://10.0.0.5:9100")
    assert ps == [
        {"name": "Bixolon", "can_pdf": False, "target": "Bixolon"},
        {"name": "netz", "can_pdf": False, "target": "socket://10.0.0.5:9100"},
    ]


def test_parse_printers_socket_is_forced_raw_only():
    # |pdf on a socket target is ignored — no renderer behind a bare socket (gotcha #1)
    ps = print_agent.parse_printers("lbl|pdf = socket://10.0.0.5:9100")
    assert ps == [{"name": "lbl", "can_pdf": False, "target": "socket://10.0.0.5:9100"}]
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `python -m pytest agent/tests/test_agent.py -k parse_printers -v`
Expected: FAIL (`test_parse_printers_pdf_is_opt_in_default_raw` KeyError/mismatch on `target`; socket tests fail — no `target` produced).

- [ ] **Step 4: Implement the new `parse_printers`**

Replace `parse_printers` in `agent/print_agent.py` with:

```python
def parse_printers(spec):
    """agent.ini 'printers' (semicolon-separated) -> [{name, can_pdf, target}].
    Grammar: name [|pdf] [= target].
      - no '='  -> target is the name (a CUPS queue / Windows printer).
      - '= socket://host:port' -> agent opens a raw TCP socket to it.
    Append '|pdf' to declare a document printer PDF-capable; default is raw-only so a label
    printer is never auto-sent a PDF (gotcha #1). A socket:// target is always raw-only."""
    out = []
    for entry in spec.split(";"):
        left, _, target = entry.partition("=")
        name, _, tag = left.strip().partition("|")
        name = name.strip()
        if not name:
            continue
        target = target.strip() or name
        can_pdf = tag.strip().lower() == "pdf"
        if target.startswith("socket://"):
            can_pdf = False  # gotcha #1: no renderer behind a bare socket
        out.append({"name": name, "can_pdf": can_pdf, "target": target})
    return out
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `python -m pytest agent/tests/test_agent.py -k parse_printers -v`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add agent/print_agent.py agent/tests/test_agent.py
git commit -m "feat(agent): parse_printers supports socket:// targets (raw-only)"
```

---

### Task 2: `raw_to_socket` sender

**Files:**
- Modify: `agent/print_agent.py` (add `import socket` at top; add `raw_to_socket`)
- Test: `agent/tests/test_agent.py`

**Interfaces:**
- Produces: `raw_to_socket(target, data, connect=socket.create_connection, timeout=30)` — parses `socket://host:port`, opens the connection, `sendall(data)`. `connect` is injectable for tests.

- [ ] **Step 1: Write the failing test**

Append to `agent/tests/test_agent.py`:

```python
def test_raw_to_socket_parses_addr_and_sends_bytes():
    seen = {}

    class FakeSock:
        def __enter__(self): return self
        def __exit__(self, *a): return False
        def sendall(self, d): seen["data"] = d

    def fake_connect(addr, timeout=None):
        seen["addr"] = addr
        return FakeSock()

    print_agent.raw_to_socket("socket://10.0.0.5:9100", b"^XA^XZ", connect=fake_connect)
    assert seen["addr"] == ("10.0.0.5", 9100)
    assert seen["data"] == b"^XA^XZ"
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest agent/tests/test_agent.py::test_raw_to_socket_parses_addr_and_sends_bytes -v`
Expected: FAIL with `AttributeError: module ... has no attribute 'raw_to_socket'`.

- [ ] **Step 3: Implement**

Add `import socket` to the imports at the top of `agent/print_agent.py`, then add near the other `*_to_printer*` functions:

```python
def raw_to_socket(target, data, connect=socket.create_connection, timeout=30):
    # Raw bytes straight to a network printer's socket (e.g. Zebra :9100). Already-rendered only
    # (ZPL/ESC-POS) — gotcha #1: a bare socket has no renderer, so PDF must never reach here.
    addr = target[len("socket://"):] if target.startswith("socket://") else target
    host, _, port = addr.rpartition(":")  # ponytail: IPv4 host:port; no IPv6-in-brackets support
    with connect((host, int(port)), timeout=timeout) as s:
        s.sendall(data)
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest agent/tests/test_agent.py::test_raw_to_socket_parses_addr_and_sends_bytes -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add agent/print_agent.py agent/tests/test_agent.py
git commit -m "feat(agent): raw_to_socket sends bytes to a network printer socket"
```

---

### Task 3: `print_job` routes socket vs local; wire `run_once`/`main`

**Files:**
- Modify: `agent/print_agent.py` (`print_job`, `main`)
- Test: `agent/tests/test_agent.py` (new socket-routing tests; update `test_print_job_bad_mode_raises` and the three `run_once` tests to pass printer **entries**)

**Interfaces:**
- Consumes: entries from `parse_printers` (Task 1); `raw_to_socket` (Task 2).
- Produces: `print_job(mode, entry, data, raw_fn=raw_to_printer, pdf_fn=pdf_to_printer, socket_fn=raw_to_socket)` — `entry` is a `{"name","can_pdf","target"}` dict. `run_once`'s `printer_by_id` now maps `job_id`→**entry** (not a name string).

- [ ] **Step 1: Update the existing tests that pass a printer as a bare string**

In `agent/tests/test_agent.py`:

Replace `test_print_job_bad_mode_raises` with:

```python
def test_print_job_bad_mode_raises():
    entry = {"name": "P", "can_pdf": False, "target": "P"}
    with pytest.raises(ValueError, match="bad mode"):
        print_agent.print_job("docx", entry, b"x")
```

In the three `run_once` tests, change the `printer_by_id` argument from `{1: "Zebra"}` to an entry dict `{1: {"name": "Zebra", "can_pdf": False, "target": "Zebra"}}`. Concretely:

- `test_run_once_prints_and_reports_success`: `"http://x", "k", {1: {"name": "Zebra", "can_pdf": False, "target": "Zebra"}},`
- `test_run_once_reports_failure_on_print_error`: same `{1: {"name": "Zebra", "can_pdf": False, "target": "Zebra"}}`
- `test_run_once_prints_pdf_and_reports_success`: `{1: {"name": "Zebra", "can_pdf": True, "target": "Zebra"}}`

(The `raw_fn`/`pdf_fn` lambdas receive `entry["target"]` == `"Zebra"`, so the `printed == {"printer": "Zebra", ...}` assertions still hold. `test_run_once_no_job_returns_false` keeps `{}`.)

- [ ] **Step 2: Add the failing socket-routing tests**

Append to `agent/tests/test_agent.py`:

```python
def test_print_job_socket_raw_routes_to_socket_fn():
    seen = {}
    entry = {"name": "netz", "can_pdf": False, "target": "socket://10.0.0.5:9100"}
    print_agent.print_job(
        "raw", entry, b"Z",
        socket_fn=lambda t, d: seen.update(t=t, d=d),
        raw_fn=lambda *a: (_ for _ in ()).throw(AssertionError("local raw not expected")),
        pdf_fn=lambda *a: None)
    assert seen == {"t": "socket://10.0.0.5:9100", "d": b"Z"}


def test_print_job_socket_pdf_is_refused():
    entry = {"name": "netz", "can_pdf": False, "target": "socket://10.0.0.5:9100"}
    with pytest.raises(ValueError, match="raw-only"):
        print_agent.print_job("pdf", entry, b"%PDF",
                              socket_fn=lambda t, d: None,
                              raw_fn=lambda *a: None, pdf_fn=lambda *a: None)


def test_print_job_local_raw_uses_target_name():
    seen = {}
    entry = {"name": "Zebra", "can_pdf": False, "target": "Zebra"}
    print_agent.print_job("raw", entry, b"x",
                          raw_fn=lambda t, d: seen.update(t=t, d=d), pdf_fn=lambda *a: None)
    assert seen == {"t": "Zebra", "d": b"x"}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `python -m pytest agent/tests/test_agent.py -v`
Expected: the socket-routing tests FAIL (`print_job` still expects a string printer / has no `socket_fn`).

- [ ] **Step 4: Implement `print_job` routing and wire `main`**

Replace `print_job` in `agent/print_agent.py` with:

```python
def print_job(mode, entry, data, raw_fn=raw_to_printer, pdf_fn=pdf_to_printer,
              socket_fn=raw_to_socket):
    target = entry["target"]
    if target.startswith("socket://"):
        if mode != "raw":
            raise ValueError("network socket printer is raw-only (cannot render PDF)")
        socket_fn(target, data)
        return
    if mode == "raw":
        raw_fn(target, data)
    elif mode == "pdf":
        pdf_fn(target, data)
    else:
        raise ValueError(f"bad mode: {mode}")
```

`run_once` needs no code change — it already does `printer = printer_by_id.get(job["printer_id"])` and passes it to `print_job`; that value is now an entry dict.

In `main()`, change how `printer_by_id` is built so it maps id→entry:

```python
    printers = parse_printers(cfg["agent"]["printers"])
    reg = register(base, key, name, printers)
    entry_by_name = {p["name"]: p for p in printers}
    printer_by_id = {pid: entry_by_name[pname] for pname, pid in reg["printer_ids"].items()}
    print(f"print-agent registered as computer {reg['computer_id']}, printers={printer_by_id}")
```

(The `register` call still sends the full entry dicts; the server reads only `name`/`can_pdf` and ignores `target`.)

- [ ] **Step 5: Run the whole agent suite**

Run: `python -m pytest agent/tests/test_agent.py -v`
Expected: PASS (all, including the updated `run_once`/`bad_mode` tests and the 3 new socket tests).

- [ ] **Step 6: Commit**

```bash
git add agent/print_agent.py agent/tests/test_agent.py
git commit -m "feat(agent): print_job routes socket:// printers; run_once carries entries"
```

---

## Part B — WebUI

### Task 4: `pdf_uri` content type

**Files:**
- Modify: `app/dispatch.py:54-59` (`raw_uri` branch) and `agent_mode`
- Test: `tests/test_dispatch.py`

**Interfaces:**
- Produces: `type: "pdf_uri"` — server GETs `url` and prints the bytes as PDF. `agent_mode("pdf_uri") == "pdf"`.

- [ ] **Step 1: Write the failing test**

Append to `tests/test_dispatch.py`:

```python
def test_pdf_uri_uses_injected_fetcher_and_is_pdf_mode():
    seen = {}
    def fake(url):
        seen["url"] = url
        return b"%PDF-bytes"
    assert decode_payload({"type": "pdf_uri", "url": "https://x/a.pdf"}, fetch_url=fake) == b"%PDF-bytes"
    assert seen["url"] == "https://x/a.pdf"
    assert agent_mode("pdf_uri") == "pdf"
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_dispatch.py::test_pdf_uri_uses_injected_fetcher_and_is_pdf_mode -v`
Expected: FAIL (`DispatchError: unknown type: 'pdf_uri'`).

- [ ] **Step 3: Implement**

In `app/dispatch.py`, change the `raw_uri` branch to accept `pdf_uri` too:

```python
    if t in ("raw_uri", "pdf_uri"):
        url = _checked_http_url(body.get("url"))
        try:
            return fetch_url(url)
        except Exception as e:
            raise FetchError(f"fetch failed: {e}") from e
```

And extend `agent_mode`:

```python
def agent_mode(type_):
    return "pdf" if type_ in ("pdf_base64", "pdf_uri_post", "pdf_uri") else "raw"
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `python -m pytest tests/test_dispatch.py -v`
Expected: PASS (new test + all existing dispatch tests).

- [ ] **Step 5: Commit**

```bash
git add app/dispatch.py tests/test_dispatch.py
git commit -m "feat: add pdf_uri content type (GET a URL, print as PDF)"
```

---

### Task 5: `jobs.title` column + `agent_name` in history

**Files:**
- Modify: `app/store.py` (`_SCHEMA` jobs table; `init_db` migration; `enqueue_job`; `recent_jobs`)
- Test: `tests/test_store.py`

**Interfaces:**
- Produces: `enqueue_job(conn, printer_id, type_, mode, payload, user_id=DEFAULT_USER, title=None)`; `recent_jobs` rows gain `title` and `agent_name`.

- [ ] **Step 1: Write the failing tests**

Append to `tests/test_store.py`:

```python
def test_enqueue_and_recent_jobs_carry_title_and_agent_name():
    conn = _db()
    reg = store.register_agent(conn, "pc-1", "k", [{"name": "Z", "can_pdf": False}])
    pid = reg["printer_ids"]["Z"]
    store.enqueue_job(conn, pid, "raw_base64", "raw", b"x", title="Versandlabel")
    row = store.recent_jobs(conn)[0]
    assert row["title"] == "Versandlabel"
    assert row["agent_name"] == "pc-1"
    assert row["printer_name"] == "Z"


def test_title_column_migration_is_idempotent():
    conn = _db()
    store.init_db(conn)  # run migration a second time — must not raise or duplicate
    cols = [r["name"] for r in conn.execute("PRAGMA table_info(jobs)").fetchall()]
    assert cols.count("title") == 1
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `python -m pytest tests/test_store.py -k "title or agent_name" -v`
Expected: FAIL (`enqueue_job` has no `title` kwarg / `recent_jobs` lacks `title`+`agent_name`).

- [ ] **Step 3: Add `title` to the schema and an idempotent migration**

In `app/store.py`, add `title TEXT` to the `jobs` CREATE (self-documenting for fresh DBs):

```python
CREATE TABLE IF NOT EXISTS jobs(
  id INTEGER PRIMARY KEY AUTOINCREMENT, org_id INTEGER NOT NULL, user_id INTEGER NOT NULL,
  printer_id INTEGER NOT NULL, agent_id INTEGER NOT NULL, type TEXT NOT NULL, mode TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'queued', payload BLOB NOT NULL, error TEXT, title TEXT,
  retries INTEGER NOT NULL DEFAULT 0, created_at REAL NOT NULL, claimed_at REAL, finished_at REAL);
```

In `init_db`, right after `conn.executescript(_SCHEMA)` (still inside the `with _LOCK: try:` block), add the migration for DBs created before this change:

```python
            conn.executescript(_SCHEMA)
            try:
                conn.execute("ALTER TABLE jobs ADD COLUMN title TEXT")
            except sqlite3.OperationalError:
                pass  # column already exists (fresh DB or prior run) — idempotent
```

- [ ] **Step 4: Thread `title` through `enqueue_job` and `recent_jobs`**

`enqueue_job` — add the `title` param and column:

```python
def enqueue_job(conn, printer_id, type_, mode, payload, user_id=DEFAULT_USER, title=None):
    now = time.time()
    with _LOCK:
        try:
            p = conn.execute("SELECT org_id, agent_id FROM printers WHERE id=?",
                             (printer_id,)).fetchone()
            if p is None:
                raise UnknownPrinter(f"unknown printer: {printer_id}")
            cur = conn.execute(
                "INSERT INTO jobs(org_id, user_id, printer_id, agent_id, type, mode, state, "
                "payload, title, created_at) VALUES(?,?,?,?,?,?,?,?,?,?)",
                (p["org_id"], user_id, printer_id, p["agent_id"], type_, mode, "queued",
                 sqlite3.Binary(payload), title, now))
            conn.commit()
            return cur.lastrowid
        except Exception:
            conn.rollback()
            raise
```

`recent_jobs` — join `agents` and return `agent_name` + `title`:

```python
def recent_jobs(conn, limit=50):
    with _LOCK:
        rows = conn.execute(
            "SELECT j.id, j.printer_id, p.name AS printer_name, a.name AS agent_name, j.title, "
            "j.state, j.type, j.mode, j.error, j.created_at, j.finished_at "
            "FROM jobs j JOIN printers p ON p.id = j.printer_id "
            "JOIN agents a ON a.id = j.agent_id "
            "ORDER BY j.id DESC LIMIT ?", (limit,)).fetchall()
    return [dict(r) for r in rows]
```

- [ ] **Step 5: Run the store suite**

Run: `python -m pytest tests/test_store.py -v`
Expected: PASS (new tests + existing `test_recent_jobs_*` and `test_init_seeds_*` still green).

- [ ] **Step 6: Commit**

```bash
git add app/store.py tests/test_store.py
git commit -m "feat: job title column (idempotent migration) + agent_name in history"
```

---

### Task 6: `POST /jobs` accepts `title`

**Files:**
- Modify: `app/server.py` (the `POST /jobs` branch, ~line 304)
- Test: `tests/test_server.py`

**Interfaces:**
- Consumes: `store.enqueue_job(..., title=...)` (Task 5).

- [ ] **Step 1: Write the failing test**

Append to `tests/test_server.py`:

```python
def test_job_title_roundtrips_through_http():
    conn = _mem()
    reg = store.register_agent(conn, "pc", "ak", [{"name": "Z", "can_pdf": False}])
    pid = reg["printer_ids"]["Z"]
    httpd, base = _serve(conn)
    try:
        code, _ = _req("POST", base + "/jobs", token="t",
                       body={"printer_id": pid, "type": "raw_base64",
                             "content": "QUJD", "title": "Label #5"})
        assert code == 200
        jobs = json.loads(_req("GET", base + "/jobs", token="t")[1])["jobs"]
        assert jobs[0]["title"] == "Label #5"
        assert jobs[0]["agent_name"] == "pc"
    finally:
        httpd.shutdown()
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_server.py::test_job_title_roundtrips_through_http -v`
Expected: FAIL (`title` is `None` in the response — server doesn't pass it).

- [ ] **Step 3: Implement**

In `app/server.py`, in the `POST /jobs` branch, pass the title to `enqueue_job`:

```python
                    jid = store.enqueue_job(conn, body.get("printer_id"), body.get("type"),
                                            mode, data, title=body.get("title"))
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_server.py -v`
Expected: PASS (new test + all existing server tests).

- [ ] **Step 5: Commit**

```bash
git add app/server.py tests/test_server.py
git commit -m "feat: POST /jobs stores an optional job title"
```

---

### Task 7: Create `app/dashboard.html` (multi-view SPA)

**Files:**
- Create: `app/dashboard.html`

**Interfaces:**
- Consumes (client-side, over HTTP): `GET /printers`, `GET /jobs`, `POST /jobs`, `GET/POST /apikeys`, `DELETE /apikeys/{id}`. Uses content types `pdf_base64`/`raw_base64`/`raw_uri`/`pdf_uri` and the `title` field (Tasks 4-6).
- Produces: the static asset `app/server.py` loads in Task 8. Placeholders `__PDF_B64__` / `__ZPL_B64__` are substituted at load time. Must contain the literal substrings the existing `test_dashboard_served_at_root_as_html` asserts: `<!doctype html>`, `printpapi`, `/printers`, `/jobs`.

- [ ] **Step 1: Create the file with this exact content**

Create `app/dashboard.html`:

```html
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>printpapi</title>
<style>
:root{color-scheme:light dark;--bg:#f4f5f7;--panel:#fff;--ink:#1f2430;--muted:#6b7280;--line:#e5e7eb;--side:#1f2430;--side-ink:#cbd2e0;--accent:#f59e0b;--accent-ink:#7c4a03;--blue:#2f6feb;}
*{box-sizing:border-box}
body{margin:0;font:15px/1.5 system-ui,-apple-system,Segoe UI,Roboto,sans-serif;background:var(--bg);color:var(--ink)}
a{color:var(--blue);text-decoration:none}
code{font-family:ui-monospace,SFMono-Regular,Menlo,monospace}
#login{max-width:22rem;margin:8vh auto;background:var(--panel);border:1px solid var(--line);border-radius:12px;padding:1.6rem}
#login h1{margin:.2rem 0 1rem;font-size:1.3rem}
input,select{font:inherit;color:var(--ink);background:var(--panel);border:1px solid #cbd0da;border-radius:8px;padding:.5rem .6rem;width:100%}
button{font:inherit;cursor:pointer;border:0;border-radius:8px;padding:.5rem .9rem;background:var(--blue);color:#fff}
button:hover{filter:brightness(1.07)}
button.accent{background:var(--accent);color:var(--accent-ink);font-weight:700}
button.ghost{background:#eceef2;color:var(--ink)}
button.danger{background:#ef4444;color:#fff}
#app{display:none;grid-template-columns:220px 1fr 300px;min-height:100vh}
#app.show{display:grid}
nav{background:var(--side);color:var(--side-ink);padding:1rem .75rem;display:flex;flex-direction:column;gap:.25rem}
nav .brand{color:#fff;font-weight:700;font-size:1.15rem;padding:.4rem .6rem 1rem}
nav a{color:var(--side-ink);padding:.55rem .7rem;border-radius:8px}
nav a.active,nav a:hover{background:#2b3140;color:#fff}
main{padding:1.4rem 1.6rem;min-width:0}
aside{padding:1.4rem 1.2rem;border-left:1px solid var(--line);background:var(--panel)}
aside h3{margin:.2rem 0 .5rem;color:var(--accent-ink)}
.topbar{display:flex;align-items:center;gap:1rem;margin-bottom:1rem}
h2{font-size:1.3rem;margin:.2rem 0}
.card{background:var(--panel);border:1px solid var(--line);border-radius:12px;padding:1rem 1.1rem;margin-bottom:1.2rem}
label.f{display:block;font-size:.85rem;color:var(--muted);margin:.7rem 0 .25rem}
.row{display:flex;gap:.6rem;align-items:center}
table{width:100%;border-collapse:collapse}
th,td{text-align:left;padding:.55rem .6rem;border-bottom:1px solid var(--line);white-space:nowrap}
th{font-size:.75rem;text-transform:uppercase;letter-spacing:.4px;color:var(--muted)}
tr:last-child td{border-bottom:0}
.badge{display:inline-block;padding:.1rem .5rem;border-radius:999px;font-size:.78rem;font-weight:600}
.on{background:#dcfce7;color:#15803d}.off{background:#fee2e2;color:#b91c1c}
.queued{background:#e5e7eb;color:#374151}.claimed{background:#dbeafe;color:#1d4ed8}.done{background:#dcfce7;color:#15803d}.failed{background:#fee2e2;color:#b91c1c}
.muted{color:var(--muted)}.err{color:#b91c1c}
.drop{border:2px dashed #cbd0da;border-radius:10px;padding:1.4rem;text-align:center;color:var(--muted);cursor:pointer}
.drop.hl{border-color:var(--blue);color:var(--blue)}
.keybox{background:#fff7ed;border:1px solid #fed7aa;border-radius:8px;padding:.6rem .7rem;font-family:ui-monospace,monospace;word-break:break-all;margin-top:.7rem}
.jrow{cursor:pointer}
pre{background:#0f1115;color:#e7e9ee;border-radius:8px;padding:.8rem;overflow:auto}
.hint{font-size:.9rem;color:var(--muted)}
</style>
</head>
<body>
<div id="login">
  <h1>printpapi</h1>
  <p class="hint">Enter your API token to connect.</p>
  <input id="tok" type="password" placeholder="API token" autocomplete="off">
  <div style="height:.6rem"></div>
  <button id="connect" style="width:100%">Connect</button>
  <p id="loginerr" class="err"></p>
</div>

<div id="app">
  <nav>
    <div class="brand">🖨 printpapi</div>
    <a href="#print">Print Something</a>
    <a href="#devices">Devices</a>
    <a href="#history">Print History</a>
    <a href="#keys">API Keys</a>
    <a href="#downloads">Downloads</a>
    <div style="flex:1"></div>
    <a href="#" id="signout">Sign Out</a>
  </nav>
  <main>
    <div class="topbar"><h2 id="title">Print Something</h2></div>
    <div id="view"></div>
  </main>
  <aside><h3>Help &amp; Tips</h3><div id="help" class="hint"></div></aside>
</div>

<script>
const PDF_B64="__PDF_B64__", ZPL_B64="__ZPL_B64__";
const $=s=>document.querySelector(s);
const esc=s=>(s==null?"":String(s)).replace(/[&<>"]/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;"}[c]));
const token=()=>localStorage.getItem("printpapi_token")||"";
const fmt=t=>t?new Date(t*1000).toLocaleString():"";
let PRINTERS=[], JUST_ISSUED=null;

async function api(path,opts){
  opts=opts||{};
  opts.headers=Object.assign({},opts.headers,{Authorization:"Bearer "+token()});
  const r=await fetch(path,opts);
  if(r.status===401){ logout(); throw new Error("unauthorized"); }
  return r;
}
function fileToB64(file){
  return new Promise((res,rej)=>{const r=new FileReader();
    r.onload=()=>res(r.result.split(",",2)[1]); r.onerror=rej; r.readAsDataURL(file);});
}
async function loadPrinters(){ const r=await api("/printers"); PRINTERS=(await r.json()).printers||[]; return PRINTERS; }

function showApp(){ $("#login").style.display="none"; $("#app").classList.add("show"); }
function showLogin(){ $("#app").classList.remove("show"); $("#login").style.display="block"; }
function logout(){ localStorage.removeItem("printpapi_token"); showLogin(); }
$("#connect").onclick=async()=>{
  const t=$("#tok").value.trim(); if(!t)return;
  localStorage.setItem("printpapi_token",t);
  try{ const r=await fetch("/printers",{headers:{Authorization:"Bearer "+t}});
    if(r.status===401){ $("#loginerr").textContent="Invalid token."; localStorage.removeItem("printpapi_token"); return; }
    if(!location.hash) location.hash="#print"; showApp(); route();
  }catch(e){ $("#loginerr").textContent="Cannot reach server."; }
};
$("#tok").addEventListener("keydown",e=>{if(e.key==="Enter")$("#connect").click();});
$("#signout").onclick=e=>{e.preventDefault(); logout();};

const HELP={
  print:"Pick a source and a printer, then hit PRINT. Label printers (raw) can’t render PDFs — upload ZPL/raw for those.",
  devices:"Printers registered by your agents. Green = the agent polled within the last minute. Test-print sends a sample label or PDF.",
  history:"Recent jobs. Click a row for type, mode and any error. State: queued → claimed → done/failed.",
  keys:"Per-client API keys — one per integration. The key is shown once; revoke to cut access instantly.",
  downloads:"Install the agent on the machine with the printers. It polls this server — no inbound ports needed."
};
const TITLES={print:"Print Something",devices:"Devices",history:"Print History",keys:"API Keys",downloads:"Downloads"};
function route(){
  const h=location.hash.replace("#","")||"print";
  const view=["print","devices","history","keys","downloads"].includes(h)?h:"print";
  document.querySelectorAll("nav a").forEach(a=>a.classList.toggle("active",a.getAttribute("href")==="#"+view));
  $("#title").textContent=TITLES[view];
  $("#help").innerHTML=HELP[view];
  ({print:viewPrint,devices:viewDevices,history:viewHistory,keys:viewKeys,downloads:viewDownloads}[view])();
}
window.addEventListener("hashchange",route);

async function viewPrint(){
  await loadPrinters();
  const opts=PRINTERS.map(p=>`<option value="${p.id}" data-pdf="${p.can_pdf?1:0}">${esc(p.name)} — ${esc(p.agent_name)}${p.online?"":" (offline)"}</option>`).join("");
  $("#view").innerHTML=`
   <div class="card" style="max-width:640px">
    <label class="f">Source</label>
    <select id="src">
      <option value="pdf">Upload a PDF (document printer)</option>
      <option value="raw">Upload a raw file (ZPL / label printer)</option>
      <option value="url">Fetch from URL</option>
      <option value="test">Test document</option>
    </select>
    <label class="f">Printer</label>
    <select id="printer">${opts||'<option value="">No printers registered</option>'}</select>
    <label class="f">Title (optional)</label>
    <input id="jobtitle" placeholder="e.g. Versandlabel">
    <div id="srcinput"></div>
    <div style="height:1rem"></div>
    <button class="accent" id="print">PRINT</button>
    <span id="printmsg" class="hint"></span>
   </div>`;
  let picked=null;
  function wireDrop(){
    const drop=$("#drop"), file=$("#file"); if(!drop)return;
    drop.onclick=()=>file.click();
    file.onchange=()=>{picked=file.files[0]; $("#fname").textContent=picked?picked.name:"";};
    ["dragover","dragenter"].forEach(ev=>drop.addEventListener(ev,e=>{e.preventDefault();drop.classList.add("hl");}));
    ["dragleave"].forEach(ev=>drop.addEventListener(ev,e=>{e.preventDefault();drop.classList.remove("hl");}));
    drop.addEventListener("drop",e=>{e.preventDefault();drop.classList.remove("hl");picked=e.dataTransfer.files[0]; $("#fname").textContent=picked?picked.name:"";});
  }
  function srcInput(){
    const s=$("#src").value;
    if(s==="url") $("#srcinput").innerHTML=`<label class="f">URL</label><input id="url" placeholder="https://…"><label class="f"><input type="checkbox" id="urlpdf" style="width:auto"> This URL returns a PDF (render it)</label>`;
    else if(s==="test") $("#srcinput").innerHTML=`<p class="hint">Sends the built-in test page (PDF for document printers, a ZPL label otherwise).</p>`;
    else $("#srcinput").innerHTML=`<label class="f">File</label><div class="drop" id="drop">Drag &amp; drop or click to choose a file</div><input type="file" id="file" style="display:none"><span id="fname" class="hint"></span>`;
    wireDrop();
  }
  function msg(t,isErr){ const m=$("#printmsg"); m.textContent=" "+t; m.className="hint "+(isErr?"err":""); }
  $("#src").onchange=()=>{picked=null; srcInput();}; srcInput();
  $("#print").onclick=async()=>{
    const sel=$("#printer"), opt=sel.selectedOptions[0];
    const printer_id=Number(sel.value); const canPdf=!!(opt&&opt.dataset.pdf==="1");
    if(!printer_id) return msg("Select a printer.",true);
    const title=$("#jobtitle").value.trim()||null; const s=$("#src").value; let body;
    try{
      if(s==="pdf"){ if(!canPdf) return msg("This printer is raw-only (label). Use a raw file.",true);
        if(!picked) return msg("Choose a PDF first.",true);
        body={printer_id,type:"pdf_base64",content:await fileToB64(picked),title}; }
      else if(s==="raw"){ if(!picked) return msg("Choose a file first.",true);
        body={printer_id,type:"raw_base64",content:await fileToB64(picked),title}; }
      else if(s==="url"){ const u=$("#url").value.trim(); if(!u) return msg("Enter a URL.",true);
        const asPdf=$("#urlpdf").checked; if(asPdf&&!canPdf) return msg("This printer can’t render PDF.",true);
        body={printer_id,type:asPdf?"pdf_uri":"raw_uri",url:u,title}; }
      else { body=canPdf?{printer_id,type:"pdf_base64",content:PDF_B64,title:title||"test page"}
                        :{printer_id,type:"raw_base64",content:ZPL_B64,title:title||"test label"}; }
      const r=await api("/jobs",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(body)});
      msg(r.ok?"Job queued.":"Failed ("+r.status+").",!r.ok);
    }catch(e){ msg("Error: "+e.message,true); }
  };
}

async function viewDevices(){
  await loadPrinters();
  const rows=PRINTERS.length?PRINTERS.map(p=>`<tr>
    <td>${esc(p.name)}</td><td class="muted">${esc(p.agent_name)}</td>
    <td><span class="badge ${p.online?"on":"off"}">${p.online?"online":"offline"}</span></td>
    <td>${p.can_pdf?"yes":"no"}</td>
    <td><button class="ghost" data-pid="${p.id}" data-pdf="${p.can_pdf?1:0}">Test print</button></td>
    </tr>`).join(""):`<tr><td colspan="5" class="muted">No printers registered yet.</td></tr>`;
  $("#view").innerHTML=`<div class="card"><table>
    <thead><tr><th>Printer</th><th>Computer</th><th>Status</th><th>PDF</th><th></th></tr></thead>
    <tbody>${rows}</tbody></table></div>`;
  $("#view").querySelectorAll("button[data-pid]").forEach(b=>b.onclick=async()=>{
    const pid=Number(b.dataset.pid), canPdf=b.dataset.pdf==="1";
    const body=canPdf?{printer_id:pid,type:"pdf_base64",content:PDF_B64,title:"test page"}
                     :{printer_id:pid,type:"raw_base64",content:ZPL_B64,title:"test label"};
    const r=await api("/jobs",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(body)});
    b.textContent=r.ok?"Queued ✓":"Failed"; setTimeout(()=>b.textContent="Test print",1500);
  });
}

async function viewHistory(){
  const r=await api("/jobs"); const jobs=(await r.json()).jobs||[];
  const rows=jobs.length?jobs.map(j=>`
    <tr class="jrow" data-id="${j.id}">
      <td>${j.id}<div class="muted">${esc(j.title||"")}</div></td>
      <td class="muted">${esc(j.agent_name)}</td>
      <td>${esc(j.printer_name)}</td>
      <td class="muted">${fmt(j.created_at)}</td>
      <td><span class="badge ${esc(j.state)}">${esc(j.state)}</span></td>
    </tr>
    <tr class="jdet" data-for="${j.id}" style="display:none"><td colspan="5" class="muted">
      type ${esc(j.type)} · mode ${esc(j.mode)} · finished ${fmt(j.finished_at)||"—"}${j.error?` · <span class="err">${esc(j.error)}</span>`:""}
    </td></tr>`).join(""):`<tr><td colspan="5" class="muted">No jobs yet.</td></tr>`;
  $("#view").innerHTML=`<div class="card"><table>
    <thead><tr><th>Job</th><th>Computer</th><th>Printer</th><th>Created</th><th>State</th></tr></thead>
    <tbody>${rows}</tbody></table></div>`;
  $("#view").querySelectorAll(".jrow").forEach(tr=>tr.onclick=()=>{
    const d=$(`.jdet[data-for="${tr.dataset.id}"]`); d.style.display=d.style.display==="none"?"table-row":"none";
  });
}

async function viewKeys(){
  const r=await api("/apikeys"); const keys=(await r.json()).keys||[];
  const rows=keys.length?keys.map(k=>`<tr>
    <td>${esc(k.label)}</td><td class="muted">${fmt(k.created_at)}</td>
    <td><span class="badge ${k.active?"on":"off"}">${k.active?"active":"revoked"}</span></td>
    <td>${k.active?`<button class="danger" data-kid="${k.id}">Revoke</button>`:""}</td>
    </tr>`).join(""):`<tr><td colspan="4" class="muted">No keys yet.</td></tr>`;
  const box=JUST_ISSUED?`<div class="keybox"><b>${esc(JUST_ISSUED.label)}</b> — copy now, shown once:<br>${esc(JUST_ISSUED.key)}</div>`:"";
  JUST_ISSUED=null;
  $("#view").innerHTML=`
   <div class="card" style="max-width:520px">
    <label class="f">New key label</label>
    <div class="row"><input id="klabel" placeholder="e.g. n8n"><button id="newkey">Issue</button></div>
    ${box}
   </div>
   <div class="card"><table>
    <thead><tr><th>Label</th><th>Created</th><th>Status</th><th></th></tr></thead>
    <tbody>${rows}</tbody></table></div>`;
  $("#newkey").onclick=async()=>{
    const label=$("#klabel").value.trim()||"client";
    const rr=await api("/apikeys",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({label})});
    if(rr.ok){ JUST_ISSUED=await rr.json(); viewKeys(); }
  };
  $("#view").querySelectorAll("button[data-kid]").forEach(b=>b.onclick=async()=>{
    const rr=await api("/apikeys/"+b.dataset.kid,{method:"DELETE"}); if(rr.ok) viewKeys();
  });
}

function viewDownloads(){
  $("#view").innerHTML=`
   <div class="card">
    <h3>Install the agent</h3>
    <p class="hint">Run on the machine connected to the printers. It polls this server over HTTP(S) — no inbound ports required.</p>
    <ol>
      <li>Get <code>agent/print_agent.py</code> from the printpapi repo.</li>
      <li>Python 3.9+. Windows also needs <code>pywin32</code> + SumatraPDF (PDF); Linux uses CUPS <code>lp</code>.</li>
      <li>Create <code>agent.ini</code> next to the script (template below).</li>
      <li>Run <code>python print_agent.py</code> (autostart via Task Scheduler / systemd).</li>
    </ol>
    <h3>agent.ini</h3>
    <pre>[agent]
server_url = ${esc(location.origin)}
api_key = &lt;your agent key&gt;
name = office-pc
; printers: semicolon-separated. Append |pdf for document printers.
; A CUPS queue / Windows printer name, or socket://IP:PORT for a raw network printer.
printers = Zebra GK420d ; HP LaserJet|pdf ; netz-bixolon = socket://192.168.1.50:9100</pre>
    <p class="hint">socket:// printers are raw-only (ZPL/ESC-POS) — a network printer can’t render a PDF.</p>
   </div>`;
}

if(token()){ if(!location.hash) location.hash="#print"; showApp(); route(); } else { showLogin(); }
setInterval(()=>{ if(token()&&$("#app").classList.contains("show")){
  const h=location.hash.replace("#","")||"print";
  if(h==="devices")viewDevices(); else if(h==="history")viewHistory(); } },5000);
</script>
</body>
</html>
```

- [ ] **Step 2: Sanity-check the asset exists and has the required markers**

Run: `python -c "import pathlib,sys; h=pathlib.Path('app/dashboard.html').read_text(); [sys.exit('missing '+m) for m in ('<!doctype html>','printpapi','/printers','/jobs','#downloads') if m not in h]; print('ok')"`
Expected: `ok`.

- [ ] **Step 3: Commit**

```bash
git add app/dashboard.html
git commit -m "feat(ui): PrintNode-style dashboard SPA (print, devices, history, keys, downloads)"
```

---

### Task 8: Serve `dashboard.html` from `app/server.py`

**Files:**
- Modify: `app/server.py` (add `pathlib` import; replace the inline `_DASHBOARD_HTML` string with a file read; keep the `_TEST_PDF_B64` / `_TEST_ZPL_B64` constants and the `.replace(...)` substitution)
- Test: `tests/test_server.py`

**Interfaces:**
- Consumes: `app/dashboard.html` (Task 7).

- [ ] **Step 1: Write the failing test**

Append to `tests/test_server.py`:

```python
def test_dashboard_has_sidebar_nav():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        html = _req("GET", base + "/")[1].decode()
        for h in ("#print", "#devices", "#history", "#keys", "#downloads"):
            assert h in html
        assert "Sign Out" in html
    finally:
        httpd.shutdown()
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_server.py::test_dashboard_has_sidebar_nav -v`
Expected: FAIL (the old inline dashboard has no sidebar `#print`/`#devices`/… nav).

- [ ] **Step 3: Replace the inline HTML with a file read**

In `app/server.py`: add `from pathlib import Path` to the imports. Keep the `_TEST_PDF_B64` and `_TEST_ZPL_B64` constants (and the comment above them). **Delete** the entire inline `_DASHBOARD_HTML = r"""…"""` block (the `<!doctype html> … </html>` string and its trailing `.replace(...)`) and replace it with:

```python
# Static, secret-free dashboard SPA loaded from app/dashboard.html. It prompts for the API token,
# keeps it in localStorage, and calls the JSON endpoints with the bearer header — so all data stays
# behind auth and the HTML carries nothing sensitive. __PDF_B64__/__ZPL_B64__ are the built-in test
# payloads (gotcha #1: PDF only reaches PDF-capable printers; label printers get the ZPL label).
_DASHBOARD_HTML = (
    (Path(__file__).resolve().parent / "dashboard.html").read_text(encoding="utf-8")
    .replace("__PDF_B64__", _TEST_PDF_B64)
    .replace("__ZPL_B64__", _TEST_ZPL_B64)
)
```

- [ ] **Step 4: Run the server suite (old + new dashboard tests both green)**

Run: `python -m pytest tests/test_server.py -k dashboard -v`
Expected: PASS — both `test_dashboard_served_at_root_as_html` (doctype/printpapi/`/printers`/`/jobs`) and `test_dashboard_has_sidebar_nav`.

- [ ] **Step 5: Full suite**

Run: `python -m pytest`
Expected: PASS (all — target ≥ 46 existing + the new tests).

- [ ] **Step 6: Commit**

```bash
git add app/server.py tests/test_server.py
git commit -m "refactor(server): serve dashboard from app/dashboard.html"
```

---

## Manual verification (after all tasks)

Not automated (no browser harness in this repo). Do once at the end:

- [ ] Start the server (`PRINTAPI_TOKEN=dev python -m app.server`), open `http://localhost:3460/`, log in with `dev`. Confirm: sidebar nav switches views; Devices lists printers; API Keys issues + shows a key once + revokes; Downloads shows the `agent.ini` template with the `socket://` line; Sign Out returns to login.
- [ ] With a real (or fake) network printer, add `netz = socket://IP:9100` to an agent's `agent.ini`, register, and send a ZPL Test print from Devices — confirm the agent opens the socket and the label prints (gotcha #1: use ZPL, not PDF).

---

## Self-Review (completed by plan author)

**Spec coverage:**
- Part A socket output → Tasks 1-3. ✅ (grammar, sender, routing/wiring)
- Part B `pdf_uri` → Task 4. ✅
- `jobs.title` + `agent_name` + idempotent migration → Task 5. ✅
- `POST /jobs` title → Task 6. ✅
- SPA (login, Print Something, Devices, Print History+expand, API Keys, Downloads) → Task 7. ✅
- `dashboard.html` served from file → Task 8. ✅
- "More info" reuses `GET /jobs` row (no new endpoint) → Task 7 `viewHistory` expand. ✅
- Out-of-scope items (accounts, Plans, Webhooks, server-side socket, binary hosting) → not built. ✅

**Placeholder scan:** no TBD/TODO; every code step shows full code; every test step shows the assertion. ✅

**Type consistency:** `parse_printers` entry shape `{name, can_pdf, target}` is identical across Tasks 1/3 and the dashboard's printer objects use `{id, name, agent_name, can_pdf, online}` (matches `store.list_printers`). `enqueue_job(..., title=None)` signature matches its call in Task 6. `recent_jobs` fields (`id, printer_name, agent_name, title, state, type, mode, error, created_at, finished_at`) match `viewHistory`'s usage. ✅
