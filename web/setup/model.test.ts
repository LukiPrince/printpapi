// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import { addNetworkPrinter, canLeavePrinters, canTest, errorText, fromFound, needsSumatra } from "./model.ts";
import type { FoundPrinter, PrinterChoice } from "./api.ts";

const choice = (over: Partial<PrinterChoice> = {}): PrinterChoice => ({
  name: "Zebra",
  pdf: false,
  target: null,
  locked: false,
  ...over,
});

test("every found printer starts as raw", () => {
  const found: FoundPrinter[] = [
    { name: "Zebra GK420d", per_user: false, usable: true },
    { name: "HP LaserJet", per_user: true, usable: true },
  ];
  const list = fromFound(found);
  assert.deepEqual(
    list.map((p) => [p.name, p.pdf, p.target, p.locked]),
    [
      ["Zebra GK420d", false, null, false],
      ["HP LaserJet", false, null, false],
    ],
  );
});

test("a machine without printers can continue once a network printer is added", () => {
  assert.equal(canLeavePrinters(fromFound([])), false);
  const list = addNetworkPrinter([], "Dock label", "192.168.1.50", 9100);
  assert.equal(canLeavePrinters(list), true);
});

test("a network printer is a raw socket target", () => {
  const [p] = addNetworkPrinter([], "Dock label", "192.168.1.50", 9100);
  assert.equal(p.target, "socket://192.168.1.50:9100");
  assert.equal(p.pdf, false);
  assert.equal(p.locked, false);
  const [q] = addNetworkPrinter([], "Other", "10.0.0.7", 6101);
  assert.equal(q.target, "socket://10.0.0.7:6101");
});

test("addNetworkPrinter keeps the list it was given", () => {
  const before = [choice()];
  const after = addNetworkPrinter(before, "Dock label", "10.0.0.7", 9100);
  assert.equal(before.length, 1);
  assert.equal(after.length, 2);
});

test("addNetworkPrinter refuses a duplicate name and names the printers syntax cannot hold", () => {
  const base = [choice({ name: "Dock label" })];
  assert.throws(() => addNetworkPrinter(base, "Dock label", "10.0.0.7", 9100), /already/);
  for (const bad of ["a;b", "a=b", "a|b"]) {
    assert.throws(() => addNetworkPrinter([], bad, "10.0.0.7", 9100), /;/);
  }
  assert.throws(() => addNetworkPrinter([], "  ", "10.0.0.7", 9100), /name/i);
  assert.throws(() => addNetworkPrinter([], "Dock", "", 9100), /address/i);
  assert.throws(() => addNetworkPrinter([], "Dock", "10.0.0.7", 0), /port/i);
  assert.throws(() => addNetworkPrinter([], "Dock", "10.0.0.7", 70000), /port/i);
});

test("needsSumatra only on Windows with a PDF printer and no SumatraPDF", () => {
  const pdf = [choice({ pdf: true })];
  const raw = [choice()];
  assert.equal(needsSumatra("windows", pdf, false), true);
  assert.equal(needsSumatra("windows", pdf, true), false);
  assert.equal(needsSumatra("windows", raw, false), false);
  assert.equal(needsSumatra("macos", pdf, false), false);
  assert.equal(needsSumatra("linux", pdf, false), false);
});

test("canTest blocks a PDF printer on Windows without SumatraPDF", () => {
  assert.equal(canTest(choice({ pdf: true }), "windows", false), false);
  assert.equal(canTest(choice({ pdf: true }), "windows", true), true);
  assert.equal(canTest(choice(), "windows", false), true);
  assert.equal(canTest(choice({ pdf: true }), "linux", false), true);
  assert.equal(canTest(choice({ pdf: true }), "macos", false), true);
});

test("errorText explains a proxy refusal and passes every other message through", () => {
  assert.match(errorText({ code: "proxy", message: "403" }), /^A proxy in front of the server refused this\./);
  assert.match(errorText({ code: "proxy", message: "403" }), /\[headers\]/);
  assert.equal(errorText({ code: "bad_key", message: "That key was not accepted." }), "That key was not accepted.");
});
