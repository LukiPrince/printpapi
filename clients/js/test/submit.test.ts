import assert from "node:assert/strict";
import { after, before, describe, it } from "node:test";
import { PrintpapiError } from "../src/index.ts";
import type { Printpapi } from "../src/index.ts";
import { type Harness, harness } from "../testkit/harness.ts";
import { FakeAgent, PDF_BYTES, type Throwaway, createOrg, throwaway, tinyPng } from "../testkit/server.ts";

const text = (b: Uint8Array) => new TextDecoder().decode(b);
const enc = (s: string) => new TextEncoder().encode(s);

describe("all ten job types (test 5)", { timeout: 120_000 }, () => {
  let h: Harness;
  let c: Printpapi;
  let agent: FakeAgent;
  let source: Throwaway;
  let png: Uint8Array;
  let zpl = 0;
  let pdf = 0;
  before(async () => {
    h = await harness();
    const org = await createOrg(h.server, "submit");
    c = h.client(org.key);
    agent = await new FakeAgent(h.server.url, org.agentKey, h.recorder.fetch).register("pc", [
      { name: "Zebra", can_pdf: false },
      { name: "Laser", can_pdf: true },
    ]);
    zpl = agent.printer("Zebra");
    pdf = agent.printer("Laser");
    png = await tinyPng();
    source = await throwaway((req, res, seen) => {
      if (req.method === "POST") {
        res.end(enc("POSTED:" + seen.body));
      } else if (req.url === "/image") {
        res.setHeader("Content-Type", "image/png");
        res.end(png);
      } else {
        res.end(req.url === "/pdf" ? PDF_BYTES : enc("RAW-FROM-URL"));
      }
    });
    await c.labels.create({
      name: "img", content: "image", label_mm: { w: 50, h: 30 }, dpi: 203, rotate: 0,
      crop_percent: { t: 0, r: 0, b: 0, l: 0 }, place_mm: { x: 0, y: 0, w: 50, h: 30 },
    });
    await c.labels.create({
      name: "txt", content: "text", label_mm: { w: 50, h: 30 }, dpi: 203, rotate: 0,
      crop_percent: { t: 0, r: 0, b: 0, l: 0 }, place_mm: { x: 1, y: 1, w: 48, h: 28 },
      text: { wrap_chars: 12, lines_per_label: 2 },
    });
  });
  after(async () => {
    await source.close();
    await h.stop();
  });

  it("raw_base64: base64 text and bytes arrive the same", async () => {
    const zplText = "^XA^FO50,50^FDhello^FS^XZ";
    await c.jobs.submit({ printer_id: zpl, type: "raw_base64", content: btoa(zplText), title: "text", copies: 2 });
    const first = await agent.print();
    assert.equal(text(first.bytes), zplText);
    assert.equal(first.job.copies, 2);
    assert.equal(first.job.mode, "raw");
    const binary = Uint8Array.from([0, 1, 2, 250, 251, 252, 253, 254, 255, 0x1b, 0x40]);
    await c.jobs.submit({ printer_id: zpl, type: "raw_base64", content: binary });
    assert.deepEqual((await agent.print()).bytes, binary, "a Uint8Array round trips byte for byte");
  });

  it("pdf_base64 goes to a PDF printer with options", async () => {
    await c.jobs.submit({ printer_id: pdf, type: "pdf_base64", content: PDF_BYTES, options: { duplex: "long-edge", paper: "A4", color: false } });
    const { job, bytes } = await agent.print();
    assert.equal(job.mode, "pdf");
    assert.deepEqual(job.options, { duplex: "long-edge", paper: "A4", color: false });
    assert.deepEqual(bytes, PDF_BYTES);
  });

  it("raw_uri and pdf_uri are fetched by the server", async () => {
    await c.jobs.submit({ printer_id: zpl, type: "raw_uri", url: source.url + "/raw" });
    assert.equal(text((await agent.print()).bytes), "RAW-FROM-URL");
    await c.jobs.submit({ printer_id: pdf, type: "pdf_uri", url: source.url + "/pdf" });
    assert.deepEqual((await agent.print()).bytes, PDF_BYTES);
  });

  it("raw_uri_post and pdf_uri_post post their json to the URL", async () => {
    await c.jobs.submit({ printer_id: zpl, type: "raw_uri_post", url: source.url + "/render", json: { order: 4712 } });
    assert.deepEqual(JSON.parse(text((await agent.print()).bytes).slice("POSTED:".length)), { order: 4712 });
    await c.jobs.submit({ printer_id: pdf, type: "pdf_uri_post", url: source.url + "/render", json: { a: [1, 2] } });
    const { job, bytes } = await agent.print();
    assert.equal(job.mode, "pdf");
    assert.deepEqual(JSON.parse(text(bytes).slice("POSTED:".length)), { a: [1, 2] });
  });

  it("label_uri renders ZPL for a raw printer and a PDF for a PDF printer", async () => {
    await c.jobs.submit({ printer_id: zpl, type: "label_uri", template: "img", url: source.url + "/image" });
    assert.match(text((await agent.print()).bytes), /^\^XA/);
    await c.jobs.submit({ printer_id: pdf, type: "label_uri", template: "img", url: source.url + "/image" });
    assert.match(text((await agent.print()).bytes.slice(0, 5)), /^%PDF-/);
  });

  it("label_base64 takes options for a PDF printer and refuses them for ZPL", async () => {
    await c.jobs.submit({ printer_id: pdf, type: "label_base64", template: "img", content: png, options: { orientation: "landscape" } });
    const { job, bytes } = await agent.print();
    assert.deepEqual(job.options, { orientation: "landscape" }, "the option arrives at the agent");
    assert.match(text(bytes.slice(0, 5)), /^%PDF-/);
    await assert.rejects(
      c.jobs.submit({ printer_id: zpl, type: "label_base64", template: "img", content: png, options: { orientation: "landscape" } }),
      (e: unknown) => e instanceof PrintpapiError && e.kind === "invalid" && e.status === 400 && /only supported on pdf/.test(e.message),
    );
  });

  it("label_text takes the template by integer id", async () => {
    const tpl = (await c.labels.list()).find((t) => t.name === "txt");
    assert.ok(tpl);
    await c.jobs.submit({ printer_id: zpl, type: "label_text", template: tpl.id, text: "Module Replacement needed" });
    assert.match(text((await agent.print()).bytes), /^\^XA/);
  });

  it("receipt becomes ESC/POS for a raw printer", async () => {
    await c.jobs.submit({
      printer_id: zpl, type: "receipt", title: "Bon 1",
      receipt: { cut: true, content: [
        { type: "text", text: "Cafe Mueller", align: "center", bold: true, size: 2 },
        { type: "divider" },
        { type: "columns", cells: [{ text: "2 x Brot" }, { text: "5,80", align: "right", width: 10 }] },
        { type: "qr", data: "https://example.com/r/1" },
        { type: "barcode", format: "ean13", data: "4006381333931" },
        { type: "feed", lines: 2 },
        { type: "drawer" },
      ] },
    });
    const { bytes } = await agent.print();
    assert.deepEqual([bytes[0], bytes[1]], [0x1b, 0x40], "starts with ESC @");
  });

  it("refuses options on a raw type: a type error, and a 400 when forced", async () => {
    await assert.rejects(
      c.jobs.submit({
        printer_id: zpl, type: "raw_base64", content: "AA==",
        // @ts-expect-error raw jobs carry their own layout: options exist only on pdf, label and receipt jobs
        options: { duplex: "long-edge" },
      }),
      (e: unknown) => e instanceof PrintpapiError && e.kind === "invalid" && e.status === 400,
    );
  });

  it("answers a repeat with the same key by the same job and queues nothing", async () => {
    const a = await c.jobs.submit({ printer_id: zpl, type: "raw_base64", content: "AA==", idempotency_key: "order-9" });
    const b = await c.jobs.submit({ printer_id: zpl, type: "raw_base64", content: "AA==", idempotency_key: "order-9" });
    assert.equal(a.job_id, b.job_id);
    await agent.print();
  });
});
