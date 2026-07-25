# Recipes — print from your automation tool

printpapi's API is one `POST /jobs` with a JSON body, so **any tool that can make an HTTP request
can print** — n8n, Zapier, Make, Power Automate, a cron job, your backend. No SDK, no plugin.

Two things first:

1. **A client API key** — dashboard → *API keys* → *New key* (or `POST /apikeys` with the
   bootstrap token). Never put `PRINTAPI_TOKEN` itself into an automation tool; it is root.
2. **The `printer_id`** — dashboard → *Devices*, or `GET /printers`. It is stable.

Your automation tool must be able to reach the server: a self-hosted n8n on the same LAN can use
`http://printpapi:3460`, cloud SaaS (Zapier/Make) needs a public HTTPS URL.

## The one-node trick: let the server fetch the document

Automation tools are bad at binary data. Don't base64 a PDF inside them — hand the server a URL
and it fetches the bytes itself (with a browser User-Agent, so Cloudflare-fronted endpoints work):

```json
{"printer_id": 2, "type": "pdf_uri", "url": "https://shop.example/orders/4712/packing-slip.pdf",
 "title": "Packing slip #4712", "idempotency_key": "order-4712-slip", "expire_after": 3600}
```

That is the whole integration — **one HTTP node**. Same for a carrier's ZPL label
(`"type": "raw_uri"`). If the document only exists behind a POST (a render service), use
`pdf_uri_post` / `raw_uri_post` and pass the payload as `json`.

Two fields worth setting on every automated job:

- **`idempotency_key`** — order webhooks get redelivered. With a stable key (the order id) a
  resubmit returns the original `job_id` and prints nothing extra.
- **`expire_after`** — seconds. If the warehouse PC is offline, a shipping label from three hours
  ago should fail, not print at midnight.

## n8n

**HTTP Request** node:

| Field | Value |
|---|---|
| Method | `POST` |
| URL | `http://printpapi:3460/jobs` |
| Authentication | *Generic Credential Type* → *Header Auth* → name `Authorization`, value `Bearer <client-key>` |
| Send Body | on, *Body Content Type* `JSON`, *Specify Body* `Using JSON` |

Body (expressions in `{{ }}` as usual):

```json
{
  "printer_id": 2,
  "type": "pdf_uri",
  "url": "{{ $json.label_url }}",
  "title": "Order {{ $json.order_number }}",
  "idempotency_key": "order-{{ $json.id }}",
  "expire_after": 3600
}
```

Wire it behind a **Webhook** node (Shopify/WooCommerce order webhook) and the flow is:
order → HTTP Request → printed. Nothing else needed.

Printing a **packing slip** rather than an existing document? Skip the automation tool: the
WooCommerce plugin and the Shopify webhook do it end-to-end — see [ecommerce.md](ecommerce.md).

Already holding the file as binary in n8n? Its base64 lives in `$binary.<property>.data`, so
`"type": "pdf_base64", "content": "{{ $binary.data.data }}"` works — but prefer the URL form.

## Zapier

Action → **Webhooks by Zapier** → *Custom Request* (a Premium app; needs a paid plan):

- Method `POST`, URL `https://printpapi.example.com/jobs`
- Headers: `Authorization: Bearer <client-key>`, `Content-Type: application/json`
- Data:

  ```json
  {"printer_id": 2, "type": "pdf_uri", "url": "{{label_url}}",
   "title": "Order {{order_number}}", "idempotency_key": "order-{{order_id}}"}
  ```

*POST* (instead of *Custom Request*) also works — set *Payload Type* to `json` and add the
`Authorization` header under *Headers*.

## Make (Integromat)

**HTTP → Make a request**: `POST`, *Body type* `Raw`, *Content type* `JSON (application/json)`,
header `Authorization: Bearer <client-key>`, and the same JSON body.

## Plain HTTP

```bash
curl -sX POST https://printpapi.example.com/jobs \
  -H 'Authorization: Bearer <client-key>' -H 'Content-Type: application/json' \
  -d '{"printer_id":1,"type":"raw_uri","url":"https://carrier.example/label/4712.zpl"}'
```

```powershell
$body = @{ printer_id = 1; type = 'raw_base64'; content = [Convert]::ToBase64String(
    [IO.File]::ReadAllBytes('label.zpl')) } | ConvertTo-Json
Invoke-RestMethod -Method Post -Uri 'https://printpapi.example.com/jobs' -Body $body `
  -ContentType 'application/json' -Headers @{ Authorization = 'Bearer <client-key>' }
```

```python
import base64, json, urllib.request
body = json.dumps({"printer_id": 1, "type": "raw_base64",
                   "content": base64.b64encode(open("label.zpl", "rb").read()).decode()}).encode()
req = urllib.request.Request("https://printpapi.example.com/jobs", body, method="POST",
                             headers={"Authorization": "Bearer <client-key>",
                                      "Content-Type": "application/json"})
print(json.load(urllib.request.urlopen(req)))          # -> {"job_id": 1}
```

## Did it print?

`POST /jobs` returns as soon as the job is queued — it does **not** wait for paper. To find out
the outcome, pick one:

- **Poll** `GET /jobs/{id}` until `state` is `done` / `failed` / `cancelled`.
- **Webhook**: set `callback_url` on the job and the server POSTs the outcome to you once
  (at-least-once — dedupe on `job_id` + `state`). See [api.md](api.md#webhooks).
- **Fleet health**: `GET /computers` tells you whether the agent is even online, and an org's
  `event_url` gets `computer_online` / `computer_offline` edges — the useful alert is
  "the warehouse PC went offline", not "one job failed".

## Multi-location setups

One org per customer/site (`POST /orgs` as root, then issue that org's key). A key *is* the org:
its jobs, printers and agents are invisible to every other org, and a foreign `printer_id` is a
`400`. See [api.md](api.md#multi-tenancy).
