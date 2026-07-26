# HTTP API

All endpoints speak JSON over HTTP. Authentication is a bearer token:
`Authorization: Bearer <token>`.

**Roles:**

- *client* — the bootstrap `PRINTAPI_TOKEN` **or** any active issued key (see [API keys](server.md#api-keys)).
  An issued key only ever sees its own org; the bootstrap token spans all of them — see
  [Multi-tenancy](#multi-tenancy).
- *root* — the bootstrap `PRINTAPI_TOKEN` only (orgs and keys)
- *agent* — the per-agent key (bound to the agent name on first contact)

Token comparison is constant-time (`hmac.compare_digest`).

## Endpoints

| Method & path | Auth | Purpose |
|---|---|---|
| `GET /` | none | Web dashboard (static bundle from `app/web`; data fetched with the token) |
| `GET /health` | none | Liveness check |
| `GET /metrics` | client | Prometheus text: job counts by state, agent/printer liveness |
| `POST /jobs` | client | Submit a job → `{job_id}` |
| `POST /orders` | client | Render an order as a packing slip and print it → `{job_id}` ([e-commerce](ecommerce.md)) |
| `POST /integrations/shopify/orders` | HMAC | Shopify order webhook (key in the URL, signature in the header) |
| `GET /jobs` | client | Recent job history |
| `GET /jobs/{id}` | client | One job's state: `queued` \| `claimed` \| `done` \| `failed` \| `cancelled` |
| `DELETE /jobs/{id}` | client | Cancel a still-`queued` job (`409` once claimed, `404` if unknown) |
| `GET /printers` | client | Registered printers + online/offline + capabilities |
| `GET /computers` | client | Registered agents + online/offline + printer count |
| `POST /orgs` | root | Create an org → `{id, name}` |
| `GET /orgs` | root | List orgs (`event_url`, `shopify_secret_set` — never the secret itself) |
| `PUT /orgs/{id}` | root | Set/clear the org's `event_url` and/or `shopify_secret` |
| `POST /apikeys` | root | Issue a client key → `{id, label, org_id, key}` (key shown once) |
| `GET /apikeys` | root | List keys with their org (never the secret) |
| `DELETE /apikeys/{id}` | root | Revoke a key |
| `POST /agent/register` | agent | Declare name + printers → `{computer_id, printer_ids}` |
| `GET /agent/jobs` | agent | Long-poll for a job (204 on timeout) |
| `GET /agent/jobs/{id}/payload` | agent | Download the job's bytes |
| `POST /agent/jobs/{id}/result` | agent | Report `{ok, error?}` |

Request bodies are capped at 32 MB.

An `Authorization: Basic` header instead of `Bearer` switches the same server to the
**[PrintNode-compatible shapes](printnode-compat.md)** (`/whoami`, `/printjobs`, and PrintNode's
JSON for `/printers` and `/computers`), so an existing client can be pointed here unchanged. Same
keys, same orgs; everything below describes the `Bearer` API.

## Submitting a job

```bash
curl -s -X POST localhost:3460/jobs \
     -H 'Authorization: Bearer <client-key>' -H 'Content-Type: application/json' \
     -d '{"printer_id":1,"type":"raw_base64","content":"<base64 ZPL>","title":"Label #4712","copies":2}'
# -> {"job_id": 1}
```

`title` is optional and shows up in the dashboard's history.

`copies` is optional (default `1`, integer `1`–`100`) — the agent prints the job that many times.
Out of range or non-integer → `400`.

`callback_url` is optional (`http(s)` only) — the server POSTs the job's outcome there once it
reaches a terminal state. See [Webhooks](#webhooks). A non-`http(s)` scheme → `400`.

`idempotency_key` is optional (string, ≤128 chars) — **retry-safe submits**. Resubmitting the same
key inside the same org returns the *original* `job_id` and prints nothing extra, so a timed-out or
retried `POST /jobs` (order webhook redelivery, flaky network) can never double-print. Keys are
scoped per org and never expire; use something stable like the order id.

`expire_after` is optional (integer seconds, `1`–`2592000`) — **a deadline for the job**. Past it
the job is failed (`state: failed`, `error: "expired"`) instead of printed, so a stale shipping
label doesn't come out hours later when an offline agent reconnects. Without it, jobs wait forever.

`options` is optional and **pdf jobs only** (raw ZPL/ESC-POS carries its own layout — `400` on a
raw job). The agent maps them onto its backend's native flags (SumatraPDF `-print-settings` on
Windows, `lp -o` on CUPS):

```json
{"printer_id": 2, "type": "pdf_base64", "content": "<base64 PDF>",
 "options": {"duplex": "long-edge", "paper": "A4", "bin": "Tray 1",
             "color": false, "pages": "1-3,5"}}
```

| Key | Values | Maps to (Windows / CUPS) |
|---|---|---|
| `duplex` | `long-edge` \| `short-edge` \| `one-sided` | `duplexlong`… / `sides=two-sided-long-edge`… |
| `paper` | paper name, e.g. `A4`, `Letter` | `paper=` / `media=` |
| `bin` | tray name (driver-specific) | `bin=` / `InputSlot=` |
| `color` | `true` \| `false` | `color`/`monochrome` / `print-color-mode=` |
| `pages` | ranges like `1-3,5` | print settings / `page-ranges=` |

Unknown keys or invalid values → `400`. Values are what the printer driver understands — check
the printer's `capabilities` in `GET /printers` (`papers`, `bins`, `duplex`, `color`; reported
best-effort by the agent at registration via the Windows driver or CUPS `lpoptions`, `null` when
unavailable) for what it supports. An option the driver doesn't support is silently ignored by
it. On CUPS, `bin`/`paper` values must not contain spaces.

## Content types

| `type` | Payload | The server… |
|---|---|---|
| `raw_base64` | `content`: base64 bytes (ZPL/ESC-POS) | decodes and queues them |
| `pdf_base64` | `content`: base64 PDF | decodes and queues; agent renders |
| `raw_uri` | `url` | GETs the URL, queues the response bytes |
| `pdf_uri` | `url` | GETs the URL, queues as PDF |
| `raw_uri_post` | `url` + `json` | POSTs `json` to the URL, queues the response bytes |
| `pdf_uri_post` | `url` + `json` | POSTs `json` to the URL, queues as PDF |

URL fetches are `http(s)`-only and sent with a browser User-Agent (WAF/CDN-fronted services
reject the default Python one).

## Webhooks

Set `callback_url` on a job and the server POSTs this JSON once the job reaches a terminal state:

```json
{"job_id": 1, "state": "done", "error": null, "title": "Label #4712", "printer_id": 3}
```

- Fires on `done`, `failed`, and `cancelled` (state-based, so agent-reported, cancelled, and
  reaper-failed jobs all deliver).
- **Best-effort with retries:** any non-2xx or connection error is retried a few times by a
  background dispatcher, then given up (logged). Delivery is not guaranteed — if it matters, treat
  `GET /jobs/{id}` as the source of truth.
- **At-least-once:** a delivery can arrive **more than once** (e.g. the POST succeeded but its ack
  was lost, so it retries). Make your handler idempotent — dedupe on `job_id` + `state`.
- The payload is **unsigned** and the URL is fetched server-side (`http(s)` only) — same trust model
  as the `*_uri` content types. Point it at a trusted endpoint.

## Computers (agents)

```bash
curl -s localhost:3460/computers -H 'Authorization: Bearer <client-key>'
# -> {"computers":[{"id":1,"name":"warehouse-pc","online":true,"last_seen_at":1753.., 
#                  "created_at":1750..,"printers":2}]}
```

`online` means the agent polled or registered within the liveness window (60 s). Scoped to the
key's org, like every other client list.

### Agent liveness events

An org can receive a POST whenever one of its agents crosses that window — the fleet-monitoring
counterpart to per-job `callback_url` webhooks:

```bash
curl -s -X PUT localhost:3460/orgs/2 -H 'Authorization: Bearer <PRINTAPI_TOKEN>' \
     -d '{"event_url":"https://ops.example/printpapi-events"}'    # null clears it
```

```json
{"event":"computer_offline","computer_id":1,"name":"warehouse-pc","org_id":2,"last_seen_at":1753..}
```

- `computer_offline` fires once when the agent stops being seen, `computer_online` once when it
  comes back. One POST per edge, never a repeat while the state holds.
- **At-most-once, unlike job webhooks:** a failed POST is logged and dropped, not retried — the next
  real transition fires again. Treat `GET /computers` as the source of truth.
- `http(s)` only, unsigned, same trust model as `callback_url`.
- An org that sets its `event_url` later starts from the *current* state; past transitions are not
  replayed.

## Multi-tenancy

Every agent, printer, job, and key belongs to exactly one **org**. A key *is* the org: whatever
key a request presents decides what it can see, and nothing else does.

```bash
# 1. root creates the org
curl -s -X POST localhost:3460/orgs -H 'Authorization: Bearer <PRINTAPI_TOKEN>' \
     -d '{"name":"acme"}'                       # -> {"id":2,"name":"acme"}

# 2. root issues that org a key (one per agent / per integration)
curl -s -X POST localhost:3460/apikeys -H 'Authorization: Bearer <PRINTAPI_TOKEN>' \
     -d '{"label":"acme-agent","org_id":2}'     # -> {"id":1,"label":"acme-agent","org_id":2,"key":"…"}

# 3. the agent puts that key in agent.ini and registers — it lands in org 2, and so do its
#    printers and every job printed on them.
```

Rules:

- **An org key never reaches another org.** `GET /jobs`, `GET /printers` and `GET /metrics` return
  only that org's rows; a foreign job id is **`404`**, not `403` (a `403` would confirm it exists);
  `DELETE /jobs/{id}` on a foreign job is `404` too, even if that job is claimed (which would
  otherwise be `409`). Printing to a foreign `printer_id` is `400 unknown printer` — the same answer
  a nonexistent printer gets.
- **Root spans orgs.** The bootstrap `PRINTAPI_TOKEN` reads, submits and cancels across every org
  (that's what the dashboard uses), and is the only credential that can manage orgs and keys.
- **Agents inherit their key's org.** A key issued for org N puts the agent in org N. Any other key
  (including the ones existing agents already use) enrolls into the default org `1`, so nothing
  about a single-org install changes. Agent names are unique *per org*, not globally; an agent key
  is still bound to its name on first contact.
- `org_id` is optional on `POST /apikeys` and defaults to `1`. An unknown org → `400`.

**Existing installs need no migration** — everything already lives in org `1`, root behaves as
before, and every issued key resolves to org `1`.

Deliberately out of scope for now (`# ponytail:` in the code): billing, quotas, per-org dashboard
users, org-scoped key self-management, and org deletion. An agent key doubles as its org's client
key, and revoking it stops client calls but not an already-registered agent's polling.

## Metrics

`GET /metrics` returns Prometheus text (`text/plain; version=0.0.4`) — `printpapi_jobs{state=…}`
(all five states, including zeros), `printpapi_agents_online`, `printpapi_agents_total`,
`printpapi_printers_total`. The numbers cover the presented key's org (all orgs for the bootstrap
token). It needs client auth, so point your scraper at it with a bearer token:

```yaml
scrape_configs:
  - job_name: printpapi
    authorization: { credentials: <client-key> }
    static_configs: [{ targets: ["yourserver:3460"] }]
```

## Job lifecycle

`queued` → agent claims it (`claimed`) → agent reports → `done` or `failed`.
If an agent claims a job and never reports (crash, network), the visibility-timeout reaper
requeues it — after a bounded number of retries the job is marked `failed`.

A job with `expire_after` whose deadline passes is never handed to an agent (the claim query skips
it) and is failed with `error: "expired"` by the same reaper — a terminal state, so its webhook
fires like any other outcome.

`DELETE /jobs/{id}` moves a job from `queued` to `cancelled` (a terminal state the agent never
claims). The cancel is state-guarded: once an agent has claimed the job it returns `409` — there
is no mid-print interrupt.
