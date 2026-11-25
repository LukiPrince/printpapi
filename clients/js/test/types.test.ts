import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";
import { pathToFileURL } from "node:url";
import { join } from "node:path";
import { type Json, assertSupported, findOperation, validate } from "../testkit/contract.ts";
import { OPENAPI_FILE, ROOT } from "../testkit/server.ts";

const doc = JSON.parse(readFileSync(OPENAPI_FILE, "utf8")) as Json;
const CLIENT = join(ROOT, "clients/js");

describe("generated types (test 20)", () => {
  it("equal the committed src/types.ts: run 'npm run gen-types' after changing the document", async () => {
    const mod = (await import(pathToFileURL(join(CLIENT, "scripts/gen-types.mjs")).href)) as { generate(d: unknown): string };
    assert.equal(mod.generate(doc), readFileSync(join(CLIENT, "src/types.ts"), "utf8"));
  });

  it("refuse a schema keyword they do not know", async () => {
    const mod = (await import(pathToFileURL(join(CLIENT, "scripts/gen-types.mjs")).href)) as { generate(d: unknown): string };
    const bad = { components: { schemas: { X: { type: "object", properties: { a: { type: "string", format: "uri" } }, required: [] } } } };
    assert.throws(() => mod.generate(bad), /unsupported schema keyword format/);
  });
});

describe("the contract checker", () => {
  it("understands every keyword of the document and throws on any other", () => {
    assertSupported(doc);
    const bad = structuredClone(doc) as Json;
    bad.components.schemas.Ok.properties.ok.const = true;
    assert.throws(() => assertSupported(bad), /unsupported schema keyword const/);
  });

  it("fails a response with an unlisted property, a missing one, a wrong type and a bad enum", () => {
    const ok = { state: "queued", error: null, printer_id: 1, created_at: 1.5, finished_at: null };
    const schema = { $ref: "#/components/schemas/JobStatus" };
    assert.deepEqual(validate(doc, schema, ok), []);
    assert.match(validate(doc, schema, { ...ok, extra: 1 }).join(), /unlisted property extra/);
    assert.match(validate(doc, schema, { state: "queued" }).join(), /missing printer_id/);
    assert.match(validate(doc, schema, { ...ok, printer_id: "1" }).join(), /expected integer/);
    assert.match(validate(doc, schema, { ...ok, state: "lost" }).join(), /is not one of/);
  });

  it("takes exactly one alternative of a oneOf", () => {
    const schema = { $ref: "#/components/schemas/JobRequest" };
    assert.deepEqual(validate(doc, schema, { printer_id: 1, type: "raw_base64", content: "AA==" }), []);
    assert.match(validate(doc, schema, { printer_id: 1, type: "nope" }).join(), /matches 0 of 10/);
    assert.deepEqual(validate(doc, schema, { printer_id: 1, type: "label_text", text: "x", template: 3 }), []);
    assert.deepEqual(validate(doc, schema, { printer_id: 1, type: "label_text", text: "x", template: "name" }), []);
  });

  it("finds an operation by its path template", () => {
    assert.equal(findOperation(doc, "GET", "/jobs/12")?.operation.operationId, "getJob");
    assert.equal(findOperation(doc, "GET", "/jobs/12/preview")?.operation.operationId, "getJobPreview");
    assert.equal(findOperation(doc, "DELETE", "/cloudprnt/abc")?.operation.operationId, "cloudprntConfirmWithKey");
    assert.equal(findOperation(doc, "PATCH", "/jobs"), undefined);
  });
});
