// printpapi client: no runtime dependencies, only fetch. The API key sits in a private field and
// goes into the Authorization header and nowhere else: not into a URL, a message or an error.
import { toBase64 } from "./base64.ts";
import { type ErrorKind, PrintpapiError, kindOfStatus } from "./errors.ts";
import {
  type RetryClass,
  backoffMs,
  delay,
  isInProgress,
  retryableFailure,
  retryableStatus,
  timerMs,
} from "./retry.ts";
import type * as T from "./types.ts";

/** Per attempt, for everything but a submit. */
export const DEFAULT_TIMEOUT_MS = 30_000;
/**
 * Per attempt, for jobs.submit and orders.print. The server may fetch a payload for up to 120 s
 * (30 s to connect, 120 s in all) and reads a request body for up to 120 s, and a client that gives up
 * earlier does not stop it: the job is queued anyway. So the client outlasts the server.
 */
export const SUBMIT_TIMEOUT_MS = 150_000;
/** How often an answer "still in progress" is asked again, and for how long in all. */
export const IN_PROGRESS_POLL_MS = 1_000;
export const IN_PROGRESS_DEADLINE_MS = 130_000;
export const DEFAULT_RETRIES = 2;
/** What the last poll of jobs.wait gets at most: the deadline has passed, the answer of a healthy server is quick. */
const LAST_POLL_MS = 2_000;

export interface ClientOptions {
  /** Where the server is, e.g. https://print.example.com (no credentials in it). */
  baseUrl: string;
  /** A client key, a session token or the root token. Sent as a bearer token, nothing else. */
  apiKey: string;
  /** Per attempt. Default 30 s, and 150 s for submit and orders. */
  timeoutMs?: number;
  /** Repeats after a failure that is safe to repeat. Default 2. */
  retries?: number;
  /** For tests and special runtimes. Default: the global fetch. */
  fetch?: typeof fetch;
}

export interface CallOptions {
  /** Overrides the per-attempt timeout of this call. */
  timeoutMs?: number;
  /** Overrides the number of repeats of this call. */
  retries?: number;
  signal?: AbortSignal;
}

export interface WaitOptions {
  /** How long to wait for the end of the job in all. Default 5 minutes. */
  timeoutMs?: number;
  /** The first pause between two polls; it grows by 1.5 up to 5 s. Default 500 ms. */
  intervalMs?: number;
  /** Throw kind job_failed for a job that ends failed, instead of returning it. */
  throwOnFailure?: boolean;
  signal?: AbortSignal;
}

/** What jobs.get returns: the job state with the id you asked for. */
export type Job = T.JobStatus & { id: number };

type BytesAllowed<J> = J extends { type: "raw_base64" | "pdf_base64" | "label_base64" }
  ? Omit<J, "content"> & { content: string | Uint8Array }
  : J;
/** A job to submit. A *_base64 type takes its content as base64 text or as bytes. */
export type JobInput = BytesAllowed<T.JobRequest>;

interface Spec {
  method: "GET" | "POST" | "PUT" | "DELETE";
  path: string;
  body?: unknown;
  response: "json" | "bytes";
  retry: RetryClass;
  timeoutMs: number;
  retries: number;
  signal?: AbortSignal | undefined;
  idempotencyKey?: string | undefined;
  /** The one field the call reads: a 2xx object without it is not what the server sends. */
  needs?: Needs | undefined;
}

interface Needs {
  field: string;
  has(v: unknown): boolean;
}
const WHOLE: Needs["has"] = (v) => typeof v === "number" && Number.isInteger(v);
const LIST: Needs["has"] = Array.isArray;
const jobIdNeeded: Needs = { field: "job_id", has: WHOLE };
const listNeeded = (field: string): Needs => ({ field, has: LIST });

interface Core {
  send<R>(spec: Spec): Promise<R>;
  /** A decoded answer without the API key in any string of it: for what goes into an error. */
  clean<V>(v: V): V;
  timeout(fallback: number, o: CallOptions | undefined): number;
  retries(o: CallOptions | undefined): number;
}

function randomKey(): string {
  const b = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
}

/**
 * Nothing the server treats as "no key": it reads an empty or all-whitespace idempotency_key as absent
 * (dispatch.rs, parse_idempotency_key: Unicode white space and \x1c-\x1f). JS \s covers the Unicode part
 * (and a byte-order mark besides), \x85 and \x1c-\x1f are added: a superset, so a key the server would
 * ignore is never sent.
 */
const BLANK_KEY = /^[\s\x1c-\x1f\x85]*$/;

/** The key of one submit: the caller's, unless it is missing or blank, then 128 random bits. */
function keyOf(given: unknown): string {
  if (given === undefined || given === null || (typeof given === "string" && BLANK_KEY.test(given))) {
    return randomKey();
  }
  return given as string;
}

/**
 * An id that goes into a path. Only digits: "../labels/1" would be taken apart by fetch and call
 * another route with the same key, and a route parameter forwarded as it is must not pick the route.
 */
function pathId(id: unknown): string {
  const text = typeof id === "number" || typeof id === "string" ? String(id) : "";
  if (!/^\d+$/.test(text) || !Number.isSafeInteger(Number(text))) {
    throw new TypeError("an id is a whole number, got " + (typeof id === "string" ? "a string that is not one" : typeof id));
  }
  return text;
}

/** The JSON of an answer as it is, or its first 2000 characters after `scrub` (a key cut in half is no key to take out). */
function decodeText(text: string, scrub: (s: string) => string): unknown {
  if (text === "") return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return scrub(text).slice(0, 2000);
  }
}

function messageOf(status: number, body: unknown): string {
  const e = (body as { error?: unknown } | null)?.error;
  return typeof e === "string" ? e : "HTTP " + status;
}

// what the last poll of wait() may fail with at the deadline without that being an answer about the job
const NO_ANSWER_ABOUT_JOB = new Set<ErrorKind>(["network", "timeout", "unavailable", "bad_gateway", "rate_limited"]);
const TERMINAL = new Set<T.JobState>(["done", "failed", "cancelled"]);

class JobsApi {
  readonly #c: Core;
  constructor(core: Core) {
    this.#c = core;
  }

  /**
   * Queue a print job. One idempotency key is made per call (or the one you give) and every attempt
   * carries it, so a repeat after a lost answer returns the same job_id and prints nothing extra.
   */
  async submit(input: JobInput, o?: CallOptions): Promise<T.JobCreated> {
    const key = keyOf(input.idempotency_key);
    const body: Record<string, unknown> = { ...input, idempotency_key: key };
    if (body.content instanceof Uint8Array) body.content = toBase64(body.content);
    return this.#c.send({
      method: "POST", path: "/jobs", body, response: "json", retry: "keyed",
      timeoutMs: this.#c.timeout(SUBMIT_TIMEOUT_MS, o), retries: this.#c.retries(o),
      signal: o?.signal, idempotencyKey: key, needs: jobIdNeeded,
    });
  }

  /** The newest 50 jobs of the org (a print key: its own). */
  async list(o?: CallOptions): Promise<T.JobRow[]> {
    const r = await this.#c.send<T.JobList>({ ...this.#read("/jobs", o), needs: listNeeded("jobs") });
    return r.jobs;
  }

  async get(id: number, o?: CallOptions): Promise<Job> {
    const n = pathId(id);
    const r = await this.#c.send<T.JobStatus>({
      ...this.#read("/jobs/" + n, o), needs: { field: "state", has: (v) => typeof v === "string" },
    });
    return { id: Number(n), ...r };
  }

  /** Cancel a job that is still queued (conflict once an agent has it). */
  async cancel(id: number, o?: CallOptions): Promise<T.CancelResult> {
    return this.#c.send({
      method: "DELETE", path: "/jobs/" + pathId(id), response: "json", retry: "none",
      timeoutMs: this.#c.timeout(DEFAULT_TIMEOUT_MS, o), retries: 0, signal: o?.signal,
    });
  }

  /**
   * Poll until the job is done, failed or cancelled. A failed job is returned unless throwOnFailure.
   * The deadline cuts a poll that is in flight (a server that hangs does not hold the call), but
   * every wait asks once more when its last pause is over, with a budget of its own, so a job that
   * ended during that pause is reported as ended and timeoutMs 0 still checks once.
   */
  async wait(id: number, o: WaitOptions = {}): Promise<Job> {
    const n = pathId(id);
    const aborted = (attempts: number): PrintpapiError =>
      new PrintpapiError({
        kind: "network", message: "waiting for job " + n + " was aborted by the caller", method: "GET",
        path: "/jobs/" + n, attempts,
      });
    if (o.signal?.aborted) throw aborted(0);
    const started = Date.now();
    // a timeout that is no number is no deadline, an interval that is none is the default
    const total = o.timeoutMs === undefined ? 300_000 : Number.isNaN(o.timeoutMs) ? Infinity : Math.max(0, o.timeoutMs);
    let interval = o.intervalMs !== undefined && Number.isFinite(o.intervalMs) && o.intervalMs > 0 ? o.intervalMs : 500;
    let last: Job | undefined;
    // the answers of a success are the server's as they are; what an error shows of them has the key taken out
    const timeout = (): PrintpapiError => {
      const seen = this.#c.clean(last);
      return new PrintpapiError({
        kind: "timeout", message: "job " + n + " is " + (seen === undefined ? "not known" : "still " + seen.state) + " after " + total + " ms",
        body: seen, method: "GET", path: "/jobs/" + n, attempts: 1,
      });
    };
    const deadline = new AbortController();
    const timer = Number.isFinite(total) ? setTimeout(() => deadline.abort(), timerMs(total)) : undefined;
    const onAbort = () => deadline.abort();
    o.signal?.addEventListener("abort", onAbort, { once: true });
    // what a poll gets for its own: the last one is not cut by the deadline that has passed
    // (a client timeout that is no number is no timer for its own calls, but the last poll always has one)
    const own = this.#c.timeout(DEFAULT_TIMEOUT_MS, undefined);
    const lastPollMs = Number.isNaN(own) ? LAST_POLL_MS : Math.min(LAST_POLL_MS, own);
    let final = false;
    try {
      for (;;) {
        final ||= total - (Date.now() - started) <= 0;
        let job: Job;
        try {
          const poll: CallOptions = final ? { timeoutMs: lastPollMs, retries: 0 } : { signal: deadline.signal };
          if (final && o.signal) poll.signal = o.signal;
          job = await this.get(id, poll);
        } catch (e) {
          if (o.signal?.aborted) throw e;
          // the last poll, with an answer behind it: a server that gives none now (restarting, refusing,
          // busy) leaves the job as it was seen, which is the deadline's timeout; only an answer
          // about the job or the key is the caller's to see
          const noAnswer = e instanceof PrintpapiError && last !== undefined && NO_ANSWER_ABOUT_JOB.has(e.kind);
          if (final && noAnswer) throw timeout();
          // a poll the deadline cut during its retry pause fails with the error it was retrying
          const cut = e instanceof PrintpapiError && (final ? e.kind === "timeout" : deadline.signal.aborted && (e.kind === "network" || noAnswer));
          if (!cut) throw e;
          // a poll that the deadline cut has an answer of the server behind it, so one more is asked with a
          // budget of its own; a server that never answered is not asked again
          if (final || last === undefined) throw timeout();
          final = true;
          continue;
        }
        last = job;
        if (TERMINAL.has(job.state)) {
          if (job.state === "failed" && o.throwOnFailure) {
            const shown = this.#c.clean(job);
            throw new PrintpapiError({
              kind: "job_failed", message: "job " + n + " failed: " + (shown.error ?? "no reason given"),
              body: shown, method: "GET", path: "/jobs/" + n, attempts: 1,
            });
          }
          return job;
        }
        if (final) throw timeout();
        const left = total - (Date.now() - started);
        // the pause that reaches the deadline is the last one, whatever the clock says when it ends
        final = left <= interval;
        if (!(await delay(Math.min(interval, Math.max(0, left)), o.signal))) throw aborted(1);
        interval = Math.min(5_000, interval * 1.5);
      }
    } finally {
      clearTimeout(timer);
      o.signal?.removeEventListener("abort", onAbort);
    }
  }

  #read(path: string, o: CallOptions | undefined): Spec {
    return {
      method: "GET", path, response: "json", retry: "read",
      timeoutMs: this.#c.timeout(DEFAULT_TIMEOUT_MS, o), retries: this.#c.retries(o), signal: o?.signal,
    };
  }
}

class ListApi<Item, Key extends string> {
  readonly #c: Core;
  readonly #path: string;
  readonly #key: Key;
  constructor(core: Core, path: string, key: Key) {
    this.#c = core;
    this.#path = path;
    this.#key = key;
  }

  async list(o?: CallOptions): Promise<Item[]> {
    const r = await this.#c.send<Record<Key, Item[]>>({
      method: "GET", path: this.#path, response: "json", retry: "read",
      timeoutMs: this.#c.timeout(DEFAULT_TIMEOUT_MS, o), retries: this.#c.retries(o), signal: o?.signal,
      needs: listNeeded(this.#key),
    });
    return r[this.#key];
  }
}

class OrdersApi {
  readonly #c: Core;
  constructor(core: Core) {
    this.#c = core;
  }

  /** Print an order as a packing slip (the printer must take PDF). Keyed like jobs.submit. */
  async print(input: T.OrderRequest, o?: CallOptions): Promise<T.JobCreated> {
    const key = keyOf(input.idempotency_key);
    return this.#c.send({
      method: "POST", path: "/orders", body: { ...input, idempotency_key: key }, response: "json",
      retry: "keyed", timeoutMs: this.#c.timeout(SUBMIT_TIMEOUT_MS, o), retries: this.#c.retries(o),
      signal: o?.signal, idempotencyKey: key, needs: jobIdNeeded,
    });
  }
}

class LabelsApi extends ListApi<T.LabelTemplate, "templates"> {
  readonly #c: Core;
  constructor(core: Core) {
    super(core, "/labels", "templates");
    this.#c = core;
  }

  async get(id: number, o?: CallOptions): Promise<T.LabelTemplate> {
    return this.#c.send({
      method: "GET", path: "/labels/" + pathId(id), response: "json", retry: "read",
      timeoutMs: this.#c.timeout(DEFAULT_TIMEOUT_MS, o), retries: this.#c.retries(o), signal: o?.signal,
    });
  }

  /** Not repeated: a lost answer may have stored the template, and a repeat is then a conflict. */
  create(spec: T.LabelSpec, o?: CallOptions): Promise<T.LabelTemplate> {
    return this.#write("POST", "/labels", spec, o);
  }

  async update(id: number, spec: T.LabelSpec, o?: CallOptions): Promise<T.LabelTemplate> {
    return this.#write("PUT", "/labels/" + pathId(id), spec, o);
  }

  async delete(id: number, o?: CallOptions): Promise<T.Ok> {
    return this.#write("DELETE", "/labels/" + pathId(id), undefined, o);
  }

  #write<R>(method: "POST" | "PUT" | "DELETE", path: string, body: unknown, o: CallOptions | undefined): Promise<R> {
    return this.#c.send({
      method, path, body, response: "json", retry: "none",
      timeoutMs: this.#c.timeout(DEFAULT_TIMEOUT_MS, o), retries: 0, signal: o?.signal,
    });
  }
}

class ProfileApi {
  readonly #c: Core;
  constructor(core: Core) {
    this.#c = core;
  }

  async get(printerId: number, o?: CallOptions): Promise<T.ReceiptProfileAnswer> {
    return this.#c.send({
      method: "GET", path: "/printers/" + pathId(printerId) + "/receipt-profile", response: "json", retry: "read",
      timeoutMs: this.#c.timeout(DEFAULT_TIMEOUT_MS, o), retries: this.#c.retries(o), signal: o?.signal,
    });
  }

  /** Replaces the whole profile: a field you leave out takes its default. */
  async put(printerId: number, profile: T.ReceiptProfile, o?: CallOptions): Promise<T.ReceiptProfileAnswer> {
    return this.#c.send({
      method: "PUT", path: "/printers/" + pathId(printerId) + "/receipt-profile", body: profile, response: "json",
      retry: "none", timeoutMs: this.#c.timeout(DEFAULT_TIMEOUT_MS, o), retries: 0, signal: o?.signal,
    });
  }

  async delete(printerId: number, o?: CallOptions): Promise<T.Ok> {
    return this.#c.send({
      method: "DELETE", path: "/printers/" + pathId(printerId) + "/receipt-profile", response: "json", retry: "none",
      timeoutMs: this.#c.timeout(DEFAULT_TIMEOUT_MS, o), retries: 0, signal: o?.signal,
    });
  }
}

class ReceiptsApi {
  readonly #c: Core;
  /** The receipt profile of a printer: paper, width, text mode, cutter, output. */
  readonly profile: ProfileApi;
  constructor(core: Core) {
    this.#c = core;
    this.profile = new ProfileApi(core);
  }

  /** Draws a receipt as a PNG and prints nothing. Nothing changes on the server, so it is repeated like a GET. */
  preview(input: T.ReceiptPreviewRequest, o?: CallOptions): Promise<Uint8Array> {
    return this.#c.send({
      method: "POST", path: "/receipts/preview", body: input, response: "bytes", retry: "read",
      timeoutMs: this.#c.timeout(DEFAULT_TIMEOUT_MS, o), retries: this.#c.retries(o), signal: o?.signal,
    });
  }
}

export class Printpapi {
  readonly #apiKey: string;
  readonly #base: string;
  readonly #fetch: typeof fetch;
  readonly #timeoutMs: number | undefined;
  readonly #retries: number;

  readonly jobs: JobsApi;
  readonly printers: ListApi<T.Printer, "printers">;
  readonly computers: ListApi<T.Computer, "computers">;
  readonly orders: OrdersApi;
  readonly labels: LabelsApi;
  readonly receipts: ReceiptsApi;

  constructor(options: ClientOptions) {
    if (typeof options.apiKey !== "string" || options.apiKey === "") {
      throw new TypeError("apiKey is required");
    }
    if (!/^[\x21-\x7e](?:[\x20-\x7e]*[\x21-\x7e])?$/.test(options.apiKey)) {
      // a header value that fetch refuses would be reported as a network fault, three times; the key is not echoed.
      // A space inside is fine (a root token may have one), a space at either end is a paste mistake.
      throw new TypeError("apiKey must be visible ASCII characters only: a pasted key may have a space at its end, a line break or a shortened ... in it");
    }
    let url: URL;
    try {
      url = new URL(options.baseUrl);
    } catch {
      throw new TypeError("baseUrl is not a URL: give it with its scheme, e.g. https://print.example.com");
    }
    if (url.protocol !== "http:" && url.protocol !== "https:") {
      throw new TypeError("baseUrl must be http or https");
    }
    if (url.username !== "" || url.password !== "") {
      // a credential in a URL ends up in logs and error reports: the key goes in apiKey
      throw new TypeError("baseUrl must not carry a user name or password");
    }
    this.#base = url.origin + url.pathname.replace(/\/+$/, "");
    this.#apiKey = options.apiKey;
    this.#fetch = options.fetch ?? ((...a) => fetch(...a));
    this.#timeoutMs = options.timeoutMs;
    this.#retries = options.retries ?? DEFAULT_RETRIES;
    const core: Core = {
      send: (s) => this.#send(s),
      clean: (v) => this.#clean(v) as typeof v,
      timeout: (fallback, o) => o?.timeoutMs ?? this.#timeoutMs ?? fallback,
      retries: (o) => o?.retries ?? this.#retries,
    };
    this.jobs = new JobsApi(core);
    this.printers = new ListApi(core, "/printers", "printers");
    this.computers = new ListApi(core, "/computers", "computers");
    this.orders = new OrdersApi(core);
    this.labels = new LabelsApi(core);
    this.receipts = new ReceiptsApi(core);
  }

  /** The server base URL as the client uses it (without credentials, query or trailing slash). */
  get baseUrl(): string {
    return this.#base;
  }

  health(o?: CallOptions): Promise<T.Health> {
    return this.#send(this.#get("/health", o));
  }

  /** Who the key is: kind, org and, for a key that is not full, its scope. */
  me(o?: CallOptions): Promise<T.Me> {
    return this.#send(this.#get("/me", o));
  }

  #get(path: string, o: CallOptions | undefined): Spec {
    return {
      method: "GET", path, response: "json", retry: "read",
      timeoutMs: o?.timeoutMs ?? this.#timeoutMs ?? DEFAULT_TIMEOUT_MS, retries: o?.retries ?? this.#retries,
      signal: o?.signal,
    };
  }

  /** Never part of a message, a stack or a property: the key is read here and put in one header. */
  #headers(s: Spec): Record<string, string> {
    const h: Record<string, string> = {
      Authorization: "Bearer " + this.#apiKey,
      Accept: s.response === "bytes" ? "image/png" : "application/json",
    };
    if (s.body !== undefined) h["Content-Type"] = "application/json";
    // AGENTS.md pitfall 3: a WAF answers 403 to a bare library UA. A browser sets its own, and
    // drops this header anyway.
    if (typeof document === "undefined") h["User-Agent"] = "Mozilla/5.0 (printpapi-client)";
    return h;
  }

  /** A text without the key: a gateway or a debugging proxy may echo the Authorization header. */
  #scrub(text: string): string {
    return text.split(this.#apiKey).join("[key]");
  }

  /**
   * A decoded answer without the key, in every string and name of it. Done on the decoded value, not on
   * the text: the key may be written with any JSON escape (\/, \u002b, \" ) that the text does not show.
   * Only for what goes into an error. A success is returned as the server sent it: a short root token
   * ("print", "1") would rewrite real data.
   */
  #clean(v: unknown): unknown {
    if (typeof v === "string") return this.#scrub(v);
    if (Array.isArray(v)) return v.map((x) => this.#clean(x));
    if (typeof v === "object" && v !== null) {
      return Object.fromEntries(Object.entries(v).map(([k, x]) => [this.#scrub(k), this.#clean(x)]));
    }
    return v;
  }

  async #send<R>(s: Spec): Promise<R> {
    const url = this.#base + s.path;
    // built before the first attempt and outside the try: what is wrong here is the caller's (a BigInt
    // in the body), nothing was sent, and it is no network fault to repeat
    const headers = this.#headers(s);
    const body = s.body === undefined ? undefined : JSON.stringify(s.body);
    let attempts = 0;
    let repeats = 0;
    let inProgressSince: number | undefined;
    const fail = (kind: ErrorKind, message: string, status?: number, answer?: unknown): PrintpapiError =>
      new PrintpapiError({
        kind, message: s.method + " " + s.path + ": " + message, status, body: answer, method: s.method,
        path: s.path, attempts, idempotencyKey: s.idempotencyKey,
      });
    const pause = async (ms: number): Promise<void> => {
      if (!(await delay(ms, s.signal))) throw fail("network", "aborted by the caller");
    };
    for (;;) {
      if (s.signal?.aborted) throw fail("network", "aborted by the caller");
      attempts++;
      const ac = new AbortController();
      let timedOut = false;
      const timer = setTimeout(() => {
        timedOut = true;
        ac.abort();
      }, timerMs(s.timeoutMs));
      const onAbort = () => ac.abort();
      s.signal?.addEventListener("abort", onAbort, { once: true });
      let res: Response;
      let text = "";
      let bytes: Uint8Array | undefined;
      try {
        const init: RequestInit = {
          method: s.method, headers, signal: ac.signal,
          // a redirect would carry the Authorization header to another host, or turn a POST into a GET
          redirect: "manual", credentials: "omit", cache: "no-store",
        };
        if (body !== undefined) init.body = body;
        res = await this.#fetch(url, init);
        const redirect = res.type === "opaqueredirect" || (res.status >= 300 && res.status < 400);
        if (redirect) {
          // not read, and nothing is sent anywhere else
          void res.body?.cancel().catch(() => {});
        } else if (res.ok && s.response === "bytes") {
          bytes = new Uint8Array(await res.arrayBuffer());
        } else {
          text = await res.text();
        }
      } catch (e) {
        if (s.signal?.aborted) throw fail("network", "aborted by the caller");
        const kind: ErrorKind = timedOut ? "timeout" : "network";
        if (retryableFailure(s.retry) && repeats < s.retries) {
          await pause(backoffMs(repeats++));
          continue;
        }
        const code = (e as { cause?: { code?: unknown } }).cause?.code;
        throw fail(
          kind,
          timedOut ? "no answer within " + s.timeoutMs + " ms" : "no answer from " + this.#base + (typeof code === "string" ? " (" + code + ")" : ""),
        );
      } finally {
        clearTimeout(timer);
        s.signal?.removeEventListener("abort", onAbort);
      }
      if (res.type === "opaqueredirect" || (res.status >= 300 && res.status < 400)) {
        throw fail(
          "redirect",
          "the server answered " + (res.status || "a redirect") + ": check baseUrl (" + this.#base + "), for example http against https",
          res.status || undefined,
        );
      }
      if (res.ok) {
        // A success is what the document says or it is not the server: a captive portal, a login page or a
        // wrong baseUrl answer 200 with a page, and "job_id: undefined" must never look like a queued job.
        if (s.response === "bytes") {
          if (bytes && (res.headers.get("content-type") ?? "").toLowerCase().startsWith("image/png")) return bytes as R;
          throw fail("server", "the answer is not a PNG image, check baseUrl (" + this.#base + ")", res.status);
        }
        const got = decodeText(text, (t) => this.#scrub(t));
        const isObject = typeof got === "object" && got !== null && !Array.isArray(got);
        if (isObject && (s.needs === undefined || s.needs.has((got as Record<string, unknown>)[s.needs.field]))) return got as R;
        throw fail(
          "server",
          isObject
            ? "the answer has no usable " + s.needs?.field + ", check baseUrl (" + this.#base + ")"
            : "the answer is not JSON, check baseUrl (" + this.#base + ")",
          res.status,
          this.#clean(got),
        );
      }
      const raw = decodeText(text, (t) => this.#scrub(t));
      // decided on what the server sent, shown without the key: a key of one letter must not break the match
      const answer = this.#clean(raw);
      if (s.retry === "keyed" && isInProgress(res.status, raw)) {
        // the same key is still being fetched by another request: ask again, it is not a failure
        inProgressSince ??= Date.now();
        if (Date.now() - inProgressSince < IN_PROGRESS_DEADLINE_MS) {
          await pause(IN_PROGRESS_POLL_MS);
          continue;
        }
        throw fail("conflict", messageOf(res.status, answer) + " (still in progress after " + IN_PROGRESS_DEADLINE_MS + " ms)", res.status, answer);
      }
      if (retryableStatus(s.retry, res.status, raw) && repeats < s.retries) {
        await pause(backoffMs(repeats++));
        continue;
      }
      throw fail(kindOfStatus(res.status), messageOf(res.status, answer), res.status, answer);
    }
  }
}
