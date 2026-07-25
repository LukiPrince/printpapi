# E-commerce: order in → packing slip out

An order arrives in your shop, a packing slip comes out of the printer in the warehouse. No cloud
print service, no per-print fee, and the order data never leaves your server.

Two ways in, both ending at the same place — `POST /orders`, which renders the order as a PDF
packing slip and queues it as a normal print job:

| Shop | How |
|---|---|
| **WooCommerce** | the [plugin](../integrations/woocommerce) — settings page, printer dropdown, reprint action |
| **Shopify** | a webhook straight into `POST /integrations/shopify/orders` (HMAC-verified) |
| anything else | `POST /orders` yourself, or via [n8n/Zapier](recipes.md) |

Prerequisites for both: a **client API key** (dashboard → *API keys*) and a **PDF-capable
printer** — a raw/label printer is refused with `400`, because it would form-feed blanks
([gotcha #1](agent.md#labels-vs-documents-the-one-rule)).

## WooCommerce

1. Copy `integrations/woocommerce/printpapi/` into `wp-content/plugins/` (or zip that folder and
   upload it under *Plugins → Add New → Upload*), then activate it.
2. *WooCommerce → printpapi*: server URL, client API key, printer (the dropdown lists your
   printers; raw-only ones are disabled), when to print, how many copies.
3. Done. The next paid order prints. Each order gets an order note saying which job it became, so
   a failure is visible where the merchant already looks.

Details worth knowing:

- The print is queued through `wp_schedule_single_event`, so **checkout never waits** on a printer.
- Every automatic print carries `idempotency_key = woo-<order>-<status>`: a retried hook, or a
  status flapping, prints once. The **Print packing slip** entry in the order's *Actions* box
  sends without a key — a reprint is meant to print again.
- The API key lives in `wp_options` in plaintext (as WordPress plugin settings do). Issue a key
  for the shop alone, and revoke that one key if the site is ever compromised.

## Shopify

Shopify has no server-side plugin to install — an app *is* a webhook receiver, and printpapi is
one. No Partner account, no OAuth, no review.

1. **Store the signing secret** (root token):

   ```bash
   curl -sX PUT https://print.example.com/orgs/1 \
        -H 'Authorization: Bearer <PRINTAPI_TOKEN>' \
        -d '{"shopify_secret":"<the secret from Shopify>"}'
   ```

   The secret comes from *Settings → Notifications → Webhooks* (bottom of the page) for
   admin-created webhooks, or from the app's API secret key if you created a custom app.
   `GET /orgs` only ever reports `shopify_secret_set: true` — the value is never echoed back.

2. **Create the webhook** in Shopify (*Settings → Notifications → Webhooks → Create webhook*):

   - Event: **Order creation** (or *Order payment*), format **JSON**
   - URL: `https://print.example.com/integrations/shopify/orders?key=<client-key>&printer_id=2`

3. Order something in the store. The slip prints.

**Why the key is in the URL:** Shopify cannot send an `Authorization` header, so the URL says
*which org* to print for, and Shopify's `X-Shopify-Hmac-Sha256` signature proves the request is
genuine. The key alone cannot print here — a request with a valid key and a bad signature is
`401`. Treat the URL as a secret anyway (it lands in Shopify's webhook log), and use a client key
issued for this one purpose so revoking it costs nothing else.

Redelivered webhooks (Shopify retries for 48 h) are deduped on the order id, so the same order
never prints twice.

## `POST /orders` directly

```bash
curl -sX POST https://print.example.com/orders \
     -H 'Authorization: Bearer <client-key>' -H 'Content-Type: application/json' \
     -d '{"printer_id": 2, "order": {"number": "1001", "customer": "Jane Doe",
          "address": ["Musterweg 1", "12345 Berlin"],
          "lines": [{"qty": 2, "sku": "A-1", "name": "Widget", "total": "19.80 EUR"}],
          "totals": [["Total", "19.80 EUR"]], "note": "leave at the door"}}'
# -> {"job_id": 12}
```

| Field | Meaning |
|---|---|
| `printer_id` | required, must be PDF-capable |
| `order` | the order (shape below, or a raw store payload with `format`) |
| `format` | `shopify` \| `woocommerce` — map that store's own JSON instead of the shape below |
| `title`, `copies`, `callback_url`, `idempotency_key`, `expire_after` | exactly as on [`POST /jobs`](api.md#submitting-a-job) |

The order shape — every field optional except `number`:

```json
{"number": "1001", "date": "2026-07-25", "shop": "Acme Store", "customer": "Jane Doe",
 "email": "jane@example.com", "phone": "+49 30 123456",
 "address": ["Musterweg 1", "12345 Berlin", "DE"],
 "lines": [{"qty": 2, "sku": "A-1", "name": "Widget", "total": "19.80 EUR"}],
 "totals": [["Subtotal", "29.70 EUR"], ["Total", "34.60 EUR"]],
 "note": "leave at the door"}
```

**Amounts are printed as given.** printpapi never recalculates a total — the shop already did the
arithmetic, and a print bridge disagreeing with the invoice is worse than no prices at all. (The
one exception: Shopify sends only a unit price per line, so the line amount is `qty × price`,
computed in `Decimal`.) Zero amounts are dropped instead of printed as `0.00`. Up to 1000 line
items; the slip paginates and numbers the pages.

## What the slip looks like — and what it isn't

Header (title, shop, order number, date), the ship-to block, the line-item table, totals, the
customer note, and `Page n/m`. Helvetica, A4, black and white.

It is deliberately plain: **no logo, no custom template, no Latin-only-breaking scripts** — the
renderer is ~150 lines of stdlib PDF in `app/packing_slip.py`, which is what keeps the server
dependency-free. If you need a designed document, render it wherever you already have a
templating stack and submit it as `pdf_uri` ([recipes](recipes.md#the-one-node-trick-let-the-server-fetch-the-document)) —
that path takes any PDF, including your carrier's shipping label.
