# printpapi for WooCommerce

Prints a packing slip on your own printer when an order comes in, through a self-hosted
[printpapi](../../README.md) server.

## Install

1. Copy the `printpapi/` folder into `wp-content/plugins/` — or zip it and upload it under
   *Plugins → Add New → Upload Plugin*.
2. Activate **printpapi for WooCommerce**.
3. Open *WooCommerce → printpapi* and fill in:
   - **Server URL** — e.g. `https://print.example.com`
   - **API key** — a *client* key from the printpapi dashboard (never the root `PRINTAPI_TOKEN`)
   - **Printer** — picked from a dropdown of your printers; raw/label printers are disabled,
     a packing slip needs a PDF-capable printer
   - **Print when** — order created / processing / on hold / completed
   - **Copies**

## What it does

- Queues the print through WP-Cron (`wp_schedule_single_event`), so checkout never waits on a
  printer.
- Sends `idempotency_key = woo-<order>-<status>` on automatic prints — a retried hook or a status
  flapping back and forth prints exactly once.
- Adds **Print packing slip (printpapi)** to the order's *Actions* box for reprints (no
  idempotency key, so it really does print again).
- Writes the outcome into the order notes: the job id on success, the server's error otherwise.
- Declares HPOS (custom order tables) compatibility — it only uses the order CRUD getters.

Requires WooCommerce, PHP 7.4+, WordPress 6.0+. MIT licensed, like the rest of the project.

Full setup notes, including the Shopify equivalent and the raw `POST /orders` API:
[docs/ecommerce.md](../../docs/ecommerce.md).
