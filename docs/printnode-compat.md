# PrintNode-compatible API

printpapi answers on a second set of endpoint shapes that match the ones PrintNode's clients speak,
so software already written against PrintNode — the official SDKs, an ERP connector, a Zapier/Make
node — can be pointed at a printpapi base URL and keep working. Same server, same agents, same job
queue; only the JSON shape differs.

> *PrintNode is a trademark of PrintNode Ltd. printpapi is not affiliated with, endorsed by, or
> sponsored by PrintNode.* This layer was implemented from the publicly documented API surface. No
> PrintNode code or documentation text is contained in it.

## How it is selected

By the **authentication scheme**, not by a URL prefix:

| Header | You get |
|---|---|
| `Authorization: Basic <base64(key:)>` | the PrintNode-compatible shapes described here |
| `Authorization: Bearer <key>` | printpapi's own [HTTP API](api.md) |

That is not a trick — it is how their clients already authenticate: the API key goes in as the HTTP
Basic **username**, with an empty password. So there is nothing to enable and nothing to configure.
Set the client's base URL to your server and the API key to any printpapi client key (or the
bootstrap `PRINTAPI_TOKEN`), and requests arrive on this layer by themselves.

The password half is ignored. A key that is unknown or revoked gets `401` with
`{"code": "Unauthorized", "message": "…"}`.

```bash
# whoami with a printpapi client key — note the trailing colon (empty password)
curl -s -u '<client-key>:' http://localhost:3460/whoami
```

Authorization is the same as everywhere else in printpapi: an issued key sees **only its own org**,
the bootstrap token spans all of them (see [Multi-tenancy](api.md#multi-tenancy)). A foreign printer
id is `400 unknown printer`, a foreign job id is simply absent from the list.

## Endpoints

| Method & path | Answers with |
|---|---|
| `GET /whoami` | the account object — clients call this to validate credentials |
| `GET /computers` | array of computer objects (printpapi agents) |
| `GET /computers/{set}` | the same, restricted to those computer ids |
| `GET /computers/{set}/printers` | printers belonging to those computers |
| `GET /printers` | array of printer objects, each with its computer nested |
| `GET /printers/{set}` | the same, restricted to those printer ids |
| `POST /printjobs` | `201` and the new print job id as a **bare integer** |
| `GET /printjobs` | array of print job objects (newest first; `?limit=` 1–500, default 50) |
| `GET /printjobs/{set}` | the same, restricted to those job ids |
| `GET /printjobs/{set}/states` | array of state arrays, one array per job |
| `DELETE /printjobs/{set}` | number of jobs cancelled |

`{set}` is the id-set notation the clients build: `10`, `10,12`, `5-9`, or a mix of those. A set is
capped at 500 ids; a reversed or oversized range is a `400`.

## Submitting a print job

```bash
curl -s -u '<client-key>:' -X POST http://localhost:3460/printjobs \
     -H 'Content-Type: application/json' \
     -d '{"printerId":1,"title":"Invoice 1042","contentType":"pdf_base64",
          "content":"<base64 pdf>","source":"my app","qty":2,
          "options":{"paper":"A4","duplex":"long-edge"}}'
# -> 201
# 3
```

Field mapping onto [`POST /jobs`](api.md#submitting-a-job):

| Their field | Becomes | Note |
|---|---|---|
| `printerId` | `printer_id` | unknown/foreign printer → `400` |
| `contentType` | `type` | `pdf_base64`, `pdf_uri`, `raw_base64`, `raw_uri` — anything else `400` |
| `content` | `content`, or `url` for the `*_uri` types | they carry the URL in `content`, we take it in `url` |
| `title` | `title` | optional here |
| `qty` (or `options.copies`) | `copies` | integer `1`–`100`, else `400` |
| `expireAfter` | `expire_after` | seconds; a job past its deadline fails instead of printing |
| `options.paper`, `.bin`, `.color`, `.duplex`, `.pages` | `options.*` | applied by the agent |
| `options.*` (anything else) | dropped | see below |
| `source` | — | not stored; every job reports `"printpapi"` back |

Two deliberate leniencies, because the point of this layer is that an unmodified client works:

- **Unknown option keys are dropped, not rejected.** A client sends its whole option set with every
  job (`rotate`, `dpi`, `fit_to_page`, `nup`, …); a `400` over one printpapi does not implement
  would break printing for no gain. Everything we *do* map is validated normally — a bad *value*
  still `400`s.
- **Options on a raw job are dropped whole.** ZPL/ESC-POS carries its own layout, no renderer ever
  sees it, so paper/duplex are meaningless there ([gotcha #1](../HANDOFF.md#3-hard-won-gotchas--do-not-rediscover-these)).

## Job states

| printpapi | here |
|---|---|
| `queued` | `queued` |
| `claimed` | `sent` |
| `done` | `done` |
| `failed` | `error` |
| `failed` (deadline passed) | `expired` |
| `cancelled` | `deleted` |

`GET /printjobs/{set}/states` returns one array per job, as the clients expect, but each array holds
a **single** entry: printpapi keeps a job's current state, not its transition history. Clients that
poll this waiting for a terminal state work unchanged; a client that renders the full timeline sees
one row.

## What is not portable

- **Scales.** Reading a USB HID scale needs agent-side hardware support printpapi does not have.
  `…/scales` endpoints are not implemented.
- **Credits, billing, subscriptions, child accounts.** `whoami` reports `credits: null` and no
  subscriptions — self-hosted printing has no meter. Use [orgs](api.md#multi-tenancy) where you
  would have used child accounts; the `X-Child-Account-By-*` headers are ignored.
- **Paper dimensions.** The agent discovers paper *names* (`A4`, `Letter`), not their extents, so
  every entry in `capabilities.papers` maps to `null`. Likewise `dpis`, `medias`, `nup` and
  `printrate` are empty and `collate` is `false` — unknown, reported as unknown rather than invented.
- **Computer network details.** `inet`, `inet6`, `jre` and `version` are null/empty: our agent
  reports its name and its printers, nothing about the host.
- **Client-side / browser printing** and the desktop client's own control endpoints. printpapi's
  agent is a poll-and-print service; there is no JS bridge to drive.
- **`whoami.id` is `0` for the bootstrap token** (it spans every org). An issued key reports its
  org id.

## Pointing an existing client at it

Anything with a configurable base URL — the official SDKs, an HTTP node in an automation tool, your
own code — needs one line changed and works.

Shop plugins are the harder case: many **hardcode the PrintNode hostname**, so there is no setting
to change. Two ways out, both outside printpapi:

1. **Reverse proxy / DNS override** on the machine that runs the shop: resolve their hostname to
   your printpapi server and terminate TLS there. Works, but it hijacks that name for the whole
   host — do it on a dedicated box, and never on a machine that still uses the real service.
2. **Patch the base URL** in the plugin (check its license first) or use printpapi's own
   integrations instead: the [WooCommerce plugin](ecommerce.md) and the Shopify order webhook
   already do order → packing slip natively.

Printer ids are printpapi's, not the ones a previous PrintNode account handed out — re-select the
printer in the client after switching (`GET /printers` lists them).
