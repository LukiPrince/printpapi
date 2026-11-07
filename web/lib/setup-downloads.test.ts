// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import { setupDownloads } from "./setup-downloads.ts";
import raw from "./setup-downloads.json" with { type: "json" };

const BASE = "https://github.com/LukiPrince/printpapi/releases/latest/download/";

test("every url is the latest-release download of its asset", () => {
  const list = setupDownloads();
  assert.equal(list.length, raw.length);
  list.forEach((d, i) => {
    assert.equal(d.url, BASE + raw[i].asset);
    assert.ok(d.url.endsWith(raw[i].asset));
    assert.ok(raw[i].asset.startsWith("printpapi-setup-"), raw[i].asset);
  });
});

test("urls and labels are unique and the platforms are the three systems", () => {
  const list = setupDownloads();
  assert.equal(new Set(list.map((d) => d.url)).size, list.length);
  assert.equal(new Set(list.map((d) => d.label)).size, list.length);
  assert.deepEqual([...new Set(list.map((d) => d.platform))].sort(), ["linux", "macos", "windows"]);
});

test("macOS has no hand check yet, so it is beta", () => {
  const mac = setupDownloads().filter((d) => d.platform === "macos");
  assert.ok(mac.length > 0);
  assert.ok(mac.every((d) => d.beta));
});

test("the Windows setup is not beta", () => {
  const win = setupDownloads().filter((d) => d.platform === "windows");
  assert.ok(win.length > 0);
  assert.ok(win.every((d) => !d.beta));
});
