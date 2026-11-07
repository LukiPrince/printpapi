# Agent

The agent runs on the machine with the printers. It registers its printers with the server,
then long-polls for jobs, prints them, and reports the result. It makes **outbound connections
only** — nothing listens on the printer's machine, so NAT/firewalls are no problem.

## Setup app

The easiest way on Windows, macOS and Linux with a desktop: download `printpapi-setup-<platform>`
from the [latest release](https://github.com/LukiPrince/printpapi/releases/latest), open it and go
through four steps (connect to the server with an agent key, pick the printers, test print, done).
It writes `agent.ini`, installs the agent to `%ProgramFiles%\printpapi\` (Windows) or
`/opt/printpapi/` (macOS, Linux) and starts it with the machine: a scheduled task `printpapi-agent`
on Windows, a launchd daemon `com.printpapi.agent` on macOS, a systemd unit
`printpapi-agent.service` on Linux. Open the setup again to change the printers or remove the agent.

### Tray icon and settings

The setup also installs a small icon (next to the clock on Windows, in the menu bar on macOS, in
the status area on Linux) that starts with every user's login. Its colour, symbol and word show
the agent: connected, offline, or not running. Its menu has the status line, **Settings...**,
**Open dashboard** and **Quit**; Quit ends only the icon, the agent keeps printing.

**Settings...** opens a window with Overview, Printers (add, remove, change the kind, test print),
Connection (another server or key), Advanced (`timeout` and the `[headers]` values), Log,
SumatraPDF (Windows), Restart and Remove. Looking needs no administrator rights; each change asks
for them once.

- The icon only reads `status.json`, which the agent rewrites at least once a minute: Windows
  `%ProgramData%\printpapi-agent\`, macOS `/Library/Application Support/printpapi-agent/`, Linux
  `/var/lib/printpapi-agent/`. The file holds the server, device name, state, last contact and job, the
  printers and the header names, never the key and never a header value. A file that is missing or
  older than two minutes shows "not running". The agent still opens no port and takes no commands.
  The crash log of an agent the setup installed lives in the same folder.
- A new key on the same server needs a new device name, because the server ties a name to its
  key. The printers then get new ids and the old device stays in the dashboard; the window warns
  before it does that.
- GNOME shows no tray icons without the
  [AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/).
  The application menu has a **printpapi Agent** entry that opens the settings window directly.
- Removing the agent removes the icon, its autostart entry and the status folder too.

- The setup is not code-signed yet, so the system warns first. Windows: *More info*, then *Run
  anyway*. macOS: System Settings, Privacy & Security, *Open Anyway*. macOS and the aarch64 Linux
  builds are marked Beta on the downloads page until someone has run them by hand.
- Smart App Control and WDAC block the unsigned exe with no way around it. Use the Python agent
  ([Locked-down Windows](#locked-down-windows)).
- Linux without a desktop or without systemd: use the manual steps below.
- An agent you set up by hand is not taken over: the setup stops and says so, so a job is never
  printed twice.

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
server restores the old rule. Issue one key per machine: revoking it on the Keys page stops that
agent at once.

When exposing the server to agents outside your LAN, publishing only `/agent/*` (behind TLS) is
still the smallest surface.

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
`socket://IP:9100`; USB models go through the vendor driver as a RAW target. printpapi passes the
bytes through — building the ESC/POS stream (text, cuts, QR) is your job, any `escpos` library
does it.

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

**Windows — Task Scheduler:**

```powershell
schtasks /create /tn printpapi-agent /sc onstart /rl highest /ru SYSTEM `
  /tr "C:\printpapi\printpapi-agent.exe"
```

Caveat: printers installed *per user* are invisible to `SYSTEM`. Either install the printer for
all machine users, or create the task under the operator's account with "run whether user is
logged on or not". The agent is a console program; under "run whether user is logged on or not"
no window shows. `nssm install printpapi-agent C:\printpapi\printpapi-agent.exe` works too if you
want real service semantics (auto-restart, `sc` control).

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
3. Run `python print_agent.py`, and for autostart:

   ```powershell
   schtasks /create /tn printpapi-agent /sc onstart /rl highest /ru SYSTEM `
     /tr '"C:\Program Files\Python312\pythonw.exe" C:\printpapi\print_agent.py'
   ```

   (`nssm install printpapi-agent <pythonw.exe> <script>` works too; the per-user printer caveat
   above applies.)

Switch to the Rust agent once a code-signed build is released.

## Reliability

- Result reporting retries up to 5 times with exponential backoff, so a network blip after a
  successful print doesn't make the server requeue (and re-print) the job.
- If the agent dies mid-job, the server's reaper requeues the job after the visibility timeout.
- Crash log: `%LOCALAPPDATA%\print_agent-error.log` (Windows) / `print_agent-error.log` in the
  temp dir (Linux, macOS) - see [Unattended machines](#unattended-machines) for what gets logged.
