<p align="center"><img src="docs/logo.png" width="110" alt="printpapi logo"></p>

# printpapi

**Self-hosted, open-source alternative to [PrintNode](https://www.printnode.com/).** Submit a
print job to a small HTTP API from anywhere; an agent on the machine with the printer picks it
up and prints it — documents **and** label printers (Zebra/ZPL, Bixolon, thermal).

- Agent **polls outbound** — printers behind NAT/firewall work with **no inbound ports**
- Server is Python **standard library only** — no framework, nothing to install
- React dashboard (live queue, job history, test print, API keys), REST API, SQLite job queue
- Cross-platform agent: Windows (`win32print` + SumatraPDF) and Linux (CUPS)
- Print options per job (copies, duplex, paper size, tray, color, page ranges) +
  capability discovery per printer
- Multi-tenant: orgs isolate API keys, agents, printers and jobs
- Fleet monitoring: `GET /computers` + online/offline webhooks, Prometheus `/metrics`
- Retry-safe submits (`idempotency_key`) and job deadlines (`expire_after`)
- MIT licensed · 168 tests

## Get started

**Server** (Python 3.9+):

```bash
git clone https://github.com/LukiPrince/printpapi && cd printpapi
PRINTAPI_TOKEN=change-me python -m app.server
```

Open the dashboard at http://localhost:3460 and paste the token. Or use the prebuilt
Docker image — no build needed (amd64 + arm64):

```bash
docker run -e PRINTAPI_TOKEN=change-me -p 3460:3460 ghcr.io/lukiprince/printpapi
```

Or `docker compose up -d` (persists the DB in a volume — see [`docker-compose.yml`](docker-compose.yml)).
Prefer to build it yourself? `docker build -t printpapi .`

**Agent** — on the machine with the printers:

1. Copy `agent/print_agent.py` there (Windows also needs `pywin32` + SumatraPDF for PDF;
   Linux uses CUPS).
2. Create `agent.ini` next to it:

   ```ini
   [agent]
   server_url = http://yourserver:3460
   api_key    = your-agent-key
   name       = office-pc
   printers   = Zebra GK420d; HP LaserJet|pdf
   ```

3. Run `python print_agent.py`.

First print: dashboard → **Devices** → **Test print**.

## Documentation

| | |
|---|---|
| [Server](docs/server.md) | how it works, configuration, Docker, dashboard (+ rebuilding it), API keys, security |
| [Agent](docs/agent.md) | install, `agent.ini`, printer syntax, labels vs PDF |
| [HTTP API](docs/api.md) | endpoints, auth, content types, job lifecycle |
| [Roadmap](docs/roadmap.md) | what's planned for v2 |

Contributions welcome — see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE).
