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
| `PRINTAPI_SIGNUP` | `closed` | `open` lets anyone create an org via `POST /signup` — for a hosted deployment, not a private box |
| `PUBLIC_URL` | *(unset)* | This server's own address, e.g. `https://print.example`. Only then does a password-reset mail carry a link (see [api.md](api.md#password-reset)) |
| `SMTP_HOST` | *(unset)* | Mail server for password resets. Unset ⇒ the reset token is printed to stderr instead |
| `SMTP_PORT` | `587` (`465` with `SMTP_SSL`) | |
| `SMTP_USER` / `SMTP_PASSWORD` | *(unset)* | Omit for an anonymous relay |
| `SMTP_FROM` | `printpapi@localhost` | Envelope sender |
| `SMTP_SSL` | *(off)* | `1` for implicit TLS; otherwise STARTTLS is used |
| `SMTP_STARTTLS` | `1` | `0` to disable STARTTLS (plaintext relay on a trusted network) |
| `PRINTAPI_PLANS` | *(unset)* | [Billing](billing.md) catalogue: a JSON array of plans, or a path to a file holding one. Unset ⇒ no plans, no billing routes |
| `PRINTAPI_BILLING_SECRET` | *(unset)* | Shared secret for the `POST /billing/webhook` signature. Unset ⇒ the webhook answers `503` |

```bash
PRINTAPI_TOKEN=change-me PRINT_DB=data/printpapi.db python -m app.server
```

## Docker

Pull the prebuilt image (published to GHCR on every release, amd64 + arm64):

```bash
docker run -e PRINTAPI_TOKEN=change-me -p 3460:3460 \
           -v $PWD/data:/app/data -e PRINT_DB=/app/data/printpapi.db \
           ghcr.io/lukiprince/printpapi
```

The `-v` + `PRINT_DB` keep the SQLite DB (jobs **and** issued API keys) on the host, so
recreating the container doesn't wipe them. `docker compose up -d` does the same with a named
volume — see [`docker-compose.yml`](../docker-compose.yml).

Build it yourself instead: `docker build -t printpapi .` then run the same command with
`printpapi` in place of the `ghcr.io/...` image.

Image is `python:3.12-slim` + `cups-client`. The server itself needs no Python packages. The
build is two-stage: a `node:22-slim` stage compiles the dashboard, and only its static output
is copied into the runtime image — there is no Node at runtime.

## Dashboard

`GET /` serves a static, secret-free React dashboard: a live overview (queue counters,
job-outcome breakdown, activity feed), printers online/offline, searchable job history with
cancel, one-click test print, API-key management, a **Team** page (add or remove the people who
may sign in, change your own password), a **Settings** page (this month's jobs against the org's
quota, the agent-event URL, the Shopify webhook secret), agent install instructions, light/dark
theme and a ⌘K command palette. You sign in with an account — creating one yourself where the
operator allows it, and resetting a forgotten password by e-mail — or paste a token; either way
the credential is kept in
`localStorage` and sent as a bearer header — nothing sensitive lives in the served files. The test print sends a
PDF only to PDF-capable printers and a ZPL label to everything else.

**Where it comes from.** The source is a Next.js app in [`web/`](../web) (App Router,
TypeScript, Tailwind v4, [shadcn/ui](https://ui.shadcn.com) on Radix, Motion for animation).
It is built as a **static export** — plain HTML/JS/CSS — and committed to `app/web`, which
the server serves directly. That keeps the server stdlib-only and means a plain
`python -m app.server` checkout has a working dashboard with no Node installed.

Changing the UI:

```bash
PRINTAPI_TOKEN=change-me python -m app.server   # the API, on :3460

cd web
npm install
npm run dev        # localhost:3000 with hot reload; API calls are proxied to :3460 in dev
npm run build:app  # static export -> web/out, then synced into app/web (commit that)
```

`npm run dev` only works against a running server — it has no API of its own. The dev-only proxy
lives in `web/next.config.ts`; point it elsewhere with `PRINTPAPI_ORIGIN=http://host:port`. It is
stripped from the production build, where the Python server serves the bundle and the API together.
**`app/web` is build output** — change `web/`, run `npm run build:app`, commit both.

Asset paths under `/_next/static/` are content-hashed and served `immutable`; the HTML is
served `no-cache`. Requests are confined to `app/web` — a path that escapes it 404s.
If `app/web` is missing (source checkout, never built), `GET /` returns a short page telling
you to build it; the JSON API is unaffected.

## API keys

The bootstrap `PRINTAPI_TOKEN` is root: it can do everything, including issuing scoped
per-client keys (one per integration). Issued keys can submit and read jobs, but can't manage
keys. Keys are stored SHA-256-hashed; revoking one cuts access immediately.

```bash
curl -s -X POST localhost:3460/apikeys \
     -H 'Authorization: Bearer <PRINTAPI_TOKEN>' -H 'Content-Type: application/json' \
     -d '{"label":"n8n"}'
# -> {"id":1,"label":"n8n","org_id":1,"key":"<shown once — store it>"}
```

Add `"org_id"` to put the key (and any agent registering with it) in another org — see
[Multi-tenancy](api.md#multi-tenancy). Without it, everything stays in the single default org,
which is what a plain self-hosted install wants.

## Accounts

So nobody has to paste the root token into a browser, an org's people can sign in with an e-mail
and password. Root seeds the first user, that user invites the rest and manages the org's keys:

```bash
curl -s -X POST localhost:3460/orgs/1/users \
     -H 'Authorization: Bearer <PRINTAPI_TOKEN>' -H 'Content-Type: application/json' \
     -d '{"email":"ops@yourshop.example","password":"a long passphrase"}'
```

The dashboard's sign-in screen then takes those credentials (the token field is still there for
root and for machine keys). Passwords are salted `scrypt` hashes, sessions expire after 30 days,
and a machine key can never manage keys or users — see
[Accounts and login](api.md#accounts-and-login).

## Queue behavior

SQLite in WAL mode with an atomic claim. A visibility-timeout reaper requeues jobs whose agent
went silent and fails them after a bounded number of retries. Reaper errors are logged to
stderr.

## Security

Enforced at every trust boundary: bearer auth on every endpoint, constant-time token compare,
SHA-256-hashed agent, client and session credentials, salted `scrypt` password hashes with a
throttled and non-enumerating login, `http(s)`-only URL fetches, 32 MB request-body cap,
no `shell=True`, agent temp-file cleanup, and job payloads scoped so an agent can only touch
its own jobs.

The [PrintNode-compatible layer](printnode-compat.md) accepts HTTP **Basic** auth as well — the key
rides in the username, so it is the same secret with the same org scope, but base64 in a header is
not encryption: put TLS in front of it, as you should for the bearer token anyway.

[Star CloudPRNT](cloudprnt.md) printers carry the client key in their polling URL
(`/cloudprnt/<key>`), because the printer appends its own query string to whatever URL it is given.
A key in a URL ends up in access logs and proxy history — prefer the printer's Basic-auth
**User Name** field where the model offers one, and revoke the key
(`DELETE /apikeys/{id}`) if a device is lost.

The server speaks plain HTTP — put your own reverse proxy / TLS in front for anything beyond
the LAN. Vulnerability reports: see [SECURITY.md](../SECURITY.md).
