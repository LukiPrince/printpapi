<p align="center"><img src="docs/logo.png" width="110" alt="printpapi logo"></p>

# printpapi

**Self-hosted, source-available alternative to [PrintNode](https://www.printnode.com/).** Submit a
print job to a small HTTP API from anywhere; an agent on the machine with the printer picks it
up and prints it — documents **and** label printers (Zebra/ZPL, Bixolon, thermal).

- Agent **polls outbound** — printers behind NAT/firewall work with **no inbound ports**
- Server is Python **standard library only** — no framework, nothing to install
- React dashboard (live queue, job history, test print, API keys), REST API, SQLite job queue
- Cross-platform agent: Windows (`win32print` + SumatraPDF), Linux and macOS (CUPS)
- Print options per job (copies, duplex, paper size, tray, color, page ranges) +
  capability discovery per printer
- **Shop order → packing slip**: WooCommerce plugin, Shopify webhook, built-in PDF renderer
- **Virtual print server**: a `file://` printer archives the job to a directory instead of paper
  ([docs](docs/agent.md#file-output-virtual-print-server))
- Multi-tenant: orgs isolate API keys, agents, printers and jobs — each with its own
  **e-mail/password logins** for the dashboard ([docs](docs/api.md#accounts-and-login))
- Run it as a service: optional **self-signup**, password reset by e-mail, and a per-org
  **monthly job quota** ([docs](docs/api.md#quotas))
- Fleet monitoring: `GET /computers` + online/offline webhooks, Prometheus `/metrics`
- Retry-safe submits (`idempotency_key`) and job deadlines (`expire_after`)
- **PrintNode-compatible API layer** — point an existing SDK/integration at your server
  ([docs](docs/printnode-compat.md))
- Source available ([Elastic License 2.0](#license): self-host freely, don't resell it as a service) · 269 tests

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
   Linux and macOS use CUPS — on macOS read the [raw-printing note](docs/agent.md#macos)).
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

## Why not QZ Tray or PrintNode?

**[QZ Tray](https://qz.io/)** prints from the *browser*: JavaScript in your page talks to a Java
app on the same desktop over a localhost WebSocket. Different architecture, different trade-offs:

|  | QZ Tray | printpapi |
|---|---|---|
| Trigger | browser JS on the operator's machine | any HTTP client — backend, n8n/Zapier, cron |
| Silent printing | needs a **code-signing certificate** per deployment (self-signed → a prompt on every print) | nothing to sign; the agent prints what the server queues |
| Transport | `wss://localhost` from the page ([Chrome's Local Network Access](https://developer.chrome.com/blog/local-network-access) tightens this) | agent **polls outbound**, no listener, no localhost anything |
| Headless / service | [open since 2016](https://github.com/qzind/tray/issues/116) | systemd / launchd / Task Scheduler ([docs](docs/agent.md#run-as-a-service)) |
| Queue when the PC is off | none — [cloud queue open since 2021](https://github.com/qzind/tray/issues/825) | jobs wait in SQLite, print on reconnect (or expire) |
| Direct USB/HID, scales | yes | no — printers only |

QZ Tray is the better fit if the print *must* start from the user's browser session with no server
in the loop. If printing is something your **backend or automation** does, printpapi's model is
simpler: no certificates, no browser, one HTTP call.

**[PrintNode](https://www.printnode.com/)** is the closest match feature-wise — printpapi is a
self-hosted alternative to it: same poll-from-behind-NAT model, same job/printer/computer API
shape, without the per-printer monthly fee or sending your documents through someone else's cloud.
It has things we don't (scales, a hosted SLA — see the [roadmap](docs/roadmap.md)). Software you
already wrote against their API can be pointed here: send HTTP Basic auth instead of a bearer token
and the server answers in their JSON shapes
([PrintNode-compatible API](docs/printnode-compat.md)).

*PrintNode is a trademark of PrintNode Ltd. printpapi is not affiliated with, endorsed by, or
sponsored by PrintNode.*

## Documentation

| | |
|---|---|
| [Server](docs/server.md) | how it works, configuration, Docker, dashboard (+ rebuilding it), API keys, security |
| [Agent](docs/agent.md) | install, `agent.ini`, printer syntax, labels vs PDF, per-printer setup, service install |
| [HTTP API](docs/api.md) | endpoints, auth, content types, job lifecycle |
| [PrintNode-compatible API](docs/printnode-compat.md) | point an existing PrintNode client at printpapi |
| [Recipes](docs/recipes.md) | print from n8n, Zapier, Make, curl — in one HTTP node |
| [E-commerce](docs/ecommerce.md) | WooCommerce plugin, Shopify webhook, `POST /orders` |
| [Roadmap](docs/roadmap.md) | what's planned for v2 |

Contributions welcome — see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[Elastic License 2.0](LICENSE) — **source available**, not OSI open source. In plain terms:

- ✅ Run it for yourself, your company, or your clients' printing — free of charge, commercial use
  included. Read it, modify it, redistribute it.
- ✅ An IT service provider, integrator or agency may operate it **for an identified client** and
  charge for installation, hosting, administration and support (an explicit additional permission
  on top of ELv2 — see the top of [LICENSE](LICENSE)).
- ❌ Offering it to the public as your own hosted or managed print service. That is the part the
  author intends to offer.

Two scope notes:

- **Everything up to and including v1.4.0 was released under the MIT License and stays MIT**
  ([docs/licenses/MIT-until-v1.4.0.txt](docs/licenses/MIT-until-v1.4.0.txt)). The current license
  applies to later versions.
- The [WooCommerce plugin](integrations/woocommerce) is **GPL-2.0-or-later**, because WordPress
  plugins must be GPL-compatible. It only calls the HTTP API.

Not sure whether your use is covered? [Open an issue](https://github.com/LukiPrince/printpapi/issues)
and ask — the answer is almost always yes.
