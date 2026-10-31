// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The dashboard's API, answered inside the browser. Pure: the caller owns the state and the clock,
// and the browser glue (fetch patch, storage) lives in store.ts. Error texts are copied from
// crates/server/src/server.rs so the dashboard shows what the real server says.
import type { ApiKey, Computer, Job, Me, Org, OrgUser, Printer, ServerInfo } from "../lib/api";
import { BASE_COUNTS } from "./seed.ts";
import type { DemoState } from "./seed.ts";
import { advance, cancel, enqueue, mode, SEED_LAST_JOB } from "./sim.ts";

export type FakeRequest = { method: string; path: string; body: unknown };
export type FakeResponse = { status: number; json?: unknown; text?: string };

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
      mode: mode(j.type),
      error: j.error,
      created_at: j.created_at,
      finished_at: j.finished_at,
    };
  });
}

function postJob(c: Ctx): FakeResponse {
  const b = obj(c.body);
  if (!b || typeof b.printer_id !== "number") return err(400, "bad json or missing printer_id");
  if (!c.s.printers.some((p) => p.id === b.printer_id)) return notFound();
  const copies = b.copies ?? 1;
  if (typeof copies !== "number" || !Number.isInteger(copies) || copies < 1 || copies > COPIES_MAX) {
    return err(400, `copies must be an integer 1..${COPIES_MAX}`);
  }
  const title = typeof b.title === "string" && b.title ? b.title : "Print job";
  // `content` is dropped on purpose: a pasted PDF must not fill the visitor's sessionStorage
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

function issueKey(c: Ctx): FakeResponse {
  const label = String(obj(c.body)?.label ?? "").trim() || "unnamed key";
  const key: ApiKey = { id: c.s.next.key++, label, active: 1, created_at: c.now };
  c.s.keys.unshift(key);
  return ok({ id: key.id, label, org_id: 1, key: "pp_demo_" + hex32() });
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

const health: ServerInfo & { ok: boolean } = { ok: true, signup: "closed", password_reset: false };

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
  { method: "GET", re: /^\/metrics$/, run: (c) => metrics(c.s) },
  { method: "GET", re: /^\/apikeys$/, run: (c) => ok({ keys: c.s.keys }) },
  { method: "POST", re: /^\/apikeys$/, run: issueKey },
  { method: "DELETE", re: /^\/apikeys\/\d+$/, run: revokeKey },
  { method: "GET", re: /^\/users$/, run: (c) => ok({ users: c.s.users }) },
  { method: "POST", re: /^\/users$/, run: createUser },
  { method: "DELETE", re: /^\/users\/\d+$/, run: deleteUser },
  { method: "PUT", re: /^\/me\/password$/, run: () => ok() }, // the demo has no real password
  // the demo session is not root, like the real server's gate!(self.admin()) answers it
  { method: "GET", re: /^\/orgs$/, run: () => err(401, "unauthorized") },
  { method: "GET", re: /^\/orgs\/\d+$/, run: getOrg },
  { method: "PUT", re: /^\/orgs\/\d+$/, run: putOrg },
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
  const id = Number(/\/(\d+)$/.exec(req.path)?.[1] ?? 0);
  return route.run({ s, body: req.body, id, now });
}
