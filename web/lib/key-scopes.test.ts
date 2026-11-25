// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import {
  KEY_SCOPES,
  canSaveScope,
  initialEditScope,
  scopeLabel,
  scopeHint,
  signInCheck,
} from "./key-scopes.ts";

test("the five rights, in this order, with their labels", () => {
  assert.deepEqual(
    KEY_SCOPES.map((s) => [s.value, s.label]),
    [
      ["full", "Everything"],
      ["agent", "Agent only"],
      ["cloudprnt", "CloudPRNT only"],
      ["print", "Print only"],
      ["read", "Read only"],
    ],
  );
  for (const s of KEY_SCOPES) assert.ok(s.hint.length > 20, s.value);
});

test("the hints say what each right is for", () => {
  assert.equal(scopeHint("full"), "Print, read, label templates and agents. Like every key so far.");
  assert.equal(scopeHint("agent"), "For one computer with printers. Cannot submit jobs.");
  assert.equal(
    scopeHint("cloudprnt"),
    "For Star printers that poll by themselves. Reaches all of them in your org.",
  );
  assert.equal(scopeHint("print"), "Submits jobs and sees only its own. For a shop, n8n or a script.");
  assert.equal(scopeHint("read"), "Sees jobs, printers and metrics. Prints nothing.");
});

test("a key without a scope is Everything, an unknown word is shown as it is", () => {
  assert.equal(scopeLabel(undefined), "Everything");
  assert.equal(scopeLabel("full"), "Everything");
  assert.equal(scopeLabel("print"), "Print only");
  assert.equal(scopeLabel("hologram"), "hologram");
});

test("sign-in: root, a session, a full key and a read key may open the dashboard", () => {
  assert.equal(signInCheck({ kind: "root", org_id: null }), "ok");
  assert.equal(signInCheck({ kind: "session", org_id: 1, email: "a@b.c", user_id: 1 }), "ok");
  assert.equal(signInCheck({ kind: "key", org_id: 1 }), "ok");
  assert.equal(signInCheck({ kind: "key", org_id: 1, scope: "full" }), "ok");
  assert.equal(signInCheck({ kind: "key", org_id: 1, scope: "read" }), "ok");
});

test("sign-in: agent, cloudprnt, print and unknown scopes are limited", () => {
  for (const scope of ["agent", "cloudprnt", "print", "unknown", "hologram", ""]) {
    assert.equal(signInCheck({ kind: "key", org_id: 1, scope }), "limited", scope);
  }
});

test("sign-in: anything that is not this API's answer is rejected", () => {
  for (const v of [null, undefined, "<html>hi</html>", 5, [], {}, { kind: "admin" }, { kind: 3 }, { org_id: 1 }]) {
    assert.equal(signInCheck(v), "rejected", JSON.stringify(v));
  }
});

test("the rights dialog starts on the key's own right, and on nothing for a word it does not know", () => {
  assert.equal(initialEditScope(undefined), "full");
  assert.equal(initialEditScope("full"), "full");
  assert.equal(initialEditScope("print"), "print");
  // a key a newer server stored: the server denies it everything, so the dialog must not
  // pretend it is "Everything"
  assert.equal(initialEditScope("hologram"), "");
});

test("Save needs a choice that differs from the stored right", () => {
  assert.equal(canSaveScope(undefined, "full"), false);
  assert.equal(canSaveScope("print", "print"), false);
  assert.equal(canSaveScope("print", "full"), true);
  assert.equal(canSaveScope(undefined, "read"), true);
  // an unknown stored word: only an explicit pick of one of the five words saves
  assert.equal(canSaveScope("hologram", ""), false);
  assert.equal(canSaveScope("hologram", "full"), true);
});
