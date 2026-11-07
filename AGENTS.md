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
- **Licensing.** New Rust files start with
  `// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).`
  `integrations/woocommerce/` is GPL-2.0-or-later. Never re-license a file without reading
  `LICENSE`. The project is "source available", never "open source".

## Commands

```bash
cargo test --workspace          # from the repo root
cd crates/setup && cargo test   # the setup app, not a workspace member
python -m pytest agent/tests    # legacy Python agent
cd web && npm run build:app     # after any web/ change; commit crates/server/web with it
```

## Hard-won pitfalls

1. Never send a PDF to a label printer: it prints blank labels. Send ZPL/ESC-POS or render first.
2. Smart App Control/WDAC blocks the unsigned agent exe. There, the legacy Python agent
   (`agent/`) runs under the signed Python interpreter.
3. A WAF/Cloudflare 403s a bare HTTP client's User-Agent. Outbound fetches send a browser UA.

## Server (`crates/server`)

- Few well-known crates, no web framework (`hyper` directly). A new dependency needs a reason.
- Pure modules (`dispatch`, `label`, `orders`, `packing_slip`, `printnode`, `billing`,
  `cloudprnt`, `auth`) do no IO and take injected fetchers. `store` (SQLite), `server`, `http`,
  `web` and `mail` own the IO. Keep that split.
- Tests in `crates/server/tests/` use real loopback HTTP and in-memory SQLite with injected
  fetchers: no HTTP mocks, no printers.
- Agents only ever connect outbound (long-poll) because they sit behind NAT. Design new features
  around that. No platform assumptions in the server.
- `crates/server/web/` is the built dashboard, embedded at compile time. Never edit it by hand.
- `python tools/parity/parity.py --rust target/release/printpapi.exe` compares behaviour with the
  v2.5.1 Python server (it extracts that server from the `v2.5.1` tag).

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
- The agent writes `status.json` (never the key, never a header value) into the status folder
  (`status::status_dir_in`) only when the `.installed-by-setup` marker sits next to it. It is what
  the tray icon reads: it opens no port and takes no commands. Keep secrets out of it, and keep
  its test for that.

## Setup (`crates/setup`)

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
- `web/lib/setup-downloads.json` is the one list of setup files. The downloads page and
  `release.yml` both read it; a variant nobody has run by hand is `"beta": true`.

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

## Maintainer-local files

`HANDOFF.md` (origin story, history) and `docs/superpowers/` (plans, specs) exist only on the
maintainer's disk and are gitignored. Never `git add` them and never link to them from tracked
files.
