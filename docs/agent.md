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
; A CUPS queue / Windows printer name, or socket://IP:PORT for a raw network printer.
printers   = Zebra GK420d ; HP LaserJet|pdf ; warehouse-label = socket://192.168.1.50:9100
```

### Printer syntax

`name [|pdf] [= target]`, semicolon-separated:

- Plain name → a Windows printer name or CUPS queue; jobs print through it.
- `|pdf` → declares the printer PDF-capable. **Default is raw-only**, so a label printer is
  never sent a PDF by accident.
- `= socket://host:port` → the agent opens a raw TCP socket (e.g. a network label printer's
  `:9100`). Always raw-only — a bare socket has no renderer.

## Labels vs documents (the one rule)

A label printer **cannot parse a PDF** — send it raw PDF bytes and it form-feeds blank labels.
printpapi keeps the paths separate: `raw` jobs (ZPL/ESC-POS) go straight to the printer;
`pdf` jobs are rendered first (SumatraPDF on Windows, CUPS on Linux). Mark only real document
printers with `|pdf`.

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
