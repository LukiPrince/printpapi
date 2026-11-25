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

// The names are the contract with release.yml (tools/docs/test_release_assets.py reads the same file)
// and with the downloads in docs/agent.md and README.md (tools/docs/check_docs.py).
test("an asset is printpapi-setup-<system>-<arch>.<ext> and its extension fits its system", () => {
  const name = /^printpapi-setup-(windows|macos|linux)-(x86_64|aarch64)\.(exe|msi|dmg|deb|rpm|AppImage)$/;
  const fits: Record<string, string[]> = {
    windows: ["exe", "msi"],
    macos: ["dmg"],
    linux: ["deb", "rpm", "AppImage"],
  };
  for (const { asset, platform } of raw) {
    const m = name.exec(asset);
    assert.ok(m, asset);
    assert.equal(m[1], platform, asset);
    assert.ok(fits[m[1]].includes(m[3]), asset);
  }
});

test("macOS has no hand check yet, so it is beta", () => {
  const mac = setupDownloads().filter((d) => d.platform === "macos");
  assert.ok(mac.length > 0);
  assert.ok(mac.every((d) => d.beta));
});

// No platform has had its hand check on real hardware yet (install, test print, change printers,
// remove), so every setup is beta. Take this test out with the first flag that goes to false.
test("before any hand check every setup is beta", () => {
  assert.ok(setupDownloads().every((d) => d.beta));
});
