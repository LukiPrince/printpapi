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
  (`hmac.compare_digest`), hashed keys at rest, `http(s)`-only URL fetches, no `shell=True`.

## Workflow

1. Fork, branch from `main`.
2. Add a failing test, make it pass, keep the suite green.
3. Open a PR with a short description of the *why*.

## Test conventions

Tests use real loopback HTTP servers (`ThreadingHTTPServer`) and real SQLite (`:memory:`) with
injected fetchers / subprocess runners — no HTTP mocking, no real printers. Match that style.
