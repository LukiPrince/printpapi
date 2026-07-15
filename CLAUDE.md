# CLAUDE.md — printpapi (OSS self-hosted PrintNode alternative)

Open-source print bridge: HTTP API + agent that prints jobs on a remote machine (documents **and**
label printers). Extracted from a working homelab deployment; now being grown into a general OSS tool.

**Read [`HANDOFF.md`](HANDOFF.md) first** — it has the full origin story, what works (v0), the hard-won
gotchas, and the v1 plan. Don't re-derive; build on it.

## Tech stack

- **Server:** Python **stdlib only** (`http.server`, `socket`, `subprocess`, `urllib`). No web
  framework yet — keep it that way until a route count actually justifies one.
- **Agent:** Python, cross-platform. Windows: `pywin32` (raw) + SumatraPDF (PDF). Linux: CUPS `lp`.
  Backend auto-selected per OS by `select_backend`.
- **Tests:** `pytest`, no fixtures/frameworks beyond it. `python -m pytest` from repo root.
- Container: `Dockerfile` (python:3.12-slim + cups-client).

## Structure

```
app/        server: dispatch.py (pure logic) + store.py (SQLite) + server.py (HTTP + dashboard)
agent/      cross-platform agent (Windows + Linux/CUPS): print_agent.py + tests
tests/      server/dispatch/integration tests
docs/       design-v0-homelab.md (the original homelab design)
```

`app/dispatch.py` is pure (no IO, injectable fetchers) → easy to test. `app/server.py` owns the IO
(socket/agent/cups senders, all injectable for tests). Keep that split.

## Conventions

- **TDD.** Write the failing test first, then the minimal code. Every non-trivial branch keeps a test.
- **Laziness / YAGNI (ponytail).** Stdlib before a dependency; native before custom; one line before
  fifty. Mark deliberate shortcuts with a `# ponytail:` comment naming the ceiling.
- **Security at trust boundaries stays:** bearer/API-key auth on every endpoint, constant-time token
  compare (`hmac.compare_digest`), `http(s)`-only URL fetches, no `shell=True`, temp-file cleanup.
- **MIT.** Add a short license header to new source files when the project goes public.
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
