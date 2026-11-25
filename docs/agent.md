# Agent

The agent runs on the machine with the printers. It registers its printers with the server,
then long-polls for jobs, prints them, and reports the result. It makes **outbound connections
only** — nothing listens on the printer's machine, so NAT/firewalls are no problem.

## Setup app

The easiest way on Windows, macOS and Linux with a desktop: download `printpapi-setup-<platform>`
(`.exe` on Windows, `.dmg` on macOS, `.deb` or `.AppImage` on Linux) from the
[latest release](https://github.com/LukiPrince/printpapi/releases/latest), open it and go through
four steps (connect to the server with an agent key, pick the printers, test print, done).
It writes `agent.ini`, installs the agent to `%ProgramFiles%\printpapi-agent\` (Windows) or
`/opt/printpapi-agent/` (macOS, Linux) and starts it with the machine: a scheduled task
`printpapi-agent` on Windows, a launchd daemon `com.printpapi.agent` on macOS, a systemd unit
`printpapi-agent.service` (running as the system user `printpapi-agent`) on Linux. The folder and
the user are not the ones of a printpapi server on the same machine (`/opt/printpapi`, user
`printpapi`, see the [migration guide](migration.md)), so the two do not touch each other. Open the
setup again to change the printers or remove the agent; the removal deletes only what the setup
installed, and the folder only when nothing else is in it.

### Tray icon and settings

The setup also installs a small icon (next to the clock on Windows, in the menu bar on macOS, in
the status area on Linux) that starts with every user's login. Its colour, symbol and word show
the agent: connected, offline, or not running. Its menu has the status line, **Settings...**,
**Updates...** ([below](#updates)), **Open dashboard** and **Quit**; Quit ends only the icon, the
agent keeps printing.

**Settings...** opens a window with Overview, Printers (add, remove, change the kind, test print),
Connection (another server or key), Advanced (`timeout` and the `[headers]` values), Log,
SumatraPDF (Windows), Restart and Remove, and Updates after Overview. Looking needs no administrator rights; each change asks
for them once.

- The icon only reads `status.json`, which the agent rewrites every 30 seconds from a thread of
  its own, so it stays young while a long job prints or a connection hangs: Windows
  `%ProgramData%\printpapi-agent\`, macOS `/Library/Application Support/printpapi-agent/`, Linux
  `/var/lib/printpapi-agent/`. The file holds the server, device name, state, last contact and job, the
  printers, the header names, the time the agent process started, whether the stored key can be
  read, the update mode and mirror, and whether a job is being printed, never the key and never a
  header value. A file that is missing or older than two minutes shows "not running"; an agent that
  has not been in touch with the server for two minutes shows "offline" with the time it has been
  silent. The agent still opens no port and takes no commands.
  The crash log of an agent the setup installed lives in the same folder.
- A new key on the same server needs a new device name, because the server ties a name to its
  key. The printers then get new ids and the old device stays in the dashboard; the window warns
  before it does that. When the stored key cannot be read, the same key with the same name is
  accepted ([When the key cannot be read](#when-the-key-cannot-be-read)).
- On Linux the icon needs the library `libayatana-appindicator3` (`libayatana-appindicator3.so.1`;
  `libappindicator3` works too). On Debian and Ubuntu it is the package `libayatana-appindicator3-1`
  ([packages.debian.org](https://packages.debian.org/sid/libs/libayatana-appindicator3-1)).
  Without it the icon cannot be shown: it writes one line saying
  so to `printpapi-tray.log` (in `$XDG_RUNTIME_DIR/printpapi/`, else `~/.cache/printpapi/`) and ends
  quietly at every logon instead of crashing. The agent is not affected, and the application menu's
  **printpapi Agent** entry still opens the settings window.
- GNOME shows no tray icons without the
  [AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/).
  The application menu has a **printpapi Agent** entry that opens the settings window directly.
- Removing the agent removes the icon, its autostart entry and the status folder too (the files
  the agent wrote there; anything else in that folder stays).

- The setup is not code-signed yet, so the system warns first. Windows: *More info*, then *Run
  anyway*. macOS: System Settings, Privacy & Security, *Open Anyway*. Every setup download is marked
  Beta on the downloads page until someone has run it by hand.
- Smart App Control and WDAC block the unsigned exe with no way around it. Use the Python agent
  ([Locked-down Windows](#locked-down-windows)).
- An AppImage starts only once it is executable: run `chmod +x` on the file, or tick *Allow
  executing as program* in its properties.
- Linux without a desktop or without systemd: use the manual steps below.
- An agent you set up by hand is not taken over: the setup stops and says so, so a job is never
  printed twice.

## Updates

The agent and the tray icon / settings program of a setup install can update themselves. This
applies only to an agent the setup installed (the folder has the marker file
`.installed-by-setup`). An agent you set up by hand from an archive, the Python agent and the server
are never touched, and an agent from an archive makes no request for updates at all.

**What the person sees.** The tray menu has an **Updates...** item, which reads **Update to 3.7.0...**
while a newer version is known. It opens the settings window on its **Updates** section; the
Overview shows one line when a version is available. **Install 3.7.0** downloads the files, asks for
administrator rights once, replaces the agent and the setup program, checks that the new agent
connects to the server, and reopens the window as the new version. A new version that does not
connect is replaced by the old one automatically, and the window says so.

**Three modes**, in `agent.ini` under `[agent]` (the select in the window changes it for you and asks
for administrator rights once):

| `updates` | in the window | what happens |
|---|---|---|
| `notify` (default) | Tell me | the icon, and the window when it opens, look once a day and say when a version is out |
| `auto` | Install automatically | also installs it, at night, from a scheduled task (Windows), a systemd timer (Linux) or a launchd job (macOS) of its own that exists only in this mode |
| `off` | Off | no look, no request, no schedule |

Any other value counts as `off`, so a typo never switches unattended installs on. After a hand edit
of `updates`, restart the agent from the window (Restart), because that is when the schedule is
brought in line with the file.

**What is sent.** In `notify` and `auto` every signed-in user who has the icon makes one request a
day for one file, `printpapi-update.txt`, from the release page (GitHub answers it with redirects).
The window looks once when it opens (the answer is kept for a day, so it adds no second request) and
again when you press **Check now**. A file that is missing (404) or that this program refuses
(another key, a newer format) is remembered for a day, by the program that got that answer (a newer setup, installed by hand, asks again); the look is repeated hourly only while the
network fails. With `updates = off`, or an `update_url` the agent refuses, the icon and the window
make no request, show no notice and cannot start a download. Installing downloads the files named in it.
No version, device name or id is sent, only a browser's User-Agent; the agent's key and the
`[headers]` of `agent.ini` are never sent, because the address is not your printpapi server. The
unattended update asks the same way as the root or SYSTEM user, with the proxy settings of that
user. A company that wants none of this sets `updates = off`, or points `update_url` at its own
mirror (below), or builds the agent with its own keys.

**Mirror.** `update_url = https://mirror.example/printpapi` names a folder that holds the files of a
release exactly as they are on the release page (`printpapi-update.txt` and the `printpapi-update-*`
files). `http` is allowed, because the content is protected by a signature, not by the connection. An
address with a user name, a query or a fragment, or another scheme, switches checking off.

**What is checked.** The update is one file with an Ed25519 signature, `printpapi-update.txt`. It
lists the size and SHA-256 of every program file of the release. The public keys that may sign it
are in `crates/agent/update-keys.txt` and built into the agent; there is no setting and no
environment variable that changes them (see [SECURITY.md](../SECURITY.md#signed-updates)). TLS, the
host and a mirror are not trusted: a file that does not match, a version that is not newer than the
installed one, or a signature that no key verifies is refused before anything on the computer
changes. An agent that was built without a key (this is the case until the owner of a release adds
one) says "This build has no update key, so it cannot check for updates." and makes no request.

**What an install of an update does.** The installed agent, which only administrators can write,
reads each file once into memory, checks those bytes and starts the new agent and the new setup
program once with `--version`, so a program that cannot run here (Smart App Control, a wrong
architecture, a missing library) stops the update before anything is changed. It waits until the
agent is connected and not printing, swaps the two program files by renaming them, restarts the agent
the way the Restart section does, and waits up to five minutes for the new version to report in
twice with the same process. If it does not, the old files are renamed back (this needs no network)
and the old agent is started again. The result says which: "Version 3.7.0 did not connect, so 3.6.0
was put back. It is running again." Only the two program files and the marker change: `agent.ini`,
the service unit or task, the rights and the status folder stay as they were. The last attempt is
in `update-state.json` in the install folder (no secrets): `message` is the sentence the window shows,
`detail` the reason behind it (for example why the new agent did not connect), which the Windows
task and a click, both without a console, would otherwise lose. An unattended run gives a version up
after two failed nights; a click in the window always tries.

- The agent has to be connected: an agent that is stopped or offline is never updated, by click or at
  night ("The agent has to be connected, so that the update can check the new version. Otherwise run
  the setup of the new version."). A queue that is printing, or one long job (a slow printer can hold the agent busy for more than two
  minutes without a contact with the server), is waited for, up to two minutes.
- A job that was claimed between the last look and the stop is requeued and can print twice, as with
  every restart.
- The marker file now holds the installed version (`version=3.7.0`); an empty marker, which setups up
  to 3.6.0 wrote, still means "installed by the setup". A setup program that finds a newer agent than
  itself offers the installed version instead of its sections, and a deliberate **Go back to X**
  behind a confirmation. Going back needs `updates` on **Tell me** or **Off** first. The card does
  that for an automatic setting: it switches **Install automatically** to **Tell me**, so that the
  nightly update does not install the newer version again, and leaves **Tell me** and **Off** as they
  are. When the agent is stopped the window cannot read the mode, so the install reads `agent.ini`
  and lowers only an automatic setting; an `updates = off` stays off.
- Before you go back to 3.6.0 or older by other means, choose **Tell me** or **Off**. Otherwise the
  nightly entry stays behind, answers the command `restart --update now` with a usage error and does
  nothing; removing the agent then leaves the folder `.update` and `update-state.json`.
- A setup program from an older release cannot be stopped from putting an older agent back.
- The `.deb`, `.rpm` and `.msi` programs belong to the package manager or the installer, and an
  update never replaces them: it replaces the agent and the copy of the setup program in the install
  folder. A window started from such a program that is older than the installed agent shows only
  the card "A newer version is installed" (open the installed version, or go back on purpose). An AppImage stays an
  AppImage and an ELF program stays one; an AppImage is not started before it is put in place,
  because starting it as root needs FUSE.
- Every machine starts at the version its setup was. The first release that can be updated has to be
  installed by hand once; opening the newer setup offers **Install X from this setup** in one click.
- Only the server, its Docker image, the WooCommerce plugin and the Python agent are outside this.
  SumatraPDF is never updated.
- On the command line, an administrator can run `printpapi-agent restart --update now` (the look and
  install the nightly job does). It says nothing and does nothing, and exits 0, unless `updates = auto`
  with a valid folder, the agent is installed by the setup, connected, and a newer signed release is
  there; check `update-state.json` in the install folder for the last attempt. An older agent answers
  it with a usage line and does nothing.

## Install

The agent is a single Rust binary, `printpapi-agent`. No Python, no runtime.

1. Download the archive for the machine from the
   [latest release](https://github.com/LukiPrince/printpapi/releases/latest) and unpack it. Each
   archive holds the binary, `agent.ini.example` and `LICENSE`:
   - `printpapi-agent-windows-x86_64.zip`
   - `printpapi-agent-linux-x86_64.tar.gz`, `printpapi-agent-linux-aarch64.tar.gz`
   - `printpapi-agent-macos-aarch64.tar.gz` (Apple silicon), `printpapi-agent-macos-x86_64.tar.gz`
     (Intel)
2. Platform bits:
   - **Windows:** nothing for raw jobs — the agent talks to the spooler directly (no `pywin32`
     any more). For PDF printing, put `SumatraPDF.exe` next to `printpapi-agent.exe` (or on
     `PATH`) — it silent-prints through the installed driver.
   - **Linux:** CUPS (`lp` must work). Raw jobs go to the queue with `-o raw`; PDFs go through
     the CUPS filter chain.
   - **macOS:** same CUPS path as Linux, nothing to install (`lp` ships with the OS) — but read
     [macOS](#macos) below, raw printing has one setup trap.
3. Create `agent.ini` **next to the executable** (below; copy `agent.ini.example`). The agent finds
   it there whatever its working directory is.
4. Run `printpapi-agent` (`printpapi-agent.exe` on Windows; on Linux/macOS `chmod +x` it first if
   needed). It is a console program. For autostart see [Run as a service](#run-as-a-service).

On a locked-down Windows machine that blocks the exe, see [Locked-down Windows](#locked-down-windows).

Replacing a Python agent from v2? Keep its `agent.ini` (same `name` and `api_key`) so the machine
and its printer ids stay the same; the steps are in the [migration guide](migration.md#upgrade-the-agents).

## agent.ini

```ini
[agent]
server_url = http://yourserver:3460
api_key    = key-from-the-Keys-page
name       = office-pc
; printers: semicolon-separated. Append |pdf for document printers.
; A CUPS queue / Windows printer name, socket://IP:PORT for a raw network printer,
; or file:///path/to/dir to archive the job to disk instead of printing it.
printers   = Zebra GK420d ; HP LaserJet|pdf ; warehouse-label = socket://192.168.1.50:9100
```

### Optional: updates

`updates = notify` (default), `auto` or `off`, and `update_url` for a mirror: see [Updates](#updates).
Both are ignored by older agents and by the Python agent.

### Optional: timeout and extra headers

- `timeout = 60` in `[agent]` - socket timeout per request in seconds (default 60, minimum 30,
  because the server long-polls 25 s). Without it, a NAT flow that is silently dropped mid-poll
  would hang the agent forever while the dashboard already shows it offline.
- A `[headers]` section adds headers to every request. Use it when an auth proxy sits in front of
  the server's `/agent/` paths, for example a Cloudflare Access service-token policy:

  ```ini
  [headers]
  CF-Access-Client-Id     = <client id>
  CF-Access-Client-Secret = <client secret>
  ```

  `Authorization`, `User-Agent` and `Content-Type` are set by the agent and are refused here.

### Unattended machines

On start the agent registers until it succeeds (backoff 1 s … 5 min), so a machine that boots
before its network is up simply waits instead of exiting. Each failed register try, the first poll
error after a working stretch (and the matching "poll recovered"), a rejected `agent.ini` and every
crash are appended to `%LOCALAPPDATA%\print_agent-error.log` (Windows) or the temp dir - that file
is the first thing to read over remote desktop. (An agent the setup installed writes it next to
its `status.json` instead, see [Tray icon and settings](#tray-icon-and-settings), and the settings
window shows it under Log.)

`api_key` is a key issued for the agent — on the dashboard's **Keys** page or with
`POST /apikeys` — and the agent lands in that key's org. A made-up key is refused (`401`) since
v3; agents that enrolled with one under v2 keep working, and `PRINTAPI_OPEN_ENROLLMENT=1` on the
server restores the old rule. Issue one key per machine, with the rights **Agent only**: it can
enrol, poll, download and report its own jobs and list the org's computer names (the setup app
needs that), nothing else, so a lost machine cannot submit jobs or read the history. A key for printing, reading or Star printers is refused at enrolment, and
the setup app says so in step 1. Revoking the key on the Keys page stops that agent at once.

A machine that is already set up keeps its name and printer ids when you narrow its key (**Rights**
on the Keys page, or `PUT /apikeys/{id}`), so there is no need to enrol it again. Narrowing
Everything to Agent only interrupts nothing: the agent keeps its right. Only if you take the agent
right away (Print only, Read only, CloudPRNT only) does the computer stop at its next poll; a job it
has already been handed still finishes, and queued jobs wait until it has the right again or they
expire. A computer named `cloudprnt-...` (reserved for Star printers) can only be narrowed to a
right set that is not Agent only. See [Key rights](api.md#key-rights).

When exposing the server to agents outside your LAN, publishing only `/agent/*` (behind TLS) is
still the smallest surface.

### Where the key is stored

The agent key is the one secret on the machine. Where it lives depends on how the agent was
installed:

| Installed | The key is in `agent.ini` as | Who can read the file |
|---|---|---|
| Windows, with the setup app | `api_key_dpapi`: encrypted with DPAPI, machine scope (this version and later) | SYSTEM and Administrators |
| By hand on any system, and the Python agent | `api_key`, plain text | what you set; a manual install keeps its plain `api_key` and the agent never rewrites the file |
| Linux, with the setup app | `api_key`, plain text | `root` and the service user (`root:printpapi-agent`, mode 0640) |
| macOS, with the setup app | `api_key`, plain text | `root` (mode 0600) |

**What DPAPI does.** The setup stores the key with the Windows Data Protection API in machine
scope: the key is encrypted for this Windows installation. A copy of `agent.ini` that leaves the
machine by itself (a folder backup, a file collected by remote management, a mail to support, the
folder copied to a second PC) holds no usable key, and two PCs cannot end up fighting over one
device. **What it does not do.** Every program that runs on that computer can ask Windows to
decrypt a machine-scope value, so the rights on the file are still the protection between local
users: only SYSTEM and Administrators may read `agent.ini`. A local administrator can read the key
anyway. A stolen disk is answered by disk encryption (BitLocker on Windows, LUKS on Linux, FileVault on macOS), not by this. The values under
`[headers]` stay plain text in `agent.ini`. If a machine is lost, revoke its key on the **Keys**
page.

**How to see whether the key is stored encrypted.** `agent.ini` (as an administrator) has an
`api_key_dpapi` line and no `api_key` line. If encrypting fails when the setup installs over a file
that was not encrypted yet (rare), the install carries on with the plain `api_key` under the same
rights rather than fail on a print station. A file that was encrypted becomes plain text in three
cases only: you run `unprotect-key` (below), the file says `seal_key = no` (at the next change in
Settings), or you type a new key in Settings, Connection and Windows cannot encrypt it. In the last
case the install still goes through, with the new key as plain text; the `api_key_dpapi` line tells
you which state the file is in. A change of the printers or the timeout never does this: if the key
cannot be encrypted again, nothing is changed.

**Rules the agent follows.**

- A non-empty `api_key` always wins over `api_key_dpapi`. That is also the recovery by hand: add
  `api_key = ...` as an administrator and the agent uses it.
- `seal_key = no` in `[agent]` (also `false`, `off` or `0`) keeps the setup from encrypting the key
  again. An existing `api_key_dpapi` is still read. A change in Settings never sets or removes this
  line; `unprotect-key` (below) sets it.
- Every install and every change in Settings encrypts the key again, unless `seal_key = no` is
  set. An agent that was installed by an earlier version has its key encrypted at the next change
  or update; the key, the device and the printer ids stay the same.
- The agent itself never writes `agent.ini`. Settings shows the header values by having an
  elevated command copy only the `[headers]` into the working folder, never the key.

**Linux and macOS keep the file.** On Linux a key sealed to the host would need `systemd-creds`
(systemd 250 and later; Ubuntu 22.04 ships 249), and the agent runs as an unprivileged user that
cannot decrypt it by itself. A credential file that is missing stops the unit before the agent can
log anything. Disk encryption (LUKS) also covers the print jobs and the headers. On macOS a daemon
can only use the file-based keychain, whose access lists an unsigned program that changes at every
update cannot satisfy unattended
([Apple TN3137](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains)); for a
stolen Mac, turn on FileVault. macOS is looked at again together with code signing.

#### Before you replace or reinstall this PC, or rely on a backup of it

A backup of an encrypted `agent.ini` restores only on the same Windows installation. What to do
when the key is lost is in [When the key cannot be read](#when-the-key-cannot-be-read). Before you
reinstall Windows, move to new hardware or rely on a file backup of this folder, do one of these:

- keep the key in a password manager (it is the one from the **Keys** page), or
- run `printpapi-agent unprotect-key` (below) and back up the file afterwards.

A plain backup made before this version stays valid, because the key never changed. To encrypt the
key again later, delete the `seal_key` line as an administrator and make any change in Settings.

#### Going back to an older version or to the Python agent

An older agent and the Python agent read only `api_key`. Before you go back, as an administrator in
a console, run the agent that sealed the file; a freshly downloaded `printpapi-agent.exe` in any
folder will do, it works on the installed folder:

```powershell
& "C:\Program Files\printpapi-agent\printpapi-agent.exe" unprotect-key
```

It writes the key back as plain text, sets `seal_key = no` so later changes keep it that way, and
prints no key. The running agent needs no restart. (An older exe does not know the word and just
starts the agent, so use a current one.)

On a PC that blocks the exe, read the key with the signed Python interpreter from an administrator
console, and put it into `agent.ini` as `api_key`:

```python
# printpapi: show the stored agent key
import ctypes, re
from ctypes import wintypes

class Blob(ctypes.Structure):
    _fields_ = [("size", wintypes.DWORD), ("data", ctypes.c_void_p)]

ini = open(r"C:\Program Files\printpapi-agent\agent.ini", "rb").read()
raw = bytes.fromhex(re.search(rb"(?m)^api_key_dpapi *= *([0-9a-f]+)", ini).group(1).decode())
src, out = Blob(len(raw), ctypes.cast(raw, ctypes.c_void_p)), Blob()
if not ctypes.windll.crypt32.CryptUnprotectData(
        ctypes.byref(src), None, None, None, None, 1, ctypes.byref(out)):
    raise ctypes.WinError()
print(ctypes.string_at(out.data, out.size)[32:].decode())
```

#### When the key cannot be read

This happens when `agent.ini` was copied from another Windows installation, after a Windows
reset, or when the value is damaged. A setup-installed agent does not end then: it shows "offline"
with the reason in Overview and writes it once to its log, and it looks at `agent.ini` again every
minute. So a short DPAPI failure heals by itself, and a key you put in by hand works within a
minute without a restart. `status.json` says so with `key_unreadable`. An agent set up by hand
ends at once with the text on its console.

1. Settings, Connection: enter the server and the key again. The name, the server and the printers
   are filled in from the status file; the timeout and the headers stay. The window lets you keep
   the old name, because it is the same key.
2. By hand: as an administrator add `api_key = ...` to `agent.ini`. This is the way for an agent
   with `[headers]` (an access proxy), because the window's connection check sends no headers. If
   the stored key keeps failing on that PC, add `seal_key = no` too, or the next change in
   Settings encrypts it again.
3. If the key is lost, create a new key on the **Keys** page and use a new device name; the printers then get new ids.

### Printer syntax

`name [|pdf] [= target]`, semicolon-separated:

- Plain name → a Windows printer name or CUPS queue; jobs print through it.
- `|pdf` → declares the printer PDF-capable. **Default is raw-only**, so a label printer is
  never sent a PDF by accident.
- `= socket://host:port` → the agent opens a raw TCP socket (e.g. a network label printer's
  `:9100`). Always raw-only — a bare socket has no renderer.
- `= file:///path/to/dir` → the agent writes the job into that directory instead of printing it
  (see [File output](#file-output-virtual-print-server)). Always takes both `pdf` and `raw`.

## Labels vs documents (the one rule)

A label printer **cannot parse a PDF** — send it raw PDF bytes and it form-feeds blank labels.
printpapi keeps the paths separate: `raw` jobs (ZPL/ESC-POS) go straight to the printer;
`pdf` jobs are rendered first (SumatraPDF on Windows, CUPS on Linux and macOS). Mark only real document
printers with `|pdf`.

## Printer setup by family

Which path a printer takes follows from **who renders**: a printer with its own page language
(Zebra/ZPL, ESC/POS) takes `raw` and needs no rendering; everything else needs a driver and takes
`pdf`. Get that wrong and you print blanks (gotcha #1).

### Zebra and other ZPL printers — `raw`

The printer parses ZPL itself, so the driver is almost irrelevant.

- **Network model (the easy one):** don't install anything. Point the agent straight at the
  printer: `warehouse-label = socket://192.168.1.50:9100`. No driver, no queue, no spooler — works
  the same on Windows, Linux and macOS.
- **USB model:** install Zebra's driver (*Zebra Setup Utilities* / ZDesigner on Windows,
  `lpadmin -m raw` or the Zebra CUPS driver elsewhere) so the OS has a printer object; the agent
  writes RAW bytes into it, the driver never re-renders. List it **without** `|pdf`.
- **Test it** before wiring up printpapi —
  `printf '^XA^FO50,50^A0N,40,40^FDprintpapi ok^FS^XZ' | lp -d Zebra -o raw` (Linux/macOS), or on
  Windows just use the dashboard's *Devices → Test print*, which sends the equivalent one-line
  ZPL label (`^XA^FO40,40^ADN,36,20^FDprintpapi test^FS^XZ`) to any raw-only printer.
- Label geometry lives **on the printer**, not in the job: `^PW` (width), `^LL` (length), `~SD`
  (darkness), and `~JC` to recalibrate the media sensor after a roll change.

### DYMO LabelWriter — `pdf`, not raw

A LabelWriter does **not** speak ZPL. It is a raster device driven entirely by its driver, so it
belongs on the PDF path:

1. Install the driver — *DYMO Connect* (Windows/macOS), `dymo-cups-drivers` (Linux).
2. In `agent.ini` mark it PDF-capable: `DYMO LabelWriter 550|pdf`.
3. Send a PDF whose page size *is* the label (e.g. 89 × 36 mm for an address label) — scaling a
   sheet-sized PDF onto a label is what produces the tiny-print-in-the-corner classic.
4. Pick the label via `options.paper`. Don't guess the name: `GET /printers` reports the driver's
   own `capabilities.papers`, and only those values are accepted by the driver.
5. If a wide label comes out turned and small on Windows, send `options.orientation: "landscape"`.
   See [api.md](api.md#submitting-a-job).

Brother QL and other raster label printers work the same way — driver + `|pdf`.

### ESC/POS receipt printers — `raw`

Same as Zebra: the printer interprets the byte stream. Network models take
`socket://IP:9100`; USB models go through the vendor driver as a RAW target. The agent passes the
bytes through. You can build the ESC/POS stream yourself, with any `escpos` library, and send it
as `raw_base64`; or send a JSON `receipt` job and let the **server** lay it out (texts, columns,
logo, QR code, barcode, cut, cash drawer, with umlauts and the euro sign drawn right): the agent
then gets ordinary `raw` bytes and needs no change. See [receipts.md](receipts.md). The printer
must support `GS v 0` raster graphics; a driver-based receipt printer that is also marked `|pdf`
should have `"output": "escpos"` in its receipt profile.

### Star CloudPRNT models — no agent at all

Star's network printers (mC-Print2/3, TSP100IV, mC-Label3, HI01X/HI02X) can poll the server
themselves, so they need no agent on any machine: point the printer's CloudPRNT setting at
`https://your-server/cloudprnt/<client-key>` and it enrols itself as a raw-only printer. Built to
the published protocol but not yet verified on a device — see [cloudprnt.md](cloudprnt.md).

## File output (virtual print server)

A `file://` target makes the printer a **directory**: the job is written to disk instead of paper.
For archival (keep a copy of every label), for feeding a document pipeline (drop the PDF into
Paperless-ngx's consume folder), and for testing an integration without burning a roll of labels.

```ini
printers = Zebra GK420d ; archive = file:///srv/paperless/consume
```

- Windows spelling: `archive = file:///C:/printpapi/out` (forward slashes, drive letter after
  `file:///`). `%20` escapes are decoded, so a path with spaces works.
- The directory is created if missing.
- One file per job, named after the job id: `job-42.pdf` for a `pdf` job, `job-42.prn` for a `raw`
  one (the exact bytes you submitted — ZPL, ESC/POS). `copies=3` writes `job-42.pdf`,
  `job-42-2.pdf`, `job-42-3.pdf`.
- A receipt job to a `file://` printer writes a viewable `job-42.pdf` (the printer takes PDF), so a
  directory is a virtual receipt printer to test with; `"output": "escpos"` in its
  [receipt profile](receipts.md#printer-profile) writes the `.prn` instead.
- A file printer accepts **both** modes and needs no `|pdf` tag — a directory has no renderer to
  get wrong. Print `options` (duplex/tray/…) are hardware settings and are ignored here.
- Nothing changes server-side: it is a normal printer on the Devices page, jobs report `done` when
  the file is written, and a write error (permissions, disk full) fails the job with the OS error.

## macOS

macOS printing *is* CUPS, so the agent takes the same path as Linux — it picks `lp` for anything
that isn't Windows, and `lpoptions -p <queue> -l` reports capabilities. Nothing to install.

The one trap is **raw printing**. A label printer added through the normal *Printers & Scanners*
dialog usually lands as a driverless/AirPrint queue, and that queue's filter chain mangles or
rejects ZPL/ESC-POS — the labels come out blank or as literal `^XA` text. Two ways out:

- **Network label printers: skip CUPS.** Use `name = socket://IP:9100` in `agent.ini`. The agent
  opens the socket itself, so no queue, no driver, no filter — the most reliable option on macOS.
- **USB label printers: add a raw queue.** Find the device URI, then create the queue without a
  driver:

  ```sh
  lpinfo -v                                     # e.g. usb://Zebra%20Technologies/ZTC%20GK420d
  sudo lpadmin -p Zebra -E -v 'usb://Zebra%20Technologies/ZTC%20GK420d' -m raw
  lp -d Zebra -o raw label.zpl                  # verify before wiring up the agent
  ```

  If your macOS build rejects `-m raw`, install the vendor's CUPS driver (Zebra/DYMO ship one)
  and keep sending raw — a vendor queue passes ZPL through.

Document printers (`|pdf`) need no special setup: CUPS renders the PDF through the installed
driver, exactly as on Linux.

## Run as a service

**Linux — systemd** (`/etc/systemd/system/printpapi-agent.service`):

```ini
[Unit]
Description=printpapi agent
After=network-online.target cups.service

[Service]
ExecStart=/opt/printpapi/printpapi-agent
Restart=always
RestartSec=5
User=printpapi

[Install]
WantedBy=multi-user.target
```

`agent.ini` lives next to the binary (`/opt/printpapi/agent.ini`), so no `WorkingDirectory` is
needed. `sudo systemctl enable --now printpapi-agent`. The service user needs no special rights —
any local user may submit to CUPS.

**macOS — launchd** (`/Library/LaunchDaemons/com.printpapi.agent.plist`, root-owned, mode 644):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.printpapi.agent</string>
  <key>ProgramArguments</key>
  <array>
    <string>/opt/printpapi/printpapi-agent</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/var/log/printpapi-agent.log</string>
  <key>StandardErrorPath</key><string>/var/log/printpapi-agent.log</string>
</dict>
</plist>
```

`sudo launchctl bootstrap system /Library/LaunchDaemons/com.printpapi.agent.plist`
(`sudo launchctl load -w <plist>` on older macOS). A daemon can print: CUPS queues are
system-wide.

**Windows — Task Scheduler** (in PowerShell run as administrator):

```powershell
$action   = New-ScheduledTaskAction -Execute "C:\printpapi\printpapi-agent.exe"
$trigger  = New-ScheduledTaskTrigger -AtStartup
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit 0 -AllowStartIfOnBatteries `
  -DontStopIfGoingOnBatteries -RestartCount 255 -RestartInterval (New-TimeSpan -Minutes 1)
Register-ScheduledTask -TaskName printpapi-agent -User "NT AUTHORITY\SYSTEM" -RunLevel Highest `
  -Action $action -Trigger $trigger -Settings $settings
```

`-ExecutionTimeLimit 0` is the setting that matters: without it Windows stops the task 72 hours
after it started ([ExecutionTimeLimit](https://learn.microsoft.com/en-us/windows/win32/taskschd/tasksettings-executiontimelimit)),
and `schtasks /create` has no switch for it. The two battery switches keep the agent running on
an unplugged laptop, and the restart settings bring it back a minute after a crash. The setup app
creates the same task.

If you created the task with `schtasks /create`, as this page said before, it stops after three
days. Run the block above again with `-Force` added to `Register-ScheduledTask`, which replaces
the task, then restart the computer or run `Stop-ScheduledTask printpapi-agent` and
`Start-ScheduledTask printpapi-agent`.

Caveat: printers installed *per user* are invisible to `SYSTEM`. Either install the printer for
all machine users, or create the task under the operator's account with "run whether user is
logged on or not". In the Task Scheduler window, also untick "Stop the task if it runs longer
than" on the Settings tab. The agent is a console program; under "run whether user is logged on
or not" no window shows. `nssm install printpapi-agent C:\printpapi\printpapi-agent.exe` works
too if you want real service semantics (auto-restart, `sc` control).

## Locked-down Windows

Smart App Control / WDAC / AppLocker block unsigned executables, and the Rust agent exe is **not
code-signed yet**, so such machines refuse to start it. Until signed builds exist, use the Python
agent, which stays available as the *legacy agent* (`agent/print_agent.py` in the repo, unchanged
and still tested in CI). It runs through the signed Python interpreter — `pythonw.exe` is
PSF-signed, and the `.py` file is data, not an executable:

1. Copy `agent/print_agent.py` to the machine (Python 3.9+; every release also has it as
   `printpapi-agent-python.zip`) and `pip install pywin32`. For PDF
   printing put `SumatraPDF.exe` next to the script (or on `PATH`).
2. Create `agent.ini` next to the script — same format and keys as above.
3. Run `python print_agent.py`, and for autostart create the task from
   [Run as a service](#run-as-a-service) with the interpreter as the program:

   ```powershell
   $action   = New-ScheduledTaskAction -Execute "C:\Program Files\Python312\pythonw.exe" `
     -Argument "C:\printpapi\print_agent.py"
   $trigger  = New-ScheduledTaskTrigger -AtStartup
   $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit 0 -AllowStartIfOnBatteries `
     -DontStopIfGoingOnBatteries -RestartCount 255 -RestartInterval (New-TimeSpan -Minutes 1)
   Register-ScheduledTask -TaskName printpapi-agent -User "NT AUTHORITY\SYSTEM" -RunLevel Highest `
     -Action $action -Trigger $trigger -Settings $settings
   ```

   (`nssm install printpapi-agent <pythonw.exe> <script>` works too; the per-user printer caveat
   and the note on tasks made with `schtasks /create` above apply.)

A key that the setup stored encrypted has to be turned back into plain text first, see [Going back to an older version or to the Python agent](#going-back-to-an-older-version-or-to-the-python-agent).

Switch to the Rust agent once a code-signed build is released.

## Reliability

- Result reporting retries up to 5 times with exponential backoff, so a network blip after a
  successful print doesn't make the server requeue (and re-print) the job.
- If the agent dies mid-job, the server's reaper requeues the job after the visibility timeout.
- Crash log: `%LOCALAPPDATA%\print_agent-error.log` (Windows) / `print_agent-error.log` in the
  temp dir (Linux, macOS) - see [Unattended machines](#unattended-machines) for what gets logged.
