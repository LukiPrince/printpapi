// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import { maybePrintedNote } from "./maybe-printed.ts";

const job = { agent_name: "packing-pc", maybe_printed: true } as const;

test("a failed job that a computer had without an answer says look first, and that it can still turn Done", () => {
  const note = maybePrintedNote({ ...job, state: "failed" });
  assert.equal(
    note,
    "packing-pc took this job and never reported back. Check the paper before you send it again. If packing-pc reports later, the job turns Done.",
  );
});

test("a cancelled one stays cancelled, so nothing is promised", () => {
  const note = maybePrintedNote({ ...job, state: "cancelled" });
  assert.equal(note, "packing-pc took this job and never reported back. Check the paper before you send it again.");
});

test("without the field there is no note: an older server, or a job whose end is certain", () => {
  for (const state of ["queued", "claimed", "done", "failed", "cancelled"] as const) {
    assert.equal(maybePrintedNote({ agent_name: "packing-pc", state }), null, state);
    assert.equal(maybePrintedNote({ agent_name: "packing-pc", state, maybe_printed: false }), null, state);
  }
});
