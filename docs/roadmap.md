# Roadmap

v1 is deliberately small (see [CONTRIBUTING.md](../CONTRIBUTING.md) for the YAGNI rules).
Candidates for v2, roughly in order of pull:

- **Job options** — copies, duplex, tray, page range (PrintNode's `options` object).
- **Webhooks** — POST to a configured URL on job state changes, so clients don't poll.
- **Job cancel** — `DELETE /jobs/{id}` while still queued.
- **Printer capabilities** — agent reports paper sizes / duplex support at registration.
- **`/metrics`** — Prometheus text format for queue depth, job outcomes, agent liveness.
- **Code-signed Windows agent installer** — the one blocker that needs a certificate, not code.
