# Star CloudPRNT — printers that poll by themselves

Star Micronics' network printers (mC-Print2/3, TSP100IV, mC-Label3, the HI01X/HI02X interface
boards) can poll an HTTP URL on their own. printpapi speaks that protocol, so at a site with such a
printer there is **nothing to install** — no agent, no PC, no service. Configure the printer with
your server URL and it enrols itself.

> *Star, CloudPRNT and Star Micronics are trademarks of Star Micronics Co., Ltd. printpapi is not
> affiliated with, endorsed by, or sponsored by Star Micronics.* This endpoint was implemented from
> the publicly documented protocol. No Star code or documentation text is contained in it.

## Set it up

1. Issue a client key for the org the printer belongs to (dashboard → API keys, or
   `POST /apikeys`). This key **is** the printer's credential — treat it like one.
2. In the printer's CloudPRNT settings, set the server URL to:

   ```
   https://your-server.example.com/cloudprnt/<client-key>
   ```

   If the printer's settings page has **User Name** / **Password** fields, you can instead point it
   at the bare `https://your-server.example.com/cloudprnt` and put the key in **User Name** (leave
   the password empty) — that keeps the key out of URLs and access logs.
3. Power-cycle or save; the printer starts polling. Within a poll interval it appears in
   **Devices** as `cloudprnt-<mac>` and in `GET /printers`.
4. Print to it like any other printer — the printer id from `GET /printers`:

   ```bash
   curl -X POST http://localhost:3460/jobs \
     -H "Authorization: Bearer <client-key>" -H "Content-Type: application/json" \
     -d '{"printer_id": 4, "type": "raw_base64", "content": "'"$(printf 'Hello\n\n\n' | base64)"'"}'
   ```

## Raw only — send Star commands, not PDF

A CloudPRNT printer executes the byte stream it is handed; it has no renderer. So it enrols as a
**raw-only** printer, exactly like a ZPL label printer
([the one rule](agent.md#labels-vs-documents-the-one-rule)). Send Star PRNT / Star Line mode
commands, or plain text — printable text prints as text, which is enough for a simple receipt.

A `pdf` job queued to one of these is **failed on the next poll** with
`CloudPRNT printers cannot render PDF — send raw Star commands`, instead of feeding out blank
paper. `POST /orders` (packing slips) refuses these printers up front for the same reason: render
the document somewhere else and submit it as `raw_*`, or point orders at a PDF-capable printer.

`copies` works — the stream is simply repeated. Per-job `options` (duplex, tray, …) are a PDF
feature and do not apply.

## What the server answers

One URL serves all three of the protocol's methods.

| Method | The printer sends | printpapi answers |
|---|---|---|
| `POST` | its status JSON (`printerMAC`, `status`, `printingInProgress`, …) | `{"jobReady": false}`, or `{"jobReady": true, "mediaTypes": [...], "jobToken": "<job id>"}` |
| `GET` | `?mac=…&type=…&token=…` | the job's bytes, `Content-Type` = the requested media type (`404` when nothing is queued, `415` for a type we cannot serve) |
| `DELETE` | `?mac=…&code=200%20OK&token=…` | `200`, empty — the job is marked `done`, or `failed` carrying the printer's status code |

The device is identified by its **MAC address**, and authorized by the client key in the path (or
in HTTP Basic). A request without a valid key is `401`; without a MAC, `400`.

**Media type.** We never transcode: the job's bytes go out as submitted, and the media type is only
the label the printer honours. The one offered is picked from the printer's own `Accept` header, in
this order: `application/vnd.star.starprnt`, `…starprntcore`, `…line`, `…linematrix`, `…raster`,
`text/plain` — defaulting to Star PRNT mode when the header says nothing usable.

**Job handover.** A poll claims one job; the printer downloads it, prints it, and confirms with
its status code. Until it confirms, the *same* job is re-offered on every poll — a response lost on
the way costs a re-download, not a skipped print. A printer that reports `printingInProgress` is
offered nothing. If a claim is never confirmed at all, the reaper requeues it like any other stale
job ([job lifecycle](api.md#job-lifecycle)).

Everything else is the ordinary pipeline: the job counts against the org's
[quota](api.md#quotas), shows up in the history and on the dashboard, fires its `callback_url`
webhook, and honours `idempotency_key` / `expire_after`.

## Ceilings

Deliberate, and cheap to lift if someone needs them:

- **DELETE only.** The protocol allows telling the printer to confirm with a `GET` instead
  (`deleteMethod`); we don't, so a proxy that blocks `DELETE` breaks confirmations.
- **No MQTT.** The HTTP method only — CloudPRNT's MQTT transport is not implemented.
- **No client actions, no peripherals.** Barcode readers, keyboards and line displays that
  CloudPRNT can carry are ignored; `clientAction` requests are not issued.
- **No capability discovery.** The printer reports its paper width in `X-Star-*` headers; we do not
  store them, so `GET /printers` shows no capabilities for these devices.
- **Enrolment is automatic.** Any device polling with a valid client key creates a printer row in
  that org. Revoking the key stops it; there is no per-device approval step.
