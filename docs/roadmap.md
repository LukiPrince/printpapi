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
- ~~**Idempotency + expiry**~~ ✅ `idempotency_key` and `expire_after` on `POST /jobs`.
  See [api.md](api.md#submitting-a-job).
- ~~**macOS agent**~~ ✅ CUPS path, macOS raw-printing traps + service install (systemd, launchd,
  Task Scheduler) documented in [agent.md](agent.md#macos).
- ~~**Docs as a feature**~~ ✅ automation recipes ([recipes.md](recipes.md)), printer-family setup
  guides, and a "why not QZ Tray / PrintNode" comparison in the README.
- ~~**E-commerce auto-print**~~ ✅ `POST /orders` + packing-slip renderer, WooCommerce plugin,
  Shopify webhook. See [ecommerce.md](ecommerce.md).
- ~~**PrintNode-compatible API**~~ ✅ the same endpoints in PrintNode's JSON shapes, selected by
  HTTP Basic auth. See [printnode-compat.md](printnode-compat.md).
- ~~**File backend**~~ ✅ a `file://` printer target writes the job to a directory instead of
  printing it. See [agent.md](agent.md#file-output-virtual-print-server).
- ~~**Org accounts**~~ ✅ e-mail/password login, session tokens, org-scoped key and user
  self-management. See [api.md](api.md#accounts-and-login).
- ~~**Star CloudPRNT**~~ ✅ Star printers poll `/cloudprnt/<client-key>` themselves — the printer is
  the agent, nothing is installed at the site. See [cloudprnt.md](cloudprnt.md). Built against the
  published protocol; **no physical printer has printed through it yet**.
- ~~**Hosted-service plumbing**~~ ✅ opt-in self-signup, password reset by e-mail (SMTP, stderr
  fallback), account removal, a Settings page for the org's own knobs, and per-org monthly job
  quotas answering `402`. See [api.md](api.md#self-signup).

## v2 candidates

Ranked by demand evidence from a broad research sweep (July 2026) across r/selfhosted, QZ Tray's
issue tracker, PrintNode's feature surface, e-commerce/ERP forums, and OSS competitors. Roughly in
order of pull:

1. ~~**Agent/computer status API + online/offline webhooks**~~ ✅ **shipped** — `GET /computers`
   with liveness + per-org `event_url` transitions, and the Devices page built on it. Still open
   on top: signed event payloads.
2. ~~**Idempotency keys + job expiration**~~ ✅ **shipped** — `idempotency_key` (per-org, returns
   the original job) and `expire_after` (deadline-passed jobs fail as `expired`, never print).
3. ~~**macOS agent**~~ ✅ **shipped** — macOS takes the existing CUPS path (`select_backend` covers
   every non-Windows platform, now tested for `darwin`); the setup traps that are macOS-specific
   (driverless queues mangling raw ZPL, `lpadmin -m raw`, `socket://` as the reliable way out) are
   documented in [agent.md](agent.md#macos).
4. ~~**Docs as a feature**~~ ✅ **shipped** — service install
   ([agent.md](agent.md#run-as-a-service): systemd, launchd, Task Scheduler/NSSM via signed
   Python), the "one HTTP node" automation recipes for n8n/Zapier/Make
   ([recipes.md](recipes.md)), per-family printer setup (Zebra/ZPL, DYMO, ESC/POS —
   [agent.md](agent.md#printer-setup-by-family)), and the QZ Tray / PrintNode comparison in the
   README. Left open: screenshots/GIF of the dashboard, and a hosted demo.
5. ~~**E-commerce auto-print integration (Shopify/WooCommerce)**~~ ✅ **shipped** —
   `POST /orders` renders an order as a packing slip (stdlib PDF writer, no dependency) and
   queues it; a WooCommerce plugin (`integrations/woocommerce`) and an HMAC-verified Shopify
   order webhook feed it. See [ecommerce.md](ecommerce.md). Still open on top: a designed/
   templated document (logo, layout), carrier label pass-through as a first-class option, and
   the hosted SaaS this unblocks.
6. ~~**Multi-tenancy / child accounts**~~ ✅ **shipped** — org-scoped keys, printers and jobs, with
   `POST/GET /orgs`; and since then **org accounts**: e-mail/password users per org, `POST /login`
   with expiring session tokens, org-scoped key and user self-management, and a dashboard sign-in
   that no longer needs the root token. See [Accounts and login](api.md#accounts-and-login).
   And since then the plumbing a paid deployment needs: opt-in
   [self-signup](api.md#self-signup), [password reset](api.md#password-reset) by e-mail, account
   removal, an org Settings page, and monthly [job quotas](api.md#quotas). Still open on top of
   it: **billing** (the quota is enforced, nothing charges for it), plans/tiers, org deletion.
7. ~~**PrintNode API compatibility layer**~~ ✅ **shipped** — `/whoami`, `/computers`, `/printers`,
   `POST|GET|DELETE /printjobs` and `/printjobs/{set}/states` in their JSON shapes, selected by auth
   scheme (HTTP Basic → compat, `Bearer` → ours), so a client with a configurable base URL needs one
   line changed. See [printnode-compat.md](printnode-compat.md). Left open: plugins that hardcode
   their hostname still need a proxy/DNS override, state *history* is single-entry, and scales /
   credits / child accounts are not portable.
8. ~~**File backend ("virtual print server")**~~ ✅ **shipped** — a `file:///path/to/dir` printer
   target in `agent.ini`: the agent writes the job to disk (`job-<id>.pdf` / `.prn`) instead of
   printing it, for archival and Paperless-style consume folders. Agent-only change — the server
   sees a normal printer. See [agent.md](agent.md#file-output-virtual-print-server). Left open:
   no filename template (job id only) and no retention/rotation.
9. ~~**Star CloudPRNT protocol endpoint**~~ ✅ **shipped** — `/cloudprnt/<client-key>` answers their
   POST poll / GET job / DELETE confirm, so the printer *is* the agent and nothing is installed at
   the site. Enrols itself by MAC as a raw-only printer; jobs take the ordinary queue, quota,
   history and webhooks. See [cloudprnt.md](cloudprnt.md). Left open: **verification on real
   hardware** (it was built from the spec, and the firmware-dependent bits — optional `jobToken`,
   `printingInProgress`, the exact confirmation code — are where it will need a fix), DELETE-only
   confirmation (no `deleteMethod: GET`), no MQTT transport, no peripherals/client actions, no
   capability discovery.
10. **Scales API** — USB HID scales at packing stations (NetSuite/Dynamics workflows read weight
    through PrintNode's client). Biggest hardware gap, but niche; needs agent-side HID + a push channel.
11. **ESC/POS receipt rendering/templating** — images/QR/receiptline-style markdown → ESC-POS. Where
    Home-Assistant/hobbyist efforts stall today; expensive to do well, raw passthrough already works.
12. **Code-signed Windows agent installer** — the one blocker that needs a certificate, not code.

Explicitly parked: MCP/LLM printing (no measurable audience yet), email-to-print (not even
PrintNode has it), Magento-specific work (generic webhook intake covers it), browser-side silent
printing à la QZ/PrintNode-JS (different architecture; our answer is server-side jobs).
