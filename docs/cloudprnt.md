# Star CloudPRNT — printers that poll by themselves

Star Micronics' network printers (mC-Print2/3, TSP100IV, mC-Label3, the HI01X/HI02X interface
boards) can poll an HTTP URL on their own. printpapi speaks that protocol, so at a site with such a
printer there is **nothing to install** — no agent, no PC, no service. Configure the printer with
your server URL and it enrols itself.

> **Not yet verified on hardware.** This endpoint was built and tested against the published
> protocol specification, not against a physical printer — no Star device has printed through it.
> The parts most likely to need a fix are firmware-dependent: the optional `jobToken` and
> `printingInProgress` fields, and the exact status code a given model confirms with. If you have
> one of these printers, run the server with `LOG_REQUESTS=1` and
> [open an issue](https://github.com/LukiPrince/printpapi/issues) with what it actually sends — that
> is the one thing missing here.

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

**Failures read back.** The confirmation code is the whole diagnostic for a device with no agent and
no log of its own, so it lands in the job's `error`: any `2xx` counts as printed (`201` output paper
taken and `211` paper low are successful prints carrying a warning), everything else fails the job
with the code and, where the protocol documents one, a reason — `410 (out of paper)`,
`420 (cover open)`, `521 (job too large …)`.

**A missing confirmation** is not a failure yet: the job stays `claimed` and is re-offered until the
reaper's visibility timeout requeues it (5 minutes, then `max_retries`), so a printer that loses
power mid-job prints it on its next poll.

## Limits worth knowing

- **Job size.** The protocol caps a job at **512 KB**, or **2 MB** on mC-Label3 and the
  HI01X/HI02X boards. Larger and the printer aborts the download and confirms `521` — the job then
  reads `failed` with that code. printpapi does not cut jobs down; keep receipts small.
- **Poll interval vs. liveness.** A device counts as online for 60 s after its last request. Star's
  own guidance for a server is `2 × poll interval + 5 s`, so set the printer's *Polling time* to
  **25 s or less** — a slower interval makes the dashboard flap between online and offline.
- **Setting request.** MQTT-capable models ask once for `…/cloudprnt-setting.json` at power-on. We
  answer `404`, which is the documented signal for "this server speaks CloudPRNT HTTP only", and
  the printer proceeds to poll normally.
- **Latency.** A job is picked up on the next poll — there is no push. Star's firmware polls
  immediately after an event (a finished print, a scanned barcode), so a queue drains at poll
  speed, not one job per interval.

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
