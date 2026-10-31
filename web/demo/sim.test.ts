// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import { BASE_COUNTS, seedState } from "./seed.ts";
import type { JobSpec } from "./sim.ts";
import { advance, cancel, enqueue, JOBS_MAX, mode, progress, simulateOrder, trafficTick } from "./sim.ts";

const now = 1_793_400_000;
const rand = () => 0.5;
const spec = (printer_id: number): JobSpec => ({ printer_id, title: "Test", type: "raw_base64", kind: "test", ref: null, file: null, copies: 1 });

test("enqueue takes the next id and schedules claim and print", () => {
  const s = seedState(now);
  const j = enqueue(s, spec(1), now, undefined, rand);
  assert.equal(j.id, 1292);
  assert.equal(j.state, "queued");
  assert.ok(j.claim_at! > now);
  assert.ok(j.done_at! > j.claim_at!);
  assert.equal(s.jobs[0], j);
  assert.equal(s.next.job, 1293);
});

test("enqueue on an offline agent's printer is never scheduled", () => {
  const s = seedState(now);
  const j = enqueue(s, spec(6), now, undefined, rand);
  assert.equal(j.claim_at, null);
  assert.equal(j.done_at, null);
});

test("advance finishes a job once its time has come", () => {
  const s = seedState(now);
  const j = enqueue(s, spec(1), now, undefined, rand);
  s.traffic = false;
  assert.deepEqual(advance(s, now + 0.1), []);
  assert.equal(j.state, "queued");
  assert.deepEqual(advance(s, now + 10), [j.id]);
  assert.equal(j.state, "done");
  assert.equal(j.finished_at, j.done_at);
  assert.equal(s.outputs[s.outputs.length - 1], j.id);
  assert.ok(s.outputs.length <= 8);
});

test("progress rises with every step advance takes, claimed -> done included", () => {
  const s = seedState(now);
  const j = enqueue(s, spec(1), now, { claimAfter: 1, printFor: 1 }, rand);
  const start = progress(s);
  advance(s, now + 0.5);
  assert.equal(progress(s), start, "nothing was due");
  advance(s, now + 1.5);
  assert.equal(j.state, "claimed");
  const claimed = progress(s);
  assert.ok(claimed > start);
  advance(s, now + 2.5); // the step a poll can take on its own: the claimed count stays the same
  assert.equal(j.state, "done");
  assert.ok(progress(s) > claimed);
});

test("a job on an offline agent stays queued", () => {
  const s = seedState(now);
  s.traffic = false;
  const j = enqueue(s, spec(6), now, undefined, rand);
  advance(s, now + 86_400);
  assert.equal(j.state, "queued");
});

test("advance never creates traffic", () => {
  const s = seedState(now);
  s.traffic = true;
  s.traffic_next_at = now;
  const before = s.jobs.length;
  advance(s, now + 3600);
  assert.equal(s.jobs.length, before);
  assert.equal(s.traffic_next_at, now);
});

test("trafficTick catches up by one order at most after hours", () => {
  const s = seedState(now);
  s.traffic = true;
  s.traffic_next_at = now;
  const before = s.jobs.length;
  const later = now + 6 * 3600;
  assert.equal(trafficTick(s, later, rand), true);
  assert.ok(s.jobs.length - before <= 2);
  assert.equal(s.traffic_next_at, later + 13);
  assert.equal(trafficTick(s, later + 1, rand), false);
});

test("trafficTick with traffic off adds nothing", () => {
  const s = seedState(now);
  s.traffic = false;
  s.traffic_next_at = now;
  const before = s.jobs.length;
  assert.equal(trafficTick(s, now + 3600, rand), false);
  assert.equal(s.jobs.length, before);
});

test("traffic trims the oldest finished jobs but never queued, claimed or output jobs", () => {
  const s = seedState(now);
  s.traffic = true;
  for (let i = 0; i < JOBS_MAX + 50; i++) enqueue(s, spec(1), now, { claimAfter: 0, printFor: 1 }, rand);
  advance(s, now + 100);
  const oldQueued = s.jobs[s.jobs.length - 1];
  oldQueued.state = "queued";
  oldQueued.finished_at = null;
  s.traffic_next_at = now;
  trafficTick(s, now + 100, rand);
  assert.ok(s.jobs.length <= JOBS_MAX);
  assert.ok(s.jobs.includes(oldQueued));
  for (const id of s.outputs) assert.ok(s.jobs.some((j) => j.id === id));
});

test("simulateOrder queues a slip on 3 and a label on 1", () => {
  const s = seedState(now);
  const n = s.next.order;
  assert.equal(simulateOrder(s, now, rand), n);
  assert.equal(s.next.order, n + 1);
  const mine = s.jobs.filter((j) => j.ref === n);
  assert.deepEqual(mine.map((j) => [j.printer_id, j.kind, j.type]).sort(), [[1, "label", "raw_base64"], [3, "slip", "pdf_base64"]]);
  const slip = mine.find((j) => j.kind === "slip")!;
  const label = mine.find((j) => j.kind === "label")!;
  assert.equal(slip.title, `Packing slip · order #${n}`);
  assert.equal(label.title, `Shipping label · order #${n}`);
  assert.ok(label.claim_at! > slip.claim_at!);
});

test("cancel", () => {
  const s = seedState(now);
  const queued = s.jobs.find((j) => j.state === "queued")!;
  assert.equal(cancel(s, queued.id, now), "ok");
  assert.equal(queued.state, "cancelled");
  assert.equal(queued.finished_at, now);
  assert.equal(cancel(s, queued.id, now), "not-cancellable");
  const done = s.jobs.find((j) => j.state === "done")!;
  assert.equal(cancel(s, done.id, now), "not-cancellable");
  assert.equal(cancel(s, 99999, now), "not-found");
});

test("mode groups the pdf types", () => {
  assert.equal(mode("pdf_uri"), "pdf");
  assert.equal(mode("pdf_base64"), "pdf");
  assert.equal(mode("pdf_uri_post"), "pdf");
  assert.equal(mode("raw_base64"), "raw");
});

test("trimmed jobs stay in the counters and a claimed job survives", () => {
  const s = seedState(now);
  s.traffic = true;
  const claimed = enqueue(s, spec(1), now, { claimAfter: 0, printFor: 1e9 }, rand);
  let lastDone = 0;
  for (let i = 0; i < JOBS_MAX + 120; i++) {
    enqueue(s, spec(1), now, { claimAfter: 0, printFor: 1 }, rand);
    advance(s, now + 100);
    s.traffic_next_at = now;
    trafficTick(s, now + 100, rand);
    const done = BASE_COUNTS.done + s.trimmed.done + s.jobs.filter((j) => j.state === "done").length;
    assert.ok(done >= lastDone, "done never goes down");
    lastDone = done;
  }
  assert.ok(s.trimmed.done > 0);
  assert.equal(claimed.state, "claimed");
  assert.ok(s.jobs.includes(claimed));
});
