// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import { PAIRING_WORD, codeWentAway, isLocalOrigin, needsAttention, pairingLink, pairingServer } from "./pairing.ts";

test("the link is the golden one that crates/agent/src/pair.rs parses back", () => {
  assert.equal(
    pairingLink("https://print.example.com", "7K3M-Q2ZD-9XHT"),
    "printpapi://pair?server=https%3A%2F%2Fprint.example.com&code=7K3M-Q2ZD-9XHT",
  );
  assert.equal(
    pairingLink("http://192.168.1.5:3460", "7K3M-Q2ZD-9XHT"),
    "printpapi://pair?server=http%3A%2F%2F192.168.1.5%3A3460&code=7K3M-Q2ZD-9XHT",
  );
});

test("only addresses that reach this computer alone are local", () => {
  for (const o of ["http://localhost:3460", "http://LOCALHOST", "http://127.0.0.1:3460", "http://127.5.5.5", "http://[::1]:3460", "http://0.0.0.0:3460", "http://print.localhost", "not an address"]) {
    assert.equal(isLocalOrigin(o), true, o);
  }
  for (const o of ["https://print.example.com", "http://192.168.1.5:3460", "http://10.0.0.7", "https://localhost.example.com", "http://127.0.0.1.example.com"]) {
    assert.equal(isLocalOrigin(o), false, o);
  }
});

test("the dashboard's own address is used unless it is local, then the server's PUBLIC_URL, else none", () => {
  assert.equal(pairingServer("https://print.example.com", null), "https://print.example.com");
  assert.equal(pairingServer("https://print.example.com", "https://other.example.org"), "https://print.example.com");
  assert.equal(pairingServer("http://localhost:3460", "https://print.example.com"), "https://print.example.com");
  assert.equal(pairingServer("http://localhost:3460", null), null);
});

test("every state has its own word", () => {
  assert.deepEqual(Object.values(PAIRING_WORD), ["Waiting", "Expired", "Key fetched", "Ready", "Connected", "Revoked"]);
  assert.equal(new Set(Object.values(PAIRING_WORD)).size, 6);
});

test("attention: an open code, a fetched key, a waiting device and an old key that still works", () => {
  const old = { id: 3, label: "office-pc" };
  assert.equal(needsAttention({ state: "open", old_key: null }), true);
  assert.equal(needsAttention({ state: "fetched", old_key: null }), true);
  assert.equal(needsAttention({ state: "registered", old_key: null }), true);
  assert.equal(needsAttention({ state: "connected", old_key: old }), true);
  assert.equal(needsAttention({ state: "connected", old_key: null }), false);
  assert.equal(needsAttention({ state: "expired", old_key: null }), false);
  assert.equal(needsAttention({ state: "revoked", old_key: old }), false);
});

test("a code that was listed and is not any more went away, one that was never listed did not", () => {
  const rows = [{ id: 3 }, { id: 5 }];
  assert.equal(codeWentAway(rows, 5, true), false);
  assert.equal(codeWentAway(rows, 4, true), true, "replaced by a newer code of the device, or cancelled");
  // the list that was read before the code existed does not hold it yet
  assert.equal(codeWentAway(rows, 4, false), false);
  assert.equal(codeWentAway(undefined, 4, true), false, "no list yet");
  assert.equal(codeWentAway([], 4, true), true);
});
