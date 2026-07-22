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
| `GET /` | none | Web dashboard (static shell; data fetched with the token) |
| `GET /health` | none | Liveness check |
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

## Job lifecycle

`queued` → agent claims it (`claimed`) → agent reports → `done` or `failed`.
If an agent claims a job and never reports (crash, network), the visibility-timeout reaper
requeues it — after a bounded number of retries the job is marked `failed`.

`DELETE /jobs/{id}` moves a job from `queued` to `cancelled` (a terminal state the agent never
claims). The cancel is state-guarded: once an agent has claimed the job it returns `409` — there
is no mid-print interrupt.
