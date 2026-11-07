# Migrating from v2 (Python) to v3 (Rust)

v3 replaces the Python server with a single Rust binary and adds a Rust agent next to the Python
one. The HTTP API, the JSON it returns, the environment variables and the database are the same.
Most installs upgrade by swapping the server and changing nothing else.

This guide covers the server, the agents, the few defaults that changed, and how to go back if
you need to.

## What stays the same

- **The database.** v3 opens a `printpapi.db` written by v2.5.1 or older without a migration step.
  It adds a few indexes on first start, which v2.5.1 ignores, so the file still opens in v2.5.1
  afterwards. Orgs, users, passwords, sessions, API keys, agents, printers and the job history all
  carry over.
- **The environment.** `PRINTAPI_TOKEN`, `PRINT_DB`, `PRINT_PORT`, `PRINTAPI_SIGNUP`, `PUBLIC_URL`,
  `PRINTAPI_PLANS`, `PRINTAPI_BILLING_SECRET`, `LOG_REQUESTS` and the `SMTP_*` variables mean what
  they meant before.
- **The API.** Same routes, status codes, error texts and JSON (formatted the way Python's
  `json.dumps` did it, so a script that greps `"state": "done"` still matches). The
  PrintNode-compatible layer, Star CloudPRNT, the Shopify webhook, the WooCommerce plugin and the
  billing webhook need no changes. `tools/parity/parity.py` replays 785 scripted requests against
  v2.5.1 and v3 and compares every answer.
- **Agents.** A v2 Python agent keeps working against a v3 server, and the v3 Rust agent works
  against a v2 server. You can upgrade the server and the agents on different days.

## Before you start

Back up the database. Stop the server first so SQLite has folded its `-wal` file into the main
file, then copy `printpapi.db`. If the server cannot be stopped, copy `printpapi.db`,
`printpapi.db-wal` and `printpapi.db-shm` together.

With Docker and the named volume from `docker-compose.yml`:

```bash
docker compose stop
docker run --rm -v printpapi_printpapi-data:/data -v "$PWD":/backup busybox \
  cp /data/printpapi.db /backup/printpapi-v2-backup.db
```

(`docker volume ls` shows the exact volume name; compose prefixes it with the project name.)

## Upgrade the server

### Docker

The image name stays `ghcr.io/lukiprince/printpapi`. v3 is tagged `3.0.0`, `3.0` and `latest`; the
last Python image stays available as `2.5.1`.

```bash
docker compose pull
docker compose up -d
```

If your compose file pins a version, change the tag to `3.0` (or `3.0.0`) first. Nothing else in
the file changes: same port, same `PRINT_DB=/app/data/printpapi.db`, same volume.

The new image is distroless: it holds the binary and CA certificates, nothing else. There is no
shell, so `docker exec -it <container> sh` no longer works. Read logs with `docker logs`, and reach
the database through the volume (as in the backup above). The container still runs as root, so a
volume the Python image created stays writable.

### Binary

If you ran `python -m app.server` directly, replace it with the `printpapi` binary from the
[release page](https://github.com/LukiPrince/printpapi/releases/latest) (`printpapi-server-<platform>`
archive), or build it with `cargo build --release -p printpapi-server`. Run it with the same
environment and the same `PRINT_DB` path:

```bash
PRINTAPI_TOKEN=... PRINT_DB=/var/lib/printpapi/printpapi.db ./printpapi
```

Python is no longer needed on the server. A systemd unit for the binary:

```ini
[Unit]
Description=printpapi server
After=network-online.target

[Service]
ExecStart=/opt/printpapi/printpapi
Environment=PRINTAPI_TOKEN=change-me
Environment=PRINT_DB=/var/lib/printpapi/printpapi.db
Restart=always
User=printpapi

[Install]
WantedBy=multi-user.target
```

`printpapi --version` prints the version, which is a quick way to confirm which build a service
actually runs.

## Upgrade the agents

There is no deadline here. Python agents keep working; switch machines one at a time, when it
suits you.

On each machine:

1. Stop the Python agent (the Task Scheduler task, systemd unit or launchd job that runs
   `print_agent.py`).
2. Download the `printpapi-agent-<platform>` archive from the release page and unpack it, for
   example into the folder that held `print_agent.py`.
3. Put the **existing** `agent.ini` next to the `printpapi-agent` executable. Keep `name` and
   `api_key` as they are: the server then recognises the machine as the same computer, its printers
   keep their ids, and anything that refers to a `printer_id` (the WooCommerce plugin settings, a
   Shopify webhook URL, your scripts) keeps working. A new name or key would register a new
   computer with new printer ids.
4. On Windows, keep `SumatraPDF.exe` next to the executable for PDF jobs. `pywin32` is not needed
   any more.
5. Point the autostart entry at the new executable instead of the Python interpreter. On Windows,
   create the task as in [Run as a service](agent.md#run-as-a-service) with `-Force` added to
   `Register-ScheduledTask`, which replaces the old task (one made with `schtasks /create` also
   stops after 72 hours). With systemd, set `ExecStart=/opt/printpapi/printpapi-agent` in the unit.
6. Start it and check the dashboard's **Devices** page: the computer shows as online under its old
   name.

Do not run the Python agent and the Rust agent with the same key at the same time. Each job still
prints once, but you cannot tell which machine will print it.

**Locked-down Windows.** Smart App Control, WDAC and AppLocker block the Rust agent because the exe
is not code-signed yet. Leave the Python agent on those machines; it runs through the PSF-signed
`pythonw.exe` as before. See [Locked-down Windows](agent.md#locked-down-windows).

## Defaults that changed

Three defaults are stricter in v3. Check them before you upgrade.

**New agents need an issued key.** In v2 any value in `api_key` enrolled a new agent into the
default org. v3 accepts a key issued on the dashboard's **Keys** page (or with `POST /apikeys`), or
the root token; a made-up key gets `401 unknown agent key`. Agents already registered with a
self-chosen key keep working. To keep the v2 rule, set `PRINTAPI_OPEN_ENROLLMENT=1` on the server.

**Revoking a key stops its agent.** Revoking an issued key used to stop client calls only; the agent
registered with it kept polling. In v3 the agent gets `401` on its next request, a long-poll that
is already waiting included. Give each machine its own key so that revoking one affects one
machine.

**Private destinations are refused when signup is open.** With `PRINTAPI_SIGNUP=open`, payload URLs
(`*_uri`, `*_uri_post`), job `callback_url`s and org `event_url`s may only reach public addresses:
no loopback, no LAN, no link-local or cloud metadata address. A server with signup closed (the
self-hosted default) still fetches from anywhere, as v2 did. `PRINTAPI_PRIVATE_URLS=allow` or
`deny` overrides the default either way.

## Smaller differences

These only matter if a client relied on edge cases.

- **Malformed requests get an answer.** Where v2 crashed on a request and dropped the connection (a
  JSON body that is not an object, a number where a string belongs, a non-ASCII token), v3 answers
  with a JSON `400` or `401`. A missing agent `name` is `400 name required`; v2 answered `409`.
- **An unparseable `Content-Length`** is still a `400`, but the HTTP layer sends it before routing,
  so the body is empty instead of the route's JSON error.
- **Whitespace around header values is trimmed**, as RFC 9110 specifies. A `PRINTAPI_TOKEN` with
  leading or trailing spaces is used trimmed. An agent key that ends in whitespace no longer
  authenticates; issue that machine a new key.
- **Outbound fetches have limits.** At most 256 payload fetches run at once, each gets 120 s, and a
  webhook or event POST gets 15 s, so jobs pointed at a slow URL cannot stall the server.
- **Request bodies must arrive within 120 s.**
- **Jobs reach a waiting agent immediately** instead of within the next one-second database poll.
  The long-poll's response is unchanged.

## Check the upgrade

- `GET /health` answers `{"ok": true, ...}`.
- You can sign in to the dashboard with your existing account or the root token.
- **Devices** lists your computers as online once their agents have polled.
- A test print from **Devices** comes out of the printer.
- `GET /metrics` shows the job counts you expect from before the upgrade.

## Going back

v3 does not change the schema in a way v2.5.1 cannot read, so you can roll back with the same
database file: stop v3, start v2.5.1 against the same `PRINT_DB` (`ghcr.io/lukiprince/printpapi:2.5.1`
in Docker, or `git checkout v2.5.1 && python -m app.server`). Everything v3 wrote stays readable,
including agents enrolled with issued keys and jobs submitted after the upgrade.
