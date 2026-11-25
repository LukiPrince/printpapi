# HTTP API

All endpoints speak JSON over HTTP. Authentication is a bearer token:
`Authorization: Bearer <token>`.

**Roles:**

- *client* — the bootstrap `PRINTAPI_TOKEN`, any active issued key (see
  [API keys](server.md#api-keys)), **or** a session token from `POST /login`. An issued key and a
  session only ever see their own org; the bootstrap token spans all of them — see
  [Multi-tenancy](#multi-tenancy). What an issued key may do depends on its scope, see
  [Key rights](#key-rights); the table below names the right each route needs.
- *manage* — root **or** a session (an account login): the org's keys, users and settings. A
  machine key is deliberately excluded, so a leaked integration key cannot issue itself a
  successor — see [Accounts and login](#accounts-and-login).
- *root* — the bootstrap `PRINTAPI_TOKEN` only (orgs, and the first user of an org)
- *agent* — the per-agent key (bound to the agent name on first contact)

Token comparison is constant-time (`hmac.compare_digest`).

## Endpoints

| Method & path | Auth | Purpose |
|---|---|---|
| `GET /` | none | Web dashboard (static bundle embedded in the binary; data fetched with the token) |
| `GET /health` | none | Liveness check, plus whether this server offers `signup` and `password_reset` |
| `GET /metrics` | client (`Look`) | Prometheus text: job counts by state, agent/printer liveness |
| `POST /jobs` | client (`Submit`) | Submit a job → `{job_id}` |
| `GET\|POST /labels`, `GET\|PUT\|DELETE /labels/{id}` | client (`Look` to read, `Edit` to write) | [Label templates](labels.md): where the content sits on a label; jobs of type `label_*` render through them |
| `POST /receipts/preview` | client (`Submit`) | Draw a [receipt](receipts.md) as a PNG without printing it; one in flight per org, two per server (`503`) |
| `GET\|PUT\|DELETE /printers/{id}/receipt-profile` | client (`Look` to read, `Edit` to write) | The [receipt profile](receipts.md#printer-profile) of a printer: paper, width, text mode, cutter, output |
| `POST /orders` | client (`Submit`) | Render an order as a packing slip and print it → `{job_id}` ([e-commerce](ecommerce.md)) |
| `POST /integrations/shopify/orders` | HMAC + key (`Submit`) | Shopify order webhook (key in the URL, signature in the header) |
| `GET /jobs` | client (`Look`) | Recent job history. With [retention](server.md#retention) set, old jobs lose payload and title (`title` is `null`), and rows from before the current month disappear after at least 7 days |
| `GET /jobs/{id}` | client (`Look`) | One job's state: `queued` \| `claimed` \| `done` \| `failed` \| `cancelled` |
| `GET /jobs/{id}/preview` | manage | The [receipt](receipts.md#the-preview-of-a-job-in-the-history) of a job as it was printed, a PNG. `404` while the operator API is on |
| `DELETE /jobs/{id}` | client (`Submit`) | Cancel a still-`queued` job (`409` once claimed, `404` if unknown) |
| `GET /printers` | client (`Look`) | Registered printers + online/offline + capabilities |
| `GET /computers` | client (`Computers`) | Registered agents + online/offline + printer count |
| `POST /login` | none | E-mail + password → `{token, expires_at, org_id, user_id}` |
| `POST /signup` | none | Self-serve org + first account + session (`403` unless the operator opened it) |
| `POST /password/reset` | none | Mail a one-shot reset token — always `{"ok": true}` |
| `POST /password/reset/confirm` | none | `{token, password}` → sets it and logs every browser out |
| `POST /logout` | session | End this session server-side |
| `GET /me` | any | `{kind: root\|session\|key, org_id}` (+ `email`, `user_id` for a session, `scope` for a key that is not `full`) |
| `PUT /me/password` | session | `{current, new}` — changes it and logs every browser out |
| `POST /users` | manage | Add a user to the caller's own org → `{id, email, org_id}` |
| `GET /users` | manage | Users of the caller's org (root: all orgs) |
| `DELETE /users/{id}` | manage | Remove an account (never your own, never an org's last one) |
| `POST /orgs/{id}/users` | root | Create an org's first user |
| `POST /orgs` | root | Create an org → `{id, name}` |
| `GET /orgs` | root | List orgs (`event_url`, `shopify_secret_set` — never the secret itself) |
| `GET /orgs/{id}` | manage | One org's settings + `plan`, `job_quota` and `jobs_this_month` |
| `PUT /orgs/{id}` | manage | Set/clear `event_url`, `shopify_secret`; `job_quota` and `plan` are root-only |
| `DELETE /orgs/{id}` | root | Remove an org and everything in it (never the default org) |
| `GET /plans` | client (`Look`) | The [billing](billing.md) plan catalogue + the caller's current plan |
| `POST /billing/webhook` | HMAC | The payment provider's callback: this org is on that plan now |
| `POST /apikeys` | manage | Issue a client key, optionally with a `scope` → `{id, label, org_id, key}` (+ `scope` unless `full`; key shown once) |
| `GET /support-access` | manage | The [support access](#support-access) grants of the caller's org (root: `?org=`) |
| `GET /apikeys` | manage | List keys with their org and, unless `full`, their `scope` (never the secret) |
| `PUT /apikeys/{id}` | manage | Change a key's rights: `{"scope": "agent"}` → `{ok, scope}` |
| `DELETE /apikeys/{id}` | manage | Revoke a key |
| `POST /agent/register` | agent (`Agent`) | Declare name + printers → `{computer_id, printer_ids}` |
| `GET /agent/jobs` | agent | Long-poll for a job (204 on timeout); with the request header `X-Printapi-Features: job-uid` the answer also has `job_uid` |
| `GET /agent/jobs/{id}/payload` | agent | Download the job's bytes |
| `POST /agent/jobs/{id}/result` | agent | Report `{ok, error?, job_uid?}` |
| `POST /agent/results` | agent (strict) | A result delivered late, by job id and `job_uid` → `{ok, state}` ([Job lifecycle](#job-lifecycle)) |
| `POST\|GET\|DELETE /cloudprnt/{key}` | client key (`CloudPrnt`) | [Star CloudPRNT](cloudprnt.md): the printer polls, downloads and confirms here itself |

Request bodies are capped at 32 MB.

An `Authorization: Basic` header instead of `Bearer` switches the same server to the
**[PrintNode-compatible shapes](printnode-compat.md)** (`/whoami`, `/printjobs`, and PrintNode's
JSON for `/printers` and `/computers`), so an existing client can be pointed here unchanged. Same
keys, same orgs; everything below describes the `Bearer` API.

## Key rights

Every issued key has one **scope**: `full` (the default, and what every key issued before rights
existed is), `agent`, `cloudprnt`, `print` or `read`. The scope narrows what a **machine key** may
call. It does not touch the bootstrap token, browser sessions or agent keys that were never issued
(self-chosen v2 keys). Managing keys, users, the org and the support log (`/apikeys`, `/users`,
`/orgs`, `/support-access`, including `event_url` and `shopify_secret`) stays with root and sessions:
every key, a full one included, still gets `401` there. So does `GET /jobs/{id}/preview`, the
picture of a receipt job.

| Right | Routes | full | agent | cloudprnt | print | read |
|---|---|---|---|---|---|---|
| (none) | `GET /me` | yes | yes | yes | yes | yes |
| `Computers` | `GET /computers` | yes | yes | no | yes | yes |
| `Look` | `GET /printers`, `/printers/{id}/receipt-profile`, `/labels`, `/labels/{id}`, `/jobs`, `/jobs/{id}`, `/metrics`, `/plans`; every PrintNode `GET` (`/whoami`, `/computers`, `/printers`, `/printjobs`) | yes | no | no | yes (jobs: own only) | yes |
| `Submit` | `POST /jobs`, `POST /receipts/preview`, `POST /orders`, the Shopify webhook, `DELETE /jobs/{id}`; PrintNode `POST /printjobs`, `DELETE /printjobs/{set}` | yes | no | no | yes (cancel: own only) | no |
| `Edit` | `POST /labels`, `PUT /labels/{id}`, `DELETE /labels/{id}`, `PUT /printers/{id}/receipt-profile`, `DELETE /printers/{id}/receipt-profile` | yes | no | no | no | no |
| `Agent` | `POST /agent/register`, `GET /agent/jobs`, `GET /agent/jobs/{id}/payload`, `POST /agent/jobs/{id}/result`, `POST /agent/results` | yes | yes | no | no | no |
| `CloudPrnt` | `POST\|GET\|DELETE /cloudprnt[/{key}]` | yes | no | yes | no | no |

A scope this version does not know (a newer server wrote it) allows nothing, `GET /me` excepted.

- **Issuing and changing.** `POST /apikeys` takes an optional `scope`; absent or `null` is `full`,
  anything but one of the five words is `400` `scope must be one of: full, agent, cloudprnt, print,
  read` and issues nothing. The answer carries `"scope"` only when it is not `full`, so "no scope"
  always means "a full key", also against an older server. `GET /apikeys` and `GET /me` do the same.
  `PUT /apikeys/{id}` with `{"scope": "agent"}` changes it later (root or a session) and answers
  `{"ok": true, "scope": "agent"}`; a missing or invalid scope is `400`, an unknown, revoked or
  foreign key `404`. A manager may set any scope, a wider one too: they can issue full keys anyway,
  and it is the undo for a wrong narrowing.
- **When a change counts.** Every request that arrives after the change is judged by the new scope.
  A request already accepted (body read, payload being fetched) may still queue its job, exactly as
  with revoking a key. Narrowing the key of a computer ends its waiting long-poll with `401` at once
  and wakes only that computer. Jobs queued for it wait until the right is back, or until they
  expire.
- **A job already handed out.** If an agent's key loses the agent right while a job is `claimed` by
  that agent, the payload download and the result of exactly that job still work; the next poll is
  `401`. Otherwise the report would be refused, the job requeued and printed twice. A result the
  agent kept in its [journal](agent.md#the-print-journal) needs the right: `POST /agent/results` is `401` for a
  narrowed key, and the agent sends it when the right is back. A revoked key
  gets nothing, and a CloudPRNT device has no such exception (no key is bound to a device): take the
  key out of the printer before you narrow or revoke it, or a job it is printing at that moment can
  print again later.
- **Own jobs.** A `print` key sees and cancels only the jobs submitted with that key, on both API
  layers, orders and Shopify jobs included. Another key's job reads exactly like a missing one:
  `404` on `/jobs/{id}`, absent from `GET /jobs` and the PrintNode lists, not counted by the
  PrintNode delete. `full` and `read` keys see the whole org. `/metrics` and `/whoami` show
  org-wide counts to a print key; only job rows are restricted. Jobs submitted before this version
  have no key and no print key sees them.
- **Idempotency.** A resubmit with the same `idempotency_key` still returns the org's original
  `job_id`, even if another key made that job; a print key then gets `404` when it asks for it.
  The key space is the whole org's, so a print key can take an idempotency key before the
  integration that will use it, and that integration's job is then answered with the print key's
  job and not printed. The one key the server builds itself is refused: a print key may not send an
  `idempotency_key` that starts with `shopify-` (`400`). A shop plugin's own keys (WooCommerce
  uses `woo-<order>-<status>`) cannot be told apart from any other caller's, so give every
  integration its own unguessable or prefixed keys, and keep the key of a print key out of reach.
- **Replacing a key.** A new print key does not see the jobs submitted with the one it replaces.
  Change the rights of the old key instead if you need them.
- **Why `agent` and `cloudprnt` are two rights.** A CloudPRNT device is identified only by the MAC
  address the caller sends, no key is bound to it. With one shared right a computer's key could read,
  swallow and confirm the jobs of every Star printer of the org. Split, a computer's key cannot act as
  a Star printer, and a key from a printer's URL (which lands in access logs) cannot enrol a computer.
  One `cloudprnt` key still reaches **all** Star printers of its org, and so does a `full` key.
- **Reserved names.** An `agent` key cannot register a computer whose name starts with
  `cloudprnt-`: `401` `names starting with cloudprnt- are reserved for Star printers`. A full key,
  root and a never-issued key may, as before. Narrowing does not undo such a name: `PUT /apikeys/{id}`
  refuses (`400`) to give a key the rights `agent` alone while the computer enrolled with it is
  named `cloudprnt-...`, because that computer would share the Star printer's queue and its next
  registration would be refused. `full`, `print`, `read` and `cloudprnt` are fine.

What a key without the right gets:

| Surface | Unknown or revoked key | Known key, right missing |
|---|---|---|
| Bearer API | `401 {"error": "unauthorized"}` | `403 {"error": "this key's rights (read) do not allow this"}` |
| PrintNode layer (Basic) | `401 {"code": "Unauthorized", ...}` | `403 {"code": "Forbidden", "message": ...}` |
| Shopify webhook | `401` | `403`, before the signature is checked |
| CloudPRNT | `401` | `403`, and nothing is enrolled |
| `POST /agent/register` | `401` | `401` with a message naming the rights needed |
| other `/agent/*` | `401` | `401` |
| `/apikeys`, `/users`, `/orgs`, `/support-access`, `/jobs/{id}/preview` | `401` | `401` (no key may) |

On the Bearer routes and on PrintNode `POST /printjobs` the right is checked before the body is
parsed: a refused key with a broken JSON body gets `403`. CloudPRNT and `POST /agent/register`
parse the body first (broken JSON is `400` there), and the Shopify webhook has read the body but
checks the right before the signature. `/agent/*` stays `401` because the setup app reads
`403` there as "a proxy in front of the server refused", and `401` as "check the key or the name".
The dashboard signs a browser out on any `401` outside the management paths, which is why a missing
right is `403` elsewhere.


## Submitting a job

```bash
curl -s -X POST localhost:3460/jobs \
     -H 'Authorization: Bearer <client-key>' -H 'Content-Type: application/json' \
     -d '{"printer_id":1,"type":"raw_base64","content":"<base64 ZPL>","title":"Label #4712","copies":2}'
# -> {"job_id": 1}
```

`title` is optional and shows up in the dashboard's history.

`copies` is optional (default `1`, integer `1`–`100`) — the agent prints the job that many times.
Out of range or non-integer → `400`.

`callback_url` is optional (`http(s)` only) — the server POSTs the job's outcome there once it
reaches a terminal state. See [Webhooks](#webhooks). A non-`http(s)` scheme → `400`.

`idempotency_key` is optional (string, ≤128 chars) — **retry-safe submits**. Resubmitting the same
key inside the same org returns the *original* `job_id` and prints nothing extra, so a timed-out or
retried `POST /jobs` (order webhook redelivery, flaky network) can never double-print. Keys are
scoped per org. A key is honoured as long as its job row exists: with
[retention](server.md#retention) on, at least `max(N, 7)` days after the job finished (longer than a
webhook redelivery window), without retention for ever. Use something stable like the order id.

`expire_after` is optional (integer seconds, `1`–`2592000`) — **a deadline for the job**. Past it
the job is failed (`state: failed`, `error: "expired"`) instead of printed, so a stale shipping
label doesn't come out hours later when an offline agent reconnects. Without it, jobs wait until an
agent takes them, or, with [retention](server.md#retention) set, N days: then the job is failed with
`error: "retention"` and its `callback_url` still gets the outcome. A job with `expire_after` waits for
its own deadline instead.

`options` is optional and **pdf jobs only** (raw ZPL/ESC-POS carries its own layout — `400` on a
raw job). The agent maps them onto its backend's native flags (SumatraPDF `-print-settings` on
Windows, `lp -o` on CUPS):

```json
{"printer_id": 2, "type": "pdf_base64", "content": "<base64 PDF>",
 "options": {"duplex": "long-edge", "paper": "A4", "bin": "Tray 1",
             "color": false, "pages": "1-3,5"}}
```

| Key | Values | Maps to (Windows / CUPS) |
|---|---|---|
| `duplex` | `long-edge` \| `short-edge` \| `one-sided` | `duplexlong`… / `sides=two-sided-long-edge`… |
| `paper` | paper name, e.g. `A4`, `Letter` | `paper=` / `media=` |
| `bin` | tray name (driver-specific) | `bin=` / `InputSlot=` |
| `color` | `true` \| `false` | `color`/`monochrome` / `print-color-mode=` |
| `pages` | ranges like `1-3,5` | print settings / `page-ranges=` |
| `orientation` | `auto` \| `portrait` \| `landscape` | `portrait`/`landscape`, nothing for `auto` / no effect |

Unknown keys or invalid values → `400`. Values are what the printer driver understands — check
the printer's `capabilities` in `GET /printers` (`papers`, `bins`, `duplex`, `color`; reported
best-effort by the agent at registration via the Windows driver or CUPS `lpoptions`, `null` when
unavailable) for what it supports. An option the driver doesn't support is silently ignored by
it. On CUPS, `bin`/`paper` values must not contain spaces.

`orientation` is for labels that come out turned on Windows. With `auto`, the default, SumatraPDF
turns a page wider than it is tall whenever the driver reports portrait. That is right on a tall
sheet. A label driver can report portrait for a wide label too, and then the label comes out
sideways and small: send `landscape` for such a label, or `portrait` for a tall one. The option has
two costs. A wide page on a sheet that really is tall prints at about half the size, and SumatraPDF
no longer matches the paper to the page, so the driver's paper has to be right. CUPS reads the real
paper size and turns a page only when it does not fit, so the option has no effect there. It needs
agent 3.2.0; older agents ignore it. Measured with SumatraPDF 3.4.6, 3.5.2 and 3.6.1.

## Content types

| `type` | Payload | The server… |
|---|---|---|
| `raw_base64` | `content`: base64 bytes (ZPL/ESC-POS) | decodes and queues them |
| `pdf_base64` | `content`: base64 PDF | decodes and queues; agent renders |
| `raw_uri` | `url` | GETs the URL, queues the response bytes |
| `pdf_uri` | `url` | GETs the URL, queues as PDF |
| `raw_uri_post` | `url` + `json` | POSTs `json` to the URL, queues the response bytes |
| `pdf_uri_post` | `url` + `json` | POSTs `json` to the URL, queues as PDF |
| `label_uri` | `url` + `template` | GETs an image, renders it through the [label template](labels.md) |
| `label_base64` | `content` + `template` | decodes a base64 image, renders it through the template |
| `label_text` | `text` + `template` | renders the text through the template |
| `receipt` | `receipt`: the document | lays it out for the printer: ESC/POS bytes, or a PDF where the printer takes one ([receipts.md](receipts.md)) |

URL fetches are `http(s)`-only and sent with a browser User-Agent (WAF/CDN-fronted services
reject a default library one such as Python's).

For the types that fetch (`raw_uri`, `pdf_uri`, `raw_uri_post`, `pdf_uri_post`, `label_uri`) the
server checks the printer, an existing `idempotency_key` and the quota **before** it fetches the
URL, so a wrong printer (`400`) or a spent quota (`402`) never causes an outbound request, and a
resubmitted key returns the original job without fetching again. A request whose new
`idempotency_key` is still being fetched by another request answers `409` (`a request with this
idempotency_key is still in progress, retry shortly`); retry it and it returns the job. With open
signup each org may also have at most 4 downloads in flight at once, a fifth answers `503` (`too
many downloads in flight for this org, try again later`); `PRINTAPI_FETCHES_PER_ORG` changes the
number, `0` removes the cap (the default without open signup, see [server.md](server.md)).

The `label_*` types are the odd ones out: the server does not queue the bytes it received but a
**rendered label**, a PDF for a `|pdf` printer and ZPL for any other. The job's mode follows the
printer, so `options` are accepted on a PDF printer and refused on a ZPL one. The template is
named by `template` (its name or its integer id). Templates, errors and examples:
[labels.md](labels.md).

`receipt` is its sibling: a JSON document (texts, columns, a QR code, a barcode, a cut, the drawer)
that the server renders for the printer: ESC/POS bytes for a raw printer, and a PDF for a printer that
takes one unless its receipt profile says `output: escpos`. Elements, limits, the profile and the
preview are in [receipts.md](receipts.md). `copies` repeats the whole stream, the drawer kick
included. A receipt with images takes one of 4 render slots of its own and answers `503` `receipt
rendering is busy, try again later` when none frees within about two seconds. It also counts against
the per-org limit of downloads in flight above (when that applies, a fifth one answers the same `503`
at once, without waiting) and holds its `idempotency_key` while it renders (a concurrent request with
the same new key answers `409`). A receipt without images does neither. A CloudPRNT printer is
refused.

## Webhooks

Set `callback_url` on a job and the server POSTs this JSON once the job reaches a terminal state:

```json
{"job_id": 1, "state": "done", "error": null, "title": "Label #4712", "printer_id": 3}
```

- Fires on `done`, `failed`, and `cancelled` (state-based, so agent-reported, cancelled, and
  reaper-failed jobs all deliver).
- **Best-effort with retries:** any non-2xx or connection error is retried a few times by a
  background dispatcher, then given up (logged). Delivery is not guaranteed — if it matters, treat
  `GET /jobs/{id}` as the source of truth.
- **At-least-once:** a delivery can arrive **more than once** (e.g. the POST succeeded but its ack
  was lost, so it retries). Make your handler idempotent — dedupe on `job_id` + `state`.
- **A `done` can follow a `failed`.** A job the reaper failed (`retry limit exceeded`, `expired`,
  `retention`) becomes `done` when its agent delivers the result late (see [Job lifecycle](#job-lifecycle)),
  and the webhook fires again with `done`. A client that resubmits on `failed` can print twice;
  wait a while, or check `GET /jobs/{id}` first.
- The payload is **unsigned** and the URL is fetched server-side (`http(s)` only) — same trust model
  as the `*_uri` content types. Point it at a trusted endpoint.

## Computers (agents)

```bash
curl -s localhost:3460/computers -H 'Authorization: Bearer <client-key>'
# -> {"computers":[{"id":1,"name":"warehouse-pc","online":true,"last_seen_at":1753.., 
#                  "created_at":1750..,"printers":2}]}
```

`online` means the agent polled or registered within the liveness window (60 s). Scoped to the
key's org, like every other client list.

### Agent liveness events

An org can receive a POST whenever one of its agents crosses that window — the fleet-monitoring
counterpart to per-job `callback_url` webhooks:

```bash
curl -s -X PUT localhost:3460/orgs/2 -H 'Authorization: Bearer <PRINTAPI_TOKEN>' \
     -d '{"event_url":"https://ops.example/printpapi-events"}'    # null clears it
```

```json
{"event":"computer_offline","computer_id":1,"name":"warehouse-pc","org_id":2,"last_seen_at":1753..}
```

- `computer_offline` fires once when the agent stops being seen, `computer_online` once when it
  comes back. One POST per edge, never a repeat while the state holds.
- **At-most-once, unlike job webhooks:** a failed POST is logged and dropped, not retried — the next
  real transition fires again. Treat `GET /computers` as the source of truth.
- `http(s)` only, unsigned, same trust model as `callback_url`.
- An org that sets its `event_url` later starts from the *current* state; past transitions are not
  replayed.

## Multi-tenancy

Every agent, printer, job, and key belongs to exactly one **org**. A key *is* the org: whatever
key a request presents decides what it can see, and nothing else does.

```bash
# 1. root creates the org
curl -s -X POST localhost:3460/orgs -H 'Authorization: Bearer <PRINTAPI_TOKEN>' \
     -d '{"name":"acme"}'                       # -> {"id":2,"name":"acme"}

# 2. root issues that org a key (one per agent / per integration)
curl -s -X POST localhost:3460/apikeys -H 'Authorization: Bearer <PRINTAPI_TOKEN>' \
     -d '{"label":"acme-agent","org_id":2,"scope":"agent"}'
     # -> {"id":1,"label":"acme-agent","org_id":2,"key":"…","scope":"agent"}

# 3. the agent puts that key in agent.ini and registers — it lands in org 2, and so do its
#    printers and every job printed on them.
```

Rules:

- **An org key never reaches another org.** `GET /jobs`, `GET /printers` and `GET /metrics` return
  only that org's rows; a foreign job id is **`404`**, not `403` (a `403` would confirm it exists);
  `DELETE /jobs/{id}` on a foreign job is `404` too, even if that job is claimed (which would
  otherwise be `409`). Printing to a foreign `printer_id` is `400 unknown printer` — the same answer
  a nonexistent printer gets.
- **Root spans orgs.** The bootstrap `PRINTAPI_TOKEN` reads, submits and cancels across every org
  (that's what the dashboard uses), and is the only credential that can manage orgs and keys.
- **Agents inherit their key's org.** A key issued for org N puts the agent in org N; the root
  token enrolls into the default org `1`. Any other key is `401` since v3 — except one an agent
  already holds (machines enrolled under v2 keep working), and except with
  `PRINTAPI_OPEN_ENROLLMENT=1`, which restores the v2 rule (any key enrolls into org `1`). Agent
  names are unique *per org*, not globally; an agent key is still bound to its name on first
  contact. Revoking an issued key also stops the agent that enrolled with it. A key whose rights
  lack `Agent` (`print`, `read`, `cloudprnt`) is `401` here, with a message saying which rights are
  needed, also with open enrollment on; an `agent` key cannot take a name that starts with
  `cloudprnt-` (see [Key rights](#key-rights)).
- `org_id` is optional on `POST /apikeys` and defaults to `1`. An unknown org → `400`.

**Existing installs need no migration** — everything already lives in org `1`, root behaves as
before, and every issued key resolves to org `1`.

Deliberately out of scope for now (`# ponytail:` in the code): billing, quotas, and org deletion.
A *full* key that an agent holds doubles as its org's client key; issue an `agent` key per machine
to keep a lost machine's blast radius to that machine.

## Accounts and login

An org's people sign in with an **e-mail and password** instead of pasting the root token. The
login mints a **session token** carried in the same `Authorization: Bearer` header as everything
else, so nothing else about the API changes.

```bash
# 1. root seeds the org's first user (or the org signs itself up — see Self-signup below)
curl -s -X POST localhost:3460/orgs/2/users -H 'Authorization: Bearer <PRINTAPI_TOKEN>' \
     -d '{"email":"ops@acme.example","password":"a long passphrase"}'

# 2. that user logs in and gets a session
curl -s -X POST localhost:3460/login \
     -d '{"email":"ops@acme.example","password":"a long passphrase"}'
# -> {"token":"sess_…","expires_at":1787…,"org_id":2,"user_id":2}

# 3. the session acts inside org 2 — and may manage it
curl -s -X POST localhost:3460/apikeys -H 'Authorization: Bearer sess_…' -d '{"label":"n8n"}'
curl -s -X POST localhost:3460/users   -H 'Authorization: Bearer sess_…' \
     -d '{"email":"packer@acme.example","password":"another passphrase"}'
```

What holds:

- **Three kinds of credential, one header.** Root manages every org; a session manages its own
  org; a machine key has rights (its [scope](#key-rights)) but never manages (`401` on
  `/apikeys`, `/users`, `PUT /orgs/{id}`). Which table the credential resolves in decides who
  manages; a session has no scope, and there is no role column and no roles.
- **Sessions expire** after 30 days and can be ended early (`POST /logout`, or the reaper once
  expired). Tokens are stored sha256-hashed, like every other credential here.
- **Passwords** are `scrypt` hashes (stdlib, salted per user), minimum 8 characters. Changing a
  password deletes that user's sessions — a stolen session dies with the password it came from.
- **Login is throttled** per e-mail (10 failures / 15 minutes → `429`), and a wrong address costs
  the same time as a wrong password, so the endpoint does not enumerate accounts.
- **E-mails are unique across the whole server** (login carries no org), lower-cased and trimmed.
- **Removing an account** (`DELETE /users/{id}`) drops its sessions and any pending reset with it.
  You cannot remove yourself (`400`), an org cannot lose its last account (`400`), and another
  org's user is a `404`. Their jobs stay in the history; their API keys are untouched — revoke
  those separately.

Not built yet: per-user roles. The throttle counters are per process, in memory and bounded
(several server processes count separately, and a large enough spray of invented addresses evicts
counters instead of growing memory). Passwords over 1024 characters are rejected rather than
hashed.

### Self-signup

`POST /signup {email, password, org_name?, accept_terms?}` creates an org, its first account and a session in one
call — the same body a login answers with. It is **off unless the operator turns it on**
(`PRINTAPI_SIGNUP=open`); otherwise it is a `403`, because a self-hosted box on the open internet
must not hand an org to whoever finds it. `GET /health` reports `"signup": "open"|"closed"` so the
dashboard only offers the door when it exists.

Signups are throttled by client address (the same limit as failed logins, but its own counter: a failed
login never uses up a signup slot), so one host cannot spray orgs. A password that is too short or too
long is a `400` before a slot is taken. IPv6 clients count per /64. Behind a reverse proxy every signup shares the proxy's
address unless `PRINTAPI_TRUSTED_PROXIES` names the proxy, which makes printpapi read the client
from `X-Forwarded-For` (see [Behind a reverse proxy](server.md#behind-a-reverse-proxy)). `org_name` defaults to the e-mail's domain. A taken address is a `409` and leaves no
half-made org behind. With `PRINTAPI_SIGNUP_QUOTA` set, the new org starts with that monthly
[job quota](#quotas); without it, it is unlimited.

**Consent.** With `PRINTAPI_TERMS_VERSION` set, `GET /health` adds
`"terms": {"version", "terms_url", "dpa_url", "privacy_url"}` (the last two may be `null`; the key is
absent when consent is not required) and `POST /signup` needs `"accept_terms": "<that version>"`:
missing, `false` or not a string is a `400` (`accept_terms required`), another version is a `409`
(`terms changed, reload`: the form was loaded before the terms changed). Both are checked before the
throttle slot is spent. The version and the time are stored with the new org and user. The
dashboard's form then shows one checkbox ("I act as a business and accept the Terms", plus "and the
Data Processing Agreement" when `PRINTAPI_DPA_URL` is set) that must be ticked.

### Password reset

```bash
curl -s -X POST localhost:3460/password/reset -d '{"email":"ops@acme.example"}'
# -> {"ok": true}   (the same answer for an address with no account)

curl -s -X POST localhost:3460/password/reset/confirm \
     -d '{"token":"sess_…","password":"a new long passphrase"}'
```

- The token is **one-shot**, valid for an hour, stored hashed, and a second request supersedes the
  first. Spending it sets the password and logs every browser out.
- Requests are throttled per address, counted whether or not the account exists — a `429` leaks
  nothing either.
- Mail goes out over **SMTP** (`SMTP_HOST`, `SMTP_PORT`, `SMTP_USER`, `SMTP_PASSWORD`, `SMTP_FROM`,
  `SMTP_SSL`, `SMTP_STARTTLS`). With no `SMTP_HOST` the message is printed to stderr instead, so a
  self-hosted instance still works — the operator reads the token out of the log.
- The mail carries a **link only if `PUBLIC_URL` is set** (`https://…/?reset=<token>`). Without it
  it carries the bare token, because building the link from the `Host` header would let anyone who
  can reach this server mail a valid token pointing at a host of their choosing.
- `GET /health` reports `"password_reset": true` once SMTP is configured; the dashboard offers
  "Forgot password?" only then.

### Quotas

An org can be capped at a number of jobs per calendar month (UTC):

```bash
curl -s -X PUT localhost:3460/orgs/2 -H 'Authorization: Bearer <PRINTAPI_TOKEN>' \
     -d '{"job_quota": 500}'      # null clears it — unlimited is the default
```

- **Root sets it, nobody else** — a session that could raise its own cap has no cap, so
  `job_quota` on `PUT /orgs/{id}` is a `403` for anything but the bootstrap token. Every other
  field on that endpoint stays manageable by the org itself.
- A submit past the cap answers **`402`** and prints nothing. The guard sits in the enqueue path,
  so `POST /jobs`, `POST /orders`, the Shopify webhook and the PrintNode-compatible
  `POST /printjobs` (`{"code": "QuotaExceeded"}`) are all covered by it.
- An org the operator has [locked](operator.md#lock-and-unlock) answers **`403` `org locked`** on
  all of those submit paths, whatever its quota.
- An **idempotent resubmit spends nothing** — it returns the original job, it does not create one.
- Orgs created through [self-signup](#self-signup) start with the operator's `PRINTAPI_SIGNUP_QUOTA`
  (none if unset); orgs root creates start unlimited.
- `GET /orgs/{id}` reports `job_quota` and `jobs_this_month`, which is what the dashboard's
  Settings page shows.

### Support access

`GET /support-access` is what the dashboard's "Support access" card reads: the times the operator
of a hosted deployment was [granted access](operator.md#support-access) to the caller's org, newest
first and for the org's current generation only. Every grant that was used is listed; of the
unused ones, the 50 newest.

```json
{ "enabled": true,
  "grants": [ { "id": 7, "reason": "ticket 4711", "granted_at": 1.7e9, "expires_at": 1.7e9,
                "ended_at": null, "active": true,
                "listed_job_ids": [12, 13], "opened_job_ids": [12] } ] }
```

A browser session sees its own org (`?org=` of another org: `403`); root passes `?org=` and
otherwise reads the default org (`404` for an unknown one); an API key is `401`. `enabled` is
`false`, with no grants, while the server has no operator token: nothing can be granted then. The
job ids come from the audit log, deduplicated and ascending. The answer is `no-store`.

### Plans and billing

A hosted deployment sells those quotas as **plans**: `PRINTAPI_PLANS` is the catalogue, a
plan's `jobs` becomes the org's `job_quota`, and a signed `POST /billing/webhook` from the payment
provider (via a small adapter) moves an org onto one. `GET /plans` lists the catalogue with each
checkout link already carrying the caller's org id. Root can also set `plan` on `PUT /orgs/{id}`
by hand, and `DELETE /orgs/{id}` removes a tenant that leaves. Unconfigured ⇒ off, which is the
self-hosted default. Full contract: **[billing.md](billing.md)**.

## Metrics

`GET /metrics` returns Prometheus text (`text/plain; version=0.0.4`) — `printpapi_jobs{state=…}`
(all five states, including zeros), `printpapi_agents_online`, `printpapi_agents_total`,
`printpapi_printers_total`. The numbers cover the presented key's org (all orgs for the bootstrap
token). It needs client auth, so point your scraper at it with a bearer token:

```yaml
scrape_configs:
  - job_name: printpapi
    authorization: { credentials: <client-key> }
    static_configs: [{ targets: ["yourserver:3460"] }]
```

## Job lifecycle

`queued` → agent claims it (`claimed`) → agent reports → `done` or `failed`.
If an agent claims a job and never reports (crash, network), the visibility-timeout reaper
requeues it — after a bounded number of retries the job is marked `failed`.

**Late results.** The agent writes the outcome of a printed job to its
[journal](agent.md#the-print-journal) before it reports it, and delivers it later when the report
fails. Two things on the server make that safe:

- `GET /agent/jobs` carries `"job_uid": "<32 hex>"` when the request has the header
  `X-Printapi-Features: job-uid` (a comma list; the agent sends it on every request). Without the header
  the answer is exactly what it was. The uid is the first 32 hex digits of
  `sha256("printpapi-job:{id}:{created_at bits as 16 hex digits}")`: derived, nothing stored, and a job
  number that a reset database uses again has another one. It is no credential; every call still
  needs the agent key. The agent may add `job_uid` to the body of `POST /agent/jobs/{id}/result`; a uid that
  is not the job's gives `404` and changes nothing, an absent one is the old behaviour.
- `POST /agent/results` `{"job_id": 7, "job_uid": "…", "ok": true, "error": null}` takes a result by id and
  uid, whatever the job's state is now. Needs the agent right (`401` otherwise); a body that is no
  JSON object is `400`, as is a `job_id` that is no integer or a `job_uid` that is no string. Every well-formed
  call is `200` `{"ok": true, "state": "<state of the job now>"}`, or `"state": null` when this
  agent has no such job or the uid is another one (nothing changed). What it does:

  | state now | `ok: true` | `ok: false` |
  |---|---|---|
  | `claimed` (also claimed again), `queued` (the reaper requeued it) | `done` | `failed` with the error |
  | `failed` (`retry limit exceeded`, `expired`, `retention`) | `done` | unchanged |
  | `done`, `cancelled` | unchanged | unchanged |

  A job that printed for longer than the reaper's five minutes is therefore finished, not printed
  again. A job the reaper had failed becomes `done` and its [webhook](#webhooks) fires again with that
  state. `cancelled` stays: somebody decided that.

A job with `expire_after` whose deadline passes is never handed to an agent (the claim query skips
it) and is failed with `error: "expired"` by the same reaper — a terminal state, so its webhook
fires like any other outcome.

`DELETE /jobs/{id}` moves a job from `queued` to `cancelled` (a terminal state the agent never
claims). The cancel is state-guarded: once an agent has claimed the job it returns `409` — there
is no mid-print interrupt.
