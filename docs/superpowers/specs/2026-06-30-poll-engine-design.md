# Spec — #0 Core poll engine

Date: 2026-06-30
Status: approved design, pre-implementation
Part of: the printpapi OSS "PrintNode rival" roadmap (this is sub-project #0, the foundation)

## Context

v0 (the code in this repo) is **push**: the server fetches/decodes a payload to bytes and pushes it
over HTTP to an agent's inbound listener on the same LAN. PrintNode's actual value is that agents
anywhere — behind NAT/firewall, no inbound port — reach the server by polling *out*. This sub-project
flips push → poll. Everything else on the roadmap (REST API surface, orgs/users/auth, dashboard,
print-options, webhooks, cross-platform agent, scaling) hangs off this core.

Repo role: production runs from the original (private) deployment repo. This `printpapi`
repo is the extracted OSS dev copy, so #0 may replace the push path with the poll path cleanly —
production is unaffected.

## Goals

- Agent registers with the server over an outbound connection and auto-lists its printers.
- Client submits a job to the server; the owning agent pulls it, prints it, reports the result.
- Job status is queryable end-to-end.
- Multi-tenant schema is in place from day 1 (columns only; org/user management is sub-project #3).
- Stdlib only, no new dependencies.

## Non-goals (deferred to later sub-projects)

- Org/user management, per-user API keys, RBAC (#3).
- Web dashboard (#4).
- Print-options matrix / capability modeling (#5).
- Webhooks (#6).
- Linux/CUPS agent, code-signing, installers (#7).
- Network-socket (`:9100`) and CUPS *output kinds* in the agent — small follow-ons, not in #0.
- Queue/worker scaling, multi-instance (#8).

## Architecture

Server flips from pusher to **queue + registry + payload store**. Three actors:

- **Client** — `POST /jobs`, queries status. Single bearer token (as v0), per-user keys come at #3.
- **Server** — SQLite store; decodes payload to bytes at submit time; serves agent polls.
  Stdlib `ThreadingHTTPServer`.
- **Agent** — outbound poll loop: register → long-poll for a job → download bytes → render+print →
  report result. No inbound listener.

What moves where:

| Code | Disposition |
|------|-------------|
| `dispatch.decode_payload`, `_http_get`/`_http_post`, UA gotcha | **stays server-side** (server holds bytes). Reused as-is. |
| Agent `raw_to_printer`, `pdf_to_printer` | **unchanged**; driven by the poll loop instead of an HTTP handler. |
| v0 push senders `send_agent`/`send_socket`/`send_cups`, env `PRINT_TARGETS`, `load_targets`, `resolve_target` | **deleted**. Printers live in DB, registered by agents. |

## Data model (SQLite, WAL mode)

Every table carries `org_id` and `created_at` — the tenancy bet (nearly free now, a full rewrite if
retrofitted later). Seed one default `org` and one default `user` so the FKs resolve before #3 exists.

- `orgs`(id, name)
- `users`(id, org_id, name) — minimal seed row
- `agents`(id, org_id, name, api_key_hash, last_seen_at, created_at) — PrintNode "computers"
- `printers`(id, org_id, agent_id, name, can_pdf, state, created_at)
- `jobs`(id, org_id, user_id, printer_id, agent_id, type, mode, state, payload BLOB, error,
  created_at, claimed_at, finished_at)

Job states: `queued → claimed → done | failed`.

Agent API key stored as `sha256` hash; compared with `hmac.compare_digest`.

## Endpoints

### Client (bearer token)

- `POST /jobs` — body `{printer_id, type, content|url, json}`. Server runs `decode_payload` → bytes,
  infers `mode` (`agent_mode`), looks up the printer's owning agent, inserts a `queued` job with the
  blob. Returns `{job_id}`.
- `GET /jobs/{id}` — returns `{state, error, printer_id, created_at, finished_at}`.
- `GET /printers` — registered printers, owning agent, and online flag (from `last_seen_at`).
- `GET /health` — `{ok: true}` (kept).

### Agent (per-agent API key)

- `POST /agent/register` — body `{name, printers:[{name, can_pdf}]}`. Upserts the agent and its
  printers (by name within the agent). Returns `{computer_id, printer_ids}`.
- `GET /agent/jobs` — **long-poll**, ~25s. Atomically claims one `queued` job for this agent's
  printers and returns metadata `{job_id, printer_id, mode}`; otherwise `204`. Updates `last_seen_at`.
- `GET /agent/jobs/{id}/payload` — `application/octet-stream` of the job bytes. Separate from metadata
  so binary never gets base64-inflated through JSON. 403/404 if the job isn't this agent's.
- `POST /agent/jobs/{id}/result` — body `{ok, error?}`. Marks `done` or `failed` (+ stores `error`).

## Data flow

```
client POST /jobs
  -> server: decode_payload() -> bytes; agent_mode() -> mode
  -> insert jobs(state=queued, payload=bytes, agent_id=<printer owner>)
agent GET /agent/jobs   (blocked long-poll)
  -> atomic claim: UPDATE jobs SET state='claimed', claimed_at=now
       WHERE id=(SELECT ... WHERE agent_id=me AND state='queued' LIMIT 1)
  -> return {job_id, printer_id, mode}
agent GET /agent/jobs/{id}/payload  -> bytes
agent render + print (raw_to_printer | pdf_to_printer)
agent POST /agent/jobs/{id}/result {ok|error}
  -> jobs.state = done | failed
client GET /jobs/{id}  -> state
```

## Long-poll mechanism

`ThreadingHTTPServer` (one thread per blocked poll). The handler loops: query for a claimable job;
if found, claim and return; else `time.sleep(~1s)` and retry until ~25s elapsed, then `204`.

`# ponytail: 1s DB-poll granularity; switch to a threading.Condition notify-on-enqueue if sub-second
latency is ever needed.`

SQLite: WAL mode + a single global write lock (`threading.Lock`) around writes; reads go straight.

`# ponytail: global write lock; per-table/per-account locks only if throughput demands.`

Claim atomicity comes from the single `UPDATE … WHERE state='queued'` statement (a row is claimed by
exactly one statement), so overlapping poll threads for the same agent cannot double-claim.

## Reliability

- **Visibility-timeout reaper:** a background sweep requeues `claimed` jobs whose `claimed_at` is older
  than N minutes (agent crashed mid-job), with a bounded retry count; on exhaustion the job → `failed`.
- **Online/offline:** derived from `last_seen_at` freshness (updated on every poll/register).

## Scope cut for #0

The agent prints **raw + pdf on Windows** only (exactly what the existing agent does) — enough to
prove the loop end-to-end. Network-socket (`:9100`) and CUPS *output kinds* are small follow-ons added
with their printer kinds (CUPS arrives in #7). All server-side **content types**
(`raw_base64`, `pdf_base64`, `raw_uri`, `raw_uri_post`, `pdf_uri_post`) are kept — they already work
via `decode_payload`.

## Security (trust boundaries — non-negotiable, per CLAUDE.md)

- Bearer token on client endpoints, per-agent API key on agent endpoints; both compared with
  `hmac.compare_digest`. Agent keys stored hashed.
- **Agent registration binds name↔key:** first registration of a name binds it to that key; a later
  registration of the same name with a different key is rejected (401). No silent key rotation, so a
  caller who only knows an agent's name cannot hijack it. (Full agent identity / RBAC is sub-project #3.)
- `decode_payload` keeps `http(s)`-only URL checks and the browser UA.
- No `shell=True`; temp PDF files cleaned up (existing agent behavior preserved).
- An agent may only claim / download / result its own jobs (scoped by `agent_id`).

## File layout (proposed)

```
app/
  dispatch.py   # unchanged (decode_payload, fetch, agent_mode)
  store.py      # NEW: SQLite schema + queries (enqueue, claim, reaper, register, status)
  server.py     # rewritten: client + agent endpoints over ThreadingHTTPServer; push code removed
agent/
  print_agent.py  # rewritten: poll loop (register, long-poll, download, print, result);
                  # raw_to_printer/pdf_to_printer kept; inbound listener removed
tests/
  test_store.py        # NEW
  test_dispatch.py     # stays green
  test_server.py       # rewritten for new endpoints
  test_integration.py  # rewritten: full register->submit->poll->result round-trip
```

## Testing (TDD, real loopback like the existing suite)

- `decode_payload` tests stay green (no behavior change).
- `store`: enqueue → claim atomicity (overlapping claims yield one winner), reaper requeue of stale
  `claimed`, register upsert (re-register same agent/printers is idempotent).
- Full round-trip integration on a real loopback `ThreadingHTTPServer`: register → `POST /jobs` →
  agent poll claims it → download payload → `POST result` → `GET /jobs/{id}` is `done`.
- Long-poll: returns `204` after the timeout when idle; returns the job promptly when one is enqueued
  mid-poll.
- Auth: wrong bearer → 401 on client endpoints; wrong agent key → 401 on agent endpoints; agent
  cannot touch another agent's job → 403/404.

## Open ceilings (tracked, not built)

- 1s poll granularity → condition-variable notify.
- Global write lock → finer locking.
- SQLite blob payloads → filesystem/object store if payloads grow large.
- Single client bearer token → per-user API keys (#3).
