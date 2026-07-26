# Contributing to printpapi

Thanks for helping! The bar for merging is low ceremony, high discipline:

## Ground rules

- **Server stays stdlib-only.** No web framework, no runtime dependencies for `app/`. The agent
  may use platform packages (`pywin32` on Windows); nothing else without discussion.
- **TDD.** Write the failing test first, then the minimal code that passes. Every non-trivial
  branch keeps a test. Run `python -m pytest` from the repo root — it must stay green.
- **YAGNI.** Prefer the standard library, native platform features, and short diffs. Deliberate
  shortcuts are marked with a `# ponytail:` comment naming the ceiling and the upgrade path.
- **Security invariants stay:** bearer auth on every endpoint, constant-time token compare
  (`hmac.compare_digest`), hashed keys at rest, `http(s)`-only URL fetches, no `shell=True`. The
  one documented exception is the Shopify webhook, which cannot send a bearer header: there the
  HMAC over the raw body is what authorizes the print.
- **`app/web` is build output, never hand-edited.** The dashboard source is `web/` (Next.js). It is
  committed as a static export so a Node-less checkout still has a UI — see
  [docs/server.md](docs/server.md#dashboard).

## Workflow

1. Fork, branch from `main`.
2. Add a failing test, make it pass, keep the suite green.
3. Touched `web/`? Run `cd web && npm run build:app` and commit the regenerated `app/web` with it —
   CI fails the PR otherwise, and self-hosters would keep serving the old dashboard.
4. Open a PR with a short description of the *why*.

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

Tests use real loopback HTTP servers (`ThreadingHTTPServer`) and real SQLite (`:memory:`) with
injected fetchers / subprocess runners — no HTTP mocking, no real printers. Match that style.
