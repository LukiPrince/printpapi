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
3. Create `agent.ini` next to the script (below).
4. Run: `python print_agent.py` — autostart via Task Scheduler (Windows) or a systemd unit (Linux).

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

## Locked-down Windows

Smart App Control / WDAC / AppLocker block unsigned executables. Run the agent through the
signed Python interpreter (`pythonw.exe` is PSF-signed; the `.py` file is data, not an
executable) — or use a code-signed build when one is available.

## Reliability

- Result reporting retries up to 5 times with exponential backoff, so a network blip after a
  successful print doesn't make the server requeue (and re-print) the job.
- If the agent dies mid-job, the server's reaper requeues the job after the visibility timeout.
- Crash log: `%LOCALAPPDATA%\print_agent-error.log` (Windows) / the temp dir (Linux).
