# Design v0 — the original homelab push bridge (sanitized retrospective)

**Date:** 2026-06-29 (original design) / sanitized 2026-07-15
**Status:** Superseded by the v1 poll engine (see `docs/superpowers/specs/2026-06-30-poll-engine-design.md`)

This is a sanitized summary of the original homelab design that printpapi grew out of. The
original document described a specific small-business deployment (real hostnames, LAN IPs,
database IDs, printer serials); those details are replaced with placeholders here. The
architecture lessons are what matter.

## The problem

Replace PrintNode (cloud print SaaS, ~€7/month) for a small business that prints shipping
labels and fault-code labels daily. Print-ready content (PDF or ZPL) was already produced by
an in-house PDF/label-rendering service; PrintNode only provided **transport** — bytes to a
physical printer. A workflow tool (n8n) orchestrated: webhook → look up the order record →
fetch rendered bytes → print.

## v0 architecture (push, same LAN)

```
workflow tool (n8n) ──LAN HTTP + Bearer──► print-api (:3460, LAN-only)
                                              ├─ network label printer → TCP socket <ip>:9100 (raw)
                                              └─ USB printers on a PC  → mini-agent (LAN HTTP)
                                                    ├─ raw → win32print RAW   (ZPL to label printer)
                                                    └─ pdf → SumatraPDF silent (driver renders)
```

- **print-api:** Python stdlib `http.server`, one route (`POST /print`), bearer auth,
  content types `raw_base64` / `pdf_base64` / `raw_uri` (server fetches the URL).
- **Mini-agent:** Python + pywin32 on the Windows PC with the USB printers; RAW datatype for
  ZPL, SumatraPDF silent-print for PDF. Printer whitelist, bearer auth, Task-Scheduler autostart.
- **Rollout rule:** old system stays live until the new one demonstrably prints every case.

## Why v0 was replaced

The push model requires the server to reach the agent — same LAN or inbound ports. PrintNode's
real trick is the opposite: agents behind NAT/firewalls poll *out* to the server. v1 flipped to
agent-polls-server (long-poll), which is what this repo implements.

## Lessons that survived into v1 (the gotchas)

1. **Label printers cannot render PDF** — raw PDF bytes to a `:9100` socket print blanks.
   Render first (driver/SumatraPDF/CUPS); only pre-rendered ZPL/ESC-POS goes raw.
2. **Locked-down Windows blocks unsigned executables** (Smart App Control/WDAC) — run the agent
   via the signed Python interpreter or code-sign the binary.
3. **WAF/CDN-fronted URLs 403 the default `Python-urllib` UA** — send a browser UA on all
   outbound fetches.
