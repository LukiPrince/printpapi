import assert from "node:assert/strict";
import { after, before, describe, it } from "node:test";
import { DEFAULT_TIMEOUT_MS, Printpapi, PrintpapiError, SUBMIT_TIMEOUT_MS } from "../src/index.ts";
import { type Harness, harness } from "../testkit/harness.ts";
import { FakeAgent, type Org, createOrg, throwaway } from "../testkit/server.ts";

const err = async (p: Promise<unknown>): Promise<PrintpapiError> => {
  const e = await p.then(() => assert.fail("must reject"), (x: unknown) => x);
  assert.ok(e instanceof PrintpapiError, String(e));
  return e;
};
const spec = (name: string) => ({
  name, content: "image" as const, label_mm: { w: 100, h: 150 }, dpi: 203, rotate: 90 as const,
  crop_percent: { t: 0, r: 0, b: 14.3, l: 0 }, place_mm: { x: 0.8, y: 1.3, w: 100.1, h: 149.8 },
});

describe("orders, labels, redirects and headers (tests 16 to 19)", { timeout: 120_000 }, () => {
  let h: Harness;
  let org: Org;
  let agent: FakeAgent;
  before(async () => {
    h = await harness();
    org = await createOrg(h.server, "misc");
    agent = await new FakeAgent(h.server.url, org.agentKey, h.recorder.fetch).register("pc", [
      { name: "Zebra", can_pdf: false },
      { name: "Laser", can_pdf: true },
    ]);
  });
  after(() => h.stop());

  it("prints an order on a PDF printer, and refuses a raw-only one (test 16)", async () => {
    const c = h.client(org.key);
    const order = { number: "1001", customer: "Jane Doe", address: ["Musterweg 1", "12345 Berlin"], lines: [{ qty: 2, sku: "A-1", name: "Widget", total: "19.80 EUR" }], totals: [["Total", "19.80 EUR"]] };
    const { job_id } = await c.orders.print({ printer_id: agent.printer("Laser"), order, title: "Slip 1001" });
    assert.ok(job_id > 0);
    const { job, bytes } = await agent.print();
    assert.equal(job.mode, "pdf");
    assert.match(new TextDecoder().decode(bytes.slice(0, 5)), /^%PDF-/);
    const e = await err(c.orders.print({ printer_id: agent.printer("Zebra"), order }));
    assert.equal(e.kind, "invalid");
    assert.match(e.message, /raw-only/);
    assert.ok(e.idempotencyKey, "orders carry a key too");
  });

  it("creates, lists, reads, replaces and deletes label templates, and a duplicate name conflicts (test 17)", async () => {
    const c = h.client(org.key);
    const made = await c.labels.create(spec("shipping-4x6"));
    assert.equal(made.name, "shipping-4x6");
    assert.equal(made.text.wrap_chars, 0, "the server fills in the defaults");
    assert.ok(made.created_at > 0);
    assert.deepEqual((await c.labels.list()).map((t) => t.name), ["shipping-4x6"]);
    assert.deepEqual(await c.labels.get(made.id), made);
    const changed = await c.labels.update(made.id, { ...spec("shipping-4x6"), rotate: 180, zpl_darkness: 10, size_ref: "Standard:4x6" });
    assert.equal(changed.rotate, 180);
    assert.equal(changed.zpl_darkness, 10);
    const dup = await err(c.labels.create(spec("shipping-4x6")));
    assert.equal(dup.kind, "conflict");
    assert.equal(dup.attempts, 1);
    const bad = await err(c.labels.create({ ...spec("bad name!"), dpi: 50 }));
    assert.equal(bad.kind, "invalid");
    assert.deepEqual(await c.labels.delete(made.id), { ok: true });
    assert.equal((await err(c.labels.get(made.id))).kind, "not_found");
  });

  it("does not follow a redirect: one request, none at the target, kind redirect (test 18)", async () => {
    const target = await throwaway((_req, res) => res.end("{}"));
    const redirecting = await throwaway((_req, res) => {
      res.writeHead(302, { Location: target.url + "/jobs" });
      res.end();
    });
    try {
      const key = "key-that-must-not-travel-0123";
      const c = new Printpapi({ baseUrl: redirecting.url, apiKey: key });
      const e = await err(c.jobs.list());
      assert.equal(e.kind, "redirect");
      assert.equal(e.status, 302);
      assert.equal(e.attempts, 1, "a redirect is not repeated");
      assert.ok(e.message.includes(redirecting.url), e.message);
      assert.match(e.message, /http against https/);
      assert.ok(!e.message.includes(key));
      const s = await err(c.jobs.submit({ printer_id: 1, type: "raw_base64", content: "AA==" }));
      assert.equal(s.kind, "redirect");
      assert.ok(s.idempotencyKey);
      assert.equal(redirecting.seen.length, 2, "one request per call");
      assert.equal(target.seen.length, 0, "the target never saw anything: not the key either");
    } finally {
      await redirecting.close();
      await target.close();
    }
  });

  it("sends a browser-like User-Agent and the key only in the Authorization header (test 19)", async () => {
    const t = await throwaway((_req, res) => {
      res.setHeader("Content-Type", "application/json");
      res.end(JSON.stringify({ ok: true, signup: "closed", password_reset: false }));
    });
    try {
      await new Printpapi({ baseUrl: t.url, apiKey: "the-key-0123" }).health();
      const seen = t.seen[0];
      assert.equal(seen?.headers["user-agent"], "Mozilla/5.0 (printpapi-client)");
      assert.equal(seen?.headers.authorization, "Bearer the-key-0123");
      assert.equal(seen?.path, "/health", "no key in the URL");
      assert.equal(seen?.headers.cookie, undefined);
    } finally {
      await t.close();
    }
  });

  it("times a submit at 150 s and everything else at 30 s (test 14)", async () => {
    assert.equal(SUBMIT_TIMEOUT_MS, 150_000, "above the server's 120 s fetch and body budgets");
    assert.equal(DEFAULT_TIMEOUT_MS, 30_000);
    const delays: number[] = [];
    const realSetTimeout = globalThis.setTimeout;
    globalThis.setTimeout = ((fn: () => void, ms?: number, ...a: unknown[]) => {
      delays.push(ms ?? 0);
      return realSetTimeout(fn, ms, ...a);
    }) as typeof setTimeout;
    try {
      const ok = (body: unknown) => async () => new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
      const submit = new Printpapi({ baseUrl: "http://x.test", apiKey: "k", fetch: ok({ job_id: 1 }) });
      await submit.jobs.submit({ printer_id: 1, type: "raw_base64", content: "AA==" });
      await submit.orders.print({ printer_id: 1, order: { number: "1" } });
      assert.deepEqual(delays, [150_000, 150_000]);
      delays.length = 0;
      await new Printpapi({ baseUrl: "http://x.test", apiKey: "k", fetch: ok({ jobs: [] }) }).jobs.list();
      await new Printpapi({ baseUrl: "http://x.test", apiKey: "k", fetch: ok({ jobs: [] }) }).jobs.list({ timeoutMs: 777 });
      assert.deepEqual(delays, [30_000, 777]);
    } finally {
      globalThis.setTimeout = realSetTimeout;
    }
  });
});
