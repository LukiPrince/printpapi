# CLAUDE.md — printpapi (self-hosted PrintNode alternative, source available)

Print bridge: HTTP API + agent that prints jobs on a remote machine (documents **and** label
printers). Extracted from a working homelab deployment; now a public, source-available project
(Elastic License 2.0 — self-hosting free, reselling it as a hosted service is not; see `LICENSE`).

**Read [`HANDOFF.md`](HANDOFF.md) first** — it has the full origin story, what works (v0), the hard-won
gotchas, and the v1 plan. Don't re-derive; build on it.

## Tech stack

- **Server:** Python **stdlib only** (`http.server`, `socket`, `subprocess`, `urllib`). No web
  framework yet — keep it that way until a route count actually justifies one.
- **Agent:** Python, cross-platform. Windows: `pywin32` (raw) + SumatraPDF (PDF). Linux: CUPS `lp`.
  Backend auto-selected per OS by `select_backend`.
- **Dashboard:** Next.js (App Router, TS) + Tailwind v4 + **shadcn/ui** (Radix) + Motion, built as
  a **static export** into `app/web` and served by the Python server. No Node at runtime — the
  stdlib-only rule applies to the *server*, not to the build-time UI toolchain.
- **Tests:** `pytest`, no fixtures/frameworks beyond it. `python -m pytest` from repo root.
- Container: `Dockerfile` (node stage builds the UI → python:3.12-slim + cups-client runs it).

## Structure

```
app/        server: dispatch.py (pure logic) + store.py (SQLite) + server.py (HTTP + static serve)
            orders.py (shop payload -> order dict) + packing_slip.py (order dict -> PDF), both pure
app/web/    built dashboard bundle — generated, committed, never hand-edited
web/        dashboard source (Next.js). `npm run build:app` builds + syncs into app/web
agent/      cross-platform agent (Windows + Linux/macOS/CUPS): print_agent.py + tests
integrations/  store-side plugins that call the API (woocommerce/: a WordPress plugin, PHP)
tests/      server/dispatch/integration tests
docs/       design-v0-homelab.md (the original homelab design)
```

**Never edit `app/web` by hand** — it is build output. Change `web/`, run `npm run build:app`,
commit both. It is committed so a Node-less `python -m app.server` checkout still has a UI.

`app/dispatch.py` is pure (no IO, injectable fetchers) → easy to test. `app/server.py` owns the IO
(socket/agent/cups senders, all injectable for tests). Keep that split.

## Conventions

- **TDD.** Write the failing test first, then the minimal code. Every non-trivial branch keeps a test.
- **Laziness / YAGNI (ponytail).** Stdlib before a dependency; native before custom; one line before
  fifty. Mark deliberate shortcuts with a `# ponytail:` comment naming the ceiling.
- **Security at trust boundaries stays:** bearer/API-key auth on every endpoint, constant-time token
  compare (`hmac.compare_digest`), `http(s)`-only URL fetches, no `shell=True`, temp-file cleanup.
- **Elastic License 2.0** (source available, since after v1.4.0 — v1.0.0–v1.4.0 stay MIT). New
  source files get the one-line header: `Elastic License 2.0 (see LICENSE)`. The WooCommerce
  plugin in `integrations/woocommerce/` is the one exception: GPL-2.0-or-later, because
  wordpress.org requires it. Never re-license a file without checking `LICENSE` first.
- **Cross-platform from v1 on:** don't hard-code Windows assumptions in the server; the agent is the
  platform-specific part.

## The one architectural decision that matters

v0 is **push** (server → agent, same LAN). **v1 must make the agent POLL the server** (long-poll or
WebSocket) so agents behind NAT/firewalls work without inbound ports — that is PrintNode's core trick
and the main thing that makes this a real alternative. Design new work around the poll model.

## Don't repeat these (see HANDOFF for detail)

1. Never send raw PDF to a label printer — it prints blanks. Render first (driver/CUPS) or send
   already-rendered data.
2. Locked-down Windows (Smart App Control/WDAC) blocks unsigned `.exe`. Ship the agent code-signed,
   or run it via the signed Python interpreter.
3. A WAF/Cloudflare in front of a fetched URL 403s the default `Python-urllib` UA — set a browser UA
   on outbound HTTP.
