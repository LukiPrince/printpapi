# HANDOFF — printpapi

Continuation notes for building the OSS self-hosted PrintNode alternative. If you are a fresh
session: read this top to bottom, then `CLAUDE.md`, then `docs/design-v0-homelab.md`. The code in
`app/` + `agent/` already works in production — it is your starting point, not a blank page.

## 1. Origin

Built first as a **homelab replacement for PrintNode** (cloud print SaaS, ~€7/mo) for a small
business. That deployment is live and validated: real shipping labels + fault-code labels print
through it daily. Full source + ops detail live in the original (private) deployment repo; the
design rationale is summarized in `docs/design-v0-homelab.md`. This `printpapi` repo is the
extracted, generalized OSS project.

## 2. What works today (v1 core poll engine)

**Server** (`app/`, Python stdlib only):

- `GET /` — **web dashboard**: a Next.js **static export** (source in `web/`, built bundle committed
  to `app/web`), served straight off disk by the stdlib server. Live overview (queue counters,
  outcome breakdown, activity feed), devices, searchable job history with cancel, API keys,
  agent setup, light/dark, ⌘K palette. Secret-free shell — it prompts for the token and calls the
  JSON endpoints with it, so all data stays behind auth. Test print picks a PDF for PDF-capable
  printers, a ZPL label otherwise (gotcha #1). Static serving is confined to `app/web`
  (traversal 404s); `/_next/static/` is immutable-cached, HTML is `no-cache`.
- Client endpoints (bearer auth — the bootstrap `PRINTAPI_TOKEN` or any issued per-client key):
  - `POST /jobs` — submit a job `{printer_id, type, content|url}` → `{job_id}`. Optional: `title`,
    `copies` (1–100), `callback_url` (webhook on terminal state), `options` (pdf only: duplex,
    paper, bin, color, pages — agent maps to Sumatra `-print-settings` / `lp -o`),
    `idempotency_key` (per-org; a resubmit returns the original job, never a second print),
    `expire_after` (seconds; past the deadline the job fails as `expired` instead of printing).
  - `GET /jobs` — recent job history; `GET /jobs/{id}` — job state:
    `queued | claimed | done | failed | cancelled`; `DELETE /jobs/{id}` — cancel while queued.
  - `GET /printers` — printers with online/offline + capabilities (papers/bins/duplex/color,
    reported best-effort by the agent at registration).
  - `GET /computers` — agents with online/offline, last seen, printer count. Liveness *transitions*
    are POSTed to the org's `event_url` (`PUT /orgs/{id}`, root) as `computer_online` /
    `computer_offline` — once per edge, at-most-once (see `docs/api.md#computers-agents`).
  - `GET /metrics` — Prometheus text. `GET /health`.
- Admin endpoints (bootstrap `PRINTAPI_TOKEN` only) — **per-client API keys**:
  - `POST /apikeys {label}` → issues a new random key (shown once); `GET /apikeys` lists labels;
    `DELETE /apikeys/{id}` revokes. Keys are stored sha256-hashed; revoked keys stop authorizing.
- Agent endpoints (per-agent key, hashed in DB):
  - `POST /agent/register` — agent declares its name and printer list; server upserts and returns
    `{computer_id, printer_ids}`. Name is bound to the key on first contact (no hijack).
  - `GET /agent/jobs` — long-poll (default 25 s); returns `{job_id, printer_id, mode}` or 204.
  - `GET /agent/jobs/{id}/payload` — raw bytes of the job payload.
  - `POST /agent/jobs/{id}/result` — agent reports `{ok, error}`.
- SQLite job store (`PRINT_DB`, default `printpapi.db`), WAL mode, global write lock.
- Visibility-timeout reaper: stale claimed jobs requeued (up to `max_retries`), then failed;
  same loop expires deadline-passed jobs.
- Content types: `raw_base64`, `pdf_base64`, `raw_uri`, `raw_uri_post`, `pdf_uri_post`.
- `app/dispatch.py` pure logic (injectable fetchers); `app/store.py` SQLite store; `app/server.py` HTTP.
- Env vars: `PRINTAPI_TOKEN` (required), `PRINT_DB` (default `printpapi.db`), `PRINT_PORT` (default `3460`).

**Agent** (`agent/print_agent.py`, **cross-platform** — Windows + Linux/macOS via CUPS):

- Registers its printers on startup, then loops: `run_once` → long-poll → download payload → print →
  report result.
- `select_backend()` picks the print path by OS at startup:
  - **Windows:** `raw` → `win32print` RAW (ZPL/ESC-POS straight to the spooler); `pdf` → SumatraPDF
    silent-print via the installed driver.
  - **Linux + macOS/CUPS:** `raw` → `lp -d <queue> -o raw` (already-rendered bytes, CUPS must not
    re-render); `pdf` → `lp -d <queue>` (CUPS filter chain renders it). macOS needs no code of its
    own — but a driverless/AirPrint queue mangles raw ZPL, so `docs/agent.md#macos` tells operators
    to use `socket://IP:9100` or an `lpadmin -m raw` queue for label printers.
- Configured via `agent.ini` (next to the script): `server_url`, `api_key`, `name`, `printers`
  (semicolon-separated — Windows printer names, or CUPS queue names on Linux).
- Shipped in the homelab as a signed-Python install (see gotcha #2), autostart via Task Scheduler.

**Tests:** 168, all green (`python -m pytest`). Real loopback HTTP servers (ThreadingHTTPServer),
real SQLite (:memory:), injected render fns / subprocess runners — no mocks, no real printers.

**Model:** **poll** — agent opens a long-poll `GET /agent/jobs` to the server, receives jobs, prints,
reports back. No inbound ports needed on the agent's machine (PrintNode's core trick). The old push
model (server → agent on same LAN) is superseded.

## 3. Hard-won gotchas — do not rediscover these

1. **Label printers cannot render PDF.** Sending raw PDF bytes to a label printer's `:9100` socket
   makes it form-feed garbage — we printed 40 blank labels learning this. A print path needs a
   renderer: the agent renders PDFs via SumatraPDF (Windows) or CUPS/`lp` (Linux).
   Already-rendered formats (ZPL/ESC-POS) can go raw to `:9100`.
2. **Locked-down Windows blocks unsigned executables.** Smart App Control / WDAC / AppLocker reject an
   unsigned PyInstaller `.exe` (and exes run from `%TEMP%`, so a normal installer fails with "Error
   4551: application control policy"). Workarounds: run the agent via the **signed Python interpreter**
   (`pythonw.exe` is PSF-signed → allowed; the `.py` is data, not an executable), or **code-sign** the
   exe. For OSS: code-signing (Azure Trusted Signing or an OV/EV cert) is the proper answer for a
   shippable agent.
3. **A WAF/Cloudflare in front of a fetched URL 403s `Python-urllib`.** The original deployment's
   PDF-rendering service and workflow API both sat behind Cloudflare and returned 403/1010 to the
   default urllib User-Agent. Fix: send a browser UA (`Mozilla/5.0`) on every outbound HTTP request.
   Already done in `app/dispatch.py`.

## 4. v1 OSS scope (the plan)

Keep it tight — a v1 that ships beats a perfect v2 that doesn't. YAGNI hard.

**THE key change — agent polls the server.** v0 pushes (server → agent, same LAN). PrintNode's actual
value is that agents anywhere (behind NAT/firewall, no inbound port) reach the cloud server by polling
*out*. v1 must flip to: agent opens a long-poll / WebSocket to the server, receives jobs, prints,
reports status back. This is the #1 piece of rework and everything else hangs off it.

**v1 feature set:**
- **Server:** REST API (create job, list printers/computers, job status + history), SQLite store,
  **web dashboard** (which agents/printers are online, recent jobs + status, a test-print button),
  token / API-key auth, Docker image.
- **Agent:** polls the server; **auto-registers** its printers (+ basic capabilities) on connect;
  prints raw/pdf/uri; reports job status. **Cross-platform:** Windows (have it) + Linux/CUPS. Code-signed.
- Content types: pdf, raw, uri (have these).

**Explicitly OUT of v1** (resist the urge): multi-user/multi-tenant orgs, queue/worker scaling,
webhooks, a full print-options matrix (copies/tray/duplex/rotate), deep capability modeling, billing.
*(v1 has shipped; webhooks, print options, and capabilities have since landed as v2 work, and
multi-tenancy is now the active next step — see §5 and `docs/roadmap.md`.)*

## 5. Current state & what's next

v1 is **published** (public repo, v1.0.0 release, GHCR image) and the post-v1 feature wave has
shipped: dashboard, Linux/CUPS agent, per-client API keys, Docker, job copies, cancel, `/metrics`,
webhooks, per-job print options, printer capability discovery, **multi-tenancy**, **computer status
+ liveness events** (roadmap #1), **idempotency keys + job expiry** (roadmap #2), **macOS support**
(roadmap #3), **docs as a feature** (roadmap #4: service install, `docs/recipes.md` for
n8n/Zapier/Make, per-printer-family setup, QZ Tray/PrintNode comparison in the README).
168 tests green.
A demand-research sweep (July 2026) produced the ranked v2 roadmap in `docs/roadmap.md` — read it
before inventing features. Roadmap #1–#4 and #6 are done, so **#5 (e-commerce auto-print:
Shopify/WooCommerce order → packing slip/label) is the next one with real pull** — and it is the
prerequisite for the hosted SaaS in the strategic note below. It needs two pieces: a store-side
app/plugin that POSTs to `/jobs` on an order webhook (the HTTP contract for that already exists —
`docs/recipes.md`), and order → PDF rendering, which is the part we do not have at all.
Non-code leftover: code-sign the Windows agent (needs a cert, gotcha #2).

**Multi-tenancy (roadmap #6) is done** — see `docs/api.md#multi-tenancy` for the contract. Shape:

- A key *is* the org. `authenticate_client` resolves a key to `{id, org_id}`; every store query
  takes `org_id=None` (= root, no filter) or an id, written as `(:org IS NULL OR x.org_id = :org)`.
- Root = the bootstrap `PRINTAPI_TOKEN`: spans every org, sole manager of `/orgs` and `/apikeys`.
- Agents enroll into the org of the key they present (an issued client key → its org, anything
  else → `DEFAULT_ORG`, which is what every pre-existing agent does). Printers/jobs inherit.
  Agent names are unique per org. Agent endpoints need no org filter — they key off `agent_id`.
- Invariant, tested: a foreign job id is `404` (never `403`/`409`), a foreign printer is
  `400 unknown printer`, and lists/metrics only ever carry the caller's own org.
- Legacy DBs (everything `org_id=1`) keep working untouched — no schema change was needed.
- Still open on top: billing, quotas, per-org dashboard users/login, org-scoped key
  self-management, org deletion; and an agent key doubling as its org's client key (see the
  `# ponytail:` note in `server.py`'s `/agent/register`).

**Roadmap #1 + #2 are done** (`docs/api.md#computers-agents`, `#submitting-a-job`):

- `GET /computers` mirrors `GET /printers` (org-scoped, same 60 s liveness window). Liveness
  *edges* are claimed-and-marked in one lock (`store.claim_agent_transitions`) so each fires once,
  and delivered by the existing webhook-dispatcher thread. At-most-once on purpose — an org that
  sets its `event_url` later starts from the current state, no replay.
- `idempotency_key` dedupes per `(org_id, key)` — a UNIQUE index plus a lookup inside
  `enqueue_job`'s transaction; `expire_after` writes `jobs.expires_at`, the claim query skips
  deadline-passed jobs (that is what guarantees no late print) and the reaper fails them as
  `expired`. No new job state — the error string carries the reason.
- The dashboard's Devices page is driven by `/computers` (with `/printers` for the cards), so an
  agent that reported no printers is visible instead of silently missing, and an offline one shows
  "last seen 4m ago". If `/computers` is unavailable it falls back to grouping `/printers`.
- Left open: event payloads are unsigned.

**Strategic context for what comes next:** the repo owner wants a cheap hosted SaaS
("Shopify/WooCommerce order comes in → label/packing slip prints") undercutting the paid
PrintNode-wrapper plugins. Org isolation was the technical blocker; the store app and document
rendering (roadmap #5) are what's left before that's possible.

## 6. Reference

- v0 production deployment + ops: the original (private) deployment repo.
- Design rationale: `docs/design-v0-homelab.md` (in this repo, sanitized).
- The homelab used n8n as the orchestrator calling the server; for OSS the server itself is the API
  surface (clients call it directly), so the n8n part does not carry over.
