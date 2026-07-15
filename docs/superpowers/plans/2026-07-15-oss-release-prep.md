# OSS Release Prep Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the audit's bug findings, scrub private data, add OSS hygiene files, and redesign the dashboard — so the printpapi repo can go public.

**Architecture:** No structural changes. Server stays stdlib `http.server` + SQLite; agent stays a single polling script. Dashboard stays one static HTML file (logo embedded as data-URI, generated via `agy`, background stripped via `rembg`).

**Tech Stack:** Python 3.12+ stdlib, pytest, GitHub Actions, agy CLI (image gen), rembg (background removal), Pillow (downscale).

## Global Constraints

- Server: Python **stdlib only** — no new runtime dependencies.
- TDD: failing test first, then minimal code. `python -m pytest` from repo root must stay green.
- Security invariants stay: bearer auth everywhere, `hmac.compare_digest`, http(s)-only fetches, no `shell=True`.
- Deliberate shortcuts get a `# ponytail:` comment naming the ceiling.
- Dashboard: single static file, no CDN, no build step.

---

### Task 1: Server — validate Content-Length (DoS guard)

**Files:**
- Modify: `app/server.py:78-80` (`_read_json`)
- Test: `tests/test_server.py`

**Interfaces:**
- Produces: `_read_json` raises `ValueError` on negative / non-numeric / oversized Content-Length; existing callers already map `ValueError` → 400.

- [ ] **Step 1: Write the failing test** (raw socket, since http.client won't send a bogus Content-Length)

```python
def test_bad_content_length_rejected(server):
    base, token, conn = server
    import socket as s
    host, port = base.replace("http://", "").split(":")
    for cl in ("-1", "999999999999", "nan"):
        with s.create_connection((host, int(port)), timeout=5) as sock:
            sock.sendall((
                f"POST /jobs HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\n"
                f"Content-Length: {cl}\r\n\r\n").encode())
            resp = sock.recv(1024).decode()
        assert " 400 " in resp.splitlines()[0]
    # server still alive
    assert _get(base + "/health") == {"ok": True}
```

- [ ] **Step 2: Run test to verify it fails** — `python -m pytest tests/test_server.py -k content_length -v` → FAIL (hangs/500 instead of 400; use the existing server fixture pattern from that file).

- [ ] **Step 3: Implement**

```python
_MAX_BODY = 32 * 1024 * 1024  # ponytail: flat 32 MB cap; make env-tunable if someone needs bigger

def _read_json(self):
    length = int(self.headers.get("Content-Length", 0))   # ValueError -> caller's 400
    if length < 0 or length > _MAX_BODY:
        raise ValueError("bad content-length")
    return json.loads(self.rfile.read(length) or b"{}")
```

(`_MAX_BODY` at module level; note `int("nan")` raises ValueError which callers already turn into 400 — but the raise must happen *inside* the callers' try blocks, which it does since they wrap `_read_json()`.)

- [ ] **Step 4: Run test to verify it passes** → PASS
- [ ] **Step 5: Commit** — `fix(server): reject negative/huge/bogus Content-Length (DoS guard)`

### Task 2: Server — reaper logs failures instead of swallowing them

**Files:**
- Modify: `app/server.py:225-235` (`start_reaper`)
- Test: `tests/test_server.py`

- [ ] **Step 1: Failing test**

```python
def test_reaper_logs_failures(capsys, monkeypatch):
    calls = []
    def boom(*a, **k):
        calls.append(1)
        raise RuntimeError("db gone")
    monkeypatch.setattr(store, "requeue_stale", boom)
    t = server_mod.start_reaper(None, interval_s=0.01)
    for _ in range(100):
        if calls:
            break
        time.sleep(0.01)
    time.sleep(0.05)
    assert "reaper" in capsys.readouterr().err
```

- [ ] **Step 2: Verify FAIL** — `python -m pytest tests/test_server.py -k reaper_logs -v`
- [ ] **Step 3: Implement**

```python
def start_reaper(conn, *, timeout_s=300, max_retries=2, interval_s=30):
    def loop():
        while True:
            try:
                store.requeue_stale(conn, timeout_s, max_retries)
            except Exception as e:
                print(f"reaper error: {e}", file=sys.stderr)
            time.sleep(interval_s)
    ...
```

(add `import sys` to server.py)

- [ ] **Step 4: Verify PASS**
- [ ] **Step 5: Commit** — `fix(server): reaper failures go to stderr instead of vanishing`

### Task 3: Agent — connection errors (URLError) mapped like HTTP errors

**Files:**
- Modify: `agent/print_agent.py:100-107` (`_req`)
- Test: `agent/tests/test_agent.py`

- [ ] **Step 1: Failing test**

```python
def test_req_maps_urlerror_to_oserror(monkeypatch):
    import urllib.error, urllib.request
    def refuse(req, **kw):
        raise urllib.error.URLError("connection refused")
    monkeypatch.setattr(urllib.request, "urlopen", refuse)
    with pytest.raises(OSError, match="connection failed"):
        print_agent._req("http://127.0.0.1:1/x", "k")
```

- [ ] **Step 2: Verify FAIL** (URLError propagates raw)
- [ ] **Step 3: Implement** — in `_req`, after the HTTPError handler (HTTPError subclasses URLError, order matters):

```python
    except urllib.error.HTTPError as e:
        raise OSError(f"server returned {e.code}") from e
    except urllib.error.URLError as e:
        raise OSError(f"connection failed: {e.reason}") from e
```

- [ ] **Step 4: Verify PASS**
- [ ] **Step 5: Commit** — `fix(agent): map connection failures to OSError like HTTP errors`

### Task 4: Agent — report_result retries (prevents duplicate prints)

**Files:**
- Modify: `agent/print_agent.py:154-170` (`run_once` + new `_report_with_retry`)
- Test: `agent/tests/test_agent.py`

**Interfaces:**
- Produces: `_report_with_retry(base, key, job_id, ok, error, http_post, attempts=5, sleep=time.sleep) -> bool`

- [ ] **Step 1: Failing tests**

```python
def test_report_retries_then_succeeds():
    calls = []
    def flaky(url, key, body):
        calls.append(body)
        if len(calls) < 3:
            raise OSError("net down")
    ok = print_agent._report_with_retry("http://x", "k", 1, True, None,
                                        http_post=flaky, sleep=lambda s: None)
    assert ok and len(calls) == 3

def test_run_once_survives_report_failure():
    def dead(url, key, body):
        raise OSError("net down")
    job = {"job_id": 1, "printer_id": 5, "mode": "raw"}
    printed = []
    ret = print_agent.run_once(
        "http://x", "k", {5: {"name": "p", "can_pdf": False, "target": "p"}},
        http_get=lambda u, k: job, http_get_bytes=lambda u, k: b"DATA",
        http_post=dead, raw_fn=lambda t, d: printed.append(d),
        report_sleep=lambda s: None)
    assert ret is True and printed == [b"DATA"]   # printed, and no exception escaped
```

- [ ] **Step 2: Verify FAIL** — `python -m pytest agent/tests -k report -v`
- [ ] **Step 3: Implement**

```python
def _report_with_retry(base, key, job_id, ok, error=None, *, http_post=_post,
                       attempts=5, sleep=time.sleep):
    # A lost result makes the server's reaper requeue the job -> duplicate print. Retry hard.
    for i in range(attempts):
        try:
            report_result(base, key, job_id, ok, error, http_post=http_post)
            return True
        except OSError as e:
            print(f"report_result failed (try {i + 1}/{attempts}): {e}", file=sys.stderr)
            if i + 1 < attempts:
                sleep(min(2 ** i, 30))
    return False  # ponytail: after 5 tries we drop the result; reaper requeues -> possible dup
```

`run_once` gains `report_sleep=time.sleep` kwarg and calls `_report_with_retry(..., http_post=http_post, sleep=report_sleep)` in both spots.

- [ ] **Step 4: Verify PASS**
- [ ] **Step 5: Commit** — `fix(agent): retry result reporting so flaky network can't duplicate prints`

### Task 5: Agent — clear startup error for missing/invalid agent.ini

**Files:**
- Modify: `agent/print_agent.py:173-184` (`main` → extract `load_config`)
- Test: `agent/tests/test_agent.py`

**Interfaces:**
- Produces: `load_config(base_dir) -> configparser.SectionProxy` (the `[agent]` section); raises `SystemExit` with a helpful message when the file or section is missing.

- [ ] **Step 1: Failing tests**

```python
def test_load_config_missing_file(tmp_path):
    with pytest.raises(SystemExit, match="agent.ini"):
        print_agent.load_config(str(tmp_path))

def test_load_config_ok(tmp_path):
    (tmp_path / "agent.ini").write_text(
        "[agent]\nserver_url=http://x\napi_key=k\nprinters=p\n")
    cfg = print_agent.load_config(str(tmp_path))
    assert cfg["server_url"] == "http://x"
```

- [ ] **Step 2: Verify FAIL**
- [ ] **Step 3: Implement**

```python
def load_config(base_dir):
    ini = os.path.join(base_dir, "agent.ini")
    cfg = configparser.ConfigParser()
    if not cfg.read(ini) or "agent" not in cfg:
        raise SystemExit(
            f"missing or invalid {ini}: need an [agent] section with server_url, api_key, printers")
    return cfg["agent"]
```

`main()` uses `agent = load_config(base_dir)` instead of the inline ConfigParser lines.

- [ ] **Step 4: Verify PASS** + full suite `python -m pytest`
- [ ] **Step 5: Commit** — `fix(agent): fail fast with a clear message when agent.ini is missing`

### Task 6: Scrub private data from docs and comments

**Files:**
- Rewrite: `docs/design-v0-homelab.md` (sanitized ~40-line English summary, same filename)
- Modify: `HANDOFF.md` (drop the business name, private repo paths, personal domain)
- Modify: `app/dispatch.py:16,60-61` (generalize gif2pdf/n8n comments)

- [ ] **Step 1: Rewrite design-v0-homelab.md** — keep the educational content (push architecture, why it was replaced by poll, the three gotchas' origin) with placeholder names (`pdf-service.example.com`, `192.0.2.x`, "the business's order tool"). Delete: real domain, LAN IPs, registry, hostnames, database record IDs, printer serials, PrintNode IDs, business workflow names.
- [ ] **Step 2: HANDOFF.md** — business name → "a small business"; private repo paths → "the original (private) deployment repo"; the render-service domain → "a PDF-rendering service behind Cloudflare".
- [ ] **Step 3: dispatch.py comments** — name the *pattern* (WAF-fronted services 403 the default urllib UA), not the private services.
- [ ] **Step 4: Verify** — grep the tree for every identifier on the audit's private-data list (business name, personal domain, LAN IPs, hostnames, database IDs) → no matches.
- [ ] **Step 5: Commit** — `docs: scrub private homelab/business details for public release`

### Task 7: OSS hygiene — .gitignore, CONTRIBUTING, SECURITY, CI, headers, roadmap

**Files:**
- Modify: `.gitignore` (add `*.db`, `*.db-wal`, `*.db-shm`)
- Create: `CONTRIBUTING.md`, `SECURITY.md`, `.github/workflows/ci.yml`
- Modify: `app/*.py`, `agent/print_agent.py` (2-line MIT header), `README.md` (roadmap section)

- [ ] **Step 1: .gitignore** — append `*.db`, `*.db-wal`, `*.db-shm`.
- [ ] **Step 2: CONTRIBUTING.md** — short: TDD convention, `python -m pytest`, stdlib-only server rule, `# ponytail:` shortcut markers.
- [ ] **Step 3: SECURITY.md** — private disclosure via GitHub security advisories, supported version = latest main, no bounty.
- [ ] **Step 4: CI** — `.github/workflows/ci.yml`:

```yaml
name: ci
on: [push, pull_request]
jobs:
  test:
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-python@v5
        with: {python-version: "3.12"}
      - run: pip install pytest
      - run: python -m pytest -q
```

- [ ] **Step 5: License headers** — first lines of `app/dispatch.py`, `app/store.py`, `app/server.py`, `agent/print_agent.py`:

```python
# printpapi — self-hosted PrintNode alternative. MIT License (see LICENSE).
```

- [ ] **Step 6: README roadmap** — add a short "Roadmap" section (job options like copies/duplex, webhooks on job state, job cancel endpoint, printer capability reporting, /metrics, code-signed Windows agent installer).
- [ ] **Step 7: Verify** — full `python -m pytest` green; YAML parses (`python -c "import yaml"` not needed — CI will validate; eyeball indentation).
- [ ] **Step 8: Commit** — `chore: OSS hygiene — CI, CONTRIBUTING, SECURITY, db gitignore, license headers, roadmap`

### Task 8: Dashboard redesign (agy logo + rembg + visual overhaul)

**Files:**
- Modify: `app/dashboard.html`
- Assets via scratchpad only (embedded as data-URI; nothing new served)

- [ ] **Step 1: Generate logo** — `agy -p "<logo prompt>"` (output lands in `~/.gemini/antigravity-cli/scratch/`), strip background with `rembg i in.png out.png`, downscale to ≤160px with Pillow, base64-encode.
- [ ] **Step 2: Redesign** — load `frontend-design:frontend-design` skill first. Keep: single file, vanilla JS, hash router, same view functions/IDs the tests rely on (`nav` sidebar, view names). Improve: typography, spacing, color system (light+dark), cards, tables, badges, login screen with logo, empty states.
- [ ] **Step 3: Verify** — `python -m pytest tests/test_server.py -q` (dashboard-serving tests green); Playwright: open `/`, screenshot login + main views, check console for errors.
- [ ] **Step 4: Commit** — `feat(ui): dashboard redesign with generated logo`

### Task 9: Release checklist + clean public history prep

**Files:**
- Create: `RELEASE_CHECKLIST.md`
- Create: local orphan branch `public-main` (single squashed commit of the sanitized tree)

- [ ] **Step 1: RELEASE_CHECKLIST.md** — the manual steps only the owner can do: choose author identity (iCloud email is in current history → the squashed branch avoids it), force-push or re-create GitHub repo from `public-main`, flip repo public, enable GitHub security advisories, code-sign Windows agent (gotcha #2).
- [ ] **Step 2: Orphan branch** — `git checkout --orphan public-main && git add -A && git commit -m "printpapi v1.0 — self-hosted PrintNode alternative"` then back to main. No push (owner's call).
- [ ] **Step 3: Commit checklist on main** — `docs: add public-release checklist`

### Task 10: Final verification

- [ ] **Step 1:** `python -m pytest` → all green (47 existing + new).
- [ ] **Step 2:** Boot server locally with a dev token, Playwright screenshot dashboard, confirm no JS console errors.
- [ ] **Step 3:** `git grep` scrub-check from Task 6 Step 4 once more over the final tree.
- [ ] **Step 4:** Code review of the full diff (cavecrew-reviewer) before claiming done.
