import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

// `next dev` proxies only the paths in API_PATHS (next.config.ts). A path lib/api.ts calls that is
// missing there is answered by Next itself with its 404 page. Two paths are also dashboard pages
// (a proxy would replace the page with JSON), so they are deliberately absent.
const PAGES = new Set(["/users", "/labels"]);

test("every root path lib/api.ts calls is proxied by next dev, unless a page lives there", () => {
  const api = readFileSync(new URL("./api.ts", import.meta.url), "utf8");
  const config = readFileSync(new URL("../next.config.ts", import.meta.url), "utf8");
  const list = /const API_PATHS = \[([\s\S]*?)\];/.exec(config);
  assert.ok(list, "API_PATHS not found");
  const proxied = new Set([...list[1].matchAll(/"(\/[a-z-]+)"/g)].map((m) => m[1]));
  const called = new Set([...api.matchAll(/["`](\/[a-z-]+)(?=[/?"`$])/g)].map((m) => m[1]));
  assert.ok(called.has("/pairing") && called.has("/support-access"), [...called].join(" "));
  const missing = [...called].filter((p) => !proxied.has(p) && !PAGES.has(p));
  assert.deepEqual(missing, []);
});
