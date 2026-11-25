// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import { test } from "node:test";
import assert from "node:assert/strict";
import { loadState, saveState } from "./persist.ts";
import { seedState } from "./seed.ts";

const now = 1_793_400_000;

test("no stored state seeds fresh", () => {
  assert.deepEqual(loadState(null, now), seedState(now));
});

test("corrupt JSON seeds fresh", () => {
  assert.deepEqual(loadState("{nope", now), seedState(now));
  assert.deepEqual(loadState("null", now), seedState(now));
});

test("a wrong version seeds fresh", () => {
  const raw = saveState({ ...seedState(now), v: 2 } as unknown as ReturnType<typeof seedState>);
  assert.deepEqual(loadState(raw, now), seedState(now));
});

test("missing arrays seed fresh", () => {
  const s = seedState(now) as unknown as Record<string, unknown>;
  delete s.jobs;
  assert.deepEqual(loadState(JSON.stringify(s), now), seedState(now));
});

test("a saved state round-trips", () => {
  const s = seedState(now);
  s.jobs.unshift({ ...s.jobs[0], id: s.next.job++, title: "extra" });
  assert.deepEqual(loadState(saveState(s), now + 99), s);
});

test("stored keys without a scope still load, and read as Everything", () => {
  const s = seedState(now);
  for (const k of s.keys) delete (k as { scope?: string }).scope;
  const back = loadState(saveState(s), now + 99);
  assert.equal(back.keys.length, s.keys.length);
  assert.ok(back.keys.every((k) => k.scope === undefined));
});

test("a state stored before label templates gets the sample ones", () => {
  const s = seedState(now) as unknown as Record<string, unknown> & { next: Record<string, unknown> };
  delete s.labels;
  delete s.next.label;
  const back = loadState(JSON.stringify(s), now + 99);
  assert.deepEqual(back.labels, seedState(now + 99).labels);
  assert.equal(back.next.label, 3);
  assert.equal(back.jobs.length, seedState(now).jobs.length);
});

test("stored label templates survive a reload", () => {
  const s = seedState(now);
  s.labels.pop();
  s.next.label = 9;
  const back = loadState(saveState(s), now + 99);
  assert.equal(back.labels.length, 1);
  assert.equal(back.next.label, 9);
});

test("a state stored before receipt profiles existed loads, with none stored", () => {
  const s = seedState(now) as unknown as Record<string, unknown>;
  delete s.receipt_profiles;
  const back = loadState(JSON.stringify(s), now + 99);
  assert.deepEqual(back.receipt_profiles, {});
  assert.equal(back.jobs.length, seedState(now).jobs.length);
});

test("stored receipt profiles survive a reload", () => {
  const s = seedState(now);
  s.receipt_profiles = { 1: { paper_mm: 58, width_dots: 320, codepage: "cp1252", codepage_number: 19, cut: "full", output: "escpos" } };
  const back = loadState(saveState(s), now + 99);
  assert.deepEqual(back.receipt_profiles, s.receipt_profiles);
});

test("the seed gives the receipt printer 58 mm paper", () => {
  assert.equal(seedState(now).receipt_profiles?.[4]?.paper_mm, 58);
});

test("a state stored before pairing existed gets an empty list of codes and keeps loading", () => {
  const s = seedState(now) as unknown as Record<string, unknown> & { next: Record<string, unknown> };
  delete s.pairings;
  delete s.next.pairing;
  const back = loadState(JSON.stringify(s), now + 99);
  assert.deepEqual(back.pairings, []);
  // and the numbers of the other counters are untouched
  assert.equal(back.next.job, seedState(now).next.job);
  // codes made before survive a reload
  const made = seedState(now);
  made.pairings = [{ id: 1, org_id: 1, computer_id: null, computer_name: null, created_at: now, expires_at: now + 1800, used_at: null, device_name: null, key_id: null, state: "open", old_key: null }];
  assert.deepEqual(loadState(saveState(made), now + 5).pairings, made.pairings);
});
