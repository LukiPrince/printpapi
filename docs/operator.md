# Operator API

A hosted deployment has an operator: someone who looks after many customers' orgs and is not one
of them. The operator API is what a separate console talks to for that. It is generic (orgs, jobs
as numbers, an audit log) and nothing in it is specific to one business. **It is off unless
`PRINTAPI_OPERATOR_TOKEN` is set**: with the token unset, every path under `/operator/` answers
`404`, as if the API did not exist.

This page covers the gate, hosted mode, the audit log, the read endpoints (overview, orgs and their
jobs) and the write endpoints (change, lock, delete and create an org, send a reset mail, revoke a
key).

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
database at startup. They are additive and the Python server of v2.x ignores them.

## Writes

Every write is **one store function and one transaction with its audit row**: the change and the
row are committed together or not at all. If the audit row cannot be written, the change is rolled
back and the answer is `500`. A mail and the WAL truncate that belong to an action run **after** the
commit. Bodies are JSON; the optional `reason` is a string of at most 500 characters and lands in
the audit log, so keep customer data out of it. An org or id that does not exist answers `404`; a
body that is not a JSON object, or a field that fails validation, answers `400` and changes nothing.

**Every write on an existing org names its generation.** Org and user ids are reused by SQLite after
the newest row is deleted, so an id alone does not say which customer a page showed. Each write
(`PUT`, `lock`, `unlock`, `DELETE` of an org, the user reset and the key revoke) carries
`org_created_at`, the `created_at` the read endpoints returned for that org, and the server compares
it with the org's `created_at` inside the transaction. A different or missing value answers **`409`
`the org changed, reload`**; nothing is changed, no audit row is written and no mail goes out.
`POST /operator/orgs` creates the org and takes none.

| Call | Audit action |
|---|---|
| `PUT /operator/orgs/{id}` | `org.update` |
| `POST /operator/orgs/{id}/lock`, `.../unlock` | `org.lock`, `org.unlock` |
| `DELETE /operator/orgs/{id}` | `org.delete` |
| `POST /operator/orgs` | `org.create` |
| `POST /operator/users/{id}/reset` | `user.reset` (`user.reset_mail_failed` when the mail did not go out) |
| `DELETE /operator/keys/{id}` | `key.revoke` |

The audit rows hold ids and numbers: `org.update` says which fields changed (`name: true`, never the
name), `org.lock` how many queued jobs it cancelled, `org.create` the quota the org started with.
No e-mail address, org name or title is ever written.

### `PUT /operator/orgs/{id}`

`{name?, job_quota?, plan?, reason?}`, at least one of the three fields. Everything is validated
first and applied in one transaction. `name` is a non-empty string of at most 200 characters;
`job_quota` a non-negative integer or `null` (unlimited); `plan` an id of the plan catalogue
([billing](billing.md)), which also sets the quota the plan grants. An explicit `job_quota` in the
same body wins over the plan's. Answer: `{"ok": true}`.

### Lock and unlock

`POST /operator/orgs/{id}/lock` with `{reason}` of at least 8 characters inserts a row into the
table `org_locks` (`org_id`, `org_created_at`, `reason`, `locked_at`) and cancels the org's queued
jobs (`error: "org locked"`). A job an agent had already claimed is not interrupted; if that agent
goes quiet, the reaper cancels the job (`org locked`) instead of putting it back in the queue.
Answer: `{"ok": true, "cancelled_jobs": 3}`; `409` if the org is already locked.

A lock is a **pure flag**: quota and plan stay as they are, so billing and quota writes need no
change. While it is set, every way a job gets in answers **`403` `org locked`**: `POST /jobs`,
`POST /orders`, the Shopify webhook and the PrintNode-compatible `POST /printjobs`. The check sits
next to the quota check in the enqueue path and again before any payload is downloaded, so a locked
org causes no outbound request, and an idempotent resubmit does not get an old job back either.
Logins, the dashboard and agents keep working, so the customer can see what is going on.

A lock row belongs to one org generation: it counts only while its `org_created_at` equals the
org's `created_at`. A row left by an earlier org of the same id locks nothing and shows nowhere.

**Locks are enforced only while `PRINTAPI_OPERATOR_TOKEN` is set.** Without the token the whole
operator API is a `404`, so nothing could show or lift a lock; it is inert like the rest. The
submit check and the reaper rule above then do nothing, the rows stay in the database, and the server
prints one line at startup: `N org locks are not enforced: PRINTAPI_OPERATOR_TOKEN is unset`. Set the
token again and they hold again.

`POST /operator/orgs/{id}/unlock` (`{reason?}`) deletes the row; `409` if the org is not locked.
`GET /operator/orgs` and the org detail show the lock (`locked`, `status: "locked"`, `lock`).

> **Rolling back to an older version drops the lock.** Versions before this one (and the Python
> server of v2.x) do not read `org_locks`: a locked org can submit again, and the table is kept
> untouched. Unlock first, or accept that. Deleting an org there also leaves its lock row behind; a
> later org that reuses the id is not locked by it, because the row names the old org's generation.

### `DELETE /operator/orgs/{id}`

`{reason, confirm}`: `confirm` must equal the org's **current name** exactly, otherwise `400`. The
default org is never removed (`400`). The org's jobs, printers, agents, keys, accounts, label
templates, terms history and its `org_locks` row go with it; the audit rows stay. The WAL is
truncated after the commit, so removed payloads do not linger in the `-wal` file.

### `POST /operator/orgs`

`{name, email, reason?}` creates the org with the signup quota (`PRINTAPI_SIGNUP_QUOTA`, as a
self-signup would get) and its first user. The user's password hash is that of 32 random bytes,
never `NULL`, so a login attempt reveals nothing; the **reset mail is the invitation**, sent after
the commit. Answer: `{"ok": true, "org_id", "user_id", "mail_sent"}`. Without mail (`SMTP_HOST`) and
`PUBLIC_URL` the answer is `409` `mail is not configured` and nothing is created; a taken address is
`409` too. If the mail server fails after the commit, the org stays, `mail_sent` is `false` and a
`user.reset_mail_failed` row is written: use the reset below to send it again.

### `POST /operator/users/{id}/reset`

`{org_id, reason?}` sends a password-reset mail to the user and never returns the token. `org_id`
must be the user's org (`409` otherwise) and `org_created_at` that org's generation (`409`
`the org changed, reload`), so a stale page cannot reach a user that reuses an id. `409`
`mail is not configured` without mail and `PUBLIC_URL`; `502` if the mail did not go out (the row
`user.reset_mail_failed` says so). A user without an e-mail address (the seeded legacy one) is `404`.

### `DELETE /operator/keys/{id}`

`{org_id, org_created_at, reason?}` revokes an API key (`409` if the key belongs to another org or
generation). Agents enrolled
with it end their long-poll with `401`. Removing a single user is not offered here: customers remove
their own accounts, and root still can.
