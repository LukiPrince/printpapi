# Design — PrintNode-clone WebUI + direct `:9100` network printer

Date: 2026-07-03
Status: approved (brainstorming), pending implementation plan

Grows printpapi from a bare two-table dashboard into a PrintNode-shaped self-hosted UI, and adds the
one agent capability PrintNode has that printpapi lacked: printing straight to a network printer by
IP, no CUPS queue required.

Two independent parts:

- **Part A** — direct `socket://host:port` output in the agent (the follow-on flagged in
  `2026-06-30-poll-engine-design.md`: "Network-socket (`:9100`) output kinds in the agent").
- **Part B** — WebUI rework: sidebar nav, a Print Something page, Devices, Print History with
  expandable detail, an API Keys page (endpoints already exist, no UI today), and a Downloads help
  page.

Scope was set with the user: **real features only** — none of PrintNode's SaaS chrome (Plans,
Payment Info, Email Notifications, Profile, Integrators, user accounts). Auth stays the existing
single API-token model, reskinned as a login screen with Sign Out.

---

## Part A — Direct network-printer output (agent-side)

### Why agent-side, not server-side

v0 (homelab, push model) had the *server* open the socket to `ip:9100` because server and printer
shared a LAN. v1's whole point (CLAUDE.md, poll-engine spec) is that the agent polls so it works
behind NAT with the server anywhere. Server-side socket sending would only work when the server is
LAN-local and would re-introduce the push model. So the **agent** (on the LAN) opens the socket. A
network printer is just another entry in the agent's config; the server treats it like any other
registered printer.

### Config grammar (`agent.ini` `printers`)

Extends today's `name [|pdf]` with an optional `= target`:

```
entry   := name [ '|pdf' ] [ '=' target ]
printers = Zebra|pdf ; Bixolon ; netz-bixolon = socket://192.168.1.50:9100
```

- **No `=`** → target is the name itself (today's behavior: a CUPS queue name on Linux, a Windows
  printer name).
- **`= socket://host:port`** → the agent opens a raw TCP socket to `host:port` and writes the job
  bytes verbatim.
- A `socket://` target is **forced raw-only** (`can_pdf = false`) regardless of a `|pdf` tag —
  gotcha #1: there is no renderer behind a bare socket, so a PDF must never be sent to one. Only
  already-rendered formats (ZPL / ESC-POS) go to a socket printer.

### Code changes — `agent/print_agent.py`

- `parse_printers(spec)` returns entries with a new `target` field:
  `[{"name", "can_pdf", "target"}]`.
  - Split each `;` entry on the first `=` into `left` / `target`; `target` defaults to the name when
    absent.
  - `left` keeps the current `|pdf` partition for `can_pdf`.
  - If `target` starts with `socket://`, set `can_pdf = False`.
- New `raw_to_socket(target, data, connect=socket.create_connection, timeout=30)`:
  - Parse `socket://host:port` → `(host, int(port))`.
  - `with connect((host, port), timeout=timeout) as s: s.sendall(data)`.
  - `connect` is injectable so tests exercise it without a real printer.
- `print_job(mode, entry, data, raw_fn, pdf_fn, socket_fn=raw_to_socket)` routes by target scheme:
  - `entry["target"]` starts with `socket://` → `raw` calls `socket_fn(target, data)`; `pdf` raises
    `ValueError("network socket printer is raw-only (cannot render PDF)")`.
  - otherwise → `raw` calls `raw_fn(target, data)`, `pdf` calls `pdf_fn(target, data)` (target ==
    name for queue/Windows printers).
- `run_once` / `main`: carry the whole entry per printer id (not just the name string) so `print_job`
  can read `target`. `main` builds `entry_by_id = {id: entry_by_name[name]}` from `parse_printers`
  plus the register response's `printer_ids` (`{name: id}`).

### Server — no change

A socket printer registers exactly like any other: the agent still sends `{name, can_pdf}` on
`/agent/register`. The `name`→`target` mapping lives only in the agent. Jobs are dispatched by
`printer_id` as today.

### Tests — `agent/tests/test_agent.py`

- `parse_printers` parses a `socket://` entry: correct `name`, `target`, and `can_pdf == False`
  (even with a `|pdf` tag present).
- `raw_to_socket` writes the exact bytes to an injected fake `connect` (capture the `sendall`
  payload; assert host/port parsed from `socket://192.168.1.50:9100`).
- `print_job` with a socket entry in `raw` mode calls `socket_fn`; in `pdf` mode raises `ValueError`.

---

## Part B — WebUI rework

Single static SPA, hash-routed, served at `/`. **Moved out of `server.py` into
`app/dashboard.html`**, loaded once at import. Rationale: the inline HTML is already ~155 lines; a
full multi-page SPA would push `server.py` past the point of being readable in one screen. No JS
framework, no build step — plain HTML/CSS/JS, matching the stdlib-only server convention.

Layout: left sidebar nav + top bar (instance label + Sign Out) + a right-hand "Help & Tips" panel
whose text switches per page. This is PrintNode's shape without its billing chrome.

### Pages

| Page | Backend | Status |
|------|---------|--------|
| **Login** | token → `localStorage`; Sign Out clears it and returns here | reskin of the current header token box |
| **Print Something** | Source dropdown + printer dropdown + PRINT → `POST /jobs` | new UI |
| **Devices** | `GET /printers` — table grouped by agent (Computer), online/offline, PDF, Test-print | restyle of current Printers table |
| **Print History** | `GET /jobs` — Job id + title, Computer, Printer, Created, State, "More info" expand | restyle + title + agent column + expand |
| **API Keys** | `GET / POST / DELETE /apikeys` | new UI (endpoints already exist, unused today) |
| **Downloads** | static: agent install steps + `agent.ini` template incl. `socket://` syntax | new, static text |

### Print Something — Source → content-type mapping

The browser reads the file, base64-encodes it, and POSTs to `/jobs`. Every source maps to a content
type the server already handles (`app/dispatch.py`), so no new server dispatch logic:

- **Upload PDF** (document printers) → `type: pdf_base64`
- **Upload raw** (`.zpl` / `.prn` / `.bin`, label printers) → `type: raw_base64`
- **Fetch from URL** → GET by the server (browser UA already set). Raw content (rendered ZPL — the v0
  use case) → existing `type: raw_uri`. A PDF at a URL → new `type: pdf_uri` (see server delta 4).
  The existing POST-fetch types (`raw_uri_post` / `pdf_uri_post`, which need a JSON body for the
  external render-service integration) stay available via the API but are **not** surfaced in the form.
- **Test document** → the built-in test PDF/ZPL (same payloads as today's Test-print button)

The printer dropdown is populated from `GET /printers`; a printer's `can_pdf` flag drives which
sources are offered/sensible (never offer a PDF upload to a raw-only printer — gotcha #1).

### "More info" expand

Reuses the row already returned by `GET /jobs` (it carries `type`, `mode`, `error`, `created_at`,
`finished_at`). The expand shows those plus title and Computer. **No new endpoint.**

### Server deltas

1. **`jobs.title`** — optional text column.
   - `store.enqueue_job(...)` accepts `title` and stores it.
   - `POST /jobs` reads `body.get("title")`.
   - `store.recent_jobs` returns `title`.
   - Migration in `store.init_db`: `ALTER TABLE jobs ADD COLUMN title TEXT`, wrapped to swallow the
     "duplicate column name" `OperationalError` so it is idempotent across restarts and on fresh DBs.
2. **`agent_name` in history** — `store.recent_jobs` joins `agents` (via `jobs.agent_id`) and returns
   `agent_name` for the Computer column.
3. Nothing else — printers list, API-key CRUD, job submit, and the poll loop are unchanged.
4. **`pdf_uri` type** (`app/dispatch.py`) — GET a URL and print as PDF. One branch in
   `decode_payload` (identical fetch to `raw_uri`) and add `pdf_uri` to `agent_mode`'s pdf set.
   Enables the print form's "PDF at a URL" case without the POST-body machinery of `pdf_uri_post`.

### Look

Approximate PrintNode's clean look (light content area, dark sidebar, a prominent primary PRINT
button). Not pixel-exact — "real features only" scope. Keep `color-scheme: light dark`. This is
cosmetic; exact palette decided during implementation.

### Tests

- `tests/test_store.py`: `enqueue_job` with a title round-trips through `recent_jobs`; `recent_jobs`
  returns `agent_name`; `init_db` run twice adds the `title` column exactly once (idempotent
  migration).
- `tests/test_server.py`: `POST /jobs` with a `title` persists and appears in `GET /jobs`; `GET /`
  returns 200 and the dashboard HTML (contains the nav markup).
- `tests/test_dispatch.py`: `pdf_uri` fetches by GET (injected `fetch_url`) and `agent_mode("pdf_uri")
  == "pdf"`.
- No browser/UI automation — matches the repo's convention (real loopback HTTP + real SQLite, no
  mocks, no frameworks).

---

## Explicitly out of scope (YAGNI — recorded as decisions, not omissions)

- User accounts / passwords / sessions — single API-token model kept.
- Plans, Payment Info, Email Notifications, Profile, Integrators — SaaS chrome.
- Webhooks and email-to-print — real PrintNode features, deferred. Webhooks is the natural next
  follow-on (hooks off the existing job-result path); not in this scope.
- Server-side socket sending (agent-side only).
- Hosting agent binaries — Downloads is a static instructions page, not a file server.

## Files touched

- `agent/print_agent.py` — socket target parsing + `raw_to_socket` + `print_job` routing (Part A).
- `agent/tests/test_agent.py` — Part A tests.
- `app/dashboard.html` — new, the SPA (Part B).
- `app/server.py` — load `dashboard.html`; read `title` on `POST /jobs`.
- `app/store.py` — `title` column + migration; `title` in `enqueue_job`/`recent_jobs`; `agent_name`
  join in `recent_jobs`.
- `app/dispatch.py` — new `pdf_uri` type (Part B, print-form URL source).
- `tests/test_store.py`, `tests/test_server.py`, `tests/test_dispatch.py` — Part B tests.
