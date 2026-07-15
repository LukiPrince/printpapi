# Server

Python **standard library only** — no framework, no dependencies. HTTP handler + SQLite queue.

## How it works

```
   client apps                 printpapi server                   agent (remote machine)
 ┌──────────────┐  POST /jobs   ┌────────────────┐  long-poll     ┌───────────────┐
 │ n8n, scripts │ ────────────▶ │  HTTP handler  │ ◀───────────── │ registers its │
 │ your backend │               │  SQLite queue  │  GET /agent/   │ printers,     │ ──▶ printer
 │              │ ◀──────────── │  (WAL + reaper)│ ─ job+bytes ─▶ │ polls, prints │  (USB / :9100
 └──────────────┘  GET /jobs/id └────────────────┘ ◀── result ─── │ raw or pdf    │   / CUPS)
                                        │                          └───────────────┘
                                   GET /  dashboard
```

1. An **agent** registers its printers, then long-polls `GET /agent/jobs`.
2. A **client** submits a job (`POST /jobs`); the server decodes the payload and queues it.
3. The agent receives the job, downloads the bytes, prints, reports the result.
4. The agent connects outbound only — no inbound ports on the printer's machine.

## Configuration

Environment variables:

| Var | Default | Meaning |
|---|---|---|
| `PRINTAPI_TOKEN` | *(required)* | Bootstrap/admin token — treat like a password |
| `PRINT_DB` | `printpapi.db` | SQLite database path |
| `PRINT_PORT` | `3460` | Listen port |
| `LOG_REQUESTS` | *(off)* | Set to log every HTTP request |

```bash
PRINTAPI_TOKEN=change-me PRINT_DB=data/printpapi.db python -m app.server
```

## Docker

```bash
docker build -t printpapi .
docker run -e PRINTAPI_TOKEN=change-me -p 3460:3460 \
           -v $PWD/data:/app/data -e PRINT_DB=/app/data/printpapi.db printpapi
```

Image is `python:3.12-slim` + `cups-client`. The server itself needs no Python packages.

## Dashboard

`GET /` serves a static, secret-free single page: printers online/offline, job history,
one-click test print, API-key management, agent install instructions. You paste the token
once; it's kept in `localStorage` and sent as a bearer header — nothing sensitive lives in
the HTML. The test print sends a PDF only to PDF-capable printers and a ZPL label to
everything else.

## API keys

The bootstrap `PRINTAPI_TOKEN` is root: it can do everything, including issuing scoped
per-client keys (one per integration). Issued keys can submit and read jobs, but can't manage
keys. Keys are stored SHA-256-hashed; revoking one cuts access immediately.

```bash
curl -s -X POST localhost:3460/apikeys \
     -H 'Authorization: Bearer <PRINTAPI_TOKEN>' -H 'Content-Type: application/json' \
     -d '{"label":"n8n"}'
# -> {"id":1,"label":"n8n","key":"<shown once — store it>"}
```

## Queue behavior

SQLite in WAL mode with an atomic claim. A visibility-timeout reaper requeues jobs whose agent
went silent and fails them after a bounded number of retries. Reaper errors are logged to
stderr.

## Security

Enforced at every trust boundary: bearer auth on every endpoint, constant-time token compare,
SHA-256-hashed agent and client keys, `http(s)`-only URL fetches, 32 MB request-body cap,
no `shell=True`, agent temp-file cleanup, and job payloads scoped so an agent can only touch
its own jobs.

The server speaks plain HTTP — put your own reverse proxy / TLS in front for anything beyond
the LAN. Vulnerability reports: see [SECURITY.md](../SECURITY.md).
