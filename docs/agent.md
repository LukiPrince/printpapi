# Agent

The agent runs on the machine with the printers. It registers its printers with the server,
then long-polls for jobs, prints them, and reports the result. It makes **outbound connections
only** — nothing listens on the printer's machine, so NAT/firewalls are no problem.

## Install

1. Copy `agent/print_agent.py` to the machine (Python 3.9+).
2. Platform bits:
   - **Windows:** `pip install pywin32`. For PDF printing, put `SumatraPDF.exe` next to the
     script (or on `PATH`) — it silent-prints through the installed driver.
   - **Linux:** CUPS (`lp` must work). Raw jobs go to the queue with `-o raw`; PDFs go through
     the CUPS filter chain.
   - **macOS:** same CUPS path as Linux, nothing to install (`lp` ships with the OS) — but read
     [macOS](#macos) below, raw printing has one setup trap.
3. Create `agent.ini` next to the script (below).
4. Run: `python print_agent.py` — for autostart see [Run as a service](#run-as-a-service).

## agent.ini

```ini
[agent]
server_url = http://yourserver:3460
api_key    = your-agent-key
name       = office-pc
; printers: semicolon-separated. Append |pdf for document printers.
; A CUPS queue / Windows printer name, socket://IP:PORT for a raw network printer,
; or file:///path/to/dir to archive the job to disk instead of printing it.
printers   = Zebra GK420d ; HP LaserJet|pdf ; warehouse-label = socket://192.168.1.50:9100
```

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
`pdf` jobs are rendered first (SumatraPDF on Windows, CUPS on Linux). Mark only real document
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

Brother QL and other raster label printers work the same way — driver + `|pdf`.

### ESC/POS receipt printers — `raw`

Same as Zebra: the printer interprets the byte stream. Network models take
`socket://IP:9100`; USB models go through the vendor driver as a RAW target. printpapi passes the
bytes through — building the ESC/POS stream (text, cuts, QR) is your job, any `escpos` library
does it.

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

macOS printing *is* CUPS, so the agent takes the same path as Linux — `select_backend()` picks
`lp` for anything that isn't Windows, and `lpoptions -p <queue> -l` reports capabilities. Nothing
to install beyond Python (`/usr/bin/python3` asks for the Xcode Command Line Tools the first time;
`xcode-select --install` if it does).

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
ExecStart=/usr/bin/python3 /opt/printpapi/print_agent.py
WorkingDirectory=/opt/printpapi
Restart=always
RestartSec=5
User=printpapi

[Install]
WantedBy=multi-user.target
```

`sudo systemctl enable --now printpapi-agent`. The service user needs no special rights — any
local user may submit to CUPS.

**macOS — launchd** (`/Library/LaunchDaemons/com.printpapi.agent.plist`, root-owned, mode 644):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.printpapi.agent</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/bin/python3</string>
    <string>/opt/printpapi/print_agent.py</string>
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

**Windows — Task Scheduler**, running the *signed* interpreter (see below):

```powershell
schtasks /create /tn printpapi-agent /sc onstart /rl highest /ru SYSTEM `
  /tr '"C:\Program Files\Python312\pythonw.exe" C:\printpapi\print_agent.py'
```

Caveat: printers installed *per user* are invisible to `SYSTEM`. Either install the printer for
all machine users, or create the task under the operator's account with "run whether user is
logged on or not". `nssm install printpapi-agent <pythonw.exe> <script>` works too if you want
real service semantics (auto-restart, `sc` control).

## Locked-down Windows

Smart App Control / WDAC / AppLocker block unsigned executables. Run the agent through the
signed Python interpreter (`pythonw.exe` is PSF-signed; the `.py` file is data, not an
executable) — or use a code-signed build when one is available.

## Reliability

- Result reporting retries up to 5 times with exponential backoff, so a network blip after a
  successful print doesn't make the server requeue (and re-print) the job.
- If the agent dies mid-job, the server's reaper requeues the job after the visibility timeout.
- Crash log: `%LOCALAPPDATA%\print_agent-error.log` (Windows) / `print_agent-error.log` in the
  temp dir (Linux, macOS).
