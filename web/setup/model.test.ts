// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import { DEFAULT_RAW_PORT, addNetworkPrinter, canLeavePrinters, canSwitchOn, canTest, connectMayHelp, errorText, fromFound, logLocation, needsSumatra, returnsToConnect } from "./model.ts";
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

test("canTest skips a locked printer: it was set up by hand and stays as it is", () => {
  assert.equal(canTest(choice({ locked: true, target: "file:///dev/usb/lp0" }), "linux", true), false);
  assert.equal(canTest(choice({ locked: false }), "linux", true), true);
});

test("the network printer form starts at the raw port 9100", () => {
  assert.equal(DEFAULT_RAW_PORT, 9100);
});

test("errorText explains a proxy refusal and passes every other message through", () => {
  assert.match(errorText({ code: "proxy", message: "403" }), /^A proxy in front of the server refused this\./);
  assert.match(errorText({ code: "proxy", message: "403" }), /\[headers\]/);
  assert.equal(errorText({ code: "bad_key", message: "That key was not accepted." }), "That key was not accepted.");
});

test("the log of the installed agent is named per platform", () => {
  assert.equal(
    logLocation("windows"),
    "C:\\ProgramData\\printpapi\\print_agent-error.log",
  );
  assert.equal(logLocation("linux"), "/var/lib/printpapi/print_agent-error.log");
  assert.equal(logLocation("macos"), "/Library/Application Support/printpapi/print_agent-error.log");
});

test("a refused key or name sends the window back to the first step, every other failure stays", () => {
  assert.equal(returnsToConnect("key_in_use"), true);
  assert.equal(returnsToConnect("name_taken"), true);
  // nothing about the connection is wrong in these: another try, not another key
  for (const code of ["proxy", "unreachable", "system", "bad_staging", "error"]) {
    assert.equal(returnsToConnect(code), false, code);
  }
});

test("only a failure that another address, key or name fixes offers to go back to step 1", () => {
  for (const code of ["key_in_use", "name_taken", "unreachable"]) {
    assert.equal(connectMayHelp(code), true, code);
  }
  // the user said no, a proxy refused, the machine failed: nothing in step 1 changes that
  for (const code of ["cancelled", "proxy", "system", "bad_staging", "error"]) {
    assert.equal(connectMayHelp(code), false, code);
  }
});

test("a printer that is connected for one user only cannot be switched on, but can be switched off", () => {
  const found = (per_user: boolean): FoundPrinter => ({ name: "Buero", per_user, usable: true });
  assert.equal(canSwitchOn(found(true), false), false);
  assert.equal(canSwitchOn(found(true), true), true);
  assert.equal(canSwitchOn(found(false), false), true);
  assert.equal(canSwitchOn(null, false), true);
  assert.equal(canSwitchOn({ ...found(false), usable: false }, false), false);
});
