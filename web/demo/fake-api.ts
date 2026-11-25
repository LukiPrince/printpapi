// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The dashboard's API, answered inside the browser. Pure: the caller owns the state and the clock,
// and the browser glue (fetch patch, storage) lives in store.ts. Error texts are copied from
// crates/server/src/server.rs so the dashboard shows what the real server says.
import type { ApiKey, Computer, Job, LabelTemplate, Me, Org, OrgUser, PairingInfo, Printer, ServerInfo } from "../lib/api";
import { KEY_SCOPES, type KeyScope } from "../lib/key-scopes.ts";
import { BASE_COUNTS } from "./seed.ts";
import type { DemoState } from "./seed.ts";
import { advance, cancel, enqueue, mode, SEED_LAST_JOB } from "./sim.ts";
import { DEFAULT_PROFILE, parseProfile, previewOf, sampleAsset } from "./receipt-fake.ts";

export type FakeRequest = { method: string; path: string; body: unknown };
// `asset`: answer with this file of the demo (the pre-rendered sample receipt) instead of a body
export type FakeResponse = { status: number; json?: unknown; text?: string; asset?: string };

type Ctx = { s: DemoState; body: unknown; id: number; now: number };
type Route = { method: string; re: RegExp; run: (c: Ctx) => FakeResponse };

const JOBS_SHOWN = 50;
const COPIES_MAX = 100;

const ok = (json: unknown = { ok: true }): FakeResponse => ({ status: 200, json });
const err = (status: number, error: string): FakeResponse => ({ status, json: { error } });
const notFound = () => err(404, "not found");
const obj = (b: unknown): Record<string, unknown> | null => (b && typeof b === "object" && !Array.isArray(b) ? (b as Record<string, unknown>) : null);

function printerOut(s: DemoState): Printer[] {
  return s.printers.map((p) => {
    const a = s.agents.find((x) => x.id === p.agent_id);
    return { id: p.id, name: p.name, agent_id: p.agent_id, agent_name: a?.name ?? "", can_pdf: p.can_pdf, online: a?.online === true };
  });
}

function jobsOut(s: DemoState): Job[] {
  return s.jobs.slice(0, JOBS_SHOWN).map((j) => {
    const p = s.printers.find((x) => x.id === j.printer_id);
    const a = s.agents.find((x) => x.id === p?.agent_id);
    return {
      id: j.id,
      printer_id: j.printer_id,
      printer_name: p?.name ?? "",
      agent_name: a?.name ?? "",
      title: j.title,
      state: j.state,
      type: j.type,
      mode: mode(j.type, p?.can_pdf, s.receipt_profiles?.[j.printer_id]?.output === "escpos"),
      error: j.error,
      created_at: j.created_at,
      finished_at: j.finished_at,
    };
  });
}

const LABEL_JOBS = ["label_uri", "label_base64", "label_text"];

// The label_* checks of crates/server/src/dispatch.rs (parse_label_job) and server.rs
// (label_payload), in the server's order. The demo renders nothing, so it stops at the checks.
function labelJob(c: Ctx, b: Record<string, unknown>): FakeResponse | null {
  const t = b.template;
  if (t == null) return err(400, "missing 'template'");
  if (!((typeof t === "string" && t !== "") || (typeof t === "number" && Number.isInteger(t)))) return err(400, "template must be a name or an integer id");
  const isText = b.type === "label_text";
  if (isText) {
    if (b.text == null) return err(400, "missing 'text'");
    if (typeof b.text !== "string") return err(400, "text must be a string");
  }
  if (!c.s.printers.some((p) => p.id === b.printer_id)) return err(400, `unknown printer: ${String(b.printer_id)}`);
  const found = c.s.labels.find((l) => (typeof t === "string" ? l.name === t : l.id === t));
  if (!found) return err(400, `unknown label template: ${String(t)}`);
  if (found.content !== (isText ? "text" : "image")) return err(400, `label template ${found.name} does not take ${isText ? "text" : "image"} jobs`);
  return null;
}

function postJob(c: Ctx): FakeResponse {
  const b = obj(c.body);
  if (!b || typeof b.printer_id !== "number") return err(400, "bad json or missing printer_id");
  const isLabel = typeof b.type === "string" && LABEL_JOBS.includes(b.type);
  if (b.type === "receipt") {
    // the server's order: the receipt is checked before the printer is looked at
    if (b.receipt == null) return err(400, "missing 'receipt'");
    if (!c.s.printers.some((p) => p.id === b.printer_id)) return err(400, `unknown printer: ${String(b.printer_id)}`);
  } else if (!isLabel && !c.s.printers.some((p) => p.id === b.printer_id)) return notFound();
  const copies = b.copies ?? 1;
  if (typeof copies !== "number" || !Number.isInteger(copies) || copies < 1 || copies > COPIES_MAX) {
    return err(400, `copies must be an integer 1..${COPIES_MAX}`);
  }
  if (isLabel) {
    const refused = labelJob(c, b);
    if (refused) return refused;
  }
  const title = typeof b.title === "string" && b.title ? b.title : "Print job";
  // `content` and `receipt` are dropped on purpose: a pasted PDF must not fill the visitor's sessionStorage
  const job = enqueue(
    c.s,
    {
      printer_id: b.printer_id,
      title,
      type: typeof b.type === "string" ? b.type : "raw_base64",
      kind: title === "test page" || title === "test label" ? "test" : "doc",
      ref: null,
      file: title,
      copies,
    },
    c.now,
  );
  c.s.todo.test = true;
  return ok({ job_id: job.id });
}

// ---- receipts: the routes of crates/server/src/server.rs, same texts ----

const profileOf = (s: DemoState, id: number) => s.receipt_profiles?.[id] ?? DEFAULT_PROFILE;

function getReceiptProfile(c: Ctx): FakeResponse {
  if (!c.s.printers.some((p) => p.id === c.id)) return notFound();
  return ok({ printer_id: c.id, stored: c.s.receipt_profiles?.[c.id] !== undefined, profile: profileOf(c.s, c.id) });
}

function putReceiptProfile(c: Ctx): FakeResponse {
  const body = obj(c.body);
  if (!body) return err(400, "bad json");
  const profile = parseProfile(body);
  if (typeof profile === "string") return err(400, profile);
  if (!c.s.printers.some((p) => p.id === c.id)) return notFound();
  (c.s.receipt_profiles ??= {})[c.id] = profile;
  return ok({ printer_id: c.id, stored: true, profile });
}

function deleteReceiptProfile(c: Ctx): FakeResponse {
  if (!c.s.printers.some((p) => p.id === c.id)) return notFound();
  delete c.s.receipt_profiles?.[c.id];
  return ok();
}

function previewReceipt(c: Ctx): FakeResponse {
  const body = obj(c.body);
  if (!body) return err(400, "bad json");
  if (body.receipt == null) return err(400, "missing 'receipt'");
  const named = body.printer_id ?? null;
  const given = body.profile ?? null;
  if (named !== null && given !== null) return err(400, "send printer_id or profile, not both");
  let profile = DEFAULT_PROFILE;
  if (named !== null) {
    if (typeof named !== "number" || !c.s.printers.some((p) => p.id === named)) return err(400, `unknown printer: ${String(named)}`);
    profile = profileOf(c.s, named);
  } else if (given !== null) {
    const m = obj(given);
    if (!m) return err(400, "profile must be an object");
    const parsed = parseProfile(m);
    if (typeof parsed === "string") return err(400, parsed);
    profile = parsed;
  }
  const r = previewOf(body.receipt, profile);
  return r.status === 200 ? { status: 200, asset: r.asset } : err(400, r.error);
}

// GET /jobs/{id}/preview: the receipt of a job that was made as a receipt, drawn for its printer
function jobPreview(c: Ctx): FakeResponse {
  const j = c.s.jobs.find((x) => x.id === c.id);
  if (!j || j.type !== "receipt") return notFound();
  return { status: 200, asset: sampleAsset(profileOf(c.s, j.printer_id)) };
}

function metrics(s: DemoState): FakeResponse {
  const n = { queued: 0, claimed: 0, done: BASE_COUNTS.done + s.trimmed.done, failed: BASE_COUNTS.failed + s.trimmed.failed, cancelled: BASE_COUNTS.cancelled + s.trimmed.cancelled };
  for (const j of s.jobs) n[j.state]++;
  const lines = [
    ...(Object.keys(n) as (keyof typeof n)[]).map((k) => `printpapi_jobs{state="${k}"} ${n[k]}`),
    `printpapi_agents_online ${s.agents.filter((a) => a.online).length}`,
    `printpapi_agents_total ${s.agents.length}`,
    `printpapi_printers_total ${s.printers.length}`,
  ];
  return { status: 200, text: lines.join("\n") + "\n" };
}

function hex32(): string {
  let out = "";
  for (let i = 0; i < 32; i++) out += Math.floor(Math.random() * 16).toString(16);
  return out;
}

const SCOPE_WORDS = "scope must be one of: full, agent, cloudprnt, print, read";

/** The `scope` of a key request: one of the five words, `undefined` when absent or null. */
function scopeOf(body: Record<string, unknown> | null): KeyScope | undefined | "bad" {
  const v = body?.scope;
  if (v === undefined || v === null) return undefined;
  return KEY_SCOPES.some((s) => s.value === v) ? (v as KeyScope) : "bad";
}

function issueKey(c: Ctx): FakeResponse {
  const body = obj(c.body);
  const label = String(body?.label ?? "").trim() || "unnamed key";
  const scope = scopeOf(body);
  if (scope === "bad") return err(400, SCOPE_WORDS);
  const key: ApiKey = { id: c.s.next.key++, label, active: 1, created_at: c.now };
  // a full key carries no scope, like the server's answers
  if (scope && scope !== "full") key.scope = scope;
  c.s.keys.unshift(key);
  return ok({ id: key.id, label, org_id: 1, key: "pp_demo_" + hex32(), ...(key.scope ? { scope: key.scope } : {}) });
}

// --- pairing: the Devices page's Connect device. Nothing is ever spent in the demo: no machine
// redeems a code, so a code stays open until it runs out or is cancelled.
const PAIRING_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const PAIRING_TTL = 1800;

/** A code that looks like the server's, made from the clock and a counter (the demo has no secrets). */
function demoCode(now: number, n: number): string {
  let x = (Math.floor(now) * 2654435761 + n * 40503) >>> 0;
  let out = "";
  for (let i = 0; i < 12; i++) {
    x = (Math.imul(x, 1664525) + 1013904223) >>> 0;
    out += PAIRING_ALPHABET[(x >>> 27) & 31];
    if (i % 4 === 3 && i < 11) out += "-";
  }
  return out;
}

function pairingsOf(s: DemoState): PairingInfo[] {
  return (s.pairings ??= []);
}

function createPairing(c: Ctx): FakeResponse {
  const b = obj(c.body);
  const list = pairingsOf(c.s);
  let device: { id: number; name: string } | null = null;
  const id = b?.computer_id;
  if (id !== undefined && id !== null) {
    if (typeof id !== "number" || !Number.isInteger(id)) return err(400, "computer_id must be a number");
    const a = c.s.agents.find((x) => x.id === id);
    if (!a) return notFound();
    device = { id: a.id, name: a.name };
  }
  // the server's limit: 20 unused codes
  if (list.filter((p) => p.state === "open" && p.expires_at > c.now).length >= 20) return err(429, "too many open pairing codes");
  if (device) {
    // a newer code of a device replaces its unused ones
    c.s.pairings = list.filter((p) => !(p.state === "open" && p.computer_id === device.id));
  }
  c.s.next.pairing ??= 1;
  const row: PairingInfo = {
    id: c.s.next.pairing++,
    org_id: 1,
    computer_id: device?.id ?? null,
    computer_name: device?.name ?? null,
    created_at: c.now,
    expires_at: c.now + PAIRING_TTL,
    used_at: null,
    device_name: null,
    key_id: null,
    state: "open",
    old_key: null,
  };
  pairingsOf(c.s).unshift(row);
  return ok({
    id: row.id,
    code: demoCode(c.now, row.id),
    expires_at: row.expires_at,
    server_url: null,
    computer_id: row.computer_id,
    computer_name: row.computer_name,
    old_key: null,
  });
}

function listPairings(c: Ctx): FakeResponse {
  const codes = pairingsOf(c.s).map((p) => (p.state === "open" && p.expires_at <= c.now ? { ...p, state: "expired" as const } : p));
  return ok({ codes: codes.slice(0, 100) });
}

function cancelPairing(c: Ctx): FakeResponse {
  const list = pairingsOf(c.s);
  const i = list.findIndex((p) => p.id === c.id && p.state === "open");
  if (i < 0) return notFound();
  list.splice(i, 1);
  return ok();
}

function setKeyScope(c: Ctx): FakeResponse {
  const scope = scopeOf(obj(c.body));
  if (scope === undefined || scope === "bad") return err(400, SCOPE_WORDS);
  const k = c.s.keys.find((x) => x.id === c.id);
  if (!k || !k.active) return notFound();
  if (scope === "full") delete k.scope;
  else k.scope = scope;
  return ok({ ok: true, scope });
}

function revokeKey(c: Ctx): FakeResponse {
  const k = c.s.keys.find((x) => x.id === c.id);
  if (!k) return notFound();
  k.active = 0;
  return ok();
}

function createUser(c: Ctx): FakeResponse {
  const b = obj(c.body);
  const email = typeof b?.email === "string" ? b.email.trim().toLowerCase() : "";
  if (!email.includes("@")) return err(400, "email required");
  if (c.s.users.some((u) => u.email === email)) return err(409, "email already registered");
  const u: OrgUser = { id: c.s.next.user++, org_id: 1, email, created_at: c.now };
  c.s.users.push(u);
  return ok({ id: u.id, email, org_id: 1 });
}

function deleteUser(c: Ctx): FakeResponse {
  if (c.id === c.s.me.user_id) return err(400, "cannot remove your own account");
  const i = c.s.users.findIndex((u) => u.id === c.id);
  if (i < 0) return notFound();
  if (c.s.users.length <= 1) return err(400, "an org must keep at least one account");
  c.s.users.splice(i, 1);
  return ok();
}

function getOrg(c: Ctx): FakeResponse {
  if (c.id !== 1) return notFound();
  const o = c.s.org;
  const org: Org = {
    id: 1,
    name: o.name,
    event_url: o.event_url,
    shopify_secret_set: o.shopify_secret_set,
    job_quota: o.job_quota,
    plan: null,
    jobs_this_month: o.month_base + c.s.trimmed.since_seed + c.s.jobs.filter((j) => j.id > SEED_LAST_JOB && j.state !== "cancelled").length,
    created_at: o.created_at,
  };
  return ok(org);
}

function putOrg(c: Ctx): FakeResponse {
  if (c.id !== 1) return notFound();
  const b = obj(c.body) ?? {};
  if ("job_quota" in b) return err(403, "job_quota is set by the server operator");
  const applied: Record<string, unknown> = {};
  if ("event_url" in b && (typeof b.event_url === "string" || b.event_url === null)) {
    c.s.org.event_url = b.event_url?.trim() || null;
    applied.event_url = c.s.org.event_url;
  }
  if ("shopify_secret" in b && (typeof b.shopify_secret === "string" || b.shopify_secret === null)) {
    c.s.org.shopify_secret_set = !!b.shopify_secret;
    applied.shopify_secret_set = c.s.org.shopify_secret_set;
  }
  if (Object.keys(applied).length === 0) return err(400, "nothing to update");
  return ok({ ok: true, ...applied });
}

// ---- label templates: the validation of crates/server/src/label.rs (parse_spec), same texts ----

type Obj = Record<string, unknown>;
type Spec = Omit<LabelTemplate, "id" | "created_at" | "updated_at">;
const MAX_DOTS = 8000;
const LABEL_NAME_TAKEN = "label template name already in use";
const dotsOf = (mm: number, dpi: number) => (mm / 25.4) * dpi;

class Refused extends Error {}
const refuse = (m: string): never => {
  throw new Refused(m);
};
function onlyKeys(m: Obj, allowed: string[], what: string): void {
  const k = Object.keys(m).find((x) => !allowed.includes(x));
  if (k !== undefined) refuse(`unknown field in ${what}: ${k}`);
}
function getObj(m: Obj, key: string): Obj {
  if (!(key in m)) refuse(`${key} is required`);
  return obj(m[key]) ?? refuse(`${key} must be an object`);
}
function getNum(m: Obj, key: string, what: string): number {
  if (!(key in m)) refuse(`${what}.${key} is required`);
  const v = m[key];
  return typeof v === "number" && Number.isFinite(v) ? v : refuse(`${what}.${key} must be a number`);
}
function getInt(m: Obj, key: string, what: string): number {
  if (!(key in m)) refuse(`${what} is required`);
  const v = m[key];
  return typeof v === "number" && Number.isInteger(v) ? v : refuse(`${what} must be an integer`);
}

// The template in the server's canonical form (label::spec_json): text always, the optional
// fields only when set.
function parseSpec(v: Obj): Spec {
  onlyKeys(v, ["name", "content", "label_mm", "dpi", "rotate", "crop_percent", "place_mm", "text", "zpl_darkness", "size_ref"], "template");
  if (!("name" in v)) refuse("name is required");
  // valid_option_value in dispatch.rs: 1-64 chars of letters, digits, space and ._-
  const name = typeof v.name === "string" && /^[\p{Alphabetic}\p{N}_. -]{1,64}$/u.test(v.name) ? v.name : refuse("name must be 1-64 chars of letters/digits/space/._-");
  if (!("content" in v)) refuse("content is required");
  const content = v.content === "image" || v.content === "text" ? v.content : refuse('content must be "image" or "text"');

  const dpi = getInt(v, "dpi", "dpi");
  if (dpi < 100 || dpi > 600) refuse("dpi must be 100-600");
  const rotate = getInt(v, "rotate", "rotate");
  if (rotate !== 0 && rotate !== 90 && rotate !== 180 && rotate !== 270) refuse("rotate must be 0, 90, 180 or 270");

  const lm = getObj(v, "label_mm");
  onlyKeys(lm, ["w", "h"], "label_mm");
  const label_mm = { w: getNum(lm, "w", "label_mm"), h: getNum(lm, "h", "label_mm") };
  for (const axis of ["w", "h"] as const) {
    const d = Math.round(dotsOf(label_mm[axis], dpi));
    if (!(label_mm[axis] > 0 && d >= 1 && d <= MAX_DOTS)) refuse(`label_mm.${axis} must be 1-${MAX_DOTS} dots at ${dpi} dpi`);
  }

  const cp = getObj(v, "crop_percent");
  onlyKeys(cp, ["t", "r", "b", "l"], "crop_percent");
  const crop_percent = { t: getNum(cp, "t", "crop_percent"), r: getNum(cp, "r", "crop_percent"), b: getNum(cp, "b", "crop_percent"), l: getNum(cp, "l", "crop_percent") };
  if (!Object.values(crop_percent).every((x) => x >= 0 && x <= 95)) refuse("crop_percent values must be 0-95");
  if (crop_percent.t + crop_percent.b >= 100 || crop_percent.l + crop_percent.r >= 100) refuse("crop_percent leaves nothing of the image");

  const pm = getObj(v, "place_mm");
  onlyKeys(pm, ["x", "y", "w", "h"], "place_mm");
  const place_mm = { x: getNum(pm, "x", "place_mm"), y: getNum(pm, "y", "place_mm"), w: getNum(pm, "w", "place_mm"), h: getNum(pm, "h", "place_mm") };
  if (place_mm.w <= 0 || place_mm.h <= 0) refuse("place_mm.w and place_mm.h must be greater than 0");
  const pw = Math.max(1, Math.round(dotsOf(place_mm.w, dpi)));
  const ph = Math.max(1, Math.round(dotsOf(place_mm.h, dpi)));
  if (pw > 2 * MAX_DOTS || ph > 2 * MAX_DOTS || pw * ph > 64_000_000) refuse("place_mm is too large");
  if (Math.abs(dotsOf(place_mm.x, dpi)) > 1e6 || Math.abs(dotsOf(place_mm.y, dpi)) > 1e6) refuse("place_mm.x and place_mm.y are too far from the label");

  const text = { wrap_chars: 0, lines_per_label: 0 };
  if ("text" in v) {
    const t = obj(v.text) ?? refuse("text must be an object");
    onlyKeys(t, ["wrap_chars", "lines_per_label"], "text");
    for (const key of ["wrap_chars", "lines_per_label"] as const) {
      if (key in t) {
        const n = getInt(t, key, `text.${key}`);
        if (n < 0 || n > 200) refuse(`text.${key} must be 0-200`);
        text[key] = n;
      }
    }
  }

  const out: Spec = { name, content, label_mm, dpi, rotate: rotate as Spec["rotate"], crop_percent, place_mm, text };
  if ("zpl_darkness" in v) {
    const n = getInt(v, "zpl_darkness", "zpl_darkness");
    if (n < -30 || n > 30) refuse("zpl_darkness must be -30 to 30");
    out.zpl_darkness = n;
  }
  if ("size_ref" in v) {
    const r = v.size_ref;
    if (typeof r !== "string" || [...r].length > 64) refuse("size_ref must be a string of at most 64 chars");
    out.size_ref = r as string;
  }
  return out;
}

// ORDER BY name COLLATE NOCASE, id: SQLite's NOCASE folds ASCII only.
const nocase = (n: string) => n.replace(/[A-Z]/g, (ch) => ch.toLowerCase());
function listLabels(c: Ctx): FakeResponse {
  const sorted = [...c.s.labels].sort((a, b) => {
    const x = nocase(a.name);
    const y = nocase(b.name);
    return x < y ? -1 : x > y ? 1 : a.id - b.id;
  });
  return ok({ templates: sorted });
}

function saveLabel(c: Ctx, id: number | null): FakeResponse {
  const body = obj(c.body);
  if (!body) return err(400, "bad json");
  let spec: Spec;
  try {
    spec = parseSpec(body);
  } catch (e) {
    if (e instanceof Refused) return err(400, e.message);
    throw e;
  }
  const existing = id === null ? undefined : c.s.labels.find((l) => l.id === id);
  if (id !== null && !existing) return notFound();
  if (c.s.labels.some((l) => l.name === spec.name && l.id !== id)) return err(409, LABEL_NAME_TAKEN);
  if (existing) {
    // a replaced template keeps its id and creation time, the contract fields are all new
    const t: LabelTemplate = { id: existing.id, ...spec, created_at: existing.created_at, updated_at: c.now };
    c.s.labels[c.s.labels.indexOf(existing)] = t;
    return ok(t);
  }
  const t: LabelTemplate = { id: c.s.next.label++, ...spec, created_at: c.now, updated_at: c.now };
  c.s.labels.push(t);
  return ok(t);
}

function deleteLabel(c: Ctx): FakeResponse {
  const i = c.s.labels.findIndex((l) => l.id === c.id);
  if (i < 0) return notFound();
  c.s.labels.splice(i, 1);
  return ok();
}

const health: ServerInfo & { ok: boolean } ={ ok: true, signup: "closed", password_reset: false };

// Exact patterns: Next's own requests (/users/index.txt?_rsc=..., /_next/...) never match.
const ROUTES: Route[] = [
  { method: "GET", re: /^\/health$/, run: () => ok(health) },
  {
    method: "GET",
    re: /^\/me$/,
    run: (c) => ok({ kind: "session", org_id: 1, email: c.s.me.email, user_id: c.s.me.user_id } satisfies Me),
  },
  { method: "GET", re: /^\/printers$/, run: (c) => ok({ printers: printerOut(c.s) }) },
  {
    method: "GET",
    re: /^\/computers$/,
    run: (c) =>
      ok({
        computers: c.s.agents.map(
          (a): Computer => ({ id: a.id, name: a.name, online: a.online, last_seen_at: a.last_seen_at, created_at: a.created_at, printers: c.s.printers.filter((p) => p.agent_id === a.id).length }),
        ),
      }),
  },
  { method: "GET", re: /^\/jobs$/, run: (c) => ok({ jobs: jobsOut(c.s) }) },
  { method: "POST", re: /^\/jobs$/, run: postJob },
  {
    method: "DELETE",
    re: /^\/jobs\/\d+$/,
    run: (c) => {
      const r = cancel(c.s, c.id, c.now);
      if (r === "not-found") return err(404, "no job");
      if (r === "not-cancellable") return err(409, "job not cancellable (already claimed or finished)");
      c.s.todo.cancel = true;
      return ok();
    },
  },
  { method: "POST", re: /^\/receipts\/preview$/, run: previewReceipt },
  { method: "GET", re: /^\/printers\/\d+\/receipt-profile$/, run: getReceiptProfile },
  { method: "PUT", re: /^\/printers\/\d+\/receipt-profile$/, run: putReceiptProfile },
  { method: "DELETE", re: /^\/printers\/\d+\/receipt-profile$/, run: deleteReceiptProfile },
  { method: "GET", re: /^\/jobs\/\d+\/preview$/, run: jobPreview },
  { method: "GET", re: /^\/labels$/, run: listLabels },
  { method: "POST", re: /^\/labels$/, run: (c) => saveLabel(c, null) },
  { method: "PUT", re: /^\/labels\/\d+$/, run: (c) => saveLabel(c, c.id) },
  { method: "DELETE", re: /^\/labels\/\d+$/, run: deleteLabel },
  { method: "GET", re: /^\/metrics$/, run: (c) => metrics(c.s) },
  { method: "GET", re: /^\/apikeys$/, run: (c) => ok({ keys: c.s.keys }) },
  { method: "POST", re: /^\/apikeys$/, run: issueKey },
  { method: "PUT", re: /^\/apikeys\/\d+$/, run: setKeyScope },
  { method: "DELETE", re: /^\/apikeys\/\d+$/, run: revokeKey },
  { method: "POST", re: /^\/pairing$/, run: createPairing },
  { method: "GET", re: /^\/pairing$/, run: listPairings },
  { method: "DELETE", re: /^\/pairing\/\d+$/, run: cancelPairing },
  { method: "GET", re: /^\/users$/, run: (c) => ok({ users: c.s.users }) },
  { method: "POST", re: /^\/users$/, run: createUser },
  { method: "DELETE", re: /^\/users\/\d+$/, run: deleteUser },
  { method: "PUT", re: /^\/me\/password$/, run: () => ok() }, // the demo has no real password
  // the demo session is not root, like the real server's gate!(self.admin()) answers it
  { method: "GET", re: /^\/orgs$/, run: () => err(401, "unauthorized") },
  { method: "GET", re: /^\/orgs\/\d+$/, run: getOrg },
  { method: "PUT", re: /^\/orgs\/\d+$/, run: putOrg },
  { method: "GET", re: /^\/support-access$/, run: () => ok({ enabled: false, grants: [] }) },
  { method: "GET", re: /^\/plans$/, run: () => ok({ plans: [], current: null }) },
  { method: "POST", re: /^\/logout$/, run: () => ok() },
];

export function isFakeRoute(method: string, path: string): boolean {
  return ROUTES.some((r) => r.method === method && r.re.test(path));
}

export function handle(s: DemoState, req: FakeRequest, now: number): FakeResponse {
  advance(s, now);
  const route = ROUTES.find((r) => r.method === req.method && r.re.test(req.path));
  if (!route) return notFound();
  // the first number in the path: /jobs/12, /printers/4/receipt-profile, /jobs/12/preview
  const id = Number(/\/(\d+)(?=\/|$)/.exec(req.path)?.[1] ?? 0);
  return route.run({ s, body: req.body, id, now });
}
