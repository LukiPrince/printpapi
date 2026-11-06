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

// ---- label templates -------------------------------------------------------------------------
// Texts are the ones crates/server/src/label.rs and server.rs answer.
const tpl = (over: Record<string, unknown> = {}): Record<string, unknown> => ({
  name: "mine",
  content: "image",
  label_mm: { w: 100, h: 50 },
  dpi: 203,
  rotate: 0,
  crop_percent: { t: 0, r: 0, b: 0, l: 0 },
  place_mm: { x: 0, y: 0, w: 100, h: 50 },
  ...over,
});
const bad = (s: ReturnType<typeof seedState>, body: unknown, error: string, status = 400) => {
  const r = call(s, "POST", "/labels", body);
  assert.deepEqual([r.status, r.json], [status, { error }]);
};

test("isFakeRoute knows the label routes", () => {
  assert.equal(isFakeRoute("GET", "/labels"), true);
  assert.equal(isFakeRoute("POST", "/labels"), true);
  assert.equal(isFakeRoute("PUT", "/labels/2"), true);
  assert.equal(isFakeRoute("DELETE", "/labels/2"), true);
  assert.equal(isFakeRoute("GET", "/labels/index.txt"), false);
  assert.equal(isFakeRoute("PUT", "/labels"), false);
});

test("GET /labels lists the two sample templates by name", () => {
  const { templates } = json(call(seedState(now), "GET", "/labels"));
  assert.deepEqual(templates.map((t: J) => t.name), ["error-code-1x0.5", "shipping-4x6"]);
  const t = templates[1];
  assert.equal(t.content, "image");
  assert.deepEqual(t.label_mm, { w: 101.6, h: 152.4 });
  assert.equal(t.rotate, 90);
  assert.equal(typeof t.id, "number");
  assert.equal(typeof t.created_at, "number");
  assert.equal(typeof t.updated_at, "number");
});

test("POST /labels creates, fills the defaults and sorts by name", () => {
  const s = seedState(now);
  const r = call(s, "POST", "/labels", tpl({ name: "Alpha" }), now + 5);
  assert.equal(r.status, 200);
  const t = json(r);
  assert.equal(t.id, 3);
  assert.deepEqual(t.text, { wrap_chars: 0, lines_per_label: 0 });
  assert.equal("zpl_darkness" in t, false);
  assert.equal("size_ref" in t, false);
  assert.deepEqual([t.created_at, t.updated_at], [now + 5, now + 5]);
  const kept = json(call(s, "POST", "/labels", tpl({ name: "zz", zpl_darkness: -5, size_ref: "Standard:4x6", text: { wrap_chars: 12 } })));
  assert.equal(kept.zpl_darkness, -5);
  assert.equal(kept.size_ref, "Standard:4x6");
  assert.deepEqual(kept.text, { wrap_chars: 12, lines_per_label: 0 });
  const names = json(call(s, "GET", "/labels")).templates.map((x: J) => x.name);
  assert.deepEqual(names, ["Alpha", "error-code-1x0.5", "shipping-4x6", "zz"]);
});

test("POST /labels refuses a name that is taken", () => {
  const s = seedState(now);
  bad(s, tpl({ name: "shipping-4x6" }), "label template name already in use", 409);
  assert.equal(json(call(s, "GET", "/labels")).templates.length, 2);
});

test("POST /labels validates like the server", () => {
  const s = seedState(now);
  bad(s, undefined, "bad json");
  bad(s, [], "bad json");
  bad(s, tpl({ colour: 1 }), "unknown field in template: colour");
  const noName = tpl();
  delete noName.name;
  bad(s, noName, "name is required");
  for (const name of ["", "a/b", "x".repeat(65), 7]) bad(s, tpl({ name }), "name must be 1-64 chars of letters/digits/space/._-");
  assert.equal(call(s, "POST", "/labels", tpl({ name: "Größe 4.1_x-y" })).status, 200);
  bad(s, tpl({ content: "pdf" }), 'content must be "image" or "text"');
  bad(s, tpl({ dpi: 99 }), "dpi must be 100-600");
  bad(s, tpl({ dpi: 601 }), "dpi must be 100-600");
  bad(s, tpl({ dpi: 203.5 }), "dpi must be an integer");
  bad(s, tpl({ rotate: 45 }), "rotate must be 0, 90, 180 or 270");
  const noRotate = tpl();
  delete noRotate.rotate;
  bad(s, noRotate, "rotate is required");
  bad(s, tpl({ label_mm: 5 }), "label_mm must be an object");
  bad(s, tpl({ label_mm: { w: 100, h: 50, d: 1 } }), "unknown field in label_mm: d");
  bad(s, tpl({ label_mm: { w: 100 } }), "label_mm.h is required");
  bad(s, tpl({ label_mm: { w: "1", h: 50 } }), "label_mm.w must be a number");
  bad(s, tpl({ label_mm: { w: 0, h: 50 } }), "label_mm.w must be 1-8000 dots at 203 dpi");
  bad(s, tpl({ label_mm: { w: 100, h: 1100 } }), "label_mm.h must be 1-8000 dots at 203 dpi");
  bad(s, tpl({ crop_percent: { t: 0, r: 0, b: 96, l: 0 } }), "crop_percent values must be 0-95");
  bad(s, tpl({ crop_percent: { t: 50, r: 0, b: 50, l: 0 } }), "crop_percent leaves nothing of the image");
  bad(s, tpl({ place_mm: { x: 0, y: 0, w: 0, h: 5 } }), "place_mm.w and place_mm.h must be greater than 0");
  bad(s, tpl({ place_mm: { x: 0, y: 0, w: 5000, h: 5 } }), "place_mm is too large");
  bad(s, tpl({ place_mm: { x: 1e9, y: 0, w: 5, h: 5 } }), "place_mm.x and place_mm.y are too far from the label");
  bad(s, tpl({ text: 3 }), "text must be an object");
  bad(s, tpl({ text: { wrap_chars: 201 } }), "text.wrap_chars must be 0-200");
  bad(s, tpl({ text: { lines_per_label: 1.5 } }), "text.lines_per_label must be an integer");
  bad(s, tpl({ text: { wrap: 1 } }), "unknown field in text: wrap");
  bad(s, tpl({ zpl_darkness: 31 }), "zpl_darkness must be -30 to 30");
  bad(s, tpl({ size_ref: "x".repeat(65) }), "size_ref must be a string of at most 64 chars");
  bad(s, tpl({ size_ref: 4 }), "size_ref must be a string of at most 64 chars");
  // off the label is allowed: the content is clipped
  assert.equal(call(s, "POST", "/labels", tpl({ name: "off", place_mm: { x: -20, y: 80, w: 100, h: 50 } })).status, 200);
});

test("PUT /labels/{id} replaces the template", () => {
  const s = seedState(now);
  const r = call(s, "PUT", "/labels/2", tpl({ name: "renamed", content: "text" }), now + 60);
  assert.equal(r.status, 200);
  const t = json(r);
  assert.equal(t.id, 2);
  assert.equal(t.name, "renamed");
  assert.equal(t.content, "text");
  assert.equal(t.updated_at, now + 60);
  assert.ok(t.created_at < now);
  assert.equal(json(call(s, "GET", "/labels")).templates.length, 2);
  // its own name again is no conflict
  assert.equal(call(s, "PUT", "/labels/2", tpl({ name: "renamed" })).status, 200);
});

test("PUT /labels/{id}: 400 before 404, 409 on another template's name", () => {
  const s = seedState(now);
  const invalid = call(s, "PUT", "/labels/99", tpl({ rotate: 45 }));
  assert.deepEqual([invalid.status, invalid.json], [400, { error: "rotate must be 0, 90, 180 or 270" }]);
  const gone = call(s, "PUT", "/labels/99", tpl());
  assert.deepEqual([gone.status, gone.json], [404, { error: "not found" }]);
  const taken = call(s, "PUT", "/labels/2", tpl({ name: "error-code-1x0.5" }));
  assert.deepEqual([taken.status, taken.json], [409, { error: "label template name already in use" }]);
  assert.equal(s.labels.find((l) => l.id === 2)!.name, "shipping-4x6");
});

test("DELETE /labels/{id}", () => {
  const s = seedState(now);
  const r = call(s, "DELETE", "/labels/1");
  assert.deepEqual([r.status, r.json], [200, { ok: true }]);
  assert.deepEqual(json(call(s, "GET", "/labels")).templates.map((t: J) => t.id), [2]);
  const again = call(s, "DELETE", "/labels/1");
  assert.deepEqual([again.status, again.json], [404, { error: "not found" }]);
  // ids are not reused
  assert.equal(json(call(s, "POST", "/labels", tpl())).id, 3);
});

test("label jobs: accepted, content dropped, mode follows the printer", () => {
  const s = seedState(now);
  const pdf = call(s, "POST", "/jobs", { printer_id: 3, type: "label_base64", template: "shipping-4x6", content: "A".repeat(2_000_000), title: "test: shipping-4x6" });
  assert.deepEqual([pdf.status, pdf.json], [200, { job_id: 1292 }]);
  const raw = call(s, "POST", "/jobs", { printer_id: 1, type: "label_text", template: 1, text: "B1^XZ" });
  assert.deepEqual([raw.status, raw.json], [200, { job_id: 1293 }]);
  const url = call(s, "POST", "/jobs", { printer_id: 1, type: "label_uri", template: "shipping-4x6", url: "https://shop.example/l.gif" });
  assert.equal(url.status, 200);
  assert.equal("content" in s.jobs[2], false);
  assert.ok(JSON.stringify(s).length < 200_000);
  const { jobs } = json(call(s, "GET", "/jobs"));
  assert.deepEqual([jobs[2].type, jobs[2].mode], ["label_base64", "pdf"]);
  assert.deepEqual([jobs[1].type, jobs[1].mode], ["label_text", "raw"]);
  assert.equal(jobs[0].mode, "raw");
});

test("label jobs are checked like the server checks them", () => {
  const s = seedState(now);
  const post = (body: Record<string, unknown>) => call(s, "POST", "/jobs", { printer_id: 1, type: "label_text", text: "x", template: 1, ...body });
  const e = (r: FakeResponse, error: string) => assert.deepEqual([r.status, r.json], [400, { error }]);
  e(post({ template: undefined }), "missing 'template'");
  e(post({ template: true }), "template must be a name or an integer id");
  e(post({ text: undefined }), "missing 'text'");
  e(post({ text: 5 }), "text must be a string");
  e(post({ printer_id: 99 }), "unknown printer: 99");
  e(post({ template: "nope" }), "unknown label template: nope");
  e(post({ template: 99 }), "unknown label template: 99");
  e(post({ template: "shipping-4x6" }), "label template shipping-4x6 does not take text jobs");
  e(post({ type: "label_base64", content: "AAAA", template: 1 }), "label template error-code-1x0.5 does not take image jobs");
  e(post({ copies: 0 }), "copies must be an integer 1..100");
  assert.equal(s.jobs[0].id, 1291); // none of them was queued
  assert.equal(post({}).status, 200);
});
