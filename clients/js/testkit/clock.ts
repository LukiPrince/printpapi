/**
 * A clock that does not wait: a pause of the client (the delay() of src/retry.ts: backoff, polling of an
 * answer "in progress", the pause of wait) is recorded and skipped, and Date.now moves on by it. Every
 * other timer (the per-attempt timeout, the deadline of wait, the HTTP stack's) stays real.
 */
export function fastClock() {
  const delays: number[] = [];
  const realSetTimeout = globalThis.setTimeout;
  const realNow = Date.now;
  let skipped = 0;
  globalThis.setTimeout = ((fn: () => void, ms?: number, ...a: unknown[]) => {
    if (!/src[\\/]retry\.js/.test(new Error().stack ?? "")) return realSetTimeout(fn, ms, ...a);
    delays.push(ms ?? 0);
    skipped += ms ?? 0;
    return realSetTimeout(fn, 0, ...a);
  }) as typeof setTimeout;
  Date.now = () => realNow() + skipped;
  return {
    delays,
    restore() {
      globalThis.setTimeout = realSetTimeout;
      Date.now = realNow;
    },
  };
}
