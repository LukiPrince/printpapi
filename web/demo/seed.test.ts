// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import { b64, orderData, zplFor } from "./data.ts";
import { seedState } from "./seed.ts";

const now = 1_793_400_000;

test("seedState is deterministic", () => {
  const a = seedState(now);
  assert.deepEqual(a, seedState(now));
  assert.equal(a.v, 1);
});

test("four agents, only packing-station-2 offline", () => {
  const s = seedState(now);
  assert.equal(s.agents.length, 4);
  const off = s.agents.filter((a) => !a.online);
  assert.equal(off.length, 1);
  assert.equal(off[0].name, "packing-station-2");
  assert.ok(off[0].last_seen_at < now - 3 * 3600);
});

test("seven printers, pdf-capable 3/5/7, 7 saves to a file", () => {
  const s = seedState(now);
  assert.deepEqual(s.printers.map((p) => p.id), [1, 2, 3, 4, 5, 6, 7]);
  assert.deepEqual(s.printers.filter((p) => p.can_pdf).map((p) => p.id), [3, 5, 7]);
  assert.equal(s.printers[6].paper, "file");
});

test("job history is contiguous, newest first, all in the past", () => {
  const { jobs } = seedState(now);
  assert.equal(jobs[0].id, 1291);
  for (let i = 1; i < jobs.length; i++) {
    assert.equal(jobs[i].id, jobs[i - 1].id - 1);
    assert.ok(jobs[i].created_at < jobs[i - 1].created_at);
  }
  assert.ok(jobs.every((j) => j.created_at < now));
});

test("the interesting cases are there", () => {
  const { jobs } = seedState(now);
  const queued = jobs.filter((j) => j.state === "queued");
  assert.equal(queued.length, 1);
  assert.equal(queued[0].printer_id, 6);
  assert.ok(jobs.some((j) => j.state === "failed" && j.error === "expired" && j.printer_id === 6));
  assert.ok(jobs.some((j) => j.state === "failed" && j.error?.includes("404")));
  assert.ok(jobs.some((j) => j.state === "cancelled"));
  assert.ok(!jobs.some((j) => j.state === "claimed"));
});

test("outputs are four printed jobs on paper printers", () => {
  const s = seedState(now);
  assert.equal(s.outputs.length, 4);
  for (const id of s.outputs) {
    const j = s.jobs.find((x) => x.id === id)!;
    assert.equal(j.state, "done");
    assert.notEqual(s.printers.find((p) => p.id === j.printer_id)!.paper, "file");
  }
});

test("sample data helpers", () => {
  assert.deepEqual(orderData(10483), orderData(10483));
  const o = orderData(10483);
  const z = zplFor(o);
  assert.ok(z.startsWith("^XA"));
  assert.ok(z.endsWith("^XZ"));
  assert.ok(z.includes(o.tracking));
  assert.equal(Buffer.from(b64("Köln"), "base64").toString("utf8"), "Köln");
});
