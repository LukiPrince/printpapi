// printpapi client: what is repeated, and how long to wait.

/** GET: nothing changes on the server. Keyed: a POST whose idempotency_key makes a repeat safe. */
export type RetryClass = "read" | "keyed" | "none";

const EDGE = [520, 521, 522, 523, 524]; // Cloudflare's "the origin did not answer"

/** The server's own failed download answers 502 `{"error": "downstream: ..."}`: a repeat fetches the same URL again. */
export function isDownstream(body: unknown): boolean {
  const e = (body as { error?: unknown } | null)?.error;
  return typeof e === "string" && e.startsWith("downstream:");
}

/** The answer of POST /jobs while another request with the same key still fetches the payload. */
export function isInProgress(status: number, body: unknown): boolean {
  const e = (body as { error?: unknown } | null)?.error;
  return status === 409 && typeof e === "string" && e.startsWith("a request with this idempotency_key is still in progress");
}

/** May an answer with this status be requested again? */
export function retryableStatus(cls: RetryClass, status: number, body: unknown): boolean {
  if (cls === "none") return false;
  if (EDGE.includes(status) || status === 503 || status === 504) return true;
  if (status === 429) return true;
  // behind a proxy a bare 502 is a restart; the server's own 502 is final
  return status === 502 && (cls === "read" || !isDownstream(body));
}

/** Repeats of a failure without an answer (no connection, a reset, the attempt timeout). */
export function retryableFailure(cls: RetryClass): boolean {
  return cls !== "none";
}

/** Full jitter: a random wait up to min(4000, 250 * 2^n) ms before the (n+1)th repeat. */
export function backoffMs(n: number, random: () => number = Math.random): number {
  return Math.floor(random() * Math.min(4000, 250 * 2 ** n));
}

/** The largest delay setTimeout takes: more fires at once, which is how an Infinity timeout became 1 ms. */
const MAX_TIMER_MS = 2_147_483_647;
/** A delay for setTimeout: a value that is not a number is the longest one, not 1 ms. */
export const timerMs = (ms: number): number => (Number.isNaN(ms) ? MAX_TIMER_MS : Math.min(ms, MAX_TIMER_MS));

/** Waits `ms`; false when `signal` aborted first. */
export function delay(ms: number, signal?: AbortSignal): Promise<boolean> {
  return new Promise((resolve) => {
    if (signal?.aborted) return resolve(false);
    const done = (v: boolean) => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      resolve(v);
    };
    const onAbort = () => done(false);
    const timer = setTimeout(() => done(true), timerMs(ms));
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}
