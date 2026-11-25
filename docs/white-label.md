# White-label builds of the setup program

An IT provider who runs printpapi for one identified client can build the setup and tray program
under their own name: a compile-time **brand**, one folder. The program then carries the provider's
name, icon and tray images, and optionally a default (or fixed) server address and a support link.
Nothing changes on the server, in `agent.ini`, in the database, in the HTTP API or in the legacy Python
agent.

This is for the permission at the top of [LICENSE](../LICENSE): you may operate the software on behalf
of an identified third party, for that client's own printing, and you do not offer it to the general
public or to an open-ended set of customers as a product of your own. Read the licence itself; this
page is not legal advice. A brand cannot remove, hide or replace the licence notices (see
[What stays visible](#what-stays-visible)), and there is no switch for that.

Without `PRINTPAPI_BRAND_DIR` the program is the stock setup, as it always was.

- [The brand folder](#the-brand-folder)
- [Building](#building)
- [What a person sees](#what-a-person-sees)
- [What stays visible](#what-stays-visible)
- [What is not renamed](#what-is-not-renamed)
- [Fixed server and support link](#fixed-server-and-support-link)
- [One brand does not replace another](#one-brand-does-not-replace-another)
- [Updates and pairing links](#updates-and-pairing-links)
- [Packages](#packages)
- [Before you ship](#before-you-ship)
- [Not checked on a real machine](#not-checked-on-a-real-machine)

## The brand folder

Set `PRINTPAPI_BRAND_DIR` to the **absolute** path of a folder. A relative path is a build error, because
the folder would otherwise be looked for in one place by this variable and in another by `--config`.
The folder holds:

| File | Rules |
|---|---|
| `brand.json` | the fields below; a key that is not listed is an error |
| `icon.png` | square PNG, 256 to 1024 pixels, at most 256 KiB: the logo in the window's sidebar |
| `tray-online.png`, `tray-offline.png`, `tray-stopped.png` | PNG, exactly 64 x 64 pixels: the tray icon for a connected, an offline and a stopped agent. Keep them different in colour **and** in shape (a badge), and the menu and tooltip say the state in words as well |
| `tauri.conf.json` | a patch for the Tauri CLI, see below |

`brand.json`:

| Field | Rules |
|---|---|
| `id` | required. `^[a-z][a-z0-9-]{1,30}$`, not `printpapi`, not `stock`, and not a word that looks like a link scheme (`http`, `https`, `file`, `ftp`, `mailto`, `tel`, `ssh`, `data`, `blob`, `javascript`, `ws`, `wss`, `tauri`, `ipc`, `asset`). It is written into the marker file of every install, so choose it once |
| `name` | required. The product name: ASCII letters, digits, space, dot and hyphen, a letter first, no space at the end, at most 20 characters. 20 is what is left in the 127 characters of a Windows tray tooltip next to the status line. ASCII also makes it a valid Debian package name and safe in a menu entry that root writes |
| `client` | required. The identified third party the build runs for, 1 to 64 characters. It is shown in About |
| `server` | optional. A default server address, `http://` or `https://` with a host, in ASCII (a name with an umlaut goes in as punycode, `xn--...`), at most 300 characters, and without `@`, `?`, `#` and `%`: the rule that a pairing link and the address field apply to every server. The build also wants a closed bracket around an IPv6 address, name characters (letters, digits, `.`, `-`, `_`) in the host and a port of digits up to 65535. A host that does not exist or a mistyped name is only noticed when the window connects |
| `server_fixed` | optional, `true` or `false` (default `false`); needs `server` |
| `support_url` | optional, `https://` only |

There is no field for a notice, a licence text, a "powered by" line, a footer or an update source.

`tauri.conf.json` is a JSON merge patch that the Tauri CLI lays over `crates/setup/tauri.conf.json`
([configuration files](https://v2.tauri.app/develop/configuration-files/)). It may hold only:

- `productName`: required, equal to `name`, and not `printpapi Agent Setup`;
- `bundle.icon`: required, the files that `cargo tauri icon` makes (see
  [icons](https://v2.tauri.app/develop/icons/)), each must exist and one must be an `.ico` (Windows takes
  the program's icon from it). A relative path is relative to `crates/setup`; a folder outside the
  repository needs absolute paths;
- `bundle.publisher`, `bundle.copyright`, `bundle.shortDescription`, `bundle.longDescription`;
- `bundle.linux.deb.replaces` and `bundle.linux.deb.conflicts`: required, each exactly
  `["printpapi-agent-setup"]`, see [Packages](#packages).

Everything else is refused: `identifier`, `mainBinaryName`, `build`, `app` (arrays in a merge patch replace the
whole array, so the window's settings cannot be patched; the title is set when the program runs),
`plugins`, `bundle.targets` and every other `bundle` key.

The repository has two folders to copy from: `crates/setup/brands/example` (a valid brand with solid
test images) and `crates/setup/brands/invalid-example` (a deliberately broken one that CI builds to see
the build fail).

## Building

Prerequisites are those of the stock setup: Rust, Node.js, the Tauri system libraries on Linux, and
`tauri-cli` (`cargo install tauri-cli --version 2.12.6 --locked`). From the repository root:

```bash
cargo build --release -p printpapi-agent       # the setup embeds the agent
(cd web && npm ci && npm run build:setup)      # and the window's pages
```

Then, in `crates/setup`, name the folder **and** pass its patch. The two belong together: Tauri takes
the product name and the icons from `TAURI_CONFIG`, which the CLI sets from `--config`, and the build script
checks both against each other.

bash (Git Bash on Windows works):

```bash
PRINTPAPI_BRAND_DIR=/abs/dir cargo tauri build --no-bundle --config /abs/dir/tauri.conf.json          # the program
PRINTPAPI_BRAND_DIR=/abs/dir cargo tauri build --bundles deb --config /abs/dir/tauri.conf.json       # Linux .deb
PRINTPAPI_BRAND_DIR=/abs/dir cargo tauri build --bundles appimage --config /abs/dir/tauri.conf.json  # Linux AppImage
PRINTPAPI_BRAND_DIR=/abs/dir cargo tauri build --bundles app,dmg --config /abs/dir/tauri.conf.json   # macOS
```

PowerShell:

```powershell
$env:PRINTPAPI_BRAND_DIR = "C:\abs\dir"
cargo tauri build --no-bundle --config C:\abs\dir\tauri.conf.json
```

Plain `cargo` (the tauri-cli documents `TAURI_CONFIG` as the variable to set yourself when compiling with cargo
directly, [environment variables](https://github.com/tauri-apps/tauri/blob/dev/crates/tauri-cli/ENVIRONMENT_VARIABLES.md)):

```bash
PRINTPAPI_BRAND_DIR=/abs/dir TAURI_CONFIG="$(cat /abs/dir/tauri.conf.json)" cargo build --release
```

Back to a stock build, from the same shell. A shell that still has `PRINTPAPI_BRAND_DIR` set builds the
brand again (or stops because `TAURI_CONFIG` is missing), and one that still has a brand's `TAURI_CONFIG`
stops with a message that names `PRINTPAPI_BRAND_DIR`. Unset both first:

bash:

```bash
unset PRINTPAPI_BRAND_DIR TAURI_CONFIG
cargo tauri build --no-bundle      # or plain: cargo build --release
```

PowerShell:

```powershell
Remove-Item Env:PRINTPAPI_BRAND_DIR, Env:TAURI_CONFIG -ErrorAction SilentlyContinue
cargo tauri build --no-bundle      # or plain: cargo build --release
```

Plain `cargo` for a brand in PowerShell (`-Encoding utf8` matters: Windows PowerShell 5.1 reads a file
without a byte order mark in the ANSI code page and garbles a publisher or copyright with a non-ASCII
letter):

```powershell
$env:PRINTPAPI_BRAND_DIR = "C:\abs\dir"
$env:TAURI_CONFIG = Get-Content -Raw -Encoding utf8 C:\abs\dir\tauri.conf.json
cargo build --release
```

The build script checks everything at once and stops with every fault it finds. It fails in both
directions of a mismatch: a brand folder without the matching `TAURI_CONFIG` (the program would carry
a brand's text in a stock-looking executable) and a `TAURI_CONFIG` with a `productName` or `bundle.icon`
but no brand folder (a stock-behaving build under a brand's name and icon). The stock release builds
set neither variable and are not affected.

**Not supported in this version: a branded .msi and a branded .rpm.** Do not ship them. The build
script refuses only the Tauri config keys such a package needs in a brand's patch (`bundle.windows`,
`bundle.linux.rpm`); it does **not** stop `--bundles msi` or `--bundles rpm`. An `.rpm` built that way
carries the program under the stock file name and has no relation to the stock package, and an `.msi`
built from the brand patch alone lacks the managed-install hook (it installs no agent and ignores
`AGENT_INI`). Branded `.exe`, `.deb`, AppImage and `.dmg` are the supported forms. On a machine with the
stock `.rpm` (Fedora, openSUSE) remove it first with the package manager (`sudo dnf remove` or
`sudo zypper remove`, with the package name `rpm -q` reports for it), then run the branded AppImage or the
plain program; there is no branded `.rpm` to install.

## What a person sees

Only in a branded build:

- The window title and the box that says why the program cannot start carry `name`.
- The sidebar shows `icon.png` and `name` instead of the printpapi mark. A name of 20 letters in one word
  wraps inside the sidebar.
- The sentence "Which printers should <name> use?" uses the name.
- The tray tooltip starts with the name; the three tray images are the brand's; the menu has a disabled
  line "Based on printpapi, Elastic License 2.0 (modified)" and, with `support_url`, **Support...**.
- On Linux the application menu entry and the autostart entry are called `name`.
- The first screen of the window is shown only after the page has drawn the brand, so the
  standard printpapi page is never seen first. If that does not happen within 5 seconds the window is
  shown anyway.
- There is no **Updates** section, no update line in the overview and no **Updates...** item in the tray
  (see [Updates and pairing links](#updates-and-pairing-links)).

The stock program looks and behaves as before.

## What stays visible

These are written by the program, not by the brand, and a brand cannot change them (LICENSE: the
licensor's notices are not removed or obscured, recipients get the terms, a modified copy carries a
prominent notice):

1. A footer in the sidebar on every screen: "Modified build of printpapi <version>, (c) 2026 Lukas
   Wotzka, source available under the Elastic License 2.0. Operated for <client>."
2. The footer opens **About**: the notice, the client, the support button (if there is one) and the
   whole text of LICENSE.
3. The word "Modified" in both, and the tray menu line named above.

## What is not renamed

The names that the system and the installed agent use stay the stock ones on purpose: the scheduled
task, service, launchd labels, install folder, marker file name, the autostart (run) value, the macOS
app path, the tray's lock and log files, the Tauri identifier (which also names the web view's
profile folder), the program's file name `printpapi-setup` and the staging folder prefix. A client who looks in the
Applications folder or the list of scheduled tasks sees the real names. Changing them would break
detection of an install, removal and the link between the window and the tray.

## Fixed server and support link

With `server` the first step starts with that address. With `server_fixed` the window connects to that server
only: the address field is read-only with a line that says so, and the program refuses any other
address in the key check, in a pairing and in the install itself (error code `server_fixed`: "This setup
connects to <server> only."). The addresses are compared after the same cleaning the agent applies
(spaces and trailing slashes) and in lower case, so a capital letter in a host never matters. A
pairing link for another server is refused, a bare code uses the fixed server.

This is a convenience for the person at the keyboard, not a security control: `agent.ini` is editable by an
administrator, and a fixed server does not rewrite an install that already points elsewhere. The
**Change server or key** step of such an install starts with the fixed server, so it can never be stuck
on a read-only wrong value.

`support_url` opens in the default browser, `https` only, through the same address rule as the tray's
**Open dashboard**.

## One brand does not replace another

The marker file `.installed-by-setup` names who installed the agent. It holds `version=` and, for a brand, a
`brand=<id>` line (a stock install has no `brand` line). The elevated install reads it before it does
anything else:

| Installed by | This setup | Result |
|---|---|---|
| stock | stock | installs |
| stock | brand | installs, and the marker gets the brand (the brand takes the install over) |
| brand A | brand A | installs |
| brand A | stock | refused, `other_brand`, nothing is changed |
| brand A | brand B | refused, `other_brand` |
| a marker nobody can read | any | refused, `other_brand` |

Removing and restarting never ask: a machine can always be cleaned, whatever the marker says.

The window shows the two cases that matter:

- **other_brand**: the agent was set up by another white-label build, or its marker cannot be read. A
  notice and the **Remove** section only. Remove asks for administrator rights
  and a confirmation as always and works with any marker, also after a failed take-over or when the tray
  program is gone.
- **adoptable** (a branded window over a standard install): a notice and one **Take over** button,
  which installs the agent again without changing any setting. The install rewrites the tray, the menu
  entry and the marker; `agent.ini` stays. Any other change made in the branded window (printers, the
  connection, Advanced, SumatraPDF) takes the install over the same way, so the offer disappears after
  the first change of any kind. The notice stays on screen until the run is over, so that the wait for
  the agent, the log path when it does not report in, and a failure are still shown.

The branded window tells the elevated install which brand it is through `brand.json` (id and name) in
the staging folder. That folder can be written by the user who started the setup, so this is
protection against accidents, not a security boundary: whoever can forge the staging folder already
gets an administrator-run executable (the setup is not code-signed yet).

**A machine set up with the stock .msi.** The .msi is a stock package, and a brand has none. If the
machine was installed through it (`AGENT_INI`) and the branded window then took the agent over, the
.msi still lists itself in the system and still has its flag file. Two things follow, and both are
deliberate: the next stock .msi that is pushed without `AGENT_INI` (an upgrade of the program and
the tray) leaves the agent alone and logs "set up by another build" in `printpapi-managed.log`
instead of failing with 1603, so that agent is updated only by running the new brand setup. An
upgrade pushed with `AGENT_INI`, the command line of the first deployment, installs the agent again
from that file; the install refuses to replace a brand's agent (`other_brand`), so the .msi ends with
1603 and that line in the log, on every retry, until the deployment no longer pushes it. Removing the
old stock .msi removes the program and the flag, not the branded agent. To remove the agent use the
window's **Remove** section.

**A limit you should know.** Released stock setups (3.3 to 3.6) only check that the marker file
exists and will replace a branded install. Take over (run the brand setup again) puts it back. If those
old downloads circulate at your client, a separate marker file name per brand would stop them; that is
a known alternative and it is not built.

## Updates and pairing links

- **Updates.** A branded build never checks for, downloads or installs updates: the Updates section,
  the overview line, the tray item and the hourly look are gone, `update_info` says it is not supported, and
  the agent refuses to update a branded install (code `manual`; a nightly job that is still there ends
  without doing anything, and the install that takes the machine over, or a restart from the window,
  removes the schedule). The update keys and release folder are the stock build's, and a brand has no
  update source of its own (the `plugins` key, where an updater lives, is refused). To roll out a new
  version, build the brand again from a newer printpapi release (the version comes from
  `crates/setup/Cargo.toml`, not from the brand) and run it on each machine: the window shows "This program (...) is newer than the installed agent"
  with the button **Install ... from this setup**, and the brand check above lets it replace its own
  install. The button appears only when the version number is higher. A rebuild of the same version
  (a new icon, support link or server) is put in place by any applied change, for example **Change
  printers**, walked to the last step without changing a printer.
  A branded self-update would need a payload that carries a signed brand id for the elevated side to
  compare, which does not exist.
- **Pairing links.** `printpapi` is the one link scheme and it is never branded: the install registers it for
  every build with a tray (the server and dashboard are not branded and make `printpapi://` links). A
  link or code works as in the stock build, and its server goes through the fixed-server check before any
  prompt.

## Packages

A brand's `.deb` gets a package name from the product name but ships the same `/usr/bin/printpapi-setup`
and the same icon files as the stock package (the program's file name must not change), so dpkg
refuses to install it over the stock package unless it declares `Replaces`
([Debian Policy 7.6.1](https://www.debian.org/doc/debian-policy/ch-relationships.html)). That is what
the required `bundle.linux.deb.replaces` and `conflicts` entries are for. The result: the brand's package
installs over the stock package (the same take-over as above), and the stock package over the brand's
is refused by dpkg, which fits the overwrite guard. The agent in `/opt` is part of neither package.

**Checked with dpkg** in a Debian 13 (trixie) Docker container, with the two packages that
`cargo tauri build --bundles deb` made from this repository, once without a brand and once with
`crates/setup/brands/example`. The stock package is `printpapi-agent-setup`; the brand's is
`example-print` (the kebab case of "Example Print") and carries `Conflicts: printpapi-agent-setup` and
`Replaces: printpapi-agent-setup`. Both contain `usr/bin/printpapi-setup` and the three
`printpapi-setup.png` icons.

| Step | dpkg said | Exit |
|---|---|---|
| `dpkg -i` stock package on a clean system | installed | 0 |
| `dpkg -i` brand package over the stock one | "will remove printpapi-agent-setup in favour of example-print", the brand is installed, the stock package is gone | 0 |
| `dpkg -i` stock package over the brand's | "example-print conflicts with printpapi-agent-setup ... not installing printpapi-agent-setup"; the brand stays | 1 |
| remove both, brand package, then stock package | the second is refused the same way | 1 |

So the stock package never replaces a brand's. What was not checked: `apt` and `dpkg` on Ubuntu
or older Debian, and what a person sees on a desktop after the take-over.

## Before you ship

Run the tests with your brand, from `crates/setup`, with `PRINTPAPI_BRAND_DIR` and `TAURI_CONFIG` set as above:

```bash
cargo test --locked
cargo clippy --locked -- -D warnings
```

They decode your icon and the three tray images, check the notice and the embedded licence text, and
confirm that updates are off. One test, `example_brand_is_compiled_in`, belongs to the example brand and
returns at once for yours.

## Not checked on a real machine

Hand checks that need real systems: the macOS bundle and its name in Finder; the AppImage and `.dmg`;
the Windows executable's Details tab (product name and icon); the window that is created hidden and
then shown, on a real WebView2 and WebKit; the footer, About, the tray menu lines and a 20-letter
single-word name in the sidebar; an old stock setup (3.3 to 3.6) over a branded install, which is
expected to replace it.
