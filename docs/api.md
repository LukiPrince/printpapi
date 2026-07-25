# HTTP API

All endpoints speak JSON over HTTP. Authentication is a bearer token:
`Authorization: Bearer <token>`.

**Roles:**

- *client* — the bootstrap `PRINTAPI_TOKEN` **or** any active issued key (see [API keys](server.md#api-keys))
- *admin* — the bootstrap `PRINTAPI_TOKEN` only
- *agent* — the per-agent key (bound to the agent name on first contact)

Token comparison is constant-time (`hmac.compare_digest`).

## Endpoints

| Method & path | Auth | Purpose |
|---|---|---|
| `GET /` | none | Web dashboard (static bundle from `app/web`; data fetched with the token) |
| `GET /health` | none | Liveness check |
| `GET /metrics` | client | Prometheus text: job counts by state, agent/printer liveness |
| `POST /jobs` | client | Submit a job → `{job_id}` |
| `GET /jobs` | client | Recent job history |
| `GET /jobs/{id}` | client | One job's state: `queued` \| `claimed` \| `done` \| `failed` \| `cancelled` |
| `DELETE /jobs/{id}` | client | Cancel a still-`queued` job (`409` once claimed, `404` if unknown) |
| `GET /printers` | client | Registered printers + online/offline |
| `POST /apikeys` | admin | Issue a per-client key → `{id, label, key}` (key shown once) |
| `GET /apikeys` | admin | List key labels (never the secret) |
| `DELETE /apikeys/{id}` | admin | Revoke a key |
| `POST /agent/register` | agent | Declare name + printers → `{computer_id, printer_ids}` |
| `GET /agent/jobs` | agent | Long-poll for a job (204 on timeout) |
| `GET /agent/jobs/{id}/payload` | agent | Download the job's bytes |
| `POST /agent/jobs/{id}/result` | agent | Report `{ok, error?}` |

Request bodies are capped at 32 MB.

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

Unknown keys or invalid values → `400`. Values are what the printer driver understands — there is
no capability discovery yet (see the [roadmap](roadmap.md)); an option the driver doesn't support
is silently ignored by it. On CUPS, `bin`/`paper` values must not contain spaces.

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

## Metrics

`GET /metrics` returns Prometheus text (`text/plain; version=0.0.4`) — `printpapi_jobs{state=…}`
(all five states, including zeros), `printpapi_agents_online`, `printpapi_agents_total`,
`printpapi_printers_total`. It needs client auth, so point your scraper at it with a bearer token:

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

`DELETE /jobs/{id}` moves a job from `queued` to `cancelled` (a terminal state the agent never
claims). The cancel is state-guarded: once an agent has claimed the job it returns `409` — there
is no mid-print interrupt.
