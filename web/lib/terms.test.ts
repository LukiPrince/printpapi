// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import { parseTerms, termsMessage } from "./terms.ts";

test("a server without terms in /health asks for no consent", () => {
  assert.equal(parseTerms(undefined), null);
  assert.equal(parseTerms(null), null);
  assert.equal(parseTerms({}), null);
  assert.equal(parseTerms({ version: "", terms_url: "https://x.test/t" }), null);
  assert.equal(parseTerms({ version: "v1" }), null); // no terms link: nothing to accept
});

test("terms keep their links and turn a missing optional link into null", () => {
  assert.deepEqual(
    parseTerms({ version: "2026-11-13", terms_url: "https://x.test/t", dpa_url: "https://x.test/d" }),
    { version: "2026-11-13", terms_url: "https://x.test/t", dpa_url: "https://x.test/d", privacy_url: null },
  );
});

test("only http(s) links are ever rendered", () => {
  const t = parseTerms({ version: "v", terms_url: "https://x.test/t", dpa_url: "javascript:alert(1)" });
  assert.equal(t?.dpa_url, null);
  assert.equal(parseTerms({ version: "v", terms_url: "javascript:alert(1)" }), null);
});

test("a 409 reads as a reload hint, other statuses keep the server wording", () => {
  assert.equal(termsMessage(409, "terms changed, reload"), "The terms changed, please reload.");
  assert.equal(termsMessage(400, "email required"), "email required");
});
