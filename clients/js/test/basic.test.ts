import assert from "node:assert/strict";
import { after, before, describe, it } from "node:test";
import { inspect } from "node:util";
import { Printpapi, PrintpapiError } from "../src/index.ts";
import { type Harness, harness } from "../testkit/harness.ts";
import { FakeAgent, createOrg, raw } from "../testkit/server.ts";

describe("construction (test 1)", () => {
  it("needs a key and a plain http(s) base URL without credentials", () => {
    assert.throws(() => new Printpapi({ baseUrl: "https://print.example.com", apiKey: "" }), TypeError);
    assert.throws(() => new Printpapi({ baseUrl: "print.example.com", apiKey: "k" }), /not a URL/);
    assert.throws(() => new Printpapi({ baseUrl: "ftp://print.example.com", apiKey: "k" }), /http or https/);
    assert.throws(
      () => new Printpapi({ baseUrl: "https://user:hunter2@print.example.com", apiKey: "k" }),
      (e: Error) => e instanceof TypeError && !e.message.includes("hunter2"),
    );
  });

  it("keeps the base URL without a trailing slash, query or hash", () => {
    const c = new Printpapi({ baseUrl: "https://print.example.com/api/?x=1#y", apiKey: "k" });
    assert.equal(c.baseUrl, "https://print.example.com/api");
  });

  it("never shows its key: not in JSON, inspect, keys or the base URL", () => {
    const key = "sekret-key-0123456789";
    const c = new Printpapi({ baseUrl: "http://127.0.0.1:1", apiKey: key });
    for (const text of [JSON.stringify(c), inspect(c, { depth: 5, showHidden: true }), Object.keys(c).join(","), c.baseUrl]) {
      assert.ok(!text.includes(key), text);
    }
  });
});

describe("against the server", { timeout: 60_000 }, () => {
  let h: Harness;
  let key = "";
  let agentKey = "";
  let agent: FakeAgent;
  before(async () => {
    h = await harness();
    const org = await createOrg(h.server, "basic");
    key = org.key;
    agentKey = org.agentKey;
    agent = await new FakeAgent(h.server.url, agentKey, h.recorder.fetch).register("warehouse-pc", [
      { name: "Zebra", can_pdf: false },
      { name: "Laser", can_pdf: true, capabilities: { papers: ["A4"], duplex: true } },
    ]);
  });
  after(() => h.stop());

  it("asks the health of the server (test 2)", async () => {
    const health = await h.client(key).health();
    assert.equal(health.ok, true);
    assert.equal(health.signup, "closed");
    assert.equal(health.password_reset, false);
  });

  it("says who the key is", async () => {
    const me = await h.client(key).me();
    assert.equal(me.kind, "key");
    assert.equal(me.scope, undefined);
    assert.deepEqual(await h.client(await raw(h.server.url, h.server.token, "POST", "/apikeys", { org_id: 1, scope: "read" }).then((r) => r.json.key)).me(), {
      kind: "key", org_id: 1, scope: "read",
    });
  });

  it("answers a wrong key with unauthorized, and the key is nowhere in the error (test 3)", async () => {
    const wrong = "wrong-key-abcdef-0123456789";
    const c = h.client(wrong);
    const err = await c.jobs.list().then(
      () => assert.fail("must reject"),
      (e: unknown) => e as PrintpapiError,
    );
    assert.ok(err instanceof PrintpapiError);
    assert.equal(err.kind, "unauthorized");
    assert.equal(err.status, 401);
    assert.equal(err.method, "GET");
    assert.equal(err.path, "/jobs");
    assert.equal(err.attempts, 1, "a 401 is not repeated");
    const everything = [err.message, err.stack ?? "", JSON.stringify(err), inspect(err, { depth: 6, showHidden: true }), ...Object.values(err).map(String)];
    for (const t of everything) assert.ok(!t.includes(wrong) && !t.includes("Bearer"), t);
  });

  it("lists printers and computers once an agent has registered (test 4)", async () => {
    const c = h.client(key);
    const printers = await c.printers.list();
    assert.deepEqual(printers.map((p) => p.name).sort(), ["Laser", "Zebra"]);
    const laser = printers.find((p) => p.name === "Laser");
    assert.equal(laser?.can_pdf, true);
    assert.deepEqual(laser?.capabilities, { papers: ["A4"], duplex: true });
    const computers = await c.computers.list();
    assert.equal(computers.length, 1);
    assert.equal(computers[0]?.name, "warehouse-pc");
    assert.equal(computers[0]?.printers, 2);
    assert.equal(computers[0]?.online, true);
    assert.equal(computers[0]?.id, agent.computerId);
  });

  it("maps a key without the right to forbidden", async () => {
    const agentOnly = h.client(agentKey);
    const err = await agentOnly.printers.list().catch((e: unknown) => e as PrintpapiError);
    assert.ok(err instanceof PrintpapiError);
    assert.equal(err.kind, "forbidden");
    assert.equal(err.status, 403);
    assert.match(err.message, /rights \(agent\) do not allow this/);
  });
});
