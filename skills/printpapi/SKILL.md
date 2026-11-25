---
name: "printpapi"
description: "Use this skill when code has to print through a printpapi server over its HTTP API. It covers document printers (PDF), label and receipt printers (ZPL, ESC/POS), picking the printer, retry-safe submits, webhooks and polling, and errors. Also use it to connect an app, shop, n8n flow or script to printpapi, a self-hosted print bridge."
license: "Elastic-2.0, https://github.com/LukiPrince/printpapi/blob/main/LICENSE"
---
# Print with printpapi

printpapi is a self-hosted, source-available print bridge. Your code sends a job to the server's
HTTP API; an agent next to the printers polls the server and prints it. Ask the user for the
server URL and an API key: never invent either, never log a key, never write one into source.
The server describes itself in OpenAPI 3.1 at `/openapi.json` (no key needed). Details: [HTTP API](https://raw.githubusercontent.com/LukiPrince/printpapi/main/docs/api.md),
[labels](https://raw.githubusercontent.com/LukiPrince/printpapi/main/docs/labels.md), [receipts](https://raw.githubusercontent.com/LukiPrince/printpapi/main/docs/receipts.md), [Star CloudPRNT](https://raw.githubusercontent.com/LukiPrince/printpapi/main/docs/cloudprnt.md),
[agent and printers](https://raw.githubusercontent.com/LukiPrince/printpapi/main/docs/agent.md), [recipes](https://raw.githubusercontent.com/LukiPrince/printpapi/main/docs/recipes.md).

## Auth
Every call sends `Authorization: Bearer <key>`. Recommend a **`print` key**: it submits jobs and
reads printers and sees only its own jobs. A manager (root or a dashboard login) issues it in the
dashboard under API keys, or with `POST /apikeys` and `{"label": "shop", "scope": "print"}`; a client
key cannot (401). The answer must carry `"scope": "print"`: without it (also in `GET /me`) the
server is older than key rights and the key is a full one, so do not hand it out as a print key. Such
a server may also refuse `receipt` (400) and has no `/openapi.json`. Never use the root `PRINTAPI_TOKEN` in an app.

## Choose a printer
`GET /printers` answers `{"printers": [{"id", "name", "online", "can_pdf", "agent_name", ...}]}`.
Use `id` as `printer_id`. `GET /computers` shows whether the agent is online. Several candidates:
ask the user which one. `can_pdf` decides what the printer takes (next sections).

## Submit a job
```bash
B64=$(base64 < label.zpl | tr -d '\n')   # portable: no base64 -w0
# the body goes through stdin: a label above ~128 KB would break curl's argument limit
printf '{"printer_id":3,"type":"raw_base64","content":"%s","idempotency_key":"shop1-order-4712-label"}' "$B64" |
  curl -s -X POST "$PRINTPAPI_URL/jobs" -H "Authorization: Bearer $PRINTPAPI_KEY" \
  -H 'Content-Type: application/json' --data-binary @-
```
Answer: `{"job_id": 7}`. In Python use `urllib.request` with `base64.b64encode(...).decode()`.
Optional: `title`, `copies` (1-100), `expire_after` (1-2592000 seconds; later the job fails with
`expired` instead of printing a stale label), `idempotency_key` (at most 128 characters),
`callback_url`, `options` (PDF jobs only, else 400: `duplex`, `paper`, `bin`, `color`, `pages`, `orientation`).

| type | carries |
|---|---|
| `raw_base64` | `content`: base64 bytes (ZPL, ESC/POS) |
| `pdf_base64` | `content`: base64 PDF |
| `raw_uri` | `url`: the server fetches the bytes |
| `pdf_uri` | `url`: the server fetches a PDF |
| `raw_uri_post` | `url` + `json`: the server POSTs it, prints the response bytes |
| `pdf_uri_post` | `url` + `json`: the same, the response is a PDF |
| `label_uri` | `url` + `template`: an image, rendered through a label template |
| `label_base64` | `content` (base64 image) + `template` |
| `label_text` | `text` + `template` |
| `receipt` | `receipt`: a JSON document the server lays out |

The request body is at most 32 MB (base64 counts). A `*_uri` job makes the server fetch the URL.

## Which payload for which printer
`can_pdf` decides, not the word "label". The API does not report a raw printer's language: ask.
- `can_pdf: true` (office printers, DYMO LabelWriter, Brother QL): send `pdf_*`, `label_*` or `receipt`, never raw ZPL.
- `can_pdf: false` is raw only. ZPL printer: ZPL via `raw_*`, or `label_*`, never `receipt`. ESC/POS printer: a
  `receipt` or ESC/POS via `raw_*`, never `label_*`.
- Star CloudPRNT (agent name starts with `cloudprnt-`): Star PRNT or Star Line commands, or plain
  text, via `raw_*`. Never `label_*` (400), never `receipt` (400), never ESC/POS.
- **The server does not stop PDF bytes on a raw printer, so you must.** Never put PDF bytes (they
  start with `%PDF`) into any `raw_*` job: nothing protects you and the labels come out blank.

## Labels
A carrier's PDF label for a ZPL printer: ask the carrier for ZPL and send `raw_base64` or `raw_uri`.
Or take its PNG or GIF (or rasterise the PDF to PNG yourself) and send `label_base64` or
`label_uri` with a template. `label_*` accepts only GIF, PNG and JPEG; a PDF answers 400.
Templates fix label size and position: `GET /labels`, `POST /labels`, `GET /labels/{id}`,
`PUT /labels/{id}`, `DELETE /labels/{id}` (writing needs the Edit right, so a full key or a login; a
`print` key reads and uses them). Name one in `template` (name or id). The output follows the printer: PDF if `can_pdf`, else ZPL.

## Receipts
Send `"type": "receipt"` first: `"receipt": {"cut": true, "content": [...]}` with elements `text`,
`columns`, `divider`, `image`, `qr`, `barcode`, `feed`, `cut`, `drawer`. The server writes ESC/POS
for a raw printer or a PDF for a PDF printer and works with every agent version. Use it on an ESC/POS
receipt printer or a PDF printer, never on a ZPL printer (the server would send it ESC/POS and the job
would still end `done`) and never on CloudPRNT (400).
`POST /receipts/preview` draws it as a PNG without printing. It is not yet verified on a physical
printer: have the user print one test receipt. Hand-made ESC/POS only when the document cannot
say it: build the stream with an escpos library, send `raw_base64`. CloudPRNT takes Star commands,
not ESC/POS, at most 512 KB per job (2 MB on mC-Label3, HI01X, HI02X); larger ends `failed` with 521.
Packing slips for shop orders: `POST /orders` (PDF printers only).

## Retry-safe submits
Send one `idempotency_key` per document, not per order, and prefix it per integration:
`shop1-order-4712-label`, `shop1-order-4712-slip`. Keys are shared by the whole org, across all API
keys: a key that another integration already used returns that job and prints nothing, and a `print`
key then reads its `job_id` as 404. A print key may not send a key that starts with `shopify-` (400).
The server compares only the key: a second job with the same key returns the first `job_id`, even for
another printer or payload. Same key means a retry of the same submit (timeout, 409, 502, 503).
To print again after `failed`, `expired` or `cancelled`, or to reprint, use a new key (append
`-2`) or none.

## Did it print
`POST /jobs` returns when the job is queued, not printed. Poll `GET /jobs/{id}`: `state` is one of
`queued`, `claimed`, `done`, `failed` or `cancelled`. `error` says why. `GET /jobs` lists recent jobs.
`DELETE /jobs/{id}` cancels a `queued` job only (409 once claimed). A job failed with
`retry limit exceeded`, `expired` or `retention` can still become `done` when the agent reports late.
`"maybe_printed": true` (job and webhook, newer servers) says so: an agent had it and never answered, so wait
and ask again before you resubmit. Servers up to 3.6.0 never send it: there, always ask again first. A `print` key sees only its own jobs (others read as 404).
`callback_url` (http or https) gets a POST per final state, best effort: a failed POST is retried a few
times within about half a minute, then given up. It can arrive twice (dedupe on `job_id` + `state`, and
expect a `done` after a `failed`) and it is unsigned, so treat it as a hint. Keep polling `GET /jobs/{id}`
as the fallback. A private `url` (localhost, LAN) answers 502 under `PRINTAPI_PRIVATE_URLS=deny`, the
default with open signup or on a hosted server (operator token); a private `callback_url` is accepted
but never delivered.

## Errors
| status | meaning and what to do |
|---|---|
| 400 | bad field, unknown printer, unknown label template, option not for this job type, body over 32 MB (`bad json`; some clients see a closed connection instead, so check the size first): do not retry unchanged |
| 401 | key missing, wrong or revoked, or a client key on a manage endpoint such as `POST /apikeys` |
| 402 | the org's job quota is spent; a resubmit with a known key costs nothing |
| 403 | the key's rights do not allow this, or the org is locked by the operator |
| 404 | unknown job or template by id (GET, PUT, DELETE) |
| 409 | the idempotency key is still in flight; cancel of a claimed job; template name in use |
| 502 | your `url` failed, or the server's outbound requests are at their cap: check the URL, retry with the same key |
| 503 | too many downloads in flight for the org, or renders busy: retry with back-off and the same key |

## Common mistakes
- One key for two documents, an unprefixed key another integration may use, or the old key to retry a failed job.
- PDF bytes in `raw_*`; `label_*` for an ESC/POS or CloudPRNT printer; `receipt` for a ZPL printer;
  guessing a printer's language.
- A webhook-only flow (it can be lost); a private `callback_url` that is silently never called;
  resubmitting on the first `failed`.
- The root token in an app; a key in a log or in source.
