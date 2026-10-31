// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import { clearToken, getToken, setToken } from "./token.ts";

function mockWindow(store: Map<string, string>, failSet = false) {
  (globalThis as any).window = { // eslint-disable-line @typescript-eslint/no-explicit-any
    localStorage: {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => {
        if (failSet) throw new Error("blocked");
        store.set(k, v);
      },
      removeItem: (k: string) => void store.delete(k),
    },
  };
}

test("another tab removing the key signs this tab out", () => {
  const store = new Map<string, string>();
  mockWindow(store);
  setToken("root-token");
  assert.equal(getToken(), "root-token");
  store.delete("printpapi_token"); // the other tab's removeItem
  assert.equal(getToken(), "");
});

test("a failed storage write keeps the token in memory", () => {
  mockWindow(new Map(), true);
  setToken("t");
  assert.equal(getToken(), "t");
  clearToken();
  assert.equal(getToken(), "");
});

for (const [name, win] of [
  ["a localStorage getter that throws (site data blocked)", Object.defineProperty({}, "localStorage", { get() { throw new Error("SecurityError"); } })],
  ["no localStorage at all", {}],
] as const) {
  test(`${name}: the token lives in memory`, () => {
    (globalThis as any).window = win; // eslint-disable-line @typescript-eslint/no-explicit-any
    clearToken();
    assert.equal(getToken(), "");
    setToken("demo");
    assert.equal(getToken(), "demo");
    clearToken();
    assert.equal(getToken(), "");
  });
}
