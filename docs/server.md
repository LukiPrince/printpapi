# Server

One Rust binary, `printpapi` — no runtime, no Python, no Node. HTTP handler + SQLite queue. The
dashboard is embedded in the binary.

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
| `PRINTAPI_SIGNUP_QUOTA` | *(unset: no quota)* | Non-negative integer: the monthly [`job_quota`](api.md#quotas) of every org created through `POST /signup`, so a free signup cannot print without limit. `0` blocks printing. Orgs the root token creates (`POST /orgs`) are untouched, and a [billing](billing.md) plan still overrides it per org. Not an integer stops the server at startup. With `PRINTAPI_SIGNUP=open` and no quota, startup prints a warning |
| `PRINTAPI_JOB_RETENTION_DAYS` | *(unset or `0`: keep for ever)* | Positive integer N: see [Retention](#retention). Not a positive integer stops the server at startup. With `PRINTAPI_SIGNUP=open` and no retention, startup prints a warning |
| `PRINTAPI_TERMS_VERSION` | *(unset: no consent asked)* | A non-empty version string, e.g. `2026-11-13`. Set: `POST /signup` must carry `accept_terms` equal to it, `GET /health` reports it with the links below, and the dashboard's sign-up form shows a checkbox for the terms and the data processing agreement. Each acceptance (org, user, version, time) is stored in the `terms_acceptances` table. See [Self-signup](api.md#self-signup). With `PRINTAPI_SIGNUP=open` and no version, startup prints a warning |
| `PRINTAPI_TERMS_URL` | *(required with the version)* | `http(s)` link to the terms. Anything else, or a version without it, stops the server at startup |
| `PRINTAPI_DPA_URL`, `PRINTAPI_PRIVACY_URL` | *(unset)* | Optional `http(s)` links to the data processing agreement and the privacy policy, shown next to the checkbox. Anything else stops the server at startup |
| `PRINTAPI_PRIVATE_URLS` | `deny` with open signup, else `allow` | `deny`: payload URLs (`*_uri`, `*_uri_post`), job `callback_url`s and org `event_url`s may only reach public addresses — no loopback, LAN, link-local/cloud-metadata or other private ranges, checked after DNS on every redirect hop. `allow`: anywhere (a self-hosted box fetching PDFs from an internal service). Set `deny` whenever people you do not trust get keys |
| `PRINTAPI_TRUSTED_PROXIES` | *(unset: nobody)* | Comma-separated IPs or CIDRs (IPv4 and IPv6, e.g. `127.0.0.1,::1` or `172.16.0.0/12`) of the reverse proxies in front of printpapi. Only a request whose socket peer is listed has its `X-Forwarded-For` read: the rightmost entry that is not itself listed is the client, which the signup throttle (IPv6 per /64) and the `LOG_REQUESTS` line then use. Unset, the socket peer is the client, so behind a proxy every signup shares one throttle slot. An invalid entry stops the server at startup. See [Behind a reverse proxy](#behind-a-reverse-proxy) |
| `PRINTAPI_OPEN_ENROLLMENT` | *(off)* | `1`: any Bearer value enrolls a new agent into the default org (the v2 rule). Off: a new agent needs an issued key or the root token |

```bash
PRINTAPI_TOKEN=change-me PRINT_DB=data/printpapi.db ./printpapi
```

`printpapi --version` prints the version. An existing `printpapi.db` from the Python server
(v2.5.1 or older) opens unchanged — the schema is the same, apart from a few additive indexes.

### Upgrading from the Python server (v2.x)

Same environment, same database, same API. The [migration guide](migration.md) walks through the
server and agent upgrade, the defaults that are stricter in v3 and how to roll back.

## Install

Download the `printpapi-server-<platform>` archive from the
[latest release](https://github.com/LukiPrince/printpapi/releases/latest) and unpack it, or build it
(needs [Rust](https://rustup.rs/); on Windows the GNU or MSVC toolchain):

```bash
cargo build --release -p printpapi-server   # -> target/release/printpapi
```

The build embeds the dashboard from `crates/server/web` (committed), so no Node is needed to build
the server. See [Dashboard](#dashboard) for changing the UI.

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

The image is distroless (`gcr.io/distroless/cc-debian13`) and holds just the `printpapi` binary —
no Python, no `cups-client`, no shell (so `docker exec ... sh` does not work). The build has three
stages: `node:22-slim` compiles the dashboard, a Rust stage builds the server with it embedded
(cross-compiled for arm64), and only the binary is copied into the runtime image. The container
runs as root, as the Python image did, so an existing data volume stays writable.

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
It is built as a **static export** — plain HTML/JS/CSS — committed to `crates/server/web` and
**embedded into the binary at compile time**. A plain `cargo build` checkout therefore has a
working dashboard with no Node installed, and the running server needs no files next to it.

Changing the UI:

```bash
PRINTAPI_TOKEN=change-me cargo run -p printpapi-server   # the API, on :3460

cd web
npm install
npm run dev        # localhost:3000 with hot reload; API calls are proxied to :3460 in dev
npm run build:app  # static export -> web/out, then synced into crates/server/web (commit that)
```

`npm run dev` only works against a running server — it has no API of its own. The dev-only proxy
lives in `web/next.config.ts`; point it elsewhere with `PRINTPAPI_ORIGIN=http://host:port`. It is
stripped from the production build, where the server serves the bundle and the API together.
**`crates/server/web` is build output** — change `web/`, run `npm run build:app`, commit both, then
rebuild the binary (the bundle is embedded at compile time, so a running server keeps the old UI
until then).

`npm run build:demo` builds the hosted demo (start page plus the dashboard against an in-browser
fake API) into `web/out-demo`; it is never synced into the binary.

Asset paths under `/_next/static/` are content-hashed and served `immutable`; the HTML is
served `no-cache`. Requests are confined to the embedded bundle — a path that escapes it 404s.

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

### Retention

A job holds its payload (labels, packing slips, addresses) until you say otherwise. With
`PRINTAPI_JOB_RETENTION_DAYS=N` the reaper runs, at most once an hour:

- a job still `queued` after N days (its agent never came back) is failed with `error: "retention"`
  and cleared;
- a `done`, `failed` or `cancelled` job finished more than N days ago loses its payload, title and
  callback URL. Its row (state, printer, times) stays;
- of those, the rows created before the current UTC month are deleted. The current month's rows
  stay, because the [monthly quota](api.md#quotas) counts them: deleting them would hand quota back.

`claimed` jobs are never touched. With retention set the server opens SQLite with
`secure_delete=ON`, so freed pages are zeroed instead of keeping the old bytes. **Backups keep what
they copied** until you rotate them out: set your backup retention to match.

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

### Behind a reverse proxy

Behind a proxy every request comes from the proxy's address, so the signup throttle would see one
client. Set `PRINTAPI_TRUSTED_PROXIES` to the proxy's address and printpapi reads the real client
from `X-Forwarded-For`. List **only** the proxy: a request from any other peer keeps its socket
address, whatever header it sends. Caddy's `reverse_proxy` replaces an incoming
`X-Forwarded-For` from an untrusted client with its own, so it needs no extra configuration
([Caddy docs](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy#headers)).

With Docker Compose the peer printpapi sees is Caddy's container address, which moves unless it
is pinned. Put both on one user-defined network with a fixed subnet, give Caddy a fixed
`ipv4_address` and trust exactly that address, not the whole bridge range. Publish **no port for
printpapi**, only Caddy's 80 and 443: a published printpapi port plus a trusted bridge range would
let a direct caller's header be believed.

```yaml
services:
  printpapi:
    image: ghcr.io/lukiprince/printpapi:latest
    environment:
      PRINTAPI_TOKEN: change-me
      PRINTAPI_SIGNUP: open
      PRINT_DB: /app/data/printpapi.db
      PRINTAPI_TRUSTED_PROXIES: 172.30.0.2   # Caddy's address below, nothing wider
    volumes: [printpapi-data:/app/data]
    networks: [edge]                         # no `ports:` for printpapi
  caddy:
    image: caddy:2
    ports: ["80:80", "443:443"]
    volumes: [./Caddyfile:/etc/caddy/Caddyfile:ro, caddy-data:/data]
    networks:
      edge:
        ipv4_address: 172.30.0.2
networks:
  edge:
    ipam:
      config: [{ subnet: 172.30.0.0/24 }]
volumes:
  printpapi-data:
  caddy-data:
```

```
print.example.com {
    reverse_proxy printpapi:3460
}
```
