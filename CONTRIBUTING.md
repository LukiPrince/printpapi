# Contributing to printpapi

Thanks for helping! The bar for merging is low ceremony, high discipline:

## Ground rules

- **Few, well-known crates; no web framework.** The server speaks HTTP through `hyper` directly.
  Keep the pure-logic/IO split: `dispatch`, `orders`, `packing_slip`, `printnode`, `billing`,
  `cloudprnt` and `auth` are pure (no IO, injectable fetchers); `store`, `server` and `http` own
  the IO. A new dependency needs a reason in the PR.
- **TDD.** Write the failing test first, then the minimal code that passes. Every non-trivial
  branch keeps a test. Run `cargo test --workspace` from the repo root — it must stay green. The
  legacy Python agent keeps its tests: `python -m pytest agent/tests`.
- **YAGNI.** Prefer the standard library, native platform features, and short diffs. Deliberate
  shortcuts are marked with a `// ponytail:` comment (`# ponytail:` in the Python agent) naming
  the ceiling and the upgrade path.
- **Security invariants stay:** bearer auth on every endpoint, constant-time token compare,
  hashed keys at rest, `http(s)`-only URL fetches, no shell invocation of user input. The
  one documented exception is the Shopify webhook, which cannot send a bearer header: there the
  HMAC over the raw body is what authorizes the print.
- **License header.** New Rust files start with
  `// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).`
  The WooCommerce plugin stays GPL-2.0-or-later.
- **`crates/server/web` is build output, never hand-edited.** The dashboard source is `web/`
  (Next.js). It is committed as a static export and embedded into the binary at compile time, so
  a Node-less checkout still builds a server with a UI — see
  [docs/server.md](docs/server.md#dashboard).

## Workflow

1. Fork, branch from `main`.
2. Add a failing test, make it pass, keep the suite green.
3. Touched `web/`? Run `cd web && npm run build:app` and commit the regenerated `crates/server/web`
   with it — CI fails the PR otherwise, and self-hosters would keep serving the old dashboard.
4. Touched `README.md` or `docs/`? `python tools/docs/check_docs.py` must pass. Once per clone,
   `git config core.hooksPath tools/githooks` runs it before every push.
5. Open a PR with a short description of the *why*.

## Licensing of contributions

printpapi is **source available** under the [Elastic License 2.0](LICENSE) — self-hosting is free,
including commercially; offering it to the public as a hosted service is not. The WooCommerce
plugin is GPL-2.0-or-later instead, because wordpress.org requires that.

By opening a pull request you confirm that your contribution is licensed under the same license as
the files it touches, and you grant the author a perpetual, worldwide, irrevocable, royalty-free
right to use, modify and **re-license** it — including under different terms. That is what keeps a
future license change or a commercial license possible without tracking down every contributor. If
your employer owns your work, get their sign-off first.

## Test conventions

Tests use real loopback HTTP servers and real SQLite (in memory) with injected fetchers /
subprocess runners — no HTTP mocking, no real printers. Match that style. `tools/parity/parity.py`
checks the Rust server black-box against the last Python server (v2.5.1); see
[tools/parity/README.md](tools/parity/README.md). `tools/bench/bench.py` measures resources and
speed ([tools/bench/README.md](tools/bench/README.md)).
