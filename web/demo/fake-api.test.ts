// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import type { FakeResponse } from "./fake-api.ts";
import { handle, isFakeRoute } from "./fake-api.ts";
import { seedState } from "./seed.ts";

const now = 1_793_400_000;
// lib/api.ts cannot be imported under node --test (ApiError uses parameter properties), so the
// response bodies are read as loose JSON here; tsc checks the real shapes in fake-api.ts.
type J = any; // eslint-disable-line @typescript-eslint/no-explicit-any
const call = (s: ReturnType<typeof seedState>, method: string, path: string, body: unknown = undefined, t = now): FakeResponse =>
  handle(s, { method, path, body }, t);
const json = (r: FakeResponse): J => r.json;

test("isFakeRoute matches the exact API routes only", () => {
  assert.equal(isFakeRoute("GET", "/users"), true);
  assert.equal(isFakeRoute("GET", "/users/"), false);
  assert.equal(isFakeRoute("GET", "/users/index.txt"), false);
  assert.equal(isFakeRoute("GET", "/history/index.txt"), false);
  assert.equal(isFakeRoute("GET", "/_next/static/x.js"), false);
  assert.equal(isFakeRoute("DELETE", "/jobs/12"), true);
  assert.equal(isFakeRoute("GET", "/jobs/abc"), false);
  assert.equal(isFakeRoute("PATCH", "/jobs"), false);
});

test("GET /health", () => {
  const r = call(seedState(now), "GET", "/health");
  assert.equal(r.status, 200);
  assert.deepEqual(r.json, { ok: true, signup: "closed", password_reset: false });
});

test("GET /me", () => {
  assert.deepEqual(call(seedState(now), "GET", "/me").json, { kind: "session", org_id: 1, email: "jana@nordlicht.example", user_id: 1 });
});

test("GET /printers carries online and agent_name", () => {
  const { printers } = json(call(seedState(now), "GET", "/printers"));
  assert.equal(printers.length, 7);
  assert.equal(printers[0].agent_name, "warehouse-pc");
  assert.equal(printers[0].online, true);
  assert.equal(printers[5].online, false);
  assert.equal(printers[2].can_pdf, true);
});

test("GET /computers counts printers", () => {
  const { computers } = json(call(seedState(now), "GET", "/computers"));
  assert.equal(computers.length, 4);
  assert.equal(computers[0].printers, 3);
  assert.equal(computers[2].online, false);
});

test("GET /jobs lists 50, fills names and mode, strips internals", () => {
  const { jobs } = json(call(seedState(now), "GET", "/jobs"));
  assert.equal(jobs.length, 50);
  assert.equal(jobs[0].id, 1291);
  assert.ok(jobs[0].printer_name && jobs[0].agent_name);
  assert.ok(jobs[0].mode === "pdf" || jobs[0].mode === "raw");
  for (const k of ["claim_at", "done_at", "kind", "ref", "file", "copies"]) assert.equal(k in jobs[0], false, k);
});

test("POST /jobs validates", () => {
  const s = seedState(now);
  assert.deepEqual(call(s, "POST", "/jobs", undefined).json, { error: "bad json or missing printer_id" });
  assert.equal(call(s, "POST", "/jobs", {}).status, 400);
  const nf = call(s, "POST", "/jobs", { printer_id: 99 });
  assert.equal(nf.status, 404);
  assert.deepEqual(nf.json, { error: "not found" });
  for (const copies of [0, 101, 1.5, "2"]) {
    const r = call(s, "POST", "/jobs", { printer_id: 1, copies });
    assert.equal(r.status, 400, String(copies));
    assert.deepEqual(r.json, { error: "copies must be an integer 1..100" });
  }
});

test("POST /jobs enqueues, marks the test todo and drops content", () => {
  const s = seedState(now);
  const r = call(s, "POST", "/jobs", { printer_id: 1, type: "raw_base64", title: "test label", content: "A".repeat(2_000_000) });
  assert.equal(r.status, 200);
  assert.deepEqual(r.json, { job_id: 1292 });
  assert.equal(s.jobs[0].kind, "test");
  assert.equal(s.jobs[0].file, "test label");
  assert.equal("content" in s.jobs[0], false);
  assert.equal(s.todo.test, true);
  assert.ok(JSON.stringify(s).length < 200_000);
  const other = call(s, "POST", "/jobs", { printer_id: 3, title: "invoice.pdf" });
  assert.equal(s.jobs[0].kind, "doc");
  assert.equal(json(other).job_id, 1293);
});

test("a posted job is done ten seconds later", () => {
  const s = seedState(now);
  const { job_id } = json(call(s, "POST", "/jobs", { printer_id: 1, title: "test label" }));
  const { jobs } = json(call(s, "GET", "/jobs", undefined, now + 10));
  assert.equal(jobs.find((j: J) => j.id === job_id).state, "done");
});

test("DELETE /jobs/{id}", () => {
  const s = seedState(now);
  const queued = s.jobs.find((j) => j.state === "queued")!;
  const r = call(s, "DELETE", `/jobs/${queued.id}`);
  assert.deepEqual([r.status, r.json], [200, { ok: true }]);
  assert.equal(s.todo.cancel, true);
  const again = call(s, "DELETE", `/jobs/${queued.id}`);
  assert.equal(again.status, 409);
  assert.deepEqual(again.json, { error: "job not cancellable (already claimed or finished)" });
  const gone = call(s, "DELETE", "/jobs/99999");
  assert.equal(gone.status, 404);
  assert.deepEqual(gone.json, { error: "no job" });
});

test("GET /metrics is Prometheus text", () => {
  const s = seedState(now);
  const t = call(s, "GET", "/metrics").text!;
  for (const st of ["queued", "claimed", "done", "failed", "cancelled"]) assert.match(t, new RegExp(`^printpapi_jobs\\{state="${st}"\\} \\d+$`, "m"));
  assert.match(t, /^printpapi_jobs\{state="queued"\} 1$/m);
  assert.match(t, /^printpapi_agents_online 3$/m);
  assert.match(t, /^printpapi_agents_total 4$/m);
  assert.match(t, /^printpapi_printers_total 7$/m);
  const done = Number(/state="done"\} (\d+)/.exec(t)![1]);
  assert.ok(done > 1187);
});

test("api keys: list, issue, revoke", () => {
  const s = seedState(now);
  assert.equal(json(call(s, "GET", "/apikeys")).keys.length, 5);
  const r = json(call(s, "POST", "/apikeys", { label: "ci" }));
  assert.equal(r.label, "ci");
  assert.equal(r.org_id, 1);
  assert.match(r.key, /^pp_demo_[0-9a-f]{32}$/);
  assert.equal(json(call(s, "GET", "/apikeys")).keys.length, 6);
  assert.equal(call(s, "DELETE", `/apikeys/${r.id}`).status, 200);
  assert.equal(s.keys.find((k) => k.id === r.id)!.active, 0);
  const nf = call(s, "DELETE", "/apikeys/999");
  assert.deepEqual([nf.status, nf.json], [404, { error: "not found" }]);
});

test("users: list, create, duplicates, deleting", () => {
  const s = seedState(now);
  assert.equal(json(call(s, "GET", "/users")).users.length, 2);
  const noMail = call(s, "POST", "/users", { email: "nope", password: "x" });
  assert.deepEqual([noMail.status, noMail.json], [400, { error: "email required" }]);
  const dup = call(s, "POST", "/users", { email: "Lager@Nordlicht.example", password: "x" });
  assert.deepEqual([dup.status, dup.json], [409, { error: "email already registered" }]);
  const made = json(call(s, "POST", "/users", { email: "neu@nordlicht.example", password: "x" }));
  assert.equal(made.email, "neu@nordlicht.example");
  assert.equal(call(s, "DELETE", `/users/${made.id}`).status, 200);
  const own = call(s, "DELETE", "/users/1");
  assert.deepEqual([own.status, own.json], [400, { error: "cannot remove your own account" }]);
  s.users = s.users.filter((u) => u.id === 2);
  const last = call(s, "DELETE", "/users/2");
  assert.deepEqual([last.status, last.json], [400, { error: "an org must keep at least one account" }]);
});

test("PUT /me/password, POST /logout, GET /plans", () => {
  const s = seedState(now);
  assert.deepEqual(call(s, "PUT", "/me/password", { current: "a", new: "b" }).json, { ok: true });
  assert.deepEqual(call(s, "POST", "/logout").json, { ok: true });
  assert.deepEqual(call(s, "GET", "/plans").json, { plans: [], current: null });
});

test("GET /orgs answers like a non-root session", () => {
  const r = call(seedState(now), "GET", "/orgs");
  assert.deepEqual([r.status, r.json], [401, { error: "unauthorized" }]);
  assert.equal(isFakeRoute("GET", "/orgs"), true);
});

test("GET /orgs/1 counts jobs since the seed", () => {
  const s = seedState(now);
  const o = json(call(s, "GET", "/orgs/1"));
  assert.equal(o.plan, null);
  assert.equal(o.jobs_this_month, 812);
  assert.equal(o.job_quota, 2000);
  call(s, "POST", "/jobs", { printer_id: 1 });
  const q = call(s, "POST", "/jobs", { printer_id: 6 });
  call(s, "DELETE", `/jobs/${json(q).job_id}`);
  assert.equal(json(call(s, "GET", "/orgs/1")).jobs_this_month, 813);
  assert.equal(call(s, "GET", "/orgs/2").status, 404);
});

test("PUT /orgs/1", () => {
  const s = seedState(now);
  assert.equal(call(s, "PUT", "/orgs/1", { event_url: null }).status, 200);
  assert.equal(s.org.event_url, null);
  call(s, "PUT", "/orgs/1", { event_url: "https://hooks.example/x" });
  assert.equal(s.org.event_url, "https://hooks.example/x");
  call(s, "PUT", "/orgs/1", { shopify_secret: "s3cret" });
  assert.equal(json(call(s, "GET", "/orgs/1")).shopify_secret_set, true);
  const quota = call(s, "PUT", "/orgs/1", { job_quota: 5 });
  assert.deepEqual([quota.status, quota.json], [403, { error: "job_quota is set by the server operator" }]);
  assert.equal(s.org.job_quota, 2000);
  const none = call(s, "PUT", "/orgs/1", { foo: 1 });
  assert.deepEqual([none.status, none.json], [400, { error: "nothing to update" }]);
});

test("handle advances the simulation first", () => {
  const s = seedState(now);
  s.traffic = false;
  call(s, "POST", "/jobs", { printer_id: 1 });
  assert.equal(s.jobs[0].state, "queued");
  call(s, "GET", "/health", undefined, now + 10);
  assert.equal(s.jobs[0].state, "done");
});
