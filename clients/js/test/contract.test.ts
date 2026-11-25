import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import { after, before, describe, it } from "node:test";
import { type Harness, harness } from "../testkit/harness.ts";
import { FakeAgent, type Json, createOrg, raw } from "../testkit/server.ts";

const PLANS = JSON.stringify([
  { id: "free", name: "Free", jobs: 50, price: "0 EUR" },
  { id: "pro", name: "Pro", jobs: 5000, price: "9 EUR/mo", checkout_url: "https://pay.example/pro?org={org}" },
]);

describe("the routes the client does not call, through the same recorder (test 20)", { timeout: 120_000 }, () => {
  let h: Harness;
  before(async () => {
    h = await harness({ PRINTAPI_SIGNUP: "open", PRINTAPI_PLANS: PLANS, PRINTAPI_BILLING_SECRET: "billing-secret" });
  });
  after(() => h.stop());

  const call = (token: string | undefined, method: string, path: string, body?: unknown) =>
    raw(h.server.url, token, method, path, body, h.recorder.fetch);

  it("accounts: signup, login, users, keys, orgs, password and logout", async () => {
    const T = () => h.server.token;
    assert.equal((await call(undefined, "GET", "/health")).json.signup, "open");
    assert.equal((await call(undefined, "GET", "/openapi.json")).status, 200);
    const signup = await call(undefined, "POST", "/signup", { email: "boss@acme.example", password: "a long passphrase", org_name: "acme" });
    assert.equal(signup.status, 200);
    const session = signup.json.token as string;
    const login = await call(undefined, "POST", "/login", { email: "boss@acme.example", password: "a long passphrase" });
    assert.equal(login.status, 200);
    assert.equal((await call(undefined, "POST", "/login", { email: "boss@acme.example", password: "wrong wrong" })).status, 401);
    assert.equal((await call(session, "GET", "/me")).json.kind, "session");
    const orgId = signup.json.org_id as number;

    const user = await call(session, "POST", "/users", { email: "packer@acme.example", password: "another passphrase" });
    assert.equal(user.status, 200);
    assert.equal((await call(session, "GET", "/users")).json.users.length, 2);
    assert.equal((await call(session, "DELETE", "/users/" + user.json.id)).status, 200);

    const issued = await call(session, "POST", "/apikeys", { label: "shop", scope: "print" });
    assert.equal(issued.json.scope, "print");
    assert.equal((await call(session, "GET", "/apikeys")).json.keys.length, 1);
    assert.deepEqual((await call(session, "PUT", "/apikeys/" + issued.json.id, { scope: "read" })).json, { ok: true, scope: "read" });
    assert.equal((await call(session, "DELETE", "/apikeys/" + issued.json.id)).status, 200);
    const full = await call(session, "POST", "/apikeys", { label: "plain" });
    assert.equal(full.json.scope, undefined, "a full key carries no scope");
    const listed = (await call(session, "GET", "/apikeys")).json.keys as Json[];
    assert.equal(listed.find((k) => k.label === "plain")?.scope, undefined);
    assert.equal(listed.find((k) => k.label === "shop")?.active, 0, "a revoked key stays listed");

    assert.equal((await call(session, "GET", "/orgs/" + orgId)).json.name, "acme");
    assert.deepEqual((await call(session, "PUT", "/orgs/" + orgId, { event_url: "https://ops.example/events" })).json, { ok: true, event_url: "https://ops.example/events" });
    assert.equal((await call(session, "PUT", "/orgs/" + orgId, { job_quota: 5 })).status, 403);
    assert.equal((await call(T(), "PUT", "/orgs/" + orgId, { job_quota: 5, plan: "pro" })).status, 200);
    assert.equal((await call(T(), "PUT", "/orgs/" + orgId, { shopify_secret: "s" })).json.shopify_secret_set, true);
    assert.equal((await call(session, "GET", "/support-access")).json.enabled, false);
    assert.equal((await call(session, "GET", "/plans")).json.plans.length, 2);

    const created = await call(T(), "POST", "/orgs", { name: "second" });
    assert.equal((await call(T(), "POST", "/orgs/" + created.json.id + "/users", { email: "x@second.example", password: "a long passphrase" })).status, 200);
    assert.equal((await call(T(), "GET", "/orgs")).json.orgs.length, 3);
    assert.equal((await call(T(), "DELETE", "/orgs/" + created.json.id)).status, 200);

    assert.equal((await call(session, "PUT", "/me/password", { current: "a long passphrase", new: "a new long passphrase" })).status, 200);
    assert.equal((await call(session, "GET", "/me")).status, 401, "the password change ends every session");
    assert.equal((await call(undefined, "POST", "/password/reset", { email: "boss@acme.example" })).status, 200);
    assert.equal((await call(undefined, "POST", "/password/reset/confirm", { token: "nope", password: "a long passphrase" })).status, 400);
    const again = await call(undefined, "POST", "/login", { email: "boss@acme.example", password: "a new long passphrase" });
    assert.equal((await call(again.json.token, "POST", "/logout")).status, 200);
  });

  it("agents: pairing, register, claim, payload, result, late result, receipts of the history", async () => {
    const T = h.server.token;
    const code = await call(T, "POST", "/pairing", {});
    assert.equal(code.status, 200);
    assert.equal((await call(T, "GET", "/pairing")).json.codes[0].state, "open");
    const paired = await call(undefined, "POST", "/agent/pair", { code: code.json.code, name: "paired-pc" });
    assert.equal(paired.status, 200);
    assert.equal((await call(undefined, "POST", "/agent/pair", { code: "AAAA-AAAA-AAAA", name: "x" })).status, 401);
    const second = await call(T, "POST", "/pairing", {});
    assert.equal((await call(T, "DELETE", "/pairing/" + second.json.id)).status, 200);
    const reconnect = await call(T, "POST", "/pairing", { computer_id: 0 });
    assert.equal(reconnect.status, 404);

    const agent = await new FakeAgent(h.server.url, paired.json.key, h.recorder.fetch).register("paired-pc", [{ name: "Laser", can_pdf: true }]);
    const client = (await call(T, "POST", "/apikeys", { label: "c" })).json.key as string;
    const submitted = await call(client, "POST", "/jobs", { printer_id: agent.printer("Laser"), type: "receipt", receipt: { content: [{ type: "text", text: "hi" }] } });
    assert.equal(submitted.status, 200);
    const preview = await h.recorder.fetch(h.server.url + "/jobs/" + submitted.json.job_id + "/preview", { headers: { Authorization: "Bearer " + T } });
    assert.equal(preview.status, 200);
    assert.equal(preview.headers.get("content-type"), "image/png");
    const claimed = await agent.claim();
    assert.equal(claimed?.job_uid?.length, 32);
    assert.ok((await agent.payload(claimed?.job_id ?? 0)).length > 0, "the job's bytes");
    assert.deepEqual(await agent.result(claimed?.job_id ?? 0, true), { status: 200, json: { ok: true } });
    const late = await call(paired.json.key, "POST", "/agent/results", { job_id: claimed?.job_id, job_uid: claimed?.job_uid, ok: true });
    assert.deepEqual(late.json, { ok: true, state: "done" });
    assert.deepEqual((await call(paired.json.key, "POST", "/agent/results", { job_id: 999, job_uid: "0".repeat(32), ok: true })).json, { ok: true, state: null });
  });

  it("webhooks and CloudPRNT", async () => {
    const T = h.server.token;
    const org = (await call(T, "POST", "/orgs", { name: "hooks" })).json.id as number;
    await call(T, "PUT", "/orgs/" + org, { shopify_secret: "shop-secret" });
    const key = (await call(T, "POST", "/apikeys", { org_id: org, label: "shop" })).json.key as string;
    const agentKey = (await call(T, "POST", "/apikeys", { org_id: org, scope: "agent" })).json.key as string;
    const agent = await new FakeAgent(h.server.url, agentKey, h.recorder.fetch).register("hook-pc", [{ name: "Laser", can_pdf: true }]);
    const body = JSON.stringify({ id: 4711, name: "#1001", line_items: [{ quantity: 1, title: "Widget", price: "9.99" }] });
    const sig = createHmac("sha256", "shop-secret").update(body).digest("base64");
    const url = h.server.url + "/integrations/shopify/orders?key=" + key + "&printer_id=" + agent.printer("Laser");
    const shop = await h.recorder.fetch(url, { method: "POST", headers: { "X-Shopify-Hmac-Sha256": sig }, body });
    assert.equal(shop.status, 200);
    assert.equal((await h.recorder.fetch(url, { method: "POST", headers: { "X-Shopify-Hmac-Sha256": "bad" }, body })).status, 401);

    const event = JSON.stringify({ org_id: org, plan: "pro" });
    const signed = "sha256=" + createHmac("sha256", "billing-secret").update(event).digest("hex");
    const bill = await h.recorder.fetch(h.server.url + "/billing/webhook", { method: "POST", headers: { "X-Signature": signed }, body: event });
    assert.deepEqual(await bill.json(), { ok: true, org_id: org, plan: "pro", job_quota: 5000 });
    assert.equal((await h.recorder.fetch(h.server.url + "/billing/webhook", { method: "POST", headers: { "X-Signature": "sha256=00" }, body: event })).status, 401);

    const star = (await call(T, "POST", "/apikeys", { org_id: org, scope: "cloudprnt" })).json.key as string;
    const poll = await call(undefined, "POST", "/cloudprnt/" + star, { printerMAC: "00:11:62:aa:bb:cc" });
    assert.deepEqual(poll.json, { jobReady: false });
    const basic = await h.recorder.fetch(h.server.url + "/cloudprnt", {
      method: "POST", headers: { Authorization: "Basic " + btoa(star + ":") }, body: JSON.stringify({ printerMAC: "00:11:62:aa:bb:cc" }),
    });
    assert.deepEqual(await basic.json(), { jobReady: false });
    const auth = { Authorization: "Basic " + btoa(star + ":") };
    assert.equal((await h.recorder.fetch(h.server.url + "/cloudprnt?mac=00:11:62:aa:bb:cc", { headers: auth })).status, 404);
    assert.equal((await h.recorder.fetch(h.server.url + "/cloudprnt?mac=00:11:62:aa:bb:cc", { method: "DELETE", headers: auth })).status, 200);
    assert.equal((await h.recorder.fetch(h.server.url + "/cloudprnt/" + star + "?mac=00:11:62:aa:bb:cc")).status, 404);
    assert.equal((await h.recorder.fetch(h.server.url + "/cloudprnt/" + star + "?mac=00:11:62:aa:bb:cc", { method: "DELETE" })).status, 200);
    const starJob = (await call(T, "POST", "/apikeys", { org_id: org, scope: "print" })).json.key as string;
    const printers = (await call(starJob, "GET", "/printers")).json.printers as Json[];
    const star1 = printers.find((p) => String(p.name).startsWith("cloudprnt-"));
    assert.ok(star1, "the printer enrolled itself");
    await call(starJob, "POST", "/jobs", { printer_id: star1.id, type: "raw_base64", content: btoa("STAR") });
    const offer = await call(undefined, "POST", "/cloudprnt/" + star, { printerMAC: "00:11:62:aa:bb:cc" });
    assert.equal(offer.json.jobReady, true);
    assert.equal((await h.recorder.fetch(h.server.url + "/cloudprnt/" + star + "?mac=00:11:62:aa:bb:cc")).status, 200);
    assert.equal((await h.recorder.fetch(h.server.url + "/cloudprnt/" + star + "?mac=00:11:62:aa:bb:cc&code=200%20OK", { method: "DELETE" })).status, 200);
    const metrics = await h.recorder.fetch(h.server.url + "/metrics", { headers: { Authorization: "Bearer " + starJob } });
    assert.match(await metrics.text(), /printpapi_jobs\{state="done"\}/);
    assert.equal((await call(starJob, "GET", "/plans")).status, 200);
  });

  it("the routes of the client, once each, through the client", async () => {
    const T = h.server.token;
    const org = await createOrg(h.server, "client-routes");
    const agent = await new FakeAgent(h.server.url, org.agentKey, h.recorder.fetch).register("client-pc", [{ name: "Zebra" }, { name: "Laser", can_pdf: true }]);
    const c = h.client(org.key);
    assert.equal((await c.health()).ok, true);
    assert.equal((await c.me()).kind, "key");
    assert.equal((await c.printers.list()).length, 2);
    assert.equal((await c.computers.list()).length, 1);
    const queued = await c.jobs.submit({ printer_id: agent.printer("Zebra"), type: "raw_base64", content: btoa("x") });
    assert.equal((await c.jobs.list()).length, 1);
    assert.equal((await c.jobs.get(queued.job_id)).state, "queued");
    assert.deepEqual(await c.jobs.cancel(queued.job_id), { ok: true, state: "cancelled" });
    await c.orders.print({ printer_id: agent.printer("Laser"), order: { number: "1", lines: [{ qty: 1, name: "Widget", total: "1 EUR" }] } });
    const spec = {
      name: "t", content: "image" as const, label_mm: { w: 50, h: 30 }, dpi: 203, rotate: 0 as const,
      crop_percent: { t: 0, r: 0, b: 0, l: 0 }, place_mm: { x: 0, y: 0, w: 50, h: 30 },
    };
    const made = await c.labels.create(spec);
    assert.equal((await c.labels.list()).length, 1);
    assert.equal((await c.labels.get(made.id)).name, "t");
    await c.labels.update(made.id, { ...spec, name: "t2" });
    assert.deepEqual(await c.labels.delete(made.id), { ok: true });
    const receipt = { content: [{ type: "text" as const, text: "hi" }] };
    assert.equal((await c.receipts.preview({ receipt, profile: { paper_mm: 58 } })).length > 8, true);
    const printer = agent.printer("Zebra");
    assert.equal((await c.receipts.profile.get(printer)).stored, false);
    await c.receipts.profile.put(printer, { paper_mm: 58 });
    assert.deepEqual(await c.receipts.profile.delete(printer), { ok: true });
    // a body that is no JSON, to a route that reads it before it asks who is calling
    assert.equal((await h.recorder.fetch(h.server.url + "/password/reset", { method: "POST", body: "not json" })).status, 400);
    assert.equal((await call(T, "GET", "/me")).status, 200);
  });

  it("every documented operation was called through the recorder in this file", () => {
    const all = Object.values<Json>(h.doc.paths).flatMap((item) => Object.values<Json>(item).map((op) => op.operationId as string));
    const driven = new Set(h.recorder.calls.map((c) => c.operationId));
    const missing = all.filter((id) => !driven.has(id));
    assert.deepEqual(missing, [], "documented but never called: drive it in a test, or it is not proven");
  });
});

describe("shapes that only a configured server shows: terms, a support grant, the key a device still uses", { timeout: 120_000 }, () => {
  const operator = "operator-token-0123456789-abcdefghijklmnop";
  let h: Harness;
  before(async () => {
    h = await harness({
      PRINTAPI_OPERATOR_TOKEN: operator, PRINTAPI_OPERATOR_FROM: "127.0.0.1", PRINTAPI_PRIVATE_URLS: "allow",
      PRINTAPI_SIGNUP: "open", PRINTAPI_TERMS_VERSION: "2026-11", PRINTAPI_TERMS_URL: "https://example.com/terms",
      PRINTAPI_DPA_URL: "https://example.com/dpa", PRINTAPI_PRIVACY_URL: "https://example.com/privacy",
    });
  });
  after(() => h.stop());
  const call = (token: string | undefined, method: string, path: string, body?: unknown) =>
    raw(h.server.url, token, method, path, body, h.recorder.fetch);

  it("health names the terms, a support grant is listed for the customer, a pairing names the old key", async () => {
    const health = (await call(undefined, "GET", "/health")).json;
    assert.deepEqual(health.terms, { version: "2026-11", terms_url: "https://example.com/terms", dpa_url: "https://example.com/dpa", privacy_url: "https://example.com/privacy" });
    const signup = await call(undefined, "POST", "/signup", { email: "boss@acme.example", password: "a long passphrase", org_name: "acme", accept_terms: "2026-11" });
    assert.equal(signup.status, 200, JSON.stringify(signup.json));
    const session = signup.json.token as string;
    const orgId = signup.json.org_id as number;

    // the operator console is no part of the document: plain fetch, not the recorder
    const op = (method: string, path: string, body?: unknown) =>
      fetch(h.server.url + path, { method, headers: { Authorization: "Bearer " + operator, "Content-Type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body) });
    const orgs = (await (await op("GET", "/operator/orgs")).json()) as { orgs: { id: number; created_at: number }[] };
    const created = orgs.orgs.find((o) => o.id === orgId)?.created_at;
    assert.ok(created, "the org is listed");
    const grant = await op("POST", "/operator/orgs/" + orgId + "/support", { reason: "ticket 4711, quota", minutes: 15, org_created_at: created });
    assert.equal(grant.status, 200, await grant.clone().text());
    const seen = (await call(session, "GET", "/support-access")).json;
    assert.equal(seen.enabled, true);
    assert.equal(seen.grants.length, 1);
    assert.equal(seen.grants[0].active, true);

    // a device that still uses a full key: a reconnect code names that key
    const key = (await call(session, "POST", "/apikeys", { label: "old machine" })).json.key as string;
    const agent = await new FakeAgent(h.server.url, key, h.recorder.fetch).register("old-pc", [{ name: "Zebra" }]);
    const code = await call(session, "POST", "/pairing", { computer_id: agent.computerId });
    assert.equal(code.status, 200, JSON.stringify(code.json));
    assert.deepEqual(code.json.old_key?.label, "old machine");
    const listed = (await call(session, "GET", "/pairing")).json.codes as Json[];
    assert.equal(listed[0]?.old_key?.label, "old machine");
  });
});
