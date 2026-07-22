# Roadmap

v1 is deliberately small (see [CONTRIBUTING.md](../CONTRIBUTING.md) for the YAGNI rules).
Candidates for v2, roughly in order of pull:

- **Job options** — ✅ `copies` shipped (`POST /jobs` `copies`, default 1, cap 100). Still open:
  duplex, tray, page range (driver-specific — needs the per-backend options matrix).
- **Webhooks** — POST to a configured URL on job state changes, so clients don't poll.
- ~~**Job cancel** — `DELETE /jobs/{id}` while still queued.~~ ✅ shipped (`cancelled` state;
  `409` once claimed, no mid-print interrupt).
- **Printer capabilities** — agent reports paper sizes / duplex support at registration.
- **`/metrics`** — Prometheus text format for queue depth, job outcomes, agent liveness.
- **Code-signed Windows agent installer** — the one blocker that needs a certificate, not code.
