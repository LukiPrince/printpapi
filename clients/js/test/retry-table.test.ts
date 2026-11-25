import assert from "node:assert/strict";
import type { ServerResponse } from "node:http";
import { describe, it } from "node:test";
import { IN_PROGRESS_DEADLINE_MS, IN_PROGRESS_POLL_MS, Printpapi, PrintpapiError } from "../src/index.ts";
import { backoffMs, retryableStatus } from "../src/retry.ts";
import { fastClock } from "../testkit/clock.ts";
import { throwaway } from "../testkit/server.ts";

const err = async (p: Promise<unknown>): Promise<PrintpapiError> => {
  const e = await p.then(() => assert.fail("must reject"), (x: unknown) => x);
  assert.ok(e instanceof PrintpapiError, String(e));
  return e;
};
const json = (res: ServerResponse, status: number, body: unknown) => {
  res.statusCode = status;
  res.setHeader("Content-Type", "application/json");
  res.end(JSON.stringify(body));
};
const job = { printer_id: 1, type: "raw_base64" as const, content: "AA==" };
const spec = { name: "n", content: "image" as const, label_mm: { w: 1, h: 1 }, dpi: 203, rotate: 0 as const, crop_percent: { t: 0, r: 0, b: 0, l: 0 }, place_mm: { x: 0, y: 0, w: 1, h: 1 } };

describe("which statuses are repeated, and for which calls (test 11, 13)", { timeout: 120_000 }, () => {
  const REPEATED = [429, 503, 504, 520, 521, 522, 523, 524];
  for (const status of REPEATED) {
    it("a " + status + " is repeated once for a read and for a keyed submit, with the same key", async () => {
      const t = await throwaway((_req, res, seen) => {
        const first = t.seen.filter((s) => s.method === seen.method && s.path === seen.path).length === 1;
        if (first) {
          res.statusCode = status;
          res.end("<html>edge</html>");
        } else {
          json(res, 200, seen.path === "/jobs" && seen.method === "POST" ? { job_id: 7 } : { printers: [] });
        }
      });
      try {
        const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
        assert.deepEqual(await c.printers.list(), []);
        assert.equal((await c.jobs.submit(job)).job_id, 7);
        assert.equal(t.seen.length, 4, "two calls, each asked twice");
        const keys = t.seen.filter((s) => s.method === "POST").map((s) => JSON.parse(s.body).idempotency_key);
        assert.equal(keys[0], keys[1]);
        assert.ok(keys[0]);
      } finally {
        await t.close();
      }
    });
  }

  for (const status of REPEATED) {
    it("a " + status + " is final for PUT, DELETE and POST /labels", async () => {
      const t = await throwaway((_req, res) => {
        res.statusCode = status;
        res.end("<html>edge</html>");
      });
      try {
        const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
        const calls = [() => c.labels.create(spec), () => c.labels.update(1, spec), () => c.labels.delete(1), () => c.jobs.cancel(1), () => c.receipts.profile.put(1, {}), () => c.receipts.profile.delete(1)];
        for (const call of calls) {
          const e = await err(call());
          assert.equal(e.attempts, 1);
        }
        assert.equal(t.seen.length, calls.length, "one request each");
      } finally {
        await t.close();
      }
    });
  }

  it("retryableStatus names exactly the repeated statuses", () => {
    for (const cls of ["read", "keyed"] as const) {
      for (const s of REPEATED) assert.equal(retryableStatus(cls, s, undefined), true, cls + " " + s);
      for (const s of [400, 401, 402, 403, 404, 409, 500, 501, 505, 519, 525]) assert.equal(retryableStatus(cls, s, undefined), false, cls + " " + s);
    }
    for (const s of [...REPEATED, 502]) assert.equal(retryableStatus("none", s, undefined), false);
    assert.equal(retryableStatus("read", 502, { error: "downstream: x" }), true, "a read repeats any 502");
    assert.equal(retryableStatus("keyed", 502, { error: "downstream: x" }), false);
    assert.equal(retryableStatus("keyed", 502, undefined), true);
  });

  it("waits a random time up to min(4000, 250 * 2^n) ms before the (n+1)th repeat", () => {
    assert.deepEqual([0, 1, 2, 3, 4, 5, 9].map((n) => backoffMs(n, () => 0)), [0, 0, 0, 0, 0, 0, 0]);
    assert.deepEqual([0, 1, 2, 3, 4, 5, 9].map((n) => backoffMs(n, () => 0.5)), [125, 250, 500, 1000, 2000, 2000, 2000]);
    assert.deepEqual([0, 1, 2, 3, 4, 5, 9].map((n) => backoffMs(n, () => 0.999999)), [249, 499, 999, 1999, 3999, 3999, 3999]);
  });

  it("a repeat waits the backoff, and gives up after retries repeats", async () => {
    const t = await throwaway((_req, res) => json(res, 503, { error: "busy" }));
    const clock = fastClock();
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456", retries: 3 });
      const e = await err(c.jobs.list());
      assert.equal(e.kind, "unavailable");
      assert.equal(e.attempts, 4);
      assert.equal(clock.delays.length, 3, "a pause before each repeat");
      clock.delays.forEach((d, n) => assert.ok(d >= 0 && d < Math.min(4000, 250 * 2 ** n), n + ": " + d));
    } finally {
      clock.restore();
      await t.close();
    }
  });
});

describe("the answer 'still in progress' (test 12)", { timeout: 60_000 }, () => {
  const busy = (res: ServerResponse) =>
    json(res, 409, { error: "a request with this idempotency_key is still in progress, retry shortly" });

  it("is asked again every second with the same key, and the answer comes with no retries used", async () => {
    let n = 0;
    const t = await throwaway((_req, res) => (++n <= 3 ? busy(res) : json(res, 200, { job_id: 9 })));
    const clock = fastClock();
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456", retries: 0 });
      assert.equal((await c.jobs.submit(job)).job_id, 9);
      assert.deepEqual(clock.delays, [IN_PROGRESS_POLL_MS, IN_PROGRESS_POLL_MS, IN_PROGRESS_POLL_MS]);
      assert.equal(IN_PROGRESS_POLL_MS, 1000);
      assert.equal(new Set(t.seen.map((s) => JSON.parse(s.body).idempotency_key)).size, 1);
    } finally {
      clock.restore();
      await t.close();
    }
  });

  it("stops after 130 s with kind conflict and the key", async () => {
    const t = await throwaway((_req, res) => busy(res));
    const clock = fastClock();
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456", retries: 0 });
      const e = await err(c.jobs.submit({ ...job, idempotency_key: "held" }));
      assert.equal(IN_PROGRESS_DEADLINE_MS, 130_000);
      assert.equal(e.kind, "conflict");
      assert.equal(e.status, 409);
      assert.equal(e.idempotencyKey, "held");
      assert.match(e.message, /still in progress after 130000 ms/);
      assert.ok(clock.delays.length >= 129 && clock.delays.length <= 131, "polled about 130 times: " + clock.delays.length);
      assert.ok(clock.delays.every((d) => d === IN_PROGRESS_POLL_MS));
    } finally {
      clock.restore();
      await t.close();
    }
  });

  it("is not asked again for a read: a 409 there is final", async () => {
    const t = await throwaway((_req, res) => busy(res));
    try {
      const e = await err(new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" }).jobs.list());
      assert.equal(e.kind, "conflict");
      assert.equal(t.seen.length, 1);
    } finally {
      await t.close();
    }
  });
});

describe("jobs.wait polls with a growing pause (test 7)", { timeout: 60_000 }, () => {
  it("1.5 times longer each time up to 5 s, and the last pause ends at the deadline", async () => {
    const t = await throwaway((_req, res) => json(res, 200, { state: "queued" }));
    const clock = fastClock();
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
      const e = await err(c.jobs.wait(1, { timeoutMs: 20_000 }));
      assert.equal(e.kind, "timeout");
      assert.match(e.message, /still queued after 20000 ms/);
      assert.deepEqual(clock.delays.slice(0, 7), [500, 750, 1125, 1687.5, 2531.25, 3796.875, 5000]);
      assert.equal(clock.delays.length, 8);
      assert.ok((clock.delays[7] ?? 0) < 5000, "the last pause is what is left of the 20 s");
      const total = clock.delays.reduce((a, b) => a + b, 0);
      assert.ok(total >= 19_000 && total <= 20_000, "paused for the deadline, not past it: " + total);
    } finally {
      clock.restore();
      await t.close();
    }
  });

  it("starts at intervalMs", async () => {
    const t = await throwaway((_req, res) => json(res, 200, { state: "queued" }));
    const clock = fastClock();
    try {
      await err(new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" }).jobs.wait(1, { timeoutMs: 11_000, intervalMs: 100 }));
      assert.equal(clock.delays[0], 100);
      assert.equal(clock.delays[1], 150);
    } finally {
      clock.restore();
      await t.close();
    }
  });
});
