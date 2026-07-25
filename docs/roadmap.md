# Roadmap

v1 is deliberately small (see [CONTRIBUTING.md](../CONTRIBUTING.md) for the YAGNI rules).
Candidates for v2, roughly in order of pull:

- ~~**Job options**~~ ✅ shipped: `copies` (default 1, cap 100) and `options` on pdf jobs —
  `duplex`, `paper`, `bin` (tray), `color`, `pages` — mapped to SumatraPDF `-print-settings` /
  CUPS `lp -o` by the agent. See [api.md](api.md#submitting-a-job).
- ~~**Webhooks** — POST to a configured URL on job state changes, so clients don't poll.~~ ✅ shipped
  (per-job `callback_url`; best-effort retried delivery on terminal states; unsigned).
- ~~**Job cancel** — `DELETE /jobs/{id}` while still queued.~~ ✅ shipped (`cancelled` state;
  `409` once claimed, no mid-print interrupt).
- **Printer capabilities** — agent reports paper sizes / duplex support at registration.
- ~~**`/metrics`** — Prometheus text format for queue depth, job outcomes, agent liveness.~~ ✅ shipped.
- **Code-signed Windows agent installer** — the one blocker that needs a certificate, not code.
