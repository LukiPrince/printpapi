// printpapi client: the one error class. Nothing in it can carry the API key: the key lives in a
// private field of the client and is only ever put into the Authorization header.

/** What went wrong, in a word a program can switch on. */
export type ErrorKind =
  | "invalid" // 400 (and other 4xx not listed): the request is wrong, do not repeat it
  | "unauthorized" // 401: no key, a wrong or revoked key, or a route this key may never call
  | "forbidden" // 403: the key's rights, a locked org, or signup closed
  | "not_found" // 404
  | "conflict" // 409: not cancellable, name taken, or a request with this key still in progress
  | "quota" // 402: the monthly job quota is spent
  | "rate_limited" // 429
  | "unavailable" // 503, 504, 520-524: busy, restarting or behind a proxy that gave up
  | "bad_gateway" // 502: a proxy, or the server's own failed download ("downstream:")
  | "server" // any other 5xx or an answer that is no JSON
  | "network" // no answer: refused, reset, DNS, or aborted by the caller
  | "timeout" // the per-attempt timeout, or the deadline of wait()
  | "redirect" // 3xx: the base URL is wrong (http against https, a missing path)
  | "job_failed"; // wait() with throwOnFailure: the job reached the state failed

export interface ErrorInit {
  kind: ErrorKind;
  message: string;
  status?: number | undefined;
  body?: unknown;
  method: string;
  path: string;
  attempts: number;
  idempotencyKey?: string | undefined;
}

export class PrintpapiError extends Error {
  readonly kind: ErrorKind;
  /** The HTTP status, when there was an answer. */
  readonly status: number | undefined;
  /** The decoded answer (the server's {"error": ...} object), when there was one. */
  readonly body: unknown;
  readonly method: string;
  readonly path: string;
  /** How many requests were sent, the retries included. */
  readonly attempts: number;
  /**
   * Of submit and orders calls: the key every attempt of this call carried. To retry after an error,
   * call again with `idempotency_key: err.idempotencyKey`. Never make a new key: the server may
   * already have the job, and a new key prints twice.
   */
  readonly idempotencyKey: string | undefined;

  constructor(init: ErrorInit) {
    super(init.message);
    this.name = "PrintpapiError";
    this.kind = init.kind;
    this.status = init.status;
    this.body = init.body;
    this.method = init.method;
    this.path = init.path;
    this.attempts = init.attempts;
    this.idempotencyKey = init.idempotencyKey;
  }

  toJSON(): Record<string, unknown> {
    return {
      name: this.name,
      kind: this.kind,
      message: this.message,
      status: this.status,
      body: this.body,
      method: this.method,
      path: this.path,
      attempts: this.attempts,
      idempotencyKey: this.idempotencyKey,
    };
  }
}

/** The kind of an HTTP status of an answer that was not a success. */
export function kindOfStatus(status: number): ErrorKind {
  if (status >= 300 && status < 400) return "redirect";
  switch (status) {
    case 401: return "unauthorized";
    case 402: return "quota";
    case 403: return "forbidden";
    case 404: return "not_found";
    case 409: return "conflict";
    case 429: return "rate_limited";
    case 502: return "bad_gateway";
    case 503: case 504: return "unavailable";
  }
  if (status >= 520 && status <= 524) return "unavailable";
  return status >= 500 ? "server" : "invalid";
}
