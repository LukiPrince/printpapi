# printpapi

Self-hosted print bridge, source available under the Elastic License 2.0. Clients POST print jobs
to the server's HTTP API; an agent next to the printers polls the server outbound and prints them,
documents and label printers alike. Server and agent are Rust, the dashboard is a Next.js static
export embedded into the server binary.

## Rules for every change

- **TDD.** Failing test first, then the minimal code. Every non-trivial branch keeps a test.
- **YAGNI.** Stdlib before a dependency, native before custom, one line before fifty. Mark a
  deliberate shortcut with a `// ponytail:` comment that names its ceiling.
- **Security at trust boundaries stays:** auth on every endpoint, constant-time token compare,
  `http(s)`-only fetches, no shell with user input, temp files cleaned up.
- **Docs follow the code.** Before a push, compare `README.md` and `docs/` with the change.
  `tools/githooks/pre-push` runs `check_docs` on the commit at the tip of each pushed ref (not
  the commits in between: pull requests are squash-merged) and, in Claude Code (`CLAUDECODE=1`),
  holds the push until `python tools/docs/push_hook.py --mark`. The hook runs only once
  `git config core.hooksPath tools/githooks` is set in that clone. CI's `docs` job runs the same
  check on its checked-out commit: relative links and anchors that resolve to files in git, setup
  file names, source-available wording (never "open source").
- **Licensing.** New Rust files start with
  `// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).`
  `integrations/woocommerce/` is GPL-2.0-or-later. Never re-license a file without reading
  `LICENSE`. The project is "source available", never "open source".

## Commands

```bash
cargo test --workspace          # from the repo root
cd crates/setup && cargo test   # the setup app, not a workspace member
python -m pytest agent/tests    # legacy Python agent
git config core.hooksPath tools/githooks   # once per clone: docs check before every push
python -m pytest tools/docs
python -m pytest tools/release tools/smoke   # the signed update file's script, the smoke helpers
python tools/docs/check_docs.py   # docs at HEAD: links and anchors to files in git, setup file names, source-available wording
cd web && npm run build:app     # after any web/ change; commit crates/server/web with it
cargo build -p printpapi-server && cd clients/js && npm ci && npm test   # the JS client against the real binary (Node 20 or newer)
```

## Hard-won pitfalls

1. Never send a PDF to a label printer: it prints blank labels. Send ZPL/ESC-POS or render first.
2. Smart App Control/WDAC blocks the unsigned agent exe. There, the legacy Python agent
   (`agent/`) runs under the signed Python interpreter.
3. A WAF/Cloudflare 403s a bare HTTP client's User-Agent. Outbound fetches send a browser UA.

## Server (`crates/server`)

- Few well-known crates, no web framework (`hyper` directly). A new dependency needs a reason.
- Pure modules (`dispatch`, `label`, `receipt`, `orders`, `packing_slip`, `printnode`, `billing`,
  `cloudprnt`, `auth`) do no IO and take injected fetchers. `store` (SQLite), `server`, `http`,
  `web` and `mail` own the IO. Keep that split.
- Key rights live in one table, `auth::Scope::allows`; nothing else decides what a machine key may do
  (no scope lists in SQL). A new route names its `auth::Right` and gets a row in the matrix of
  `crates/server/tests/http_scopes.rs`.
- Tests in `crates/server/tests/` use real loopback HTTP and in-memory SQLite with injected
  fetchers: no HTTP mocks, no printers.
- Agents only ever connect outbound (long-poll) because they sit behind NAT. Design new features
  around that. No platform assumptions in the server.
- `crates/server/web/` is the built dashboard, embedded at compile time. Never edit it by hand.
- Receipts (`receipt`, docs/receipts.md): `crates/server/assets/terminus/` holds the Terminus Font files
  byte for byte (SIL OFL 1.1, `-text` in `.gitattributes`, with `web/public/licenses/`): never edit or
  re-encode them. A receipt profile has no `pdf` output (a raw printer never gets a PDF), and no
  `job_receipts` row is written while `PRINTAPI_OPERATOR_TOKEN` is set. The goldens in
  `crates/server/tests/fixtures/receipts/` and `web/demo/static/receipt-sample-*.png` are rewritten
  with `UPDATE_GOLDEN=1 cargo test -p printpapi-server --test receipt_golden`.
- `python tools/parity/parity.py --rust target/release/printpapi.exe` compares behaviour with the
  v2.5.1 Python server (it extracts that server from the `v2.5.1` tag).

- **Pairing** (`pairing_codes`, docs/api.md "Pairing a device"): codes are stored hashed and work once;
  `POST /agent/pair` is throttled in its own `pair:` slot and a taken name keeps the slot. A key that may take
  a device over registers **without touching** it (key hash, `last_seen_at`, printer flags stay, new printers are
  `pending`); the swap happens at the first poll, through `store::authenticate_poll`, and needs the agent right.
  Keep the tests for a stolen or replayed code, a narrowed or revoked key, a job in flight and two devices with one name.

- **A new or changed public route updates `crates/server/openapi.json` in the same change**, with its
  `x-roles` and `x-rights`, a first description sentence that says who may call it, the scope words of the
  keys that may (in `security`, checked against `Scope::allows`), then `cd clients/js && npm run gen-types`.
  `crates/server/tests/openapi.rs` fails for a route the router answers and the file does not list (a
  pinned list: any line that reads the request path must be documented or pinned there, and the
  dispatchers may use their request target only in the forms the scan reads: `t == "/x"`,
  `id_of(t, "/p/", "/s")` with two literals and `match t` with literal arms only. A call such as
  `self.helper(t)` or `matches!(t, ..)`, a constant or binding arm, a guard, or a path built by
  `format!` is not read: the first kinds fail until documented or pinned, the last one only test A
  covers), and for a documented route the router lacks.
  Response objects stay closed (`additionalProperties: false`). `info.version` of the document is its
  own version, bumped by hand when the document changes; it is not the server's, and the unauthenticated
  document never carries the server version.

## Agent (`crates/agent`, `agent/`)

- The platform-specific part. Windows: the spooler API directly, SumatraPDF for PDFs.
  Linux/macOS: CUPS `lp`. Config is `agent.ini` next to the executable. It never listens on a port.
- PDFs only go to printers marked `|pdf`; everything else gets raw bytes (ZPL/ESC-POS).
- The `orientation` option is SumatraPDF's `portrait`/`landscape` and nothing on CUPS. Don't add
  `disable-auto-rotation` (next to `landscape` it turns a wide page 90 degrees) or
  `orientation-requested` (`=4` prints upside down), and don't let the server pick a value: which
  one is right depends on the driver.
- `agent/print_agent.py` is the legacy Python agent for locked-down Windows, kept until the Rust
  exe is code-signed. Keep its tests green.
- The `install`, `uninstall`, `read-config` and `restart` subcommands (what the setup app calls)
  run as admin. They never trust the staging folder, which any user can write: absolute program paths only,
  the key never in argv or output. Test them with a fake `Runner`, never against the real
  scheduler, systemd or launchd.
- The agent key at rest (`keystore.rs`): on Windows a setup install stores it as `api_key_dpapi`
  (DPAPI machine scope, SHA-256 in front). A non-empty plain `api_key` always wins, `seal_key = no`
  opts out, only `install` seals (the agent never writes `agent.ini`), and a key that cannot be
  read makes a setup-installed agent wait and report `key_unreadable` instead of exiting.
  `read-config` exports `[headers]` only, never the key or the blob.
- **Signed updates** (`crates/agent/src/update.rs`, the module doc has the steps). The elevated
  program is the installed agent, never a file from staging: it reads each file once into memory,
  checks those bytes and writes only those bytes, and an update never writes `agent.ini`, the unit,
  the rights or the status folder. The public keys are in `crates/agent/update-keys.txt` (committed,
  one hex key per line, built in with `include_str!`): no environment variable, no runtime override.
  The Cargo feature `update-test-key` adds the public half of the committed test seed and shortens the
  health wait; only the CI smoke build enables it, `release.yml` never does. Do not commit a private key.
- **A new admin command line is at least three words long, the known first word included**
  (`restart --update-from <dir>`, `restart --update now`). Every released agent (3.3.0 to 3.6.0) answers a known first word with exactly
  three arguments with usage and exit 2 and does nothing, so a new form that is two or four words
  would run as something else there. A command a person types may be a new first word
  (`unprotect-key`, `--version`); an older agent just starts. Any other unknown first word keeps
  running the agent.
- **Frozen from the first release that has them** (an older updater on an installed machine relies
  on them; a mistake cannot be fixed there): the format of `printpapi-update.txt` and schema 1;
  `--version` of both programs (`printpapi-agent N.N.N`, `printpapi-setup N.N.N`); the `status.json`
  fields `version`, `state`, `started_at`, `last_contact`, `busy`, `last_job.at`; the marker
  `.installed-by-setup` as UTF-8 `key=value` lines (`version=`, `brand=`; unknown lines are kept when
  the version is rewritten); and that a new agent runs with the existing unit, `agent.ini`, rights
  and paths, else the schema is raised and the setup is needed.
- **The print journal** (`crates/agent/src/journal.rs`, docs/agent.md#the-print-journal) never holds the key, a
  payload, a title or a header value, and its error texts are masked and cut. It only records a job
  of which at least one copy was handed over (`print_copies`); in any doubt (no identity, an unknown uid, a
  journal that cannot be read, an acknowledged failure) the agent **prints**: a duplicate is better
  than a label that is silently missing. Opening never rewrites the file; only compaction does.
- **Pairing on the agent side.** `pair::parse` is the only parser of a `printpapi://` link and `pair::valid_server`
  the only rule for an address a link or the window names; nobody parses a link twice. `printpapi-agent pair`
  never takes the code as an argument. Installed `[headers]` belong to the origin they were set for:
  `config::apply_patch` drops them for a connection to another origin unless the patch says `keep_headers`.
- The agent writes `status.json` (never the key, never a header value) into the status folder
  (`status::status_dir_in`) only when the `.installed-by-setup` marker sits next to it. It is what
  the tray icon reads: it opens no port and takes no commands. Keep secrets out of it, and keep
  its test for that.

## Setup (`crates/setup`)

- A key a pairing code made stays in `AppState::paired`, in Rust: never in the window, an argument, a log or
  `result.json`. The window names the connection by address and `fill_paired` supplies the key.

- Tauri v2 shell around the agent, **outside the workspace** (it needs WebView and system
  libraries, so `cargo test --workspace` never builds it). Test it from `crates/setup`. All logic
  that can be tested lives in `crates/agent`; keep the shell thin.
- Its window is the third Next build, `npm run build:setup` (`web/out-setup`), embedded at compile
  time. It never talks to a local server, and `npm run check:bundles` keeps its code out of the
  dashboard and demo.
- `--tray` is the second mode of the same program: the icon and its menu (`src/tray.rs`), with
  the settings window (`web/setup/settings*.tsx`) behind "Settings...". A change from the window
  stages only the changed fields (`patch.json`) and goes through the admin subcommands, so it asks
  for elevation once. "Quit" ends the icon, never the agent.
- `fn mode(args: &[OsString]) -> Mode` in `crates/setup/src/main.rs` is the one parser of the program's
  arguments: exactly `--version` prints the version before anything else, an argument that starts
  with `--managed-` is a headless installer mode (before the tray lock and the web view check), any
  argument `--tray` is the icon, `--section <word>` opens a window section. The updater starts the new program with
  `--version` before it swaps anything, so keep that path free of locks, web view and windows.
- `--managed-install`, `--managed-refresh` and `--managed-uninstall` (`src/managed.rs`) are the headless
  Windows modes the .msi runs as LocalSystem; other systems print the usage and exit 2. The logic runs
  against a `Host` trait, so its tests use a fake machine. The key never reaches argv, a log line or an
  error text; keep its test. As LocalSystem they first replace the environment (Windows Installer hands
  over the starter's) and work in LocalSystem's own profile folder, written out (`src/winenv.rs`,
  `staging::private_root`): nothing SYSTEM runs or reads may lie in the starter's profile. The .msi hook is `wix/managed.wxs`, written against the template of
  tauri-cli 2.12.6 (pinned in ci.yml and release.yml): read the template again before changing the pin.
  MSI-only Tauri settings live in `tauri.msi.conf.json`, never in `tauri.conf.json`.
- White-label builds: `PRINTPAPI_BRAND_DIR` (absolute path of a brand folder) plus its `tauri.conf.json` as
  `TAURI_CONFIG`. The validator is `crates/agent/src/brand.rs`, shared with `build.rs` through
  `#[path]`, so it uses `std` and `serde_json` only. The build fails on a brand without its config and on
  a brand config without a brand. The licence notice, About and the tray line are written by the code, a
  brand has no field for them, and every brand has `updates_enabled` false. The marker
  `.installed-by-setup` carries `brand=<id>`; `marker::may_replace` guards the elevated install
  (`other_brand`). `stage_own_tray` also stages `brand.json`. The names in install.rs (task,
  service, labels, marker, paths, identifier, bin name) never change for a brand. After a brand change,
  run `cargo test` and `cargo clippy` in `crates/setup` with the example brand (CI does:
  `docs/white-label.md`). The `brand` window code is `web/setup/brand.tsx`.
- `web/lib/setup-downloads.json` is the one list of setup files. The downloads page and
  `release.yml` both read it; a variant nobody has run by hand is `"beta": true`.
  `tools/docs/test_release_assets.py` keeps the list a subset of what `release.yml` builds. The .msi
  and .rpm are built after the existing installers are uploaded, with `continue-on-error`, and ship
  in the second artifact `setup-extra-<name>`.

## Dashboard (`web/`)

- Next.js App Router, Tailwind v4, shadcn/ui and motion, built as a static export. `npm run
  build:app` builds it and copies the export into `crates/server/web`. Commit both, or CI fails.
- Every API call goes through `web/lib/api.ts` with root-relative paths (same origin as the
  server). `next dev` proxies them to `PRINTPAPI_ORIGIN` (default `http://127.0.0.1:3460`).
- Job states are always colour + icon + word (`web/lib/job-state.tsx`), never colour alone.
- The label-size catalogue of the `/labels` page is `web/lib/label-sizes.ts`; each manufacturer
  size cites its source there, so a new size needs one too.
- Checks: `npm run lint`, `npm test`.
- Demo build: `npm run build:demo` writes `web/out-demo/`, which `.github/workflows/demo.yml`
  deploys to printpapi.lukaswotzka.com; it is never synced into the binary. Demo code lives only
  in `web/demo/`. Product files reach it only through the `printpapi-demo` module, which resolves
  to the stub `web/demo/off.ts` in the normal build. `npm run check:bundles` must pass.

## Client (`clients/js`)

- `@printpapi/client` is a placeholder name; the maintainer decides the package name and the first
  publication. `.github/workflows/publish-client.yml` is started by hand, never by a push, and was never
  run. Nothing in the repository runs `npm publish`.
- ESM, `fetch` only, **no runtime dependency**; devDependencies are `typescript` and `@types/node` and
  nothing else. `src/` builds with no Node types (`tsconfig.json`, `types: []`), so it stays usable in a
  browser; the tests and `testkit/` use Node (`tsconfig.test.json`).
- `src/types.ts` is generated from `crates/server/openapi.json` by `npm run gen-types`; never edit it. The
  tests run the real server binary (explicit environment, loopback port) and check what goes through the
  recorder, request and answer, against the document. The server is never mocked; a throwaway server, a
  fault proxy or an injected fetch only stands in for what goes wrong (a page, a redirect, a lost answer,
  a clock).
- The key lives in a private field and in the `Authorization` header only: never in a URL, a message, a
  stack, a property or a log line (an error built from an answer that echoes it has it replaced by `[key]` in the decoded value; a success is never rewritten). Redirects are
  never followed. Keep the test for that.
- Anything that goes into a path is a whole number (`pathId`), a blank `idempotency_key` is no key, and a
  2xx that is not a JSON object with the field the call reads (`job_id`, a list, `state`) is an error of kind `server`. Keep those tests.
- A submit or orders call makes one idempotency key and every attempt reuses it; `err.idempotencyKey` hands
  it back. Never make a new key for a repeat.
- `clients/js/package.json` follows the workspace version (a Rust test enforces it) and is in the release
  bump list with `Cargo.toml`, `Cargo.lock`, `crates/setup/Cargo.toml`, `crates/setup/Cargo.lock`,
  `docs/operator.md` and `clients/js/package-lock.json`. `crates/server/openapi.json` is not.
- CI runs the client tests in the `parity` job (Node 22, then Node 20 for the built tests).

## Maintainer-local files

`HANDOFF.md` (origin story, history), `TODO.md` (open items) and `docs/plans/`, `docs/specs/` (plans, specs)
exist only on the maintainer's disk and are gitignored. Never `git add` them and never link to them from tracked
files.
