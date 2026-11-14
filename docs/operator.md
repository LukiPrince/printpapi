# Operator API

A hosted deployment has an operator: someone who looks after many customers' orgs and is not one
of them. The operator API is what a separate console talks to for that. It is generic (orgs, jobs
as numbers, an audit log) and nothing in it is specific to one business. **It is off unless
`PRINTAPI_OPERATOR_TOKEN` is set**: with the token unset, every path under `/operator/` answers
`404`, as if the API did not exist.

This page covers the gate, hosted mode, the audit log and the read endpoints available so far
(overview, orgs and their jobs).

## The gate

Two settings, both required together:

| Variable | Meaning |
|---|---|
| `PRINTAPI_OPERATOR_TOKEN` | The credential of the operator API. At least 32 characters, not starting with `sess_`, different from `PRINTAPI_TOKEN`. Anything else stops the server at startup |
| `PRINTAPI_OPERATOR_FROM` | The addresses it is accepted from: IPs or CIDRs, same syntax as `PRINTAPI_TRUSTED_PROXIES` ([server](server.md#behind-a-reverse-proxy)). Required when the token is set |

A request passes the gate when it carries the token as `Authorization: Bearer` **and** its socket
peer is in `PRINTAPI_OPERATOR_FROM`. The peer is the address the TCP connection came from. A
trusted proxy's `X-Forwarded-For` is never read for this, so a caller who can reach the proxy cannot
name the console's address. From any other peer the token is an unknown credential and the answer
is `401`.

- The root token (`PRINTAPI_TOKEN`) is **not** accepted on `/operator/`: `401`. Otherwise the
  address rule would bind only the operator token, and root could read the same data from anywhere.
- The operator token is **no credential anywhere else**. On every other route it is dropped before
  a session, API key, HTTP Basic (PrintNode), CloudPRNT or agent key is looked up, so it answers as
  if no credential had been sent; an agent cannot enroll with it, even with
  `PRINTAPI_OPEN_ENROLLMENT=1`.
- Every `/operator/` response carries `Cache-Control: no-store`, the unauthorized and `404` ones too.
- With `LOG_REQUESTS`, the access log writes `/operator/` paths **without their query string**
  (a search can name a customer).
- There is one operator identity. Every audit row says `actor = operator`.

Keep the console off the public host. In a Caddy setup, answer `404` for `/operator/*` on the
public site and let the console reach printpapi over the internal network:

```caddyfile
printpapi.example.com {
    @operator path /operator/*
    respond @operator 404
    reverse_proxy printpapi:3460
}
```

## Hosted mode

Setting the operator token turns on hosted mode, because strangers' jobs run on this server:

- `PRINTAPI_PRIVATE_URLS` unset means `deny` (payload URLs, callbacks and event URLs may only
  reach public addresses), whatever `PRINTAPI_SIGNUP` says.
- `PRINTAPI_FETCHES_PER_ORG` unset means `4`.

Both are fixed at startup. Set either explicitly and your value wins, for example
`PRINTAPI_PRIVATE_URLS=allow`. Setting the token never changes what these mean on a later upgrade.

## Audit log

Table `operator_audit`: `id`, `at`, `actor`, `action`, `org_id`, `org_created_at`, `target`,
`reason`, `detail`. `org_created_at` tells an org apart from an older one that had the same id.

- It holds **ids, numbers and setting values**, never an e-mail address, a name or a job title,
  so deleting a customer does not leave one behind.
- It is kept for ever. No API changes or deletes a row, and deleting an org does not remove its
  rows. It does not follow `PRINTAPI_JOB_RETENTION_DAYS`. Trim it by hand if it ever grows past
  about 100 000 rows.
- It records `/operator/` actions only. Root's own routes (for example `GET /jobs`, which lists
  titles across orgs) are **not** audited.
- It lives in a table of its own: the Python server of v2.x ignores it, and a rollback keeps it.

### `GET /operator/audit`

Newest first. Query: `org`, `action` (exact), `before_id`, `limit` (default 50, at most 200).

```json
{
  "entries": [
    {"id": 12, "at": 1760000000.5, "actor": "operator", "action": "org.update",
     "org_id": 3, "org_created_at": 1750000000.0, "target": null,
     "reason": "quota raised after support request", "detail": {"job_quota": 500}}
  ],
  "next_before_id": 9
}
```

`next_before_id` is the `id` to pass as `before_id` for the next page, `null` on the last one.
A parameter that is not an integer answers `400`.

## Overview

`GET /operator/overview` answers counts and server numbers. It never contains a job payload, title,
callback, idempotency key, key secret or password hash.

```json
{
  "orgs": 41, "orgs_active_7d": 17, "signups_7d": 3,
  "jobs_this_month": 5120, "failed_jobs_30d": 12,
  "jobs_today": {"queued": 0, "claimed": 1, "done": 188, "failed": 2, "cancelled": 0},
  "server": {"version": "3.4.0", "uptime_s": 86400},
  "db": {"size_bytes": 12582912, "wal_bytes": 0},
  "retention": {"last_pass_at": 1760000000.0, "abandoned": 0, "scrubbed": 14, "deleted": 0},
  "features": []
}
```

`orgs` counts every org including the default one; `signups_7d` does not count the default org.
`jobs_today` is the current UTC day. `retention` is `null` fields until the first pass has run.
`features` lists the optional sections this server offers, so a console can hide the ones an older
server lacks. The answer is cached for 10 seconds.

## Orgs and their jobs

Metadata only. Never in any answer: a job's payload, title, callback URL or idempotency key, an
API or agent key (nor its hash), a password hash, or an org's Shopify secret. All three are `GET`
only, and an org that does not exist (or an id that is not a number) answers `404`.

### `GET /operator/orgs`

Query: `status` (`locked`, `new`, `idle` or `active`), `q`, `limit` (default 50, at most 200),
`after`. Ordered by org id, oldest first.

- `q` matches a part of the org name or of a user's e-mail address. `%` and `_` mean themselves.
  Case is ignored for ASCII letters only (SQLite `LIKE`).
- `status` is derived, in this order: `locked` (the org has a lock), `new` (created less than 7
  days ago), `idle` (no job in the last 14 days, or never one), otherwise `active`.
- `after` is the `next_after` of the previous page; `next_after` is `null` on the last page.
  A parameter that is not an integer, or an unknown `status`, answers `400`.

```json
{
  "orgs": [
    {"id": 3, "name": "Acme", "created_at": 1750000000.0, "plan": "pro", "job_quota": 500,
     "jobs_this_month": 212, "queued_jobs": 0, "failed_today": 1, "last_job_at": 1760000000.0,
     "agents_online": 1, "agents_total": 2, "newest_agent_seen_at": 1760000100.0, "printers": 3,
     "locked": false, "first_user_email": "boss@acme.example",
     "terms_version": "2026-11", "terms_accepted_at": 1750000000.0, "status": "active"}
  ],
  "next_after": null
}
```

`job_quota` is `null` for unlimited. `jobs_this_month` is the current UTC month, `failed_today`
counts jobs created today (UTC) that failed, `queued_jobs` all jobs still queued. An agent is
online when it polled in the last minute. The statement
runs on indexes only (one lookup per org), so a page costs its size, not the size of `jobs`. A
`status` filter computes the numbers of every org it passes over.

### `GET /operator/orgs/{id}`

The row above plus:

| Field | Content |
|---|---|
| `usage_by_day` | every UTC day of the **current month** up to today: `{date, jobs, failed}`. Earlier months are not offered: with job retention they would be incomplete |
| `users` | `id`, `email`, `created_at` |
| `agents` | `id`, `name`, `last_seen_at`, `online` |
| `printers` | `id`, `name`, `can_pdf`, `state`, `agent_id` |
| `keys` | `id`, `label`, `active`, `created_at`: the key itself is never shown |
| `terms` | the acceptance history, oldest first: `version`, `accepted_at` |
| `lock` | `null`, or `{reason, locked_at}` while the org is locked |

### `GET /operator/orgs/{id}/jobs`

Query: `limit` (default 50, at most 200), `before_id`. Newest first by id. Each job has `id`,
`created_at`, `claimed_at`, `finished_at`, `type`, `mode`, `state`, `printer_id`, `printer_name`,
`copies` and `error`. `next_before_id` is the id to pass as `before_id` for the next page, `null`
on the last one.

The reads add indexes (`jobs(org_id)`, `jobs(org_id, state)` and three small ones) to an existing
database at startup. They are additive and the Python server of v2.x ignores them. The `org_locks`
table that holds the lock row is created empty; locking itself comes with the write endpoints.
