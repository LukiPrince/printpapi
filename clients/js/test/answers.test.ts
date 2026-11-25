import assert from "node:assert/strict";
import { after, before, describe, it } from "node:test";
import { inspect } from "node:util";
import { Printpapi, PrintpapiError } from "../src/index.ts";
import { fastClock } from "../testkit/clock.ts";
import { type Harness, harness } from "../testkit/harness.ts";
import { FakeAgent, type Org, createOrg, faultProxy, throwaway } from "../testkit/server.ts";

const err = async (p: Promise<unknown>): Promise<PrintpapiError> => {
  const e = await p.then(() => assert.fail("must reject"), (x: unknown) => x);
  assert.ok(e instanceof PrintpapiError, String(e));
  return e;
};
const job = (printer_id: number, s = "x") => ({ printer_id, type: "raw_base64" as const, content: btoa(s) });

describe("a key the server reads as no key is never sent (real server)", { timeout: 120_000 }, () => {
  let h: Harness;
  let org: Org;
  let agent: FakeAgent;
  before(async () => {
    h = await harness();
    org = await createOrg(h.server, "blank");
    agent = await new FakeAgent(h.server.url, org.agentKey, h.recorder.fetch).register("pc", [
      { name: "Zebra" }, { name: "Laser", can_pdf: true },
    ]);
  });
  after(() => h.stop());
  const count = async () => (await h.client(org.key).jobs.list()).length;

  // the answers of the first two requests are lost: three attempts, so an unkeyed call prints three times
  const lossy = (n: number) => faultProxy(h.server.url, (r) => (r.n <= n ? "drop" : "forward"));

  for (const [name, blank] of [["empty", ""], ["spaces", "  "], ["a separator", "\u001c"], ["a line break", "\n"], ["a no-break space", "\u00a0"]] as const) {
    it("submit with a blank key (" + name + ") still prints once after two lost answers", async () => {
      const before = await count();
      const proxy = await lossy(2);
      try {
        const c = h.client(org.key, { baseUrl: proxy.url });
        await c.jobs.submit({ ...job(agent.printer("Zebra"), "blank"), idempotency_key: blank });
        assert.equal(proxy.requests.length, 3);
        const keys = new Set(proxy.requests.map((r) => JSON.parse(r.body).idempotency_key as string));
        assert.equal(keys.size, 1, "one key for all attempts");
        assert.match([...keys][0] ?? "", /^[0-9a-f]{32}$/, "a random one, not the blank");
        assert.equal(await count(), before + 1);
      } finally {
        await proxy.close();
      }
    });
  }

  it("orders.print with a blank key prints once after two lost answers", async () => {
    const before = await count();
    const proxy = await lossy(2);
    try {
      const c = h.client(org.key, { baseUrl: proxy.url });
      const order = { number: "1", customer: "Jane", lines: [{ qty: 1, sku: "A", name: "Widget", total: "1 EUR" }] };
      for (const blank of ["", " \u001f "]) {
        proxy.requests.length = 0;
        await c.orders.print({ printer_id: agent.printer("Laser"), order, idempotency_key: blank });
        assert.equal(proxy.requests.length, 3);
      }
      assert.equal(await count(), before + 2);
    } finally {
      await proxy.close();
    }
  });

  it("a key the caller gives is kept as it is, and hands back on failure", async () => {
    const proxy = await faultProxy(h.server.url, () => "drop");
    try {
      const e = await err(h.client(org.key, { baseUrl: proxy.url, retries: 0 }).jobs.submit({ ...job(agent.printer("Zebra")), idempotency_key: "order-77" }));
      assert.equal(e.idempotencyKey, "order-77");
    } finally {
      await proxy.close();
    }
  });
});

describe("an answer that is not what the server sends is no success", { timeout: 60_000 }, () => {
  const html = "<html><body>Sign in</body></html>";
  const servers = {
    page: (_req: unknown, res: import("node:http").ServerResponse) => {
      res.setHeader("Content-Type", "text/html");
      res.end(html);
    },
    empty: (_req: unknown, res: import("node:http").ServerResponse) => res.end(),
    text: (_req: unknown, res: import("node:http").ServerResponse) => {
      res.setHeader("Content-Type", "application/json");
      res.end('"hello"');
    },
    list: (_req: unknown, res: import("node:http").ServerResponse) => {
      res.setHeader("Content-Type", "application/json");
      res.end("[]");
    },
  };
  for (const [name, handler] of Object.entries(servers)) {
    it("a 200 with " + name + " is kind server for submit, list, get, orders and preview", async () => {
      const t = await throwaway(handler);
      try {
        const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
        const calls: [string, () => Promise<unknown>][] = [
          ["POST /jobs", () => c.jobs.submit(job(1))],
          ["GET /jobs", () => c.jobs.list()],
          ["GET /jobs/3", () => c.jobs.get(3)],
          ["GET /printers", () => c.printers.list()],
          ["POST /orders", () => c.orders.print({ printer_id: 1, order: { number: "1" } })],
          ["POST /receipts/preview", () => c.receipts.preview({ receipt: { content: [{ type: "text", text: "x" }] } } as never)],
        ];
        for (const [what, call] of calls) {
          const e = await err(call());
          assert.equal(e.kind, "server", what);
          assert.equal(e.status, 200, what);
          assert.equal(e.attempts, 1, what + ": a page is no reason to ask again");
          assert.match(e.message, /not (JSON|a PNG image), check baseUrl/, what);
          if (what === "POST /jobs" || what === "POST /orders") assert.match(e.idempotencyKey ?? "", /^[0-9a-f]{32}$/, what);
        }
        assert.equal(t.seen.length, 6);
      } finally {
        await t.close();
      }
    });
  }

  it("wait does not poll a page until its deadline", async () => {
    const t = await throwaway(servers.page);
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
      const e = await err(c.jobs.wait(1, { timeoutMs: 5000 }));
      assert.equal(e.kind, "server");
      assert.equal(t.seen.length, 1);
    } finally {
      await t.close();
    }
  });
});

describe("a mistake of the caller is no network fault", { timeout: 60_000 }, () => {
  it("refuses a key that fetch could not send, without echoing it", () => {
    for (const key of ["key\u2026", "\ufeffroot-token", "line\nbreak", "tab\tinside", " lead", "trail ", "two  \n", "Bearer\u00a0abc"]) {
      assert.throws(
        () => new Printpapi({ baseUrl: "http://127.0.0.1:1", apiKey: key }),
        (e: Error) => e instanceof TypeError && /visible ASCII/.test(e.message) && !e.message.includes(key.trim()),
        JSON.stringify(key),
      );
    }
  });

  it("takes a key with a space inside, the server accepts it as a root token", async () => {
    const t = await throwaway((req, res) => {
      res.setHeader("Content-Type", "application/json");
      res.end(JSON.stringify({ ok: true, auth: req.headers.authorization }));
    });
    try {
      for (const key of ["two words", "a b  c", 'q"uote\\']) {
        const c = new Printpapi({ baseUrl: t.url, apiKey: key });
        await c.health();
        assert.equal(t.seen.at(-1)?.headers.authorization, "Bearer " + key);
      }
    } finally {
      await t.close();
    }
  });

  it("a body that cannot be JSON rejects with the TypeError and sends nothing", async () => {
    const t = await throwaway((_req, res) => res.end("{}"));
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
      await assert.rejects(c.jobs.submit({ ...job(1), printer_id: 1n as never }), TypeError);
      assert.equal(t.seen.length, 0);
    } finally {
      await t.close();
    }
  });

  it("an aborted signal says no request was sent", async () => {
    const t = await throwaway((_req, res) => res.end("{}"));
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
      const e = await err(c.jobs.list({ signal: AbortSignal.abort() }));
      assert.equal(e.kind, "network");
      assert.equal(e.attempts, 0);
      assert.equal(t.seen.length, 0);
    } finally {
      await t.close();
    }
  });

  it("a timeout that is not a finite number is no timer, not 1 ms", async () => {
    const t = await throwaway((_req, res) => {
      setTimeout(() => {
        res.setHeader("Content-Type", "application/json");
        res.end(JSON.stringify({ ok: true, signup: "closed", password_reset: false }));
      }, 100);
    });
    try {
      for (const timeoutMs of [Infinity, Number.NaN, 2 ** 40]) {
        const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456", timeoutMs });
        assert.equal((await c.health()).ok, true, String(timeoutMs));
      }
      assert.equal(t.seen.length, 3, "each took one request");
    } finally {
      await t.close();
    }
  });

  it("an id that is not a whole number sends nothing and rejects with a TypeError", async () => {
    const t = await throwaway((_req, res) => res.end("{}"));
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
      const spec = { name: "n", content: "image" as const, label_mm: { w: 1, h: 1 }, dpi: 203, rotate: 0 as const, crop_percent: { t: 0, r: 0, b: 0, l: 0 }, place_mm: { x: 0, y: 0, w: 1, h: 1 } };
      for (const bad of ["../labels/1", "1?x", "1/2", Number.NaN, -1, 1.5, "", "0x10", 1e21, null, {}] as const) {
        const id = bad as unknown as number;
        const calls = [
          () => c.jobs.get(id), () => c.jobs.cancel(id), () => c.jobs.wait(id), () => c.labels.get(id),
          () => c.labels.update(id, spec), () => c.labels.delete(id), () => c.receipts.profile.get(id),
          () => c.receipts.profile.put(id, {}), () => c.receipts.profile.delete(id),
        ];
        for (const p of calls) await assert.rejects(p(), TypeError, JSON.stringify(String(bad)));
      }
      assert.equal(t.seen.length, 0);
    } finally {
      await t.close();
    }
  });

  it("an id that is a number written as text still works: a route parameter can be forwarded", async () => {
    const t = await throwaway((_req, res) => {
      res.setHeader("Content-Type", "application/json");
      res.end(JSON.stringify({ state: "queued" }));
    });
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
      const j = await c.jobs.get("7" as unknown as number);
      assert.equal(j.id, 7);
      assert.equal(t.seen[0]?.path, "/jobs/7");
    } finally {
      await t.close();
    }
  });
});

describe("the key does not come back in an error", { timeout: 60_000 }, () => {
  it("is taken out of what a gateway echoes", async () => {
    const key = "SECRETKEY-0123456789";
    const t = await throwaway((req, res) => {
      const page = req.url !== "/jobs";
      res.setHeader("Content-Type", page ? "text/html" : "application/json");
      const echoed = "blocked: Authorization " + String(req.headers.authorization);
      res.statusCode = req.url === "/me" ? 200 : 403; // /me: a 200 page that is no answer of the server
      res.end(page ? "<p>" + echoed + "</p>" : JSON.stringify({ error: echoed }));
    });
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: key });
      const errors = [await err(c.jobs.list()), await err(c.printers.list()), await err(c.me())];
      for (const e of errors) {
        const shown = [e.message, e.stack ?? "", JSON.stringify(e), inspect(e, { depth: 6 }), JSON.stringify(e.body)].join("\n");
        assert.ok(!shown.includes(key), shown);
        assert.match(shown, /\[key\]/);
      }
      assert.equal(errors[0]?.kind, "forbidden");
    } finally {
      await t.close();
    }
  });
});

describe("wait keeps its deadline while the server hangs", { timeout: 60_000 }, () => {
  it("gives up at timeoutMs with kind timeout and one request, not after the per-attempt timeouts", async () => {
    const t = await throwaway(() => {});
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456", timeoutMs: 3000 });
      const started = Date.now();
      const e = await err(c.jobs.wait(1, { timeoutMs: 500 }));
      assert.ok(Date.now() - started < 2000, "took " + (Date.now() - started) + " ms");
      assert.equal(e.kind, "timeout");
      assert.match(e.message, /job 1 is not known after 500 ms/);
      assert.equal(t.seen.length, 1);
    } finally {
      await t.close();
    }
  });

  it("an abort by the caller is still an abort, not a timeout", async () => {
    const t = await throwaway(() => {});
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
      const ac = new AbortController();
      setTimeout(() => ac.abort(), 100);
      const e = await err(c.jobs.wait(1, { timeoutMs: 5000, signal: ac.signal }));
      assert.equal(e.kind, "network");
    } finally {
      await t.close();
    }
  });
});

describe("a short key does not rewrite a success (real server, root token \"admin\")", { timeout: 120_000 }, () => {
  it("returns the names the server sent", async () => {
    const h = await harness({ PRINTAPI_TOKEN: "admin" });
    try {
      await new FakeAgent(h.server.url, "admin", h.recorder.fetch).register("admin-pc", [{ name: "admin-printer" }]);
      const c = h.client("admin");
      const computers = await c.computers.list();
      assert.deepEqual(computers.map((x) => x.name), ["admin-pc"]);
      assert.deepEqual((await c.printers.list()).map((x) => x.name), ["admin-printer"]);
    } finally {
      await h.stop();
    }
  });
});

describe("a success is returned as the server sent it, whatever the key is", { timeout: 60_000 }, () => {
  it("a key that is a digit, a word or a prefix of a name changes nothing", async () => {
    const t = await throwaway((req, res) => {
      res.setHeader("Content-Type", "application/json");
      if (req.method === "POST") res.end('{"job_id": 1}');
      else if (req.url === "/jobs/1") res.end('{"state": "done"}');
      else res.end('{"printers": [{"name": "printer-1", "state": "done"}]}');
    });
    try {
      for (const apiKey of ["1", "done", "print", "state"]) {
        const c = new Printpapi({ baseUrl: t.url, apiKey });
        assert.equal((await c.jobs.submit(job(1))).job_id, 1, apiKey);
        assert.equal((await c.jobs.wait(1)).state, "done", apiKey);
        assert.deepEqual((await c.printers.list()).map((p) => p.name), ["printer-1"], apiKey);
      }
    } finally {
      await t.close();
    }
  });

  it("an error is still decided on what the server sent: a key of one letter does not break the in-progress answer", async () => {
    let n = 0;
    const t = await throwaway((_req, res) => {
      res.setHeader("Content-Type", "application/json");
      if (++n === 1) {
        res.statusCode = 409;
        res.end(JSON.stringify({ error: "a request with this idempotency_key is still in progress, retry shortly" }));
      } else res.end('{"job_id": 4}');
    });
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "a", retries: 0 });
      assert.equal((await c.jobs.submit(job(1))).job_id, 4);
    } finally {
      await t.close();
    }
  });
});

describe("the key does not come back in an error, in whatever JSON escape it is echoed", { timeout: 60_000 }, () => {
  const keys = ["abc/def+ghi-0123456", 'ab"cd\\ef-0123456789', "caf\u00e9-no", "p+q/r=s"];
  const forms: [string, (key: string) => string][] = [
    ["plain JSON.stringify", (k) => JSON.stringify({ error: "blocked Bearer " + k })],
    ["escaped slash and plus", (k) => JSON.stringify({ error: "blocked Bearer " + k }).replace(/\//g, "\\/").replace(/\+/g, "\\u002b")],
    ["nested and as a name", (k) => JSON.stringify({ error: "x", detail: { headers: [{ [k]: "v", Authorization: "Bearer " + k }] } })],
  ];
  for (const [name, body] of forms) {
    it(name, async () => {
      for (const key of keys.filter((k) => /^[\x21-\x7e]+$/.test(k))) {
        const t = await throwaway((_req, res) => {
          res.statusCode = 403;
          res.setHeader("Content-Type", "application/json");
          res.end(body(key));
        });
        try {
          const e = await err(new Printpapi({ baseUrl: t.url, apiKey: key }).jobs.list());
          const shown = [e.message, e.stack ?? "", JSON.stringify(e), inspect(e, { depth: 8 })].join("\n");
          assert.ok(!shown.includes(key), key + " in " + shown);
          assert.match(shown, /\[key\]/);
          assert.equal(e.kind, "forbidden");
        } finally {
          await t.close();
        }
      }
    });
  }

  it("a text body is cut after the key is taken out, not before", async () => {
    const key = "SECRETKEY-0123456789";
    const t = await throwaway((_req, res) => {
      res.statusCode = 403;
      res.end("x".repeat(1995) + key);
    });
    try {
      const e = await err(new Printpapi({ baseUrl: t.url, apiKey: key }).jobs.list());
      assert.ok(!JSON.stringify(e).includes("SECRET"), JSON.stringify(e).slice(-40));
    } finally {
      await t.close();
    }
  });
});

describe("a 2xx object that lacks what the call reads is no success", { timeout: 60_000 }, () => {
  it("submit, orders, the lists, get and wait say kind server", async () => {
    const t = await throwaway((_req, res) => {
      res.setHeader("Content-Type", "application/json");
      res.end('{"status": "ok"}');
    });
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
      const calls: [string, () => Promise<unknown>][] = [
        ["POST /jobs", () => c.jobs.submit(job(1))],
        ["POST /orders", () => c.orders.print({ printer_id: 1, order: { number: "1" } })],
        ["GET /jobs", () => c.jobs.list()],
        ["GET /printers", () => c.printers.list()],
        ["GET /computers", () => c.computers.list()],
        ["GET /labels", () => c.labels.list()],
        ["GET /jobs/3", () => c.jobs.get(3)],
      ];
      for (const [what, call] of calls) {
        const e = await err(call());
        assert.equal(e.kind, "server", what);
        assert.equal(e.attempts, 1, what);
        assert.match(e.message, /has no .*, check baseUrl/, what);
      }
      const before = t.seen.length;
      const w = await err(c.jobs.wait(1, { timeoutMs: 5000 }));
      assert.equal(w.kind, "server");
      assert.equal(t.seen.length, before + 1, "wait does not poll it until its deadline");
    } finally {
      await t.close();
    }
  });

  it("a job_id that is not a whole number does not count either", async () => {
    const t = await throwaway((_req, res) => {
      res.setHeader("Content-Type", "application/json");
      res.end('{"job_id": "7"}');
    });
    try {
      const e = await err(new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" }).jobs.submit(job(1)));
      assert.equal(e.kind, "server");
    } finally {
      await t.close();
    }
  });
});

describe("wait with a signal that is already aborted", { timeout: 60_000 }, () => {
  it("rejects at once with no request, like get and list", async () => {
    const t = await throwaway((_req, res) => {
      res.setHeader("Content-Type", "application/json");
      res.end('{"state": "queued"}');
    });
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
      const started = Date.now();
      const e = await err(c.jobs.wait(1, { signal: AbortSignal.abort(), timeoutMs: 4000 }));
      assert.ok(Date.now() - started < 1000, "took " + (Date.now() - started) + " ms");
      assert.equal(e.kind, "network");
      assert.equal(e.attempts, 0);
      assert.equal(t.seen.length, 0);
    } finally {
      await t.close();
    }
  });
});

describe("wait asks once more after its last pause", { timeout: 60_000 }, () => {
  /** queued until `ms` after the server started, then done: real time, because the deadline timer is real. */
  const doneAfter = async (ms: number) => {
    const t0 = Date.now();
    return throwaway((_req, res) => {
      res.setHeader("Content-Type", "application/json");
      res.end(JSON.stringify({ state: Date.now() - t0 >= ms ? "done" : "queued" }));
    });
  };
  const cases: [string, number, number, number][] = [
    ["interval longer than the timeout", 1000, 2000, 200],
    ["a job that ends in the last pause", 2000, 1500, 1600],
    ["a long interval and a short timeout", 1000, 5000, 100],
  ];
  for (const [name, timeoutMs, intervalMs, at] of cases) {
    it(name + ": done is returned, not a timeout naming a stale state", async () => {
      const t = await doneAfter(at);
      try {
        const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
        assert.equal((await c.jobs.wait(1, { timeoutMs, intervalMs })).state, "done");
      } finally {
        await t.close();
      }
    });
  }

  it("timeoutMs 0 checks once", async () => {
    const t = await doneAfter(0);
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
      assert.equal((await c.jobs.wait(1, { timeoutMs: 0 })).state, "done");
      assert.equal(t.seen.length, 1);
    } finally {
      await t.close();
    }
  });

  it("a job that does not end still times out with the state it was last seen in, after one more request", async () => {
    const t = await doneAfter(60_000);
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
      const e = await err(c.jobs.wait(1, { timeoutMs: 300, intervalMs: 5000 }));
      assert.equal(e.kind, "timeout");
      assert.match(e.message, /still queued after 300 ms/);
      assert.equal(t.seen.length, 2, "the first poll and the last one");
      const z = await err(c.jobs.wait(1, { timeoutMs: 0 }));
      assert.match(z.message, /still queued after 0 ms/);
    } finally {
      await t.close();
    }
  });

  it("the last request has a budget of its own and a hanging server does not hold wait for long", async () => {
    let n = 0;
    const t = await throwaway((_req, res) => {
      res.setHeader("Content-Type", "application/json");
      if (++n === 1) res.end('{"state": "queued"}'); // the last poll gets no answer
    });
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456", timeoutMs: 400 });
      const started = Date.now();
      const e = await err(c.jobs.wait(1, { timeoutMs: 200, intervalMs: 5000 }));
      assert.equal(e.kind, "timeout");
      assert.match(e.message, /still queued after 200 ms/);
      assert.ok(Date.now() - started < 1500, "took " + (Date.now() - started) + " ms");
      assert.equal(n, 2, "no repeats of the last poll");
    } finally {
      await t.close();
    }
  });
});

describe("wait with a timeout or an interval that is no number", { timeout: 60_000 }, () => {
  for (const [name, o] of [
    ["timeoutMs NaN", { timeoutMs: Number.NaN }],
    ["timeoutMs Infinity", { timeoutMs: Infinity }],
    ["timeoutMs 2 ** 40", { timeoutMs: 2 ** 40 }],
    ["intervalMs NaN", { intervalMs: Number.NaN }],
    ["intervalMs -5", { intervalMs: -5 }],
    ["intervalMs Infinity", { intervalMs: Infinity }],
  ] as const) {
    it(name + " pauses 500 ms first and does not time out at once", async () => {
      let n = 0;
      const t = await throwaway((_req, res) => {
        setTimeout(() => {
          res.setHeader("Content-Type", "application/json");
          res.end(JSON.stringify({ state: ++n >= 3 ? "done" : "queued" }));
        }, 30);
      });
      const clock = fastClock();
      try {
        const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
        assert.equal((await c.jobs.wait(1, o)).state, "done");
        assert.deepEqual(clock.delays, [500, 750]);
      } finally {
        clock.restore();
        await t.close();
      }
    });
  }
});

describe("wait keeps the key out of what its errors show of a success", { timeout: 60_000 }, () => {
  const key = "SECRETKEY-0123456789";
  it("job_failed and timeout carry the answer without the key, the returned job is as sent", async () => {
    const t = await throwaway((req, res) => {
      res.setHeader("Content-Type", "application/json");
      const echo = String(req.headers.authorization);
      res.end(JSON.stringify(req.url === "/jobs/1" ? { state: "failed", error: "refused " + echo } : { state: "queued", echo }));
    });
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: key });
      const e = await err(c.jobs.wait(1, { throwOnFailure: true }));
      const q = await err(c.jobs.wait(2, { timeoutMs: 0 }));
      for (const x of [e, q]) {
        const shown = [x.message, x.stack ?? "", JSON.stringify(x), inspect(x, { depth: 8 })].join("\n");
        assert.ok(!shown.includes(key), shown);
        assert.match(shown, /\[key\]/);
      }
      assert.equal(e.kind, "job_failed");
      assert.equal(q.kind, "timeout");
      assert.match((await c.jobs.wait(1)).error ?? "", new RegExp(key), "a success is returned as the server sent it");
    } finally {
      await t.close();
    }
  });
});

describe("wait asks again when the deadline cuts a poll that has an answer behind it", { timeout: 60_000 }, () => {
  it("a poll that starts just before the deadline does not hide a job that is done", async () => {
    const t0 = Date.now();
    const t = await throwaway((_req, res) => {
      setTimeout(() => {
        res.setHeader("Content-Type", "application/json");
        res.end(JSON.stringify({ state: Date.now() - t0 >= 500 ? "done" : "queued" }));
      }, 60);
    });
    try {
      const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
      assert.equal((await c.jobs.wait(1, { timeoutMs: 1000, intervalMs: 900 })).state, "done");
      assert.equal(t.seen.length, 3, "the first poll, the one the deadline cut, the last one");
    } finally {
      await t.close();
    }
  });
});

describe("wait ends with kind timeout at its deadline when the server gives no answer about the job", { timeout: 60_000 }, () => {
  const down: [string, (res: import("node:http").ServerResponse) => void][] = [
    ["503", (res) => { res.statusCode = 503; res.end("{}"); }],
    ["a refused connection", (res) => { res.destroy(); }],
    ["a 502", (res) => { res.statusCode = 502; res.end("{}"); }],
    ["a 429", (res) => { res.statusCode = 429; res.end("{}"); }],
  ];
  for (const [name, bad] of down) {
    it("queued, then " + name + " until the deadline: timeout, with the last known state", async () => {
      let n = 0;
      const t = await throwaway((_req, res) => {
        res.setHeader("Content-Type", "application/json");
        if (++n === 1) res.end('{"state": "queued"}');
        else bad(res);
      });
      try {
        // many retries: the deadline, not the end of the retries, is what stops the poll
        const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456", retries: 8 });
        const e = await err(c.jobs.wait(1, { timeoutMs: 600, intervalMs: 100 }));
        assert.equal(e.kind, "timeout");
        assert.match(e.message, /still queued after 600 ms/);
      } finally {
        await t.close();
      }
    });
  }

  it("an answer about the job (not found, a bad key) is still thrown as it is", async () => {
    for (const [status, kind] of [[404, "not_found"], [401, "unauthorized"]] as const) {
      let n = 0;
      const t = await throwaway((_req, res) => {
        res.setHeader("Content-Type", "application/json");
        if (++n === 1) res.end('{"state": "queued"}');
        else {
          res.statusCode = status;
          res.end('{"error": "no"}');
        }
      });
      try {
        const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456" });
        const e = await err(c.jobs.wait(1, { timeoutMs: 300, intervalMs: 100 }));
        assert.equal(e.kind, kind);
      } finally {
        await t.close();
      }
    }
  });

  for (const timeoutMs of [Number.NaN, Infinity, undefined]) {
    it("a client timeoutMs of " + String(timeoutMs) + " still ends wait on a hanging server", async () => {
      let n = 0;
      const t = await throwaway((_req, res) => {
        res.setHeader("Content-Type", "application/json");
        if (++n === 1) res.end('{"state": "queued"}'); // then it hangs
      });
      try {
        const c = new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123456", ...(timeoutMs === undefined ? {} : { timeoutMs }) });
        const started = Date.now();
        const e = await err(c.jobs.wait(1, { timeoutMs: 500, intervalMs: 5000 }));
        assert.equal(e.kind, "timeout");
        assert.ok(Date.now() - started < 4000, "took " + (Date.now() - started) + " ms");
      } finally {
        await t.close();
      }
    });
  }
});
