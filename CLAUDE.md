# CLAUDE.md — printpapi (self-hosted PrintNode alternative, source available)

Print bridge: HTTP API + agent that prints jobs on a remote machine (documents **and** label
printers). Extracted from a working homelab deployment; now a public, source-available project
(Elastic License 2.0 — self-hosting free, reselling it as a hosted service is not; see `LICENSE`).

**Read [`HANDOFF.md`](HANDOFF.md) first** — it has the full origin story, what works (v0), the hard-won
gotchas, and the v1 plan. Don't re-derive; build on it.

## Tech stack

- **Server:** Rust (`crates/server`, package `printpapi-server`, binary `printpapi`) since v3.0.0;
  the last Python server is v2.5.1. **Few, well-known crates; no web framework** (hyper directly).
  Build: `cargo build --release -p printpapi-server`. No Python and no Node at runtime.
- **Agent:** Rust (`crates/agent`, binary `printpapi-agent`), cross-platform. Windows: talks to the
  spooler directly (no pywin32) + SumatraPDF (PDF). Linux/macOS: CUPS `lp`. `agent.ini` sits next
  to the executable. The Python agent (`agent/print_agent.py`) is kept as the **legacy agent** for
  locked-down Windows (the Rust exe is not code-signed yet) and is still tested in CI.
- **Dashboard:** Next.js (App Router, TS) + Tailwind v4 + **shadcn/ui** (Radix) + Motion, built as
  a **static export** into `crates/server/web` and **embedded into the server binary** at compile
  time. No Node at runtime — the build-time UI toolchain is separate from the server.
- **Tests:** `cargo test --workspace` from repo root; `python -m pytest agent/tests` for the legacy
  Python agent. `tools/parity/` (black-box parity vs the v2.5.1 Python server) and `tools/bench/`
  (resources/speed) are Python scripts, not part of the test suite.
- Container: `Dockerfile` (node stage builds the UI → rust stage builds the binary → distroless
  `cc-debian13` runs it: the binary only, no Python, no cups-client, no shell).

## Structure

```
Cargo.toml  workspace (crates/server, crates/agent)
crates/server/  the server: src/{dispatch,orders,packing_slip,printnode,billing,cloudprnt,auth}.rs
            are pure; src/{store (SQLite),server,http,web,mail}.rs own the IO. tests/ = integration
crates/server/web/  built dashboard bundle — generated, committed, embedded into the binary,
            never hand-edited
crates/agent/  the Rust agent (printpapi-agent)
web/        dashboard source (Next.js). `npm run build:app` builds + syncs into crates/server/web
agent/      legacy Python agent (print_agent.py + tests) and agent.ini.example
integrations/  store-side plugins that call the API (woocommerce/: a WordPress plugin, PHP)
tools/      parity/ (Rust vs v2.5.1 Python server) and bench/ (resource/speed measurements)
docs/       user docs; design-v0-homelab.md (the original homelab design)
```

**Never edit `crates/server/web` by hand** — it is build output. Change `web/`, run
`npm run build:app`, commit both, then rebuild the binary. It is committed so a Node-less
`cargo build` checkout still has a UI.

The pure modules (`dispatch`, `orders`, `packing_slip`, `printnode`, `billing`, `cloudprnt`,
`auth`) do no IO and take injectable fetchers → easy to test. `store`, `server` and `http` own the
IO (SQLite, HTTP, outbound fetches, mail — the fetcher and mailer are injectable for tests). Keep that split.

## Conventions

- **TDD.** Write the failing test first, then the minimal code. Every non-trivial branch keeps a test.
- **Laziness / YAGNI (ponytail).** Stdlib before a dependency; native before custom; one line before
  fifty. Mark deliberate shortcuts with a `// ponytail:` comment naming the ceiling.
- **Security at trust boundaries stays:** bearer/API-key auth on every endpoint, constant-time token
  compare, `http(s)`-only URL fetches, no shell invocation of user input, temp-file cleanup.
- **Elastic License 2.0** (source available, since after v1.4.0 — v1.0.0–v1.4.0 stay MIT). New
  Rust files get the one-line header
  `// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).` The WooCommerce
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
2. Locked-down Windows (Smart App Control/WDAC) blocks unsigned `.exe`. The Rust agent exe is **not
   code-signed yet**; there, run the legacy Python agent via the signed Python interpreter until
   signed builds exist.
3. A WAF/Cloudflare in front of a fetched URL 403s a default library UA (`Python-urllib`, and
   likewise a bare HTTP client) — set a browser UA on outbound HTTP.
