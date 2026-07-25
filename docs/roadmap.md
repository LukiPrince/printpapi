# Roadmap

v1 is deliberately small (see [CONTRIBUTING.md](../CONTRIBUTING.md) for the YAGNI rules).

## Shipped since v1.0

- ~~**Webhooks**~~ ✅ (per-job `callback_url`; best-effort retried delivery on terminal states; unsigned).
- ~~**Job cancel**~~ ✅ (`DELETE /jobs/{id}` while queued; `cancelled` state; `409` once claimed).
- ~~**`/metrics`**~~ ✅ Prometheus text format.
- ~~**Job options**~~ ✅ `copies` + `options` on pdf jobs (`duplex`, `paper`, `bin`, `color`, `pages`),
  mapped to SumatraPDF `-print-settings` / CUPS `lp -o`. See [api.md](api.md#submitting-a-job).
- ~~**Printer capabilities**~~ ✅ agent reports papers/bins/duplex/color at registration, in `GET /printers`.
- ~~**Multi-tenancy**~~ ✅ org-scoped keys, agents, printers and jobs; `POST/GET /orgs` (root only);
  foreign ids 404. See [api.md](api.md#multi-tenancy).
- ~~**Computer status + liveness events**~~ ✅ `GET /computers`, and `computer_online`/
  `computer_offline` POSTed to an org's `event_url` (`PUT /orgs/{id}`).
  See [api.md](api.md#computers-agents).

## v2 candidates

Ranked by demand evidence from a broad research sweep (July 2026) across r/selfhosted, QZ Tray's
issue tracker, PrintNode's feature surface, e-commerce/ERP forums, and OSS competitors. Roughly in
order of pull:

1. ~~**Agent/computer status API + online/offline webhooks**~~ ✅ **shipped** — `GET /computers`
   with liveness + per-org `event_url` transitions. Still open on top: a dashboard view for it
   (the UI only lists printers) and signed event payloads.
2. **Idempotency keys + job expiration** — retry-safe `POST /jobs` (resubmit without double-print),
   and `expire_after` so a stale label job doesn't print hours later when an offline agent
   reconnects. Small, high operational value for order-printing integrations.
3. **macOS agent** — it's CUPS underneath, so likely test + docs (plus macOS raw-printing quirks).
   Closest OSS competitor (print-relay) ships macOS/Linux but no Windows; we're the inverse.
4. **Docs as a feature** — service install (systemd / Windows `sc`/NSSM via signed Python),
   "print from n8n/Zapier in one HTTP node" recipe, Zebra/DYMO driver setup guides, and a
   "why not QZ Tray" section (no per-machine certs, no $-per-year signing, no browser→localhost
   websocket → immune to Chrome's Local Network Access change, runs headless as a service —
   [qzind/tray#116](https://github.com/qzind/tray/issues/116) open since 2016,
   [#825 cloud queue](https://github.com/qzind/tray/issues/825) open since 2021).
5. **E-commerce auto-print integration (Shopify/WooCommerce)** — "order comes in → packing slip /
   label prints" is the strongest commercial demand found; today it's all paid SaaS wrapping
   PrintNode (Printus, BizPrint at $/print). Needs: a store app/plugin that POSTs to our API on
   order webhooks + document rendering (order → PDF). Prerequisite for any hosted/SaaS offering.
6. ~~**Multi-tenancy / child accounts**~~ ✅ **shipped** — org-scoped keys, printers and jobs, with
   `POST/GET /orgs`. Still open on top of it: billing, quotas, per-org dashboard users/login,
   org-scoped key self-management, org deletion.
7. **PrintNode API compatibility layer** — same endpoints/JSON so the existing plugin ecosystem
   (Zapier/Make nodes, Odoo/WooCommerce/Business Central plugins, official PHP/Python SDKs) can
   point at a printpapi base URL unchanged. Big lever, needs careful surface mapping.
8. **File backend ("virtual print server")** — a printer target that writes the rendered job to
   disk as PDF instead of paper (two independent r/selfhosted asks; archival/paperless workflows).
9. **Star CloudPRNT protocol endpoint** — Star kitchen/receipt printers poll a server URL natively;
   speaking their protocol makes the printer itself the agent, zero software installed.
10. **Scales API** — USB HID scales at packing stations (NetSuite/Dynamics workflows read weight
    through PrintNode's client). Biggest hardware gap, but niche; needs agent-side HID + a push channel.
11. **ESC/POS receipt rendering/templating** — images/QR/receiptline-style markdown → ESC-POS. Where
    Home-Assistant/hobbyist efforts stall today; expensive to do well, raw passthrough already works.
12. **Code-signed Windows agent installer** — the one blocker that needs a certificate, not code.

Explicitly parked: MCP/LLM printing (no measurable audience yet), email-to-print (not even
PrintNode has it), Magento-specific work (generic webhook intake covers it), browser-side silent
printing à la QZ/PrintNode-JS (different architecture; our answer is server-side jobs).
