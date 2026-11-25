import assert from "node:assert/strict";
import { after, before, describe, it } from "node:test";
import { PrintpapiError } from "../src/index.ts";
import { type Harness, harness } from "../testkit/harness.ts";
import { FakeAgent, type Org, createOrg } from "../testkit/server.ts";

const PNG = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
const receipt = { content: [{ type: "text" as const, text: "Hallo Welt", size: 2 }] };
const err = async (p: Promise<unknown>): Promise<PrintpapiError> => {
  const e = await p.then(() => assert.fail("must reject"), (x: unknown) => x);
  assert.ok(e instanceof PrintpapiError, String(e));
  return e;
};

describe("receipts, key rights and the other merged features (test 22)", { timeout: 120_000 }, () => {
  let h: Harness;
  let org: Org;
  let agent: FakeAgent;
  let escpos = 0;
  before(async () => {
    h = await harness();
    org = await createOrg(h.server, "siblings");
    agent = await new FakeAgent(h.server.url, org.agentKey, h.recorder.fetch).register("pc", [{ name: "Epson" }, { name: "Star-USB", can_pdf: true }]);
    escpos = agent.printer("Epson");
  });
  after(() => h.stop());

  it("draws a receipt preview as PNG bytes", async () => {
    const c = h.client(org.key);
    const png = await c.receipts.preview({ receipt, profile: { paper_mm: 58 } });
    assert.ok(png instanceof Uint8Array);
    assert.deepEqual([...png.slice(0, 8)], PNG);
    const stored = await c.receipts.preview({ receipt, printer_id: escpos });
    assert.deepEqual([...stored.slice(0, 8)], PNG);
    const both = await err(c.receipts.preview({ receipt, printer_id: escpos, profile: {} }));
    assert.equal(both.kind, "invalid");
    assert.match(both.message, /not both/);
  });

  it("reads, replaces and deletes the receipt profile of a printer", async () => {
    const c = h.client(org.key);
    const defaults = await c.receipts.profile.get(escpos);
    assert.equal(defaults.stored, false);
    assert.equal(defaults.profile.paper_mm, 80);
    assert.equal(defaults.profile.width_dots, 576);
    const put = await c.receipts.profile.put(escpos, { paper_mm: 58, cut: "none", codepage: "cp1252", codepage_number: 16 });
    assert.equal(put.stored, true);
    assert.deepEqual(put.profile, { paper_mm: 58, width_dots: 384, codepage: "cp1252", codepage_number: 16, cut: "none", output: "auto" });
    assert.deepEqual((await c.receipts.profile.get(escpos)).profile, put.profile);
    assert.deepEqual(await c.receipts.profile.delete(escpos), { ok: true });
    assert.equal((await c.receipts.profile.get(escpos)).stored, false);
    const bad = await err(c.receipts.profile.put(escpos, { paper_mm: 58, width_dots: 7 as number }));
    assert.equal(bad.kind, "invalid");
    assert.equal(bad.attempts, 1, "a PUT is not repeated");
    assert.equal((await err(c.receipts.profile.get(999_999))).kind, "not_found");
  });

  it("says forbidden for a key whose rights lack the call, and who the key is", async () => {
    const read = h.client(await org.keyWith("read"));
    assert.deepEqual(await read.me(), { kind: "key", org_id: org.orgId, scope: "read" });
    assert.ok(Array.isArray(await read.printers.list()), "reading is allowed");
    const denied = await err(read.jobs.submit({ printer_id: escpos, type: "raw_base64", content: "AA==" }));
    assert.equal(denied.kind, "forbidden");
    assert.equal(denied.status, 403);
    assert.equal(denied.attempts, 1, "a 403 is not repeated");
    assert.match(denied.message, /this key's rights \(read\) do not allow this/);
    assert.equal((await err(read.labels.create({ name: "n", content: "text", label_mm: { w: 40, h: 30 }, dpi: 203, rotate: 0, crop_percent: { t: 0, r: 0, b: 0, l: 0 }, place_mm: { x: 0, y: 0, w: 40, h: 30 } }))).kind, "forbidden");
    assert.equal((await err(read.receipts.profile.delete(escpos))).kind, "forbidden");
    assert.equal((await err(read.receipts.preview({ receipt }))).kind, "forbidden");
  });

  it("lets a print key see and cancel only its own jobs", async () => {
    const mine = h.client(await org.keyWith("print"));
    const theirs = h.client(org.key);
    const a = await mine.jobs.submit({ printer_id: escpos, type: "raw_base64", content: "AA==" });
    const b = await theirs.jobs.submit({ printer_id: escpos, type: "raw_base64", content: "AA==" });
    assert.deepEqual((await mine.jobs.list()).map((j) => j.id), [a.job_id]);
    assert.equal((await err(mine.jobs.get(b.job_id))).kind, "not_found");
    assert.equal((await err(mine.jobs.cancel(b.job_id))).kind, "not_found");
    assert.equal((await theirs.jobs.list()).length >= 2, true);
    await mine.jobs.cancel(a.job_id);
    await theirs.jobs.cancel(b.job_id);
  });

  it("lets an agent-only key list computers but nothing else the client does", async () => {
    const a = h.client(org.agentKey);
    assert.equal((await a.computers.list()).length, 1);
    assert.equal((await err(a.jobs.list())).kind, "forbidden");
  });

  it("a machine key may not manage: unauthorized, as the server does for keys, users and pairing", async () => {
    const res = await h.recorder.fetch(h.server.url + "/pairing", { method: "POST", headers: { Authorization: "Bearer " + org.key }, body: "{}" });
    assert.equal(res.status, 401, "so the client has no pairing methods");
  });
});
