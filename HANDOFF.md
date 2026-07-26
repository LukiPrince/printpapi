# HANDOFF — printpapi

Continuation notes for building the self-hosted PrintNode alternative (public and source
available — Elastic License 2.0 since after v1.4.0, see `LICENSE`). If you are a fresh
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
  **Team** (org users, add/remove, own password change), **Settings** (quota + usage, `event_url`,
  Shopify secret), agent setup, light/dark, ⌘K palette. Sign-in takes an account (e-mail +
  password) or a pasted token, and offers self-signup / "forgot password" where the server
  reports them on `/health`. Secret-free shell — it prompts for the token and calls the
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
  - `POST /orders` — a shop order in, a **packing slip** out: `{printer_id, order, format?}` where
    `format` is `shopify`/`woocommerce` (map that store's own JSON) or absent (already normalized).
    Renders with `app/packing_slip.py` (stdlib PDF writer) and queues it as a pdf job. A raw-only
    printer is a `400` — gotcha #1 enforced server-side.
  - `POST /integrations/shopify/orders?key=…&printer_id=…` — Shopify's order webhook. Shopify can
    send no auth header, so the URL key names the org and the `X-Shopify-Hmac-Sha256` signature
    (checked against the org's `shopify_secret`) proves authenticity. Redeliveries dedupe on the
    order id. The WooCommerce side is a real WP plugin in `integrations/woocommerce` that calls
    `POST /orders` with a client key.
  - `GET /metrics` — Prometheus text. `GET /health`.
- **PrintNode-compatible layer** (`app/printnode.py`, roadmap #7) — the *same* endpoints answer in
  PrintNode's JSON shapes when the request authenticates with **HTTP Basic** instead of `Bearer`
  (their clients carry the API key as the Basic username, so nothing needs enabling). Surface:
  `GET /whoami`, `/computers`, `/printers`, `POST|GET|DELETE /printjobs`,
  `/printjobs/{set}/states`, plus their id-set paths (`/printers/5-9`). Pure translation — same
  keys, same orgs, same `POST /jobs` validation path (`_submit_job`), no schema or agent change.
  Their unknown option keys are dropped rather than rejected (a client sends its whole option set);
  options on raw jobs are dropped whole. See `docs/printnode-compat.md` for the mapping tables and
  the trademark disclaimer that has to stay.
- **Star CloudPRNT** (`app/cloudprnt.py`, roadmap #9) — Star's printers poll an HTTP URL by
  themselves, so at a site with one there is **no agent to install**. `POST|GET|DELETE
  /cloudprnt/{client-key}` (or the bare path with the key as HTTP Basic user) answers their three
  methods: the poll claims a job and offers it as `{"jobReady": true, "mediaTypes": […],
  "jobToken": …}`, the GET serves the bytes under the media type the printer's `Accept` says it
  speaks, the DELETE carries its status code and finishes the job. A device enrols itself by MAC as
  a one-printer pseudo-agent (`store.register_cloudprnt`), **raw-only** — gotcha #1 again: a pdf
  job to one is failed on the next poll instead of feeding blanks. An unconfirmed job is re-offered
  (`store.claimed_job`) instead of being replaced, and a printer reporting `printingInProgress` is
  offered nothing. A confirmation code of `2xx` is a printed job (`201`/`211` are successes with a
  warning); anything else fails it with the code and, where documented, a reason
  (`cloudprnt.status_text`). See `docs/cloudprnt.md` for the setup, the ceilings (DELETE-only
  confirm, no MQTT, no peripherals, no capability discovery) and the trademark disclaimer that has
  to stay. **Caveat: built from the published spec, never run against a physical printer** — the
  firmware-dependent parts (optional `jobToken` / `printingInProgress`, the exact confirmation code)
  are where it will need a fix, and the docs say so out loud.
- **Org accounts** (`app/auth.py` + `users`/`sessions` in the store) — a person signs in with
  e-mail + password (`POST /login`) and gets a **session token** carried in the same
  `Authorization: Bearer` header. Three credential kinds, one header: *root* (bootstrap token,
  every org), *session* (one org, may manage it), *key* (one org, print + read only — a leaked
  integration key cannot issue itself a successor). Which table the credential resolves in *is*
  the permission; there is no role column. `POST /logout`, `GET /me`, `PUT /me/password`
  (invalidates that user's sessions), `POST|GET /users`, and root-only `POST /orgs/{id}/users`
  for an org's first user. Passwords are stdlib `scrypt`, sessions expire after 30 days and are
  stored hashed, login is throttled per e-mail and does not enumerate accounts.
  See `docs/api.md#accounts-and-login`.
- **Hosted-service plumbing** on top of the accounts — what a paid deployment needs and a private
  box must not get by default:
  - `POST /signup {email, password, org_name?}` — org + first user + session in one call. **Off
    unless `PRINTAPI_SIGNUP=open`**; throttled by client address (an org sprayer picks a fresh
    e-mail each time, so counting addresses bounds nothing).
  - `POST /password/reset` / `/password/reset/confirm` — a one-shot, hour-long, hashed token,
    superseded by the next request, mailed over SMTP (`app/mail.py`, stdlib `smtplib`). No
    `SMTP_HOST` ⇒ the mail goes to stderr, so a self-host still works. The mail carries a **link
    only if `PUBLIC_URL` is set** — deriving it from the `Host` header would let anyone mail a
    valid token pointing at their own host.
  - `DELETE /users/{id}` — never yourself, never an org's last account, foreign ids `404`.
  - `GET /orgs/{id}` — one org's settings + `job_quota` + `jobs_this_month` (a session may read
    its own; root any).
  - **Quotas**: `orgs.job_quota` (NULL = unlimited) capped per UTC calendar month, enforced inside
    `enqueue_job` so `POST /jobs`, `/orders`, the Shopify webhook and the PrintNode layer are all
    covered by one guard; a spent quota is `402`, an idempotent resubmit spends nothing. **Root
    sets it, a session cannot** — a tenant that could raise its own cap has no cap.
  - `GET /health` reports `signup` and `password_reset` so the sign-in screen only offers doors
    that exist. See `docs/api.md#self-signup`, `#password-reset`, `#quotas`.
- Management endpoints — **per-client API keys** (root *or* a session, never a machine key):
  - `POST /apikeys {label}` → issues a new random key (shown once); `GET /apikeys` lists labels;
    `DELETE /apikeys/{id}` revokes. Keys are stored sha256-hashed; revoked keys stop authorizing.
    A session is confined to its own org here — a foreign key id is `404`, and `PUT /orgs/{id}`
    on a foreign org is `404` too.
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
  - **File target** (`= file:///path/to/dir`, roadmap #8 — "virtual print server"): writes the job
    into that directory as `job-<id>.pdf` (pdf) or `job-<id>.prn` (raw, verbatim bytes) instead of
    printing it — archival, Paperless consume folders, testing without burning labels. Takes both
    modes (`can_pdf` forced true: a directory cannot misrender), copies get a `-N` suffix, print
    options are ignored. Agent-only — the server sees an ordinary printer.
- Configured via `agent.ini` (next to the script): `server_url`, `api_key`, `name`, `printers`
  (semicolon-separated — Windows printer names, or CUPS queue names on Linux).
- Shipped in the homelab as a signed-Python install (see gotcha #2), autostart via Task Scheduler.

**Tests:** 282, all green (`python -m pytest`). Real loopback HTTP servers (ThreadingHTTPServer),
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
n8n/Zapier/Make, per-printer-family setup, QZ Tray/PrintNode comparison in the README), and
**e-commerce auto-print** (roadmap #5: `POST /orders`, packing-slip renderer, WooCommerce plugin,
Shopify webhook — `docs/ecommerce.md`), and the **PrintNode API compatibility layer** (roadmap #7:
`app/printnode.py`, Basic-auth-selected — `docs/printnode-compat.md`), and the **file backend**
(roadmap #8: a `file:///dir` printer target in the agent — `docs/agent.md#file-output-virtual-print-server`),
and **org accounts** on top of multi-tenancy (e-mail/password login, session tokens, org-scoped
key and user self-management — `app/auth.py`, `docs/api.md#accounts-and-login`), and the
**hosted-service plumbing** on top of those (self-signup, password reset by e-mail, user removal,
org settings in the dashboard, monthly job quotas — `app/mail.py`, `docs/api.md#self-signup`), and
**Star CloudPRNT** (roadmap #9: the printer polls us itself, no agent at the site —
`app/cloudprnt.py`, `docs/cloudprnt.md`; spec-complete but unverified on hardware).
284 tests green.
A demand-research sweep (July 2026) produced the ranked v2 roadmap in `docs/roadmap.md` — read it
before inventing features. Roadmap #1–#9 are done. What is left on the ranked list: #10 scales
(agent-side USB HID), #11 ESC/POS templating. Non-code leftover: code-sign the Windows agent (needs
a cert, gotcha #2). And for a paid deployment, **billing** is still the one missing product piece.

On the PrintNode compat layer specifically, the deliberate ceilings are: a job's state *history* is
a single entry (we store the current state only), `capabilities.papers` carries names with `null`
dimensions (the agent never discovers extents), `source` is not persisted, and scales / credits /
child accounts are not portable at all. Clients that hardcode their hostname still need a
proxy/DNS override — only ones with a configurable base URL work by themselves. The legal footing:
reimplementing an API is lawful (EU 2009/24/EC Art. 1(2), CJEU C-406/10; US *Google v. Oracle*),
provided no documentation text or SDK code is copied and the trademark is used only descriptively —
which is why the disclaimer sits in the README, the compat doc and `app/printnode.py`'s docstring.

On the e-commerce work specifically, the deliberate ceilings are: the packing slip is plain
(Helvetica, no logo, no template — `# ponytail:` note at the top of `app/packing_slip.py`),
carrier labels are not first-class (submit them as `pdf_uri`/`raw_uri`), and the Shopify path
needs the client key in the webhook URL because Shopify cannot send an auth header — the HMAC is
what actually authorizes the print.

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
- **Org accounts are done on top of it** (see §2): per-org users with a password login, session
  tokens, and org-scoped key/user self-management. Still open: billing, quotas, self-signup,
  password reset by e-mail, org deletion; and an agent key doubling as its org's client key (see
  the `# ponytail:` note in `server.py`'s `/agent/register`). Since then **self-signup, password
  reset, user removal, org settings in the dashboard and monthly job quotas have shipped** (§2) —
  what is still open for a hosted offering is **billing** (a quota is enforced, nothing charges
  for it), plans/tiers, and org deletion. Known ceilings: the login/signup/reset throttles are per
  process and in memory (the signup one keys on the socket peer, so behind a reverse proxy every
  signup shares one counter — rate-limit in the proxy there), the session token lives in the
  browser's localStorage (XSS-readable — as the root token already was), quota usage is a `COUNT`
  per submit, and a reset mail links only when `PUBLIC_URL` is set.

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
PrintNode-wrapper plugins. Org isolation, document rendering (roadmap #5) and the account
plumbing (signup, reset, quotas) are done — **billing is the last piece of product** before a
paid deployment: a quota is enforced but nothing charges for exceeding or raising it.

## 6. Reference

- v0 production deployment + ops: the original (private) deployment repo.
- Design rationale: `docs/design-v0-homelab.md` (in this repo, sanitized).
- The homelab used n8n as the orchestrator calling the server; for OSS the server itself is the API
  surface (clients call it directly), so the n8n part does not carry over.
