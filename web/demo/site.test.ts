// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The crawler-facing files in demo/static must agree with each other and with the page metadata.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { demoJsonLd, demoMetadata, SITE } from "./meta.ts";

const dir = new URL("./static/", import.meta.url);
const read = (name: string) => readFileSync(new URL(name, dir), "utf8");

test("robots.txt points at the sitemap on the site's own origin", () => {
  assert.match(read("robots.txt"), new RegExp(`^Sitemap: ${SITE}/sitemap\\.xml$`, "m"));
});

test("every sitemap URL is on the site's origin", () => {
  const locs = [...read("sitemap.xml").matchAll(/<loc>(.*?)<\/loc>/g)].map((m) => m[1]);
  assert.ok(locs.length > 0);
  for (const loc of locs) assert.ok(loc.startsWith(`${SITE}/`), loc);
});

test("llms.txt opens with the project name and a summary", () => {
  assert.match(read("llms.txt"), /^# printpapi\n\n> \S/);
});

test("the IndexNow key file holds its own name", () => {
  const keys = readdirSync(dir).filter((n) => /^[0-9a-f]{32}\.txt$/.test(n));
  assert.equal(keys.length, 1);
  assert.equal(read(keys[0]).trim(), keys[0].slice(0, -4));
});

test("metadata and structured data name the same origin", () => {
  assert.equal(demoMetadata.metadataBase.href, `${SITE}/`);
  assert.equal(demoJsonLd.url, `${SITE}/`);
});
