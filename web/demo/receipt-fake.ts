// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The receipt routes of the demo. No second renderer: the sample receipt has two pre-rendered
// images (static/receipt-sample-58.png and -80.png, made by the Rust renderer and checked against
// it in crates/server/tests/receipt_golden.rs); anything else gets a plain note.
import type { ReceiptProfile } from "../lib/api";
import sample from "../lib/receipt-sample.json" with { type: "json" };
import { sameJson } from "../lib/receipt.ts";

/** The defaults of crates/server/src/receipt.rs (`Profile::default`). */
export const DEFAULT_PROFILE: ReceiptProfile = {
  paper_mm: 80,
  width_dots: 576,
  codepage: "raster",
  codepage_number: 16,
  cut: "partial",
  output: "auto",
};

export const DEMO_ONLY_SAMPLE = "the demo draws the sample receipt only, a real server draws yours";

const KEYS = ["paper_mm", "width_dots", "codepage", "codepage_number", "cut", "output"];

/** `parse_profile` of the server, same texts: the profile, or why it is not one. Omitted fields take
 *  the defaults (the width follows the paper), null reads as omitted. */
export function parseProfile(v: Record<string, unknown>): ReceiptProfile | string {
  const bad = Object.keys(v).find((k) => !KEYS.includes(k));
  if (bad !== undefined) return `unknown field in profile: ${bad}`;
  const given = (k: string) => v[k] !== undefined && v[k] !== null;
  const p: ReceiptProfile = { ...DEFAULT_PROFILE };
  if (given("paper_mm")) {
    if (v.paper_mm !== 58 && v.paper_mm !== 80) return "paper_mm must be 58 or 80";
    p.paper_mm = v.paper_mm;
    p.width_dots = v.paper_mm === 58 ? 384 : 576;
  }
  if (given("width_dots")) {
    const max = p.paper_mm * 8;
    const w = v.width_dots;
    if (typeof w !== "number" || !Number.isInteger(w) || w % 8 !== 0 || w < 192 || w > max) {
      return `width_dots must be a multiple of 8 from 192 to ${max}`;
    }
    p.width_dots = w;
  }
  if (given("codepage")) {
    if (v.codepage !== "raster" && v.codepage !== "cp1252") return "codepage must be raster or cp1252";
    p.codepage = v.codepage;
  }
  if (given("codepage_number")) {
    const n = v.codepage_number;
    if (typeof n !== "number" || !Number.isInteger(n) || n < 0 || n > 255) return "codepage_number must be a whole number from 0 to 255";
    p.codepage_number = n;
  }
  if (given("cut")) {
    if (v.cut !== "partial" && v.cut !== "full" && v.cut !== "none") return "cut must be partial, full or none";
    p.cut = v.cut;
  }
  if (given("output")) {
    if (v.output !== "auto" && v.output !== "escpos") return "output must be auto or escpos";
    p.output = v.output;
  }
  return p;
}

/** The pre-rendered picture of the sample for this paper. */
export const sampleAsset = (p: ReceiptProfile): string => `/receipt-sample-${p.paper_mm}.png`;

/** The sample receipt gets its picture, anything else the note. */
export function previewOf(receipt: unknown, p: ReceiptProfile): { status: 200; asset: string } | { status: 400; error: string } {
  return sameJson(receipt, sample)
    ? { status: 200, asset: sampleAsset(p) }
    : { status: 400, error: DEMO_ONLY_SAMPLE };
}
