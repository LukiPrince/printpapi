// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The demo's starting state: about six hours of job history for the fictional shop, ending at #1291.
import type { ApiKey, JobState, LabelTemplate, OrgUser } from "../lib/api";
import { SHOP, rng } from "./data.ts";

export type JobKind = "label" | "slip" | "receipt" | "invoice" | "test" | "doc";
export type PaperKind = "label" | "page" | "receipt" | "file";

export type DemoAgent = { id: number; name: string; os: string; online: boolean; last_seen_at: number; created_at: number };
export type DemoPrinter = { id: number; name: string; agent_id: number; can_pdf: boolean; paper: PaperKind };
export type DemoJob = {
  id: number;
  printer_id: number;
  title: string;
  type: string;
  kind: JobKind;
  ref: number | null;
  file: string | null;
  copies: number;
  state: JobState;
  error: string | null;
  created_at: number;
  finished_at: number | null;
  claim_at: number | null;
  done_at: number | null;
};
export type Todo = { test: boolean; history: boolean; order: boolean; cancel: boolean };
export type DemoState = {
  v: 1;
  agents: DemoAgent[];
  printers: DemoPrinter[];
  jobs: DemoJob[]; // newest first
  outputs: number[]; // job ids printed, oldest first, max 8
  keys: ApiKey[];
  users: OrgUser[];
  labels: LabelTemplate[]; // label templates, ids ascending
  org: { id: 1; name: string; event_url: string | null; shopify_secret_set: boolean; job_quota: number; month_base: number; created_at: number };
  me: { user_id: number; email: string };
  next: { job: number; order: number; receipt: number; key: number; user: number; label: number };
  // counts of jobs trim() dropped from `jobs`, so the totals keep rising; since_seed = those above SEED_LAST_JOB, not cancelled
  trimmed: { done: number; failed: number; cancelled: number; since_seed: number };
  traffic: boolean;
  traffic_next_at: number;
  todo: Todo;
};

// Older jobs that are no longer in GET /jobs, so the metrics page has realistic totals.
export const BASE_COUNTS = { done: 1187, failed: 6, cancelled: 11 };

const DAY = 86400;

// The two templates a visitor finds on the Labels page: the 4x6 shipping label of the demo shop
// and a two-line error-code label. Sizes are catalogue entries (web/lib/label-sizes.ts).
export function seedLabels(t: number): LabelTemplate[] {
  return [
    {
      id: 1,
      name: "error-code-1x0.5",
      content: "text",
      label_mm: { w: 25.4, h: 12.7 },
      dpi: 203,
      rotate: 0,
      crop_percent: { t: 0, r: 0, b: 0, l: 0 },
      place_mm: { x: 1.3, y: 1, w: 22.8, h: 10.7 },
      text: { wrap_chars: 12, lines_per_label: 2 },
      zpl_darkness: 30,
      size_ref: "Standard:1x0.5",
      created_at: t - DAY * 20,
      updated_at: t - DAY * 20,
    },
    {
      id: 2,
      name: "shipping-4x6",
      content: "image",
      label_mm: { w: 101.6, h: 152.4 },
      dpi: 203,
      rotate: 90,
      crop_percent: { t: 0, r: 0, b: 14.3, l: 0 },
      place_mm: { x: 0.8, y: 1.3, w: 100.1, h: 149.8 },
      text: { wrap_chars: 0, lines_per_label: 0 },
      zpl_darkness: 30,
      size_ref: "Standard:4x6",
      created_at: t - DAY * 33,
      updated_at: t - DAY * 33,
    },
  ];
}

type Raw = Pick<DemoJob, "printer_id" | "kind" | "ref" | "title" | "type" | "state" | "created_at"> & {
  error?: string;
  file?: string;
};

export function seedState(t: number): DemoState {
  const agents: DemoAgent[] = [
    { id: 1, name: "warehouse-pc", os: "Windows 11", online: true, last_seen_at: t - 4, created_at: t - DAY * 120 },
    { id: 2, name: "front-desk-mac", os: "macOS 15", online: true, last_seen_at: t - 9, created_at: t - DAY * 64 },
    { id: 3, name: "packing-station-2", os: "Windows 10", online: false, last_seen_at: t - 3 * 3600 - 640, created_at: t - DAY * 90 },
    { id: 4, name: "archive-nas", os: "Debian 13", online: true, last_seen_at: t - 2, created_at: t - DAY * 33 },
  ];
  const printers: DemoPrinter[] = [
    { id: 1, name: "Zebra GK420d", agent_id: 1, can_pdf: false, paper: "label" },
    { id: 2, name: "Brother QL-820NWB", agent_id: 1, can_pdf: false, paper: "label" },
    { id: 3, name: "HP LaserJet Pro M404", agent_id: 1, can_pdf: true, paper: "page" },
    { id: 4, name: "Epson TM-m30III", agent_id: 2, can_pdf: false, paper: "receipt" },
    { id: 5, name: "Canon PIXMA G650", agent_id: 2, can_pdf: true, paper: "page" },
    { id: 6, name: "Zebra ZD421", agent_id: 3, can_pdf: false, paper: "label" },
    { id: 7, name: "Invoice archive (file://)", agent_id: 4, can_pdf: true, paper: "file" },
  ];

  const r = rng(20261031);
  const raw: Raw[] = [];
  let order = 10461;
  let rc = 2270;
  let inv = 790;
  for (let ts = t - 6.5 * 3600; ts < t - 300; ts += 380 + r() * 820) {
    const roll = r();
    if (roll < 0.58) {
      const n = order++;
      const off = ts < t - 3.3 * 3600 && r() < 0.45; // before it went offline, station 2 packed some orders
      raw.push({ printer_id: 3, kind: "slip", ref: n, title: `Packing slip · order #${n}`, type: "pdf_base64", state: "done", created_at: ts });
      raw.push({ printer_id: off ? 6 : 1, kind: "label", ref: n, title: `Shipping label · order #${n}`, type: "raw_base64", state: "done", created_at: ts + 1.4 });
    } else if (roll < 0.82) {
      const n = rc++;
      raw.push({ printer_id: 4, kind: "receipt", ref: n, title: `Receipt #A-${n}`, type: "raw_base64", state: "done", created_at: ts });
    } else {
      const n = inv++;
      raw.push({ printer_id: 7, kind: "invoice", ref: n, title: `Invoice RE-2026-0${n}`, type: "pdf_uri", file: `RE-2026-0${n}.pdf`, state: "done", created_at: ts });
    }
  }
  // the interesting cases
  raw.push({ printer_id: 6, kind: "label", ref: 10474, title: "Shipping label · order #10474", type: "raw_base64", state: "failed", error: "expired", created_at: t - 2.9 * 3600 });
  raw.push({ printer_id: 6, kind: "label", ref: 10466, title: "Return label · order #10466", type: "raw_base64", state: "queued", created_at: t - 1.8 * 3600 });
  raw.push({ printer_id: 7, kind: "invoice", ref: 799, title: "Invoice RE-2026-0799", type: "pdf_uri", file: "RE-2026-0799.pdf", state: "failed", error: "fetch https://shop.example/invoices/RE-2026-0799.pdf: 404 Not Found", created_at: t - 1.2 * 3600 });
  raw.push({ printer_id: 3, kind: "slip", ref: 10470, title: "Packing slip · order #10470 (reprint)", type: "pdf_base64", state: "cancelled", created_at: t - 2.2 * 3600 });
  raw.sort((a, b) => a.created_at - b.created_at);

  let id = 1291 - raw.length + 1;
  const jobs: DemoJob[] = raw
    .map((j): DemoJob => {
      const finished_at = j.state === "queued" ? null : j.created_at + 0.4 + r() * 1.8;
      // an expired job was never claimed; the 404 one was claimed, then failed
      const claimed = j.state === "done" || (j.state === "failed" && j.error !== "expired");
      return {
        copies: 1,
        error: null,
        file: null,
        ...j,
        id: id++,
        finished_at,
        claim_at: claimed ? j.created_at + 0.2 + r() * 0.2 : null,
        done_at: j.state === "done" ? finished_at : null,
      };
    })
    .reverse();
  const outputs = jobs
    .filter((j) => j.state === "done" && printers.find((p) => p.id === j.printer_id)!.paper !== "file")
    .slice(0, 4)
    .reverse()
    .map((j) => j.id);

  return {
    v: 1,
    agents,
    printers,
    jobs,
    outputs,
    keys: [
      { id: 1, label: "woocommerce-shop", active: 1, created_at: t - DAY * 61 },
      { id: 2, label: "agent · warehouse-pc", active: 1, created_at: t - DAY * 120 },
      { id: 3, label: "agent · front-desk-mac", active: 1, created_at: t - DAY * 64 },
      { id: 4, label: "n8n · labels + invoices", active: 1, created_at: t - DAY * 33 },
      { id: 5, label: "old-test-key", active: 0, created_at: t - DAY * 140 },
    ],
    users: [
      { id: 1, org_id: 1, email: "jana@nordlicht.example", created_at: t - DAY * 120 },
      { id: 2, org_id: 1, email: "lager@nordlicht.example", created_at: t - DAY * 58 },
    ],
    labels: seedLabels(t),
    org: { id: 1, name: SHOP.name, event_url: "https://hooks.nordlicht.example/printpapi", shopify_secret_set: false, job_quota: 2000, month_base: 812, created_at: t - DAY * 120 },
    me: { user_id: 1, email: "jana@nordlicht.example" },
    next: { job: id, order, receipt: rc, key: 6, user: 3, label: 3 },
    trimmed: { done: 0, failed: 0, cancelled: 0, since_seed: 0 },
    traffic: true,
    traffic_next_at: t + 13,
    todo: { test: false, history: false, order: false, cancel: false },
  };
}
