# @printpapi/client

A typed JavaScript and TypeScript client for the [printpapi HTTP API](https://github.com/LukiPrince/printpapi/blob/main/docs/api.md). It has no
runtime dependencies, only `fetch`, and runs on Node 20 or newer and in a browser (see
[Browsers](#browsers)). printpapi is source available under the Elastic License 2.0; this package
carries the same licence ([LICENSE](LICENSE)).

> The package name `@printpapi/client` is a placeholder until the maintainer has chosen where it is
> published. It is not published yet. Build it from this folder (`npm ci && npm run build`) and
> `npm pack` it for now.

The types are generated from [`crates/server/openapi.json`](https://github.com/LukiPrince/printpapi/blob/main/crates/server/openapi.json), the
OpenAPI 3.1 description every server also serves at `GET /openapi.json`. A test fails when the
generated file differs from the committed one, so the client and the server cannot drift apart.

## Quick start

```ts
import { Printpapi } from "@printpapi/client";

const printpapi = new Printpapi({ baseUrl: "https://print.example.com", apiKey: process.env.PRINTPAPI_KEY! });

const printers = await printpapi.printers.list();
const zebra = printers.find((p) => p.name === "Zebra")!;

const { job_id } = await printpapi.jobs.submit({
  printer_id: zebra.id,
  type: "raw_base64",
  content: new TextEncoder().encode("^XA^FO50,50^FDHello^FS^XZ"), // bytes, or base64 text
  title: "Label 4712",
});
const job = await printpapi.jobs.wait(job_id); // done, failed or cancelled
console.log(job.state, job.error);
```

Issue a key on the dashboard's Keys page (or `POST /apikeys`). Give an integration the narrowest
rights it needs: a shop that only prints needs **Print only**, see
[Key rights](https://github.com/LukiPrince/printpapi/blob/main/docs/api.md#key-rights).

## Methods

| Call | Does | Needs the right |
|---|---|---|
| `health()` | liveness, and whether the server offers signup | none |
| `me()` | who the key is: `kind`, `org_id`, `scope` when it is not full | none |
| `jobs.submit(job)` | queue a job, returns `{ job_id }` | Submit |
| `jobs.list()` | the newest 50 jobs | Look |
| `jobs.get(id)` | one job, with its `id` | Look |
| `jobs.wait(id, options)` | poll until the job ends | Look |
| `jobs.cancel(id)` | cancel a queued job | Submit |
| `printers.list()`, `computers.list()` | what the agents registered | Look, Computers |
| `orders.print(order)` | a packing slip on a PDF printer | Submit |
| `labels.list/get/create/update/delete` | [label templates](https://github.com/LukiPrince/printpapi/blob/main/docs/labels.md) | Look, Edit |
| `receipts.preview(input)` | a [receipt](https://github.com/LukiPrince/printpapi/blob/main/docs/receipts.md) drawn as a PNG (`Uint8Array`) | Submit |
| `receipts.profile.get/put/delete(printerId)` | the receipt profile of a printer | Look, Edit |

Every call takes a last argument `{ timeoutMs, retries, signal }`. An id (a job, a label, a printer) is a
whole number; anything else, `"../labels/1"` for one, is a `TypeError` and nothing is sent. Pairing codes, keys, users and orgs
are managed by a login or the root token, never by a machine key (it gets `401`), so the client has
no methods for them: use the dashboard or the [HTTP API](https://github.com/LukiPrince/printpapi/blob/main/docs/api.md).

## The ten job types

`jobs.submit` takes one of these, told apart by `type` (the editor completes the other fields):

| `type` | Also needs | `options` |
|---|---|---|
| `raw_base64` | `content` (base64 text or bytes): ZPL, ESC/POS | no |
| `pdf_base64` | `content` | yes |
| `raw_uri` | `url`: the server fetches it | no |
| `pdf_uri` | `url` | yes |
| `raw_uri_post` | `url`, `json`: POSTed to the URL | no |
| `pdf_uri_post` | `url`, `json` | yes |
| `label_uri` | `url` (an image), `template` | on a PDF printer |
| `label_base64` | `content` (an image), `template` | on a PDF printer |
| `label_text` | `text`, `template` | on a PDF printer |
| `receipt` | `receipt`: the document | when the output is a PDF |

`template` is a label template's name or its integer id. `options` (duplex, paper, bin, color, pages,
orientation) on a raw type is a type error and a `400`; on a label or receipt type it depends on the
printer: a PDF printer takes them, a ZPL or ESC/POS printer answers `400`. Never send a PDF to a
label printer: it prints blank labels (use `label_*` or raw ZPL). All types also take `copies`,
`title`, `callback_url`, `expire_after` and `idempotency_key`.

## Retries and idempotency

A print must not come out twice. `jobs.submit` and `orders.print` make **one idempotency key per call**
(128 random bits as hex, or the `idempotency_key` you pass; an empty or all-blank one counts as none, as
it does on the server) and every attempt carries it: the server
returns the original job for a key it has seen. So a repeat after a lost answer is safe.

| Call | Repeated after | Never repeated after |
|---|---|---|
| GET, `receipts.preview` | no answer, the attempt timeout, `429`, `502`, `503`, `504`, `520` to `524` | any other answer |
| `jobs.submit`, `orders.print` | no answer, the attempt timeout, `429`, `503`, `504`, `520` to `524`, and a `502` that does not say `downstream:` | `400`, `401`, `402`, `403`, `404`, the server's own `502 downstream:` (its download failed) |
| PUT, DELETE, `labels.create`, `jobs.cancel` | never | |

Up to `retries` repeats (default 2), after `min(4000, 250 * 2^n)` ms with full jitter. An answer `409`
"still in progress" means another request with the same key is still fetching the payload: the client
asks again about every second, with the same key, for up to 130 s, and this does not use up `retries`.

**To retry after an error yourself**, call `submit` again with the key the error carries:

```ts
import { Printpapi, PrintpapiError } from "@printpapi/client";

try {
  await printpapi.jobs.submit(job);
} catch (e) {
  if (e instanceof PrintpapiError && e.idempotencyKey) {
    // the server may have the job already: ask again with the SAME key. A new key prints twice.
    await printpapi.jobs.submit({ ...job, idempotency_key: e.idempotencyKey });
  }
}
```

An aborted or timed-out request does not stop the server's work, so this is the only safe way. The key
is kept for as long as the job row exists ([API](https://github.com/LukiPrince/printpapi/blob/main/docs/api.md#submitting-a-job)). A job the server
gave up on can still become `done` later when the agent delivers its result late, and its webhook
fires again: `job.maybe_printed === true` says that this is possible (an agent had the job and never
reported), so look or wait before you resubmit. Servers up to 3.6.0 never send the field.

## Timeouts

Per attempt: **30 s** for everything, **150 s** for `jobs.submit` and `orders.print`. The server may
spend up to 120 s fetching a payload and up to 120 s reading a request body, and a client that gives up
earlier does not stop it, so the client waits longer than the server. `timeoutMs` on the client or on one
call overrides both; a value that is not a finite number means no timer. `jobs.wait` gives up after 5
minutes (`timeoutMs`) with kind `timeout`, even while the server hangs; it polls about every 500 ms, growing
by 1.5 up to 5 s. When its last pause is over, or the deadline cuts a poll after an earlier poll got an
answer, it asks once more (at most 2 s, no repeats), so a job that ended meanwhile is returned and
`timeoutMs: 0` checks once. If that last request gets no answer about the job (the connection is
refused or lost, `503`, `502`, `429`), `wait` ends with kind `timeout` and the last state it saw in `body`, not
with that error; `not_found`, `unauthorized` and `forbidden` are still thrown as they are. A `timeoutMs` that is not a number
means no deadline, an `intervalMs` that is not a positive finite number means 500 ms. A `signal` that is
already aborted rejects at once with `attempts: 0`.

## Errors

Every failure of a request is a `PrintpapiError` (a mistake of the caller, such as a body that cannot be
JSON, is a `TypeError` and nothing is sent) with `kind`, `status`, `body` (the server's `{"error": ...}`),
`method`, `path`, `attempts` and, for submit and orders, `idempotencyKey`.

| `kind` | Means |
|---|---|
| `invalid` | `400`: a field is wrong, an unknown printer, `options` on a raw job |
| `unauthorized` | `401`: no key, a wrong or revoked key, or a route a machine key may never call |
| `forbidden` | `403`: the key's rights do not allow it, the org is locked |
| `not_found` | `404`: also another org's job |
| `conflict` | `409`: not cancellable, a name is taken, or a request is still in progress after 130 s |
| `quota` | `402`: the monthly job quota is spent |
| `rate_limited` | `429` |
| `unavailable` | `503`, `504`, `520` to `524` |
| `bad_gateway` | `502`, a proxy or the server's own failed download (`downstream:`) |
| `server` | another `5xx`, or a success that is not what the server sends: a page from a proxy or a login screen, an empty body, or an object without the field the call reads (`job_id`, a list, `state`); check `baseUrl` |
| `network` | no answer; also a call aborted with its `signal` |
| `timeout` | the attempt timeout, or the deadline of `jobs.wait` |
| `redirect` | `3xx`: the base URL is wrong (often `http` against `https`) |
| `job_failed` | `jobs.wait` with `throwOnFailure` and a job that failed |

The API key never appears in a message, a stack, `JSON.stringify(error)` or any property: it lives in a
private field and in one header, and where a gateway or a debugging proxy echoes it in an answer, it is
replaced by `[key]` in every string of the decoded answer (whatever JSON escape wrote it) before the error is
built. A success is returned as the server sent it and is never rewritten, so a short root token such as
`print` costs nothing there; in an error it replaces every occurrence of the key, which garbles a message
when the key is a common word. A key that is not visible ASCII (a pasted line break, a shortened `…`, a
space at either end) is refused by the constructor; a space inside is allowed. Redirects are not followed (a redirect would carry the key to another
host), cookies are not sent, and the client reports nothing anywhere.

## Browsers

The client uses only `fetch`, `AbortController` and `crypto.getRandomValues`, and its code builds with no
Node types, so it is meant to run in a browser too. The server sends **no CORS headers today**, so a
page on another origin cannot call it: call it from your own backend, or put a proxy that adds them in
front. Do not put a key with write rights in a page anyone can read.

Outside a browser the client sends `User-Agent: Mozilla/5.0 (printpapi-client)`, because a WAF in front of
a server often refuses the default of a fetch library with `403`.

## Licence and version

[Elastic License 2.0](LICENSE) with its additional permission, as the rest of printpapi: source
available, not open source. The package version follows the server's; the
OpenAPI document has a version of its own. This is a choice the maintainer may change before the first
release.

## Development

```bash
cargo build -p printpapi-server    # the tests start the real binary
cd clients/js
npm ci
npm test                           # build, then node --test build/test/*.test.js
npm run gen-types                  # after a change of crates/server/openapi.json
```

`PRINTAPI_BIN` names another binary (CI uses `target/release/printpapi`). The harness starts the
server with an explicit environment, on a loopback port, and refuses a binary whose `/openapi.json`
differs from the file. Every exchange with the printpapi server that goes through the harness's recorder (the client's own
calls, and the account, agent, webhook and CloudPRNT routes it does not call) is checked against the
document, requests and answers, and an answer with a property the document does not list fails the
test. The tests of retries, redirects, headers and wrong answers talk to throwaway servers or to a fault
proxy in front of the real one; nothing describes what those send.
