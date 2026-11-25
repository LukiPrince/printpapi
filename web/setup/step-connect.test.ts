// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The two confirmations of a pairing show what the person agrees to. A text that is cut at the card
// edge can hide the end of an address (`print.example.com.aaaa…evil.example`), so these places wrap.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./step-connect.tsx", import.meta.url), "utf8");
const pairing = source.slice(source.indexOf("function PairConnect"), source.indexOf("function KeyConnect"));

test("the pairing step never cuts a text short", () => {
  assert.ok(pairing.length > 1000, "the slice is the pairing step");
  assert.ok(!/\btruncate\b/.test(pairing), "no truncate class in the pairing step");
  assert.ok(!/text-ellipsis|line-clamp|overflow-hidden/.test(pairing), "no other way to cut a text");
});

test("the server, the account, the creator and the device wrap anywhere", () => {
  assert.equal((pairing.match(/<dd className="break-all/g) ?? []).length, 4, "all four lines of the card");
  assert.equal((pairing.match(/<strong className="break-all font-mono">\{server\}<\/strong>/g) ?? []).length, 2, "both notices");
});

test("nothing that changes the connection can be clicked while the code is being spent", () => {
  const cancel = pairing.slice(pairing.indexOf("onClick={onCancel}") - 120, pairing.indexOf("onClick={onCancel}") + 40);
  assert.match(cancel, /disabled=\{busy\}/, "Cancel");
  const swap = pairing.slice(pairing.indexOf("onClick={onSwitch}") - 120, pairing.indexOf("onClick={onSwitch}") + 40);
  assert.match(swap, /disabled=\{busy\}/, "Use an address and a key");
});

test("nothing that changes the connection can be clicked while the key is being checked", () => {
  const keyForm = source.slice(source.indexOf("function KeyConnect"));
  const cancel = keyForm.slice(keyForm.indexOf("onClick={onCancel}") - 120, keyForm.indexOf("onClick={onCancel}") + 40);
  assert.match(cancel, /disabled=\{busy\}/, "Cancel");
  const swap = keyForm.slice(keyForm.indexOf("onClick={onSwitch}") - 120, keyForm.indexOf("onClick={onSwitch}") + 40);
  assert.match(swap, /disabled=\{busy\}/, "Use a pairing link or code");
});

test("the question about plain http can end in a refusal that the form shows as an error", () => {
  // needs_insecure_confirm answers server_fixed for a link to another server, before "Use it anyway"
  const keyForm = source.slice(source.indexOf("function KeyConnect"));
  for (const [name, text] of [["pairing", pairing], ["key", keyForm]] as const) {
    const at = text.indexOf("await setup.needsInsecureConfirm");
    assert.ok(at > 0, name);
    const body = text.slice(text.lastIndexOf("try {", at), text.indexOf("finally", at));
    assert.match(body, /catch \(e\) \{\s*setError\(errorText\(toSetupError\(e\)/, `${name}: the refusal is shown`);
  }
});

test("the install names the pairing the card showed", () => {
  const finish = readFileSync(new URL("./step-finish.tsx", import.meta.url), "utf8");
  assert.ok(finish.includes("pairId: connection.paired?.pairId"), "apply sends the pairing id back");
});

test("a server that is spent or hostile never fills the address field", () => {
  assert.ok(pairing.includes("startServer(initial, replacing, brand)"));
  assert.ok(!/typedServer: initial\?\.serverUrl/.test(source), "no field starts from initial.serverUrl directly");
  assert.ok(!/useState\(initial\?\.serverUrl/.test(source));
});

test("the last screen before the install wraps the server and the device like the confirmations", () => {
  const finish = readFileSync(new URL("./step-finish.tsx", import.meta.url), "utf8");
  assert.ok(!/\btruncate\b/.test(finish), "no truncate class in the finish step");
  assert.ok(/<dd className="break-all font-mono">\{connection\.serverUrl\}/.test(finish));
  assert.ok(/<dd className="break-all">\{deviceName\}/.test(finish));
});

test("the address in the card is the one the shell used, not one the window remembers", () => {
  assert.ok(pairing.includes("{paired.serverUrl}"));
  assert.ok(pairing.includes("serverUrl: paired.serverUrl"), "Continue sends it back");
});
