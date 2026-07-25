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
    paper, bin, color, pages — agent maps to Sumatra `-print-settings` / `lp -o`).
  - `GET /jobs` — recent job history; `GET /jobs/{id}` — job state:
    `queued | claimed | done | failed | cancelled`; `DELETE /jobs/{id}` — cancel while queued.
  - `GET /printers` — printers with online/offline + capabilities (papers/bins/duplex/color,
    reported best-effort by the agent at registration).
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
- Visibility-timeout reaper: stale claimed jobs requeued (up to `max_retries`), then failed.
- Content types: `raw_base64`, `pdf_base64`, `raw_uri`, `raw_uri_post`, `pdf_uri_post`.
- `app/dispatch.py` pure logic (injectable fetchers); `app/store.py` SQLite store; `app/server.py` HTTP.
- Env vars: `PRINTAPI_TOKEN` (required), `PRINT_DB` (default `printpapi.db`), `PRINT_PORT` (default `3460`).

**Agent** (`agent/print_agent.py`, **cross-platform** — Windows + Linux/CUPS):

- Registers its printers on startup, then loops: `run_once` → long-poll → download payload → print →
  report result.
- `select_backend()` picks the print path by OS at startup:
  - **Windows:** `raw` → `win32print` RAW (ZPL/ESC-POS straight to the spooler); `pdf` → SumatraPDF
    silent-print via the installed driver.
  - **Linux/CUPS:** `raw` → `lp -d <queue> -o raw` (already-rendered bytes, CUPS must not re-render);
    `pdf` → `lp -d <queue>` (CUPS filter chain renders it).
- Configured via `agent.ini` (next to the script): `server_url`, `api_key`, `name`, `printers`
  (semicolon-separated — Windows printer names, or CUPS queue names on Linux).
- Shipped in the homelab as a signed-Python install (see gotcha #2), autostart via Task Scheduler.

**Tests:** 136, all green (`python -m pytest`). Real loopback HTTP servers (ThreadingHTTPServer),
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

## 5. Current state & the active next step: multi-tenancy

v1 is **published** (public repo, v1.0.0 release, GHCR image) and the post-v1 feature wave has
shipped: dashboard, Linux/CUPS agent, per-client API keys, Docker, job copies, cancel, `/metrics`,
webhooks, per-job print options, printer capability discovery. 136 tests green. A demand-research
sweep (July 2026) produced the ranked v2 roadmap in `docs/roadmap.md` — read it before inventing
features. Only non-code leftover: code-sign the Windows agent (needs a cert, see gotcha #2).

**Active next step: multi-tenancy (roadmap #6).** Strategic context: the repo owner wants to offer
a cheap hosted SaaS ("Shopify/WooCommerce order comes in → label/packing slip prints") undercutting
the paid PrintNode-wrapper plugins. Org isolation is the technical blocker; the store app and
document rendering (roadmap #5) come after.

What exists today (design for it, don't re-derive):

- Every table (`agents`, `printers`, `jobs`, `api_keys`) already carries `org_id`, but **nothing
  enforces it** — everything runs as `DEFAULT_ORG=1`, queries don't filter by org.
- Client keys are flat: `authenticate_client` returns the key id and ignores org. The bootstrap
  `PRINTAPI_TOKEN` is global root (admin endpoints + always a valid client).
- Agent keys bind name↔key on first contact (`register_agent`); agents currently always land in
  `DEFAULT_ORG`.

Target shape (PrintNode's "child accounts" is the model, but YAGNI hard):

- Root token manages orgs: `POST /orgs {name}` → `{org_id}`, `GET /orgs`, plus issuing org-scoped
  client/agent keys (extend `/apikeys` with an org, don't invent a parallel key system).
- Auth resolves presented key → `org_id`; every store read/write filters by it. Agents register
  into their key's org; printers/jobs inherit. **Invariant: no cross-org reads, ever** — a client
  key of org A must 404 (not 401) on org B's job/printer ids.
- Root keeps working roughly as today (decide and test its cross-org semantics explicitly).
- OUT for now: billing, per-org dashboard users, quotas, delegated auth. Mark ceilings with
  `# ponytail:` comments.
- Migration: existing DBs are single-org (`org_id=1` everywhere) and must keep working unchanged;
  the ALTER-based idempotent pattern in `store.init_db` is the house style for schema changes.

## 6. Reference

- v0 production deployment + ops: the original (private) deployment repo.
- Design rationale: `docs/design-v0-homelab.md` (in this repo, sanitized).
- The homelab used n8n as the orchestrator calling the server; for OSS the server itself is the API
  surface (clients call it directly), so the n8n part does not carry over.
