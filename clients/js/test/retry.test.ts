import assert from "node:assert/strict";
import { after, before, describe, it } from "node:test";
import { IN_PROGRESS_POLL_MS, PrintpapiError } from "../src/index.ts";
import { type Harness, harness } from "../testkit/harness.ts";
import { FakeAgent, type Org, createOrg, faultProxy, freePort, raw, slowSource, until } from "../testkit/server.ts";

const job = (printer_id: number, s = "x") => ({ printer_id, type: "raw_base64" as const, content: btoa(s) });
const err = async (p: Promise<unknown>): Promise<PrintpapiError> => {
  const e = await p.then(() => assert.fail("must reject"), (x: unknown) => x);
  assert.ok(e instanceof PrintpapiError, String(e));
  return e;
};

describe("retries and idempotency against the real server (tests 9 to 15)", { timeout: 180_000 }, () => {
  let h: Harness;
  let org: Org;
  let agent: FakeAgent;
  let zpl = 0;
  before(async () => {
    h = await harness();
    org = await createOrg(h.server, "retry");
    agent = await new FakeAgent(h.server.url, org.agentKey, h.recorder.fetch).register("pc", [{ name: "Zebra" }]);
    zpl = agent.printer("Zebra");
  });
  after(() => h.stop());
  const jobs = async () => (await h.client(org.key).jobs.list()).length;

  it("repeats a submit whose answer was lost, with the same key, and one job exists (test 9)", async () => {
    const before = await jobs();
    const proxy = await faultProxy(h.server.url, (r) => (r.n === 1 ? "drop" : "forward"));
    try {
      const c = h.client(org.key, { baseUrl: proxy.url });
      const { job_id } = await c.jobs.submit(job(zpl, "nine"));
      assert.equal(proxy.requests.length, 2, "one repeat");
      const keys = proxy.requests.map((r) => JSON.parse(r.body).idempotency_key as string);
      assert.equal(keys[0], keys[1], "both attempts carry the one key");
      assert.match(keys[0] ?? "", /^[0-9a-f]{32}$/, "128 random bits as hex");
      assert.equal(await jobs(), before + 1);
      assert.equal((await h.client(org.key).jobs.get(job_id)).state, "queued");
      await agent.print();
    } finally {
      await proxy.close();
    }
  });

  it("does not repeat with retries: 0 and says network (test 9)", async () => {
    const proxy = await faultProxy(h.server.url, () => "drop");
    try {
      const e = await err(h.client(org.key, { baseUrl: proxy.url, retries: 0 }).jobs.submit(job(zpl, "zero")));
      assert.equal(e.kind, "network");
      assert.equal(e.attempts, 1);
      assert.equal(proxy.requests.length, 1);
      await agent.print();
    } finally {
      await proxy.close();
    }
  });

  it("hands back the key after a failed submit, and a resubmit with it prints once (test 10)", async () => {
    const before = await jobs();
    const proxy = await faultProxy(h.server.url, () => "drop");
    let key = "";
    try {
      const e = await err(h.client(org.key, { baseUrl: proxy.url }).jobs.submit(job(zpl, "ten")));
      assert.equal(e.kind, "network");
      assert.equal(e.attempts, 3, "the first try and two repeats");
      assert.match(e.idempotencyKey ?? "", /^[0-9a-f]{32}$/);
      assert.ok(!e.message.includes(org.key));
      key = e.idempotencyKey ?? "";
      assert.equal(proxy.requests.length, 3);
    } finally {
      await proxy.close();
    }
    assert.equal(await jobs(), before + 1, "the server queued it once, however often it was asked");
    const again = await h.client(org.key).jobs.submit({ ...job(zpl, "ten"), idempotency_key: key });
    assert.equal(await jobs(), before + 1, "and the resubmit with the key queued nothing");
    const row = (await h.client(org.key).jobs.list()).find((r) => r.id === again.job_id);
    assert.ok(row, "the same job");
    await agent.print();
  });

  it("does not repeat a 400, a DELETE or a POST /labels (test 11)", async () => {
    const proxy = await faultProxy(h.server.url);
    try {
      const c = h.client(org.key, { baseUrl: proxy.url });
      const bad = await err(c.jobs.submit(job(987_654)));
      assert.equal(bad.kind, "invalid");
      assert.equal(bad.attempts, 1);
      assert.equal(proxy.requests.length, 1);

      const queued = await h.client(org.key).jobs.submit(job(zpl, "del"));
      proxy.requests.length = 0;
      proxy.rule = (r) => (r.n === 1 ? "drop" : "forward");
      const d = await err(c.jobs.cancel(queued.job_id));
      assert.equal(d.kind, "network");
      assert.equal(d.attempts, 1);
      assert.equal(proxy.requests.length, 1, "a DELETE is never repeated");
      assert.equal((await h.client(org.key).jobs.get(queued.job_id)).state, "cancelled", "though the server did it");

      proxy.requests.length = 0;
      const spec = {
        name: "dropped", content: "image" as const, label_mm: { w: 50, h: 30 }, dpi: 203, rotate: 0 as const,
        crop_percent: { t: 0, r: 0, b: 0, l: 0 }, place_mm: { x: 0, y: 0, w: 50, h: 30 },
      };
      const l = await err(c.labels.create(spec));
      assert.equal(l.kind, "network");
      assert.equal(proxy.requests.length, 1, "a POST /labels is never repeated");
    } finally {
      await proxy.close();
    }
  });

  it("polls an answer 'still in progress' with the same key without using up retries (test 12)", async () => {
    const slow = await slowSource();
    const proxy = await faultProxy(h.server.url);
    try {
      const key = "slow-" + Math.random().toString(36).slice(2);
      const body = { printer_id: zpl, type: "raw_uri" as const, url: slow.url + "/payload", idempotency_key: key };
      const first = h.client(org.key).jobs.submit(body);
      await slow.arrived; // the server is in the middle of the download: the key is held
      const second = h.client(org.key, { baseUrl: proxy.url, retries: 0 }).jobs.submit(body);
      await until(() => proxy.requests.some((r) => r.upstream === 409), 10_000, "the first 409");
      slow.release(new TextEncoder().encode("SLOW-PAYLOAD"));
      const [a, b] = await Promise.all([first, second]);
      assert.equal(b.job_id, a.job_id, "the second call returns the original job");
      assert.ok(proxy.requests.length >= 2, "it asked again");
      assert.ok(IN_PROGRESS_POLL_MS >= 500);
      assert.deepEqual(new Set(proxy.requests.map((r) => JSON.parse(r.body).idempotency_key)), new Set([key]));
      assert.equal(new TextDecoder().decode((await agent.print()).bytes), "SLOW-PAYLOAD");
    } finally {
      await proxy.close();
      await slow.close();
    }
  });

  it("repeats a proxy's 502 page once and queues one job, but not the server's own 502 (test 13)", async () => {
    const before = await jobs();
    const proxy = await faultProxy(h.server.url, (r) => (r.n === 1 ? { status: 502, body: "<html>Bad Gateway</html>" } : "forward"));
    try {
      // the proxy's own 502 page is no answer of the server: not checked against its document
      const c = h.client(org.key, { baseUrl: proxy.url, fetch: (...a) => fetch(...a) });
      await c.jobs.submit(job(zpl, "gw"));
      assert.equal(proxy.requests.length, 2);
      assert.equal(await jobs(), before + 1);
      await agent.print();

      proxy.requests.length = 0;
      proxy.rule = () => "forward";
      const dead = "http://127.0.0.1:" + (await freePort()) + "/nothing";
      const e = await err(c.jobs.submit({ printer_id: zpl, type: "raw_uri", url: dead }));
      assert.equal(e.kind, "bad_gateway");
      assert.equal(e.status, 502);
      assert.match(e.message, /downstream:/);
      assert.equal(e.attempts, 1, "the server's own failed download is final");
      assert.equal(proxy.requests.length, 1);
    } finally {
      await proxy.close();
    }
  });

  it("times out per attempt and counts the attempts (test 14)", async () => {
    const proxy = await faultProxy(h.server.url, () => "hang");
    try {
      const none = await err(h.client(org.key, { baseUrl: proxy.url, timeoutMs: 200, retries: 0 }).jobs.submit(job(zpl)));
      assert.equal(none.kind, "timeout");
      assert.equal(none.attempts, 1);
      assert.match(none.message, /no answer within 200 ms/);
      assert.ok(none.idempotencyKey);
      const one = await err(h.client(org.key, { baseUrl: proxy.url, timeoutMs: 200, retries: 1 }).jobs.submit(job(zpl)));
      assert.equal(one.kind, "timeout");
      assert.equal(one.attempts, 2);
      const read = await err(h.client(org.key, { baseUrl: proxy.url, timeoutMs: 100, retries: 1 }).jobs.list());
      assert.equal(read.kind, "timeout");
      assert.equal(read.attempts, 2);
    } finally {
      await proxy.close();
    }
  });

  it("does not repeat a spent quota (test 15)", async () => {
    const q = await createOrg(h.server, "quota");
    const qa = await new FakeAgent(h.server.url, q.agentKey, h.recorder.fetch).register("qpc", [{ name: "Q" }]);
    assert.equal((await raw(h.server.url, h.server.token, "PUT", "/orgs/" + q.orgId, { job_quota: 1 })).status, 200);
    const proxy = await faultProxy(h.server.url);
    try {
      const c = h.client(q.key, { baseUrl: proxy.url });
      await c.jobs.submit(job(qa.printer("Q"), "one"));
      proxy.requests.length = 0;
      const e = await err(c.jobs.submit(job(qa.printer("Q"), "two")));
      assert.equal(e.kind, "quota");
      assert.equal(e.status, 402);
      assert.equal(e.attempts, 1);
      assert.equal(proxy.requests.length, 1);
    } finally {
      await proxy.close();
    }
  });
});
