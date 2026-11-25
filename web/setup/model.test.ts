// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import { DEFAULT_RAW_PORT, addNetworkPrinter, startServer, canLeavePrinters, canSwitchOn, canTest, connectMayHelp, errorText, pairingFields, pairingResult, formEdited, formFromLink, formServer, sameOrigin, fromFound, lastJobText, logLocation, keepsOldName, needsNewName, needsSumatra, patchFor, pathParts, returnsToConnect, statusWord, timeoutProblem, MODES, NEEDS_CONNECTED, UPDATE_URL_TEXT, autoLine, goBackNote, goBackPatch, installedIsNewer, isNewer, modePatch, modeStillOff, olderCardShown, ownIsNewer, updateAvailable, updateErrorText, updateLine } from "./model.ts";
import type { FoundPrinter, PrinterChoice, UpdateInfo } from "./api.ts";

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
    "C:\\ProgramData\\printpapi-agent\\print_agent-error.log",
  );
  assert.equal(logLocation("linux"), "/var/lib/printpapi-agent/print_agent-error.log");
  assert.equal(logLocation("macos"), "/Library/Application Support/printpapi-agent/print_agent-error.log");
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

test("the state of the agent is a word, never only a colour", () => {
  assert.equal(statusWord("online"), "Connected");
  assert.equal(statusWord("offline"), "Offline");
  assert.equal(statusWord("not_running"), "Not running");
});

test("a new key on the same server needs a new device name, another server does not", () => {
  assert.equal(needsNewName("https://print.example.com", "https://print.example.com"), true);
  // the way the agent reads the address: slashes at the end and the case of the host do not matter
  assert.equal(needsNewName("https://print.example.com/", " HTTPS://Print.Example.com//"), true);
  assert.equal(needsNewName("https://print.example.com", "https://other.example.com"), false);
  assert.equal(needsNewName("https://print.example.com", "https://print.example.com/api"), false);
  assert.equal(needsNewName("https://print.example.com", "http://print.example.com"), false);
  // nothing known about the old server (no status file): nothing to compare
  assert.equal(needsNewName("", "https://print.example.com"), false);
});

test("the old name is blocked on the same server, but not when the stored key cannot be read", () => {
  const old = { serverUrl: "https://print.example.com", name: "PACKTISCH-1" };
  assert.equal(keepsOldName(old, "https://print.example.com", "PACKTISCH-1"), true);
  // the way the agent reads the address and the name as typed
  assert.equal(keepsOldName(old, " https://Print.Example.com/ ", " PACKTISCH-1 "), true);
  assert.equal(keepsOldName(old, "https://print.example.com", "PACKTISCH-2"), false);
  // another server: the device is new there and the name can stay
  assert.equal(keepsOldName(old, "https://other.example.com", "PACKTISCH-1"), false);
  // the stored key cannot be read: the same key again with the same name must be possible
  assert.equal(keepsOldName({ ...old, keyUnreadable: true }, "https://print.example.com", "PACKTISCH-1"), false);
  assert.equal(keepsOldName({ ...old, keyUnreadable: false }, "https://print.example.com", "PACKTISCH-1"), true);
  // nothing known of the old connection
  assert.equal(keepsOldName(null, "https://print.example.com", "PACKTISCH-1"), false);
  assert.equal(keepsOldName({ serverUrl: "", name: "" }, "https://print.example.com", "x"), false);
});

test("a change names only what is different", () => {
  const same = { printers: [choice()], timeout: 60, headers: [["a", "1"]] as [string, string | null][] };
  assert.deepEqual(patchFor(same, structuredClone(same)), {});
  // locked is the window's own flag, not a setting
  assert.deepEqual(patchFor(same, { ...same, printers: [choice({ locked: true })] }), {});
  const printers = [choice(), choice({ name: "Lager", target: "socket://10.0.0.5:9100" })];
  assert.deepEqual(patchFor(same, { ...same, printers }), { printers });
  assert.deepEqual(patchFor(same, { ...same, printers: [choice({ pdf: true })] }), { printers: [choice({ pdf: true })] });
  assert.deepEqual(patchFor(same, { ...same, timeout: 90 }), { timeout: 90 });
  // a field that only one side has is not compared
  assert.deepEqual(patchFor({ printers: [choice()] }, { printers: [choice()], timeout: 90 }), {});
});

test("a header is set when it is new or changed and removed when it is gone", () => {
  const before: [string, string | null][] = [["keep", "1"], ["change", "old"], ["gone", "x"]];
  const after: [string, string | null][] = [["keep", "1"], ["change", "new"], ["added", "y"]];
  assert.deepEqual(patchFor({ headers: before }, { headers: after }), {
    headers: [["change", "new"], ["added", "y"], ["gone", null]],
  });
  assert.deepEqual(patchFor({ headers: before }, { headers: before }), {});
  // header names do not differ by case
  assert.deepEqual(patchFor({ headers: [["X-Token", "1"]] }, { headers: [["x-token", "1"]] }), {});
  assert.deepEqual(patchFor({ headers: [["X-Token", "1"]] }, { headers: [["x-token", "2"]] }), {
    headers: [["x-token", "2"]],
  });
});

test("a value that was never shown is unchanged unless it was typed", () => {
  // before the admin prompt only the names are known: null is "unknown"
  const before: [string, string | null][] = [["token", null], ["other", null]];
  assert.deepEqual(patchFor({ headers: before }, { headers: before }), {});
  assert.deepEqual(patchFor({ headers: before }, { headers: [["token", null]] }), {
    headers: [["other", null]],
  });
  assert.deepEqual(patchFor({ headers: before }, { headers: [["token", "typed"], ["other", null]] }), {
    headers: [["token", "typed"]],
  });
  // something unknown on the left, a removal on the right: the removal does not need the value
  assert.deepEqual(patchFor({ headers: [["token", null]] }, { headers: [] }), { headers: [["token", null]] });
});

test("the timeout must be a number the agent accepts", () => {
  assert.equal(timeoutProblem("60"), null);
  assert.equal(timeoutProblem(" 30 "), null);
  assert.equal(timeoutProblem("45.5"), null);
  assert.match(timeoutProblem("29") ?? "", /30/);
  for (const bad of ["", "abc", "6x", "Infinity", "-1"]) {
    assert.notEqual(timeoutProblem(bad), null, bad);
  }
});

test("the last job says none yet only while the agent runs", () => {
  const ago = (ts: number) => `at ${ts}`;
  const job = { id: 7, ok: true, at: 100 };
  assert.equal(lastJobText("online", job, ago), "#7 printed, at 100");
  assert.equal(lastJobText("offline", { ...job, ok: false }, ago), "#7 failed, at 100");
  assert.equal(lastJobText("online", null, ago), "none yet");
  assert.equal(lastJobText("offline", null, ago), "none yet");
  // an agent that is not running has told nothing: the value is missing, not "none"
  assert.equal(lastJobText("not_running", null, ago), "—");
  assert.equal(lastJobText("not_running", job, ago), "#7 printed, at 100");
});

test("a path breaks only after a separator", () => {
  assert.deepEqual(pathParts("C:\\ProgramData\\printpapi-agent\\print_agent-error.log"), [
    "C:\\",
    "ProgramData\\",
    "printpapi-agent\\",
    "print_agent-error.log",
  ]);
  assert.deepEqual(pathParts("/var/lib/x.log"), ["/", "var/", "lib/", "x.log"]);
  assert.deepEqual(pathParts("plain"), ["plain"]);
});

const info = (over: Partial<UpdateInfo> = {}): UpdateInfo => ({
  supported: true,
  mode: "notify",
  own: "3.7.0",
  installed: "3.7.0",
  fromInstalled: true,
  latest: null,
  checkedAt: null,
  lastAttempt: null,
  error: null,
  ...over,
});
const ago = (ts: number | null) => (ts === 7200 ? "2 h ago" : "just now");

test("versions compare number by number", () => {
  assert.ok(isNewer("3.10.0", "3.9.9"));
  assert.ok(isNewer("4.0.0", "3.99.99"));
  assert.ok(!isNewer("3.7.0", "3.7.0"));
  assert.ok(!isNewer("3.6.9", "3.7.0"));
  // something that is no version is never newer, and nothing is newer than it
  for (const bad of [null, undefined, "", "3.7", "v3.7.0", "3.7.0-rc1", "3.7.x"]) {
    assert.ok(!isNewer(bad, "3.0.0"), String(bad));
    assert.ok(!isNewer("3.0.0", bad), String(bad));
  }
});

test("the update line says one thing for every state", () => {
  assert.equal(updateLine(info({ supported: false }), ago), "This build has no update key, so it cannot check for updates.");
  assert.equal(
    updateLine(info({ mode: "off" }), ago),
    "Checking is switched off in agent.ini (updates = off, or an update_url that is not a usable folder).",
  );
  assert.match(updateLine(info({ mode: "unknown" }), ago), /has not said how it is set up/);
  // a restart cannot help an agent that is older than updates: the line points to the install
  assert.doesNotMatch(updateLine(info({ mode: "unknown" }), ago), /restart/i);
  assert.match(updateLine(info({ mode: "unknown" }), ago), /install this program/);
  assert.equal(updateLine(info({ latest: "3.8.0" }), ago), "Version 3.8.0 is available.");
  assert.equal(updateLine(info({ latest: "3.7.0", checkedAt: 7200 }), ago), "Up to date. Checked 2 h ago.");
  assert.equal(updateLine(info(), ago), "Not checked yet.");
  assert.equal(updateLine(info({ error: "The update server could not be reached: x" }), ago), "The update server could not be reached: x");
  const tried = { version: "3.8.0", ok: false, message: "Version 3.8.0 did not connect, so 3.7.0 was put back. It is running again.", at: 1 };
  // the stored sentence says it all: it is not wrapped into a second one
  assert.equal(updateLine(info({ latest: "3.8.0", lastAttempt: tried }), ago), tried.message);
  assert.equal(
    updateLine(info({ latest: "3.8.0", lastAttempt: { ...tried, message: "The agent did not stop, so nothing was changed." } }), ago),
    "Version 3.8.0 was not installed: The agent did not stop, so nothing was changed.",
  );
  // once that very version is installed, an old failure is history
  assert.equal(
    updateLine(info({ installed: "3.8.0", own: "3.8.0", latest: "3.8.0", checkedAt: 7200, lastAttempt: tried }), ago),
    "Up to date. Checked 2 h ago.",
  );
  // nothing published (a 404) is no "up to date"
  assert.equal(
    updateLine(info({ latest: null, checkedAt: 7200, error: "No signed update has been published yet." }), ago),
    "No signed update has been published yet.",
  );
  // an attempt at another version does not hide a newer one
  assert.equal(updateLine(info({ latest: "3.9.0", lastAttempt: tried }), ago), "Version 3.9.0 is available.");
  assert.equal(autoLine("auto"), "Installed automatically, usually at night.");
  assert.equal(autoLine("notify"), null);
});

test("an update is available when the newest is newer than the installed one, else than this program", () => {
  assert.ok(updateAvailable(info({ latest: "3.8.0" })));
  assert.ok(!updateAvailable(info({ latest: "3.7.0" })));
  assert.ok(updateAvailable(info({ latest: "3.7.1", installed: null })));
  assert.ok(!updateAvailable(info({ latest: "3.8.0", installed: "3.9.0" })));
});

test("the older card appears only for an installed agent that is newer than this program", () => {
  assert.ok(installedIsNewer(info({ installed: "3.8.0" })));
  assert.ok(!installedIsNewer(info({ installed: "3.7.0" })));
  assert.ok(!installedIsNewer(info({ installed: null })));
  assert.ok(ownIsNewer(info({ installed: "3.6.0" })));
  assert.ok(!ownIsNewer(info({ installed: null })));
});

test("a change of mode is a patch only when it is a change, and going back leaves auto", () => {
  assert.deepEqual(modePatch("notify", "auto"), { updates: "auto" });
  assert.equal(modePatch("auto", "auto"), null);
  assert.deepEqual(modePatch("unknown", "off"), { updates: "off" });
  assert.deepEqual(goBackPatch("auto"), { updates: "notify", goBack: true });
  assert.deepEqual(goBackPatch("notify"), { goBack: true });
  assert.deepEqual(goBackPatch("off"), { goBack: true });
  // a stopped agent does not say its mode: the install reads the file and lowers only "auto", so
  // an "off" that somebody chose stays off
  assert.deepEqual(goBackPatch("unknown"), { updates: "notify-if-auto", goBack: true });
  assert.match(goBackNote("auto") ?? "", /switched to "Tell me"/);
  assert.match(goBackNote("unknown") ?? "", /If updates are set to "Install automatically"/);
  assert.equal(goBackNote("notify"), null);
  assert.equal(goBackNote("off"), null);
  assert.deepEqual(
    MODES.map((m) => [m.value, m.label]),
    [["notify", "Tell me"], ["auto", "Install automatically"], ["off", "Off"]],
  );
});

test("a card or a section that started an install stays on screen until it is done", () => {
  const newer = info({ installed: "3.8.0" });
  const same = info({ installed: "3.7.0" });
  // nothing running: the card shows for a newer installed agent, unless this window installed it
  assert.ok(olderCardShown(newer, false, null));
  assert.ok(!olderCardShown(newer, true, null));
  assert.ok(!olderCardShown(same, false, null));
  assert.ok(!olderCardShown(null, false, null));
  // Go back runs: the marker reads 3.7.0 in the middle of it, and the card (with its notice) stays
  assert.ok(olderCardShown(same, false, "card"));
  assert.ok(olderCardShown(newer, false, "card"));
  // an install from Updates runs: the marker is new before the install has finished
  assert.ok(!olderCardShown(newer, false, "section"));
});

test("a mode that was chosen and is still off says why", () => {
  assert.ok(modeStillOff("notify", "off"));
  assert.ok(modeStillOff("auto", "off"));
  assert.ok(!modeStillOff("off", "off"));
  assert.ok(!modeStillOff("notify", "notify"));
  assert.ok(!modeStillOff("auto", "unknown"));
  assert.match(UPDATE_URL_TEXT, /update_url/);
});

test("the failures of an update have their own sentences", () => {
  const e = (code: string, message = "x") => updateErrorText({ code, message });
  assert.equal(e("not_published"), "No signed update has been published yet.");
  assert.equal(e("bad_signature"), "The update is not signed with a key this program knows. It was not installed.");
  assert.equal(e("bad_file"), "A downloaded file is not the one the release lists. Try again in a few minutes.");
  assert.equal(e("wont_start"), "The new version cannot be started on this computer (it may be blocked). Nothing was changed.");
  assert.equal(e("no_target"), "This release has no file for this installation. Use the setup program.");
  assert.equal(e("offline"), NEEDS_CONNECTED);
  assert.equal(e("unreachable", "The update server could not be reached: refused"), "The update server could not be reached: refused");
  assert.equal(e("rolled_back", "Version 3.8.0 did not connect, so 3.7.0 was put back. It is running again."), "Version 3.8.0 did not connect, so 3.7.0 was put back. It is running again.");
  assert.equal(e("proxy"), errorText({ code: "proxy", message: "x" }));
});

// --- pairing ---------------------------------------------------------------------------------

test("a link names its server and hides the address field, a bare code asks for it", () => {
  assert.equal(pairingFields(null), null);
  assert.deepEqual(pairingFields({ serverUrl: "https://print.example.com", code: "7K3M-Q2ZD-9XHT" }), {
    code: "7K3M-Q2ZD-9XHT",
    server: "https://print.example.com",
    askServer: false,
  });
  assert.deepEqual(pairingFields({ serverUrl: null, code: "7K3M-Q2ZD-9XHT" }), {
    code: "7K3M-Q2ZD-9XHT",
    server: null,
    askServer: true,
  });
});

const paired = { name: "warehouse-pc", org: "Acme", createdBy: "boss@acme.example", createdByEscaped: false, replaces: false, pairId: 1, serverUrl: "https://print.example.com" };

test("the server field starts from what the person typed or from the installed agent, never from a spent pairing", () => {
  const typed = { serverUrl: "https://typed.example", paired: undefined };
  const spent = { serverUrl: "https://evil.example", paired };
  const installed = { serverUrl: "https://own.example" };
  assert.equal(startServer(null, null), "");
  assert.equal(startServer(null, installed), "https://own.example");
  assert.equal(startServer(typed, installed), "https://typed.example");
  // the connection a pairing made names the server of a link: it is not typed into any field again
  assert.equal(startServer(spent, null), "");
  assert.equal(startServer(spent, installed), "https://own.example");
});

test("an address that is not a plain e-mail address carries a warning the person reads before Continue", () => {
  assert.equal(pairingResult(paired, null).createdByWarning, null);
  const w = pairingResult({ ...paired, createdBy: "boss@\\u{430}cme.example", createdByEscaped: true }, null);
  assert.equal(w.createdBy, "boss@\\u{430}cme.example");
  assert.match(w.createdByWarning ?? "", /not a plain e-mail address/);
  assert.match(w.createdByWarning ?? "", /address you expect/);
});

test("a new device is confirmed with account, creator and device, and warns about nothing on a fresh computer", () => {
  assert.deepEqual(pairingResult(paired, null), {
    account: "Acme",
    createdBy: "boss@acme.example",
    createdByWarning: null,
    device: "warehouse-pc",
    takeover: null,
    leaving: null,
  });
});

test("root's code has no creator: the server administrator is named", () => {
  assert.equal(pairingResult({ ...paired, createdBy: null }, null).createdBy, "the server administrator");
});

test("a code that takes a device over says what stops printing", () => {
  const r = pairingResult({ ...paired, replaces: true }, null);
  assert.equal(
    r.takeover,
    "This takes over the device warehouse-pc. If it runs on another computer, that one stops printing when this one starts.",
  );
  assert.equal(r.leaving, null);
});

test("leaving the connection of an installed agent is a warning, a reconnect of this very device is not", () => {
  const installed = { serverUrl: "https://print.example.com", name: "warehouse-pc" };
  // another server
  assert.equal(
    pairingResult({ ...paired, serverUrl: "https://other.example.org" }, installed).leaving,
    "This computer leaves https://print.example.com, device warehouse-pc. Its printers get new IDs, and the old device stays in that dashboard.",
  );
  // the same server but a new device
  assert.notEqual(pairingResult(paired, installed).leaving, null);
  // a reconnect of another device on the same server
  assert.notEqual(pairingResult({ ...paired, name: "other", replaces: true }, installed).leaving, null);
  // the installed name on the installed server, as a reconnect: nothing is left behind
  const same = pairingResult({ ...paired, replaces: true, serverUrl: "HTTPS://Print.Example.com/" }, installed);
  assert.equal(same.leaving, null);
  assert.notEqual(same.takeover, null);
  // the installed name but a code for a new device (no takeover): a new device is made
  assert.notEqual(pairingResult(paired, installed).leaving, null);
});

test("a proxy that refuses a pairing from a computer with an agent points at the command line", () => {
  const proxy = { code: "proxy", message: "A proxy refused." };
  assert.equal(
    errorText(proxy, { pairing: true, installed: true }),
    "A proxy in front of the server refused this. Run printpapi-agent pair on this computer; it sends the headers of the installed agent.",
  );
  const byHand = "A proxy in front of the server refused this. Set up the agent by hand with [headers], see docs/agent.md.";
  assert.equal(errorText(proxy), byHand);
  assert.equal(errorText(proxy, { pairing: true }), byHand);
  assert.equal(errorText(proxy, { installed: true }), byHand);
  assert.equal(errorText({ code: "bad_code", message: "wrong" }, { pairing: true, installed: true }), "wrong");
});

test("an agent that is not running has no server and no name: nothing is claimed about them", () => {
  // tray::load_status finds no status file after two minutes: server "" and name ""
  const unknown = { serverUrl: "", name: "" };
  const fresh = pairingResult(paired, unknown);
  assert.equal(fresh.takeover, null);
  assert.ok(fresh.leaving !== null);
  assert.equal(
    fresh.leaving,
    "This computer leaves the connection it has now. Its printers get new IDs, and the old device stays in that dashboard.",
  );
  // a Reconnect of this very device: nothing is left behind and nothing false is said about it
  const reconnect = pairingResult({ ...paired, replaces: true }, unknown);
  assert.equal(reconnect.leaving, null);
  assert.ok(reconnect.takeover !== null);
  // half known is unknown: no blank where the name or the server should stand
  for (const half of [
    { serverUrl: "https://a.example", name: "" },
    { serverUrl: "", name: "pc" },
  ]) {
    assert.equal(pairingResult(paired, half).leaving, fresh.leaving);
  }
});

test("the same server is the same origin whatever the spelling", () => {
  assert.equal(sameOrigin("https://print.example.com:443", "https://print.example.com"), true);
  assert.equal(sameOrigin("http://print.example.com:80/", "http://PRINT.example.com"), true);
  assert.equal(sameOrigin("https://print.example.com/sub/path", "https://print.example.com"), true);
  assert.equal(sameOrigin("https://print.example.com", "http://print.example.com"), false);
  assert.equal(sameOrigin("https://print.example.com:8443", "https://print.example.com"), false);
  assert.equal(sameOrigin("https://print.example.com", "https://other.example.com"), false);
  assert.equal(sameOrigin("", "https://print.example.com"), false);
  assert.equal(sameOrigin("not a url", "not a url"), false);
});

test("an installed https://print.example.com:443 is not left when the code is for https://print.example.com", () => {
  const installed = { serverUrl: "https://print.example.com:443", name: "warehouse-pc" };
  assert.equal(pairingResult({ ...paired, replaces: true }, installed).leaving, null);
});

const link = { serverUrl: "https://evil.example", code: "AAAA-AAAA-AAAA" };
const empty = { text: "", view: null, fromLink: false, typedServer: "" };

test("a bare code typed after a hostile link does not inherit the link's server", () => {
  const afterLink = formFromLink(empty, link);
  assert.equal(formServer(afterLink), "https://evil.example");
  assert.equal(afterLink.fromLink, true);
  // the person replaces the code with their own: the address field is what it was, not the link's
  const typed = formEdited(afterLink, "7K3M-Q2ZD-9XHT");
  assert.equal(typed.fromLink, false);
  assert.equal(typed.view, null, "nothing is sent for a text that has not been read");
  assert.equal(formServer({ ...typed, view: { serverUrl: null, code: "7K3M-Q2ZD-9XHT" } }), "");
  // an installed agent's address is what the field starts with, and it is still that
  const installed = formFromLink({ ...empty, typedServer: "https://print.example.com" }, link);
  assert.equal(
    formServer({ ...formEdited(installed, "7K3M-Q2ZD-9XHT"), view: { serverUrl: null, code: "7K3M-Q2ZD-9XHT" } }),
    "https://print.example.com",
  );
});

test("a second link replaces the first with its own server", () => {
  const first = formFromLink(empty, link);
  const second = formFromLink(first, { serverUrl: "https://print.example.com", code: "BBBB-BBBB-BBBB" });
  assert.equal(formServer(second), "https://print.example.com");
  assert.equal(second.text, "BBBB-BBBB-BBBB");
});
