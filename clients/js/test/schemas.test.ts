import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { Printpapi } from "../src/index.ts";
import type { LabelSpec } from "../src/index.ts";
import { validate } from "../testkit/contract.ts";
import { type Harness, harness } from "../testkit/harness.ts";
import { FakeAgent, createOrg, throwaway } from "../testkit/server.ts";

describe("what the server takes, the types take (test 20)", { timeout: 60_000 }, () => {
  it("an order of an ordinary interface type compiles, and json of a *_uri_post job may be any JSON", async () => {
    interface Order {
      number: string;
      lines: { qty: number; name: string }[];
    }
    const order: Order = { number: "1", lines: [{ qty: 1, name: "Widget" }] };
    const sent: string[] = [];
    const fetchOk = async (_url: string | URL | Request, init?: RequestInit) => {
      sent.push(String(init?.body));
      return new Response(JSON.stringify({ job_id: 1 }), { status: 200, headers: { "Content-Type": "application/json" } });
    };
    const c = new Printpapi({ baseUrl: "http://x.test", apiKey: "the-key-0123456", fetch: fetchOk as typeof fetch });
    await c.orders.print({ printer_id: 1, order });
    await c.jobs.submit({ printer_id: 1, type: "raw_uri_post", url: "http://y.test/", json: [1, 2] });
    await c.jobs.submit({ printer_id: 1, type: "pdf_uri_post", url: "http://y.test/", json: "text" });
    assert.equal(JSON.parse(sent[1] ?? "").json.length, 2);
  });

  it("a label template may leave a field of text out, and the server takes it", async () => {
    const h: Harness = await harness();
    try {
      const org = await createOrg(h.server, "schemas");
      const c = h.client(org.key);
      const spec: LabelSpec = {
        name: "wrap-only", content: "text", label_mm: { w: 50, h: 30 }, dpi: 203, rotate: 0,
        crop_percent: { t: 0, r: 0, b: 0, l: 0 }, place_mm: { x: 0, y: 0, w: 50, h: 30 }, text: { wrap_chars: 28 },
      };
      const made = await c.labels.create(spec);
      assert.equal(made.text?.wrap_chars, 28);
      assert.equal(made.text?.lines_per_label, 0);
    } finally {
      await h.stop();
    }
  });

  it("a *_uri_post job with an array as json is posted as it is, through the real server", async () => {
    const h: Harness = await harness();
    const got = await throwaway((_req, res) => res.end("PAYLOAD"));
    try {
      const org = await createOrg(h.server, "uripost");
      const agent = await new FakeAgent(h.server.url, org.agentKey, h.recorder.fetch).register("pc", [{ name: "Zebra" }]);
      await h.client(org.key).jobs.submit({ printer_id: agent.printer("Zebra"), type: "raw_uri_post", url: got.url + "/render", json: [1, 2] });
      assert.equal(got.seen[0]?.method, "POST");
      assert.deepEqual(JSON.parse(got.seen[0]?.body ?? ""), [1, 2]);
    } finally {
      await got.close();
      await h.stop();
    }
  });

  it("a column of a receipt is 1 to 100 wide in the document as it is on the server", async () => {
    const h: Harness = await harness();
    try {
      const org = await createOrg(h.server, "width");
      const c = h.client(org.key);
      const row = (width: number) => ({ receipt: { content: [{ type: "columns" as const, cells: [{ text: "a", width }, { text: "b" }] }] } });
      const cell = { $ref: "#/components/schemas/ReceiptCell" };
      for (const [width, ok] of [[1, true], [30, true], [100, true], [101, false], [0, false]] as const) {
        const documented = validate(h.doc, cell, { text: "a", width }).length === 0;
        assert.equal(documented, ok, "the document on width " + width);
        if (width === 100) continue; // 100 is valid, but no second cell fits beside it: the layout refuses
        const served = await c.receipts.preview(row(width)).then(() => true, () => false);
        assert.equal(served, ok, "the server on width " + width);
      }
    } finally {
      await h.stop();
    }
  });
});
