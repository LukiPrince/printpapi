// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import { createPreviewer, describeProfile, parseReceiptText, previewPlaceholder, receiptForJob, sameJson, showReceiptFailure } from "./receipt.ts";

test("parseReceiptText accepts an object", () => {
  const r = parseReceiptText('{"content": [{"type": "cut"}]}');
  assert.deepEqual(r, { receipt: { content: [{ type: "cut" }] } });
});

test("parseReceiptText reports a syntax error in plain words", () => {
  const r = parseReceiptText('{"content": [');
  assert.ok("error" in r);
  assert.match(r.error, /^That is not valid JSON: /);
  assert.ok("error" in parseReceiptText(""));
});

test("parseReceiptText wants an object", () => {
  for (const t of ["[]", "5", '"x"', "null", "true"]) {
    const r = parseReceiptText(t);
    assert.deepEqual(r, { error: 'The receipt must be a JSON object like {"content": [...]}.' }, t);
  }
});

test("describeProfile for a raw printer, a PDF printer and no printer", () => {
  const p = { paper_mm: 80, width_dots: 576, codepage: "raster", output: "auto" };
  assert.equal(describeProfile(p, false, true), "80 mm, 48 characters, ESC/POS");
  assert.equal(describeProfile(p, true, true), "80 mm, 48 characters, sent as a PDF");
  assert.equal(describeProfile({ ...p, output: "escpos" }, true, true), "80 mm, 48 characters, ESC/POS");
  assert.equal(describeProfile({ ...p, paper_mm: 58, width_dots: 384 }, false, true), "58 mm, 32 characters, ESC/POS");
  assert.equal(describeProfile(null, false, false), "Preview for 80 mm paper. Pick a printer to use its settings.");
});

test("describeProfile says nothing about the paper while a picked printer's profile is not known", () => {
  // loading, or a printer with no receipt settings (CloudPRNT, a read-only key): never "pick a printer"
  assert.equal(describeProfile(null, false, true), "");
  assert.equal(describeProfile(null, true, true), "");
});

test("the preview placeholder tells apart bad JSON, a failed preview and a picture on its way", () => {
  assert.equal(previewPlaceholder(false, null), "Fix the JSON to see the receipt.");
  assert.equal(previewPlaceholder(false, "x"), "Fix the JSON to see the receipt.");
  assert.equal(previewPlaceholder(true, null), "Drawing the receipt…");
  assert.equal(previewPlaceholder(true, "CloudPRNT printers cannot print receipts"), "No preview.");
});

test("receiptForJob returns the object and throws the parser's own words", () => {
  assert.deepEqual(receiptForJob('{"content": []}'), { content: [] });
  assert.throws(() => receiptForJob("[]"), { message: 'The receipt must be a JSON object like {"content": [...]}.' });
  assert.throws(() => receiptForJob("{"), /^Error: That is not valid JSON: /);
});

test("describeProfile warns that the printer's own font differs, for ESC/POS only", () => {
  const p = { paper_mm: 80, width_dots: 576, codepage: "cp1252", output: "auto" };
  assert.equal(describeProfile(p, false, true), "80 mm, 48 characters, ESC/POS. The printer's own font looks slightly different.");
  assert.equal(describeProfile(p, true, true), "80 mm, 48 characters, sent as a PDF");
});

test("sameJson ignores key order and nothing else", () => {
  assert.ok(sameJson({ a: 1, b: [1, { c: 2 }] }, { b: [1, { c: 2 }], a: 1 }));
  assert.ok(!sameJson({ a: 1 }, { a: 2 }));
  assert.ok(!sameJson({ a: 1 }, { a: 1, b: 1 }));
  assert.ok(!sameJson([1], { 0: 1 }));
  assert.ok(!sameJson(null, {}));
});

/** A send() the test settles by hand. */
function manual() {
  const calls: { body: string; ok: (u: string) => void; fail: (e: unknown) => void }[] = [];
  const send = (body: string) => new Promise<string>((ok, fail) => calls.push({ body, ok, fail }));
  return { calls, send };
}
const tick = () => new Promise((r) => setTimeout(r, 0));

test("the previewer keeps one request in flight and sends only the newest waiting body", async () => {
  const m = manual();
  const images: string[] = [];
  const p = createPreviewer<string>({ send: m.send, onImage: (u) => images.push(u), onError: () => assert.fail(), isBusy: () => false });
  p.request("a");
  p.request("b");
  p.request("c");
  assert.deepEqual(m.calls.map((c) => c.body), ["a"]);
  m.calls[0].ok("img-a");
  await tick();
  // b was replaced by c while a was in flight
  assert.deepEqual(m.calls.map((c) => c.body), ["a", "c"]);
  m.calls[1].ok("img-c");
  await tick();
  assert.deepEqual(images, ["img-a", "img-c"]);
  assert.equal(m.calls.length, 2);
  p.dispose();
});

test("a busy answer is retried after a pause, with the newest body", async () => {
  const m = manual();
  const errors: string[] = [];
  const images: string[] = [];
  const p = createPreviewer<string>({
    send: m.send,
    onImage: (u) => images.push(u),
    onError: (e) => errors.push(e),
    isBusy: (e) => e instanceof Error && e.message === "busy",
    retryMs: 5,
  });
  p.request("a");
  m.calls[0].fail(new Error("busy"));
  await tick();
  assert.equal(m.calls.length, 1, "waits before the retry");
  p.request("b"); // newer text arrives during the pause
  await new Promise((r) => setTimeout(r, 15));
  assert.deepEqual(m.calls.map((c) => c.body), ["a", "b"]);
  m.calls[1].ok("img-b");
  await tick();
  assert.deepEqual({ images, errors }, { images: ["img-b"], errors: [] });
  p.dispose();
});

test("another error is shown and the next request goes out", async () => {
  const m = manual();
  const errors: string[] = [];
  const p = createPreviewer<string>({ send: m.send, onImage: () => {}, onError: (e) => errors.push(e), isBusy: () => false });
  p.request("a");
  m.calls[0].fail(new Error("receipt.content[0].size must be 1-8 or [w, h]"));
  await tick();
  assert.deepEqual(errors, ["receipt.content[0].size must be 1-8 or [w, h]"]);
  p.request("b");
  assert.deepEqual(m.calls.map((c) => c.body), ["a", "b"]);
  p.dispose();
});

test("after dispose nothing is reported or sent", async () => {
  const m = manual();
  const p = createPreviewer<string>({ send: m.send, onImage: () => assert.fail(), onError: () => assert.fail(), isBusy: () => false });
  p.request("a");
  p.dispose();
  m.calls[0].ok("late");
  await tick();
  p.request("b");
  assert.equal(m.calls.length, 1);
});

test("a failed Show receipt can be tried again, except when the job has no receipt", () => {
  assert.deepEqual(showReceiptFailure(404, "not found"), { note: "No receipt to show for this job.", retry: false });
  assert.deepEqual(showReceiptFailure(503, "preview is busy, try again shortly"), { note: "preview is busy, try again shortly", retry: true });
  assert.deepEqual(showReceiptFailure(null, "Failed to fetch"), { note: "Failed to fetch", retry: true });
});
