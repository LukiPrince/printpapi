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
