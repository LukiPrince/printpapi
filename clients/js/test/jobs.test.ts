import assert from "node:assert/strict";
import { after, before, describe, it } from "node:test";
import { PrintpapiError } from "../src/index.ts";
import type { Printpapi } from "../src/index.ts";
import { type Harness, harness } from "../testkit/harness.ts";
import { FakeAgent, type Org, createOrg } from "../testkit/server.ts";

const raw64 = (s: string) => ({ type: "raw_base64" as const, content: btoa(s) });

describe("reading, waiting and cancelling jobs (tests 6 to 8)", { timeout: 120_000 }, () => {
  let h: Harness;
  let org: Org;
  let c: Printpapi;
  let agent: FakeAgent;
  let zpl = 0;
  before(async () => {
    h = await harness();
    org = await createOrg(h.server, "jobs");
    c = h.client(org.key);
    agent = await new FakeAgent(h.server.url, org.agentKey, h.recorder.fetch).register("pc", [{ name: "Zebra" }]);
    zpl = agent.printer("Zebra");
  });
  after(() => h.stop());

  it("lists and gets jobs, and get carries the id (test 6)", async () => {
    const { job_id } = await c.jobs.submit({ printer_id: zpl, ...raw64("^XA^XZ"), title: "Label 1" });
    const job = await c.jobs.get(job_id);
    assert.equal(job.id, job_id, "GET /jobs/{id} has no id of its own: the client adds it");
    assert.equal(job.state, "queued");
    assert.equal(job.printer_id, zpl);
    assert.equal(job.error, null);
    assert.equal(job.finished_at, null);
    const rows = await c.jobs.list();
    const row = rows.find((r) => r.id === job_id);
    assert.equal(row?.title, "Label 1");
    assert.equal(row?.type, "raw_base64");
    assert.equal(row?.printer_name, "Zebra");
    assert.equal(row?.agent_name, "pc");
    await agent.print();
    assert.equal((await c.jobs.get(job_id)).state, "done");
  });

  it("waits until a job is done, and returns a failed job unless asked to throw (test 7)", async () => {
    const done = await c.jobs.submit({ printer_id: zpl, ...raw64("a") });
    const waiting = c.jobs.wait(done.job_id, { intervalMs: 20 });
    await new Promise((r) => setTimeout(r, 60));
    await agent.print();
    const job = await waiting;
    assert.equal(job.state, "done");
    assert.equal(job.id, done.job_id);

    const bad = await c.jobs.submit({ printer_id: zpl, ...raw64("b") });
    const claimed = await agent.claim();
    await agent.result(claimed?.job_id ?? 0, false, "out of paper");
    const failed = await c.jobs.wait(bad.job_id, { intervalMs: 20 });
    assert.equal(failed.state, "failed");
    assert.equal(failed.error, "out of paper");
    await assert.rejects(
      c.jobs.wait(bad.job_id, { intervalMs: 20, throwOnFailure: true }),
      (e: unknown) => e instanceof PrintpapiError && e.kind === "job_failed" && /out of paper/.test(e.message),
    );
  });

  it("gives up after its deadline, and stops when the caller aborts", async () => {
    const stuck = await c.jobs.submit({ printer_id: zpl, ...raw64("c") });
    const started = Date.now();
    await assert.rejects(
      c.jobs.wait(stuck.job_id, { timeoutMs: 150, intervalMs: 20 }),
      (e: unknown) => e instanceof PrintpapiError && e.kind === "timeout" && /still queued/.test(e.message),
    );
    assert.ok(Date.now() - started < 5_000);
    const ac = new AbortController();
    setTimeout(() => ac.abort(), 80);
    await assert.rejects(
      c.jobs.wait(stuck.job_id, { intervalMs: 20, signal: ac.signal }),
      (e: unknown) => e instanceof PrintpapiError && e.kind === "network" && /aborted/.test(e.message),
    );
    await c.jobs.cancel(stuck.job_id);
  });

  it("wait gives not_found for an unknown job, and another org's printer is unknown to you", async () => {
    const other = await createOrg(h.server, "other-jobs");
    await assert.rejects(
      h.client(other.key).jobs.submit({ printer_id: zpl, ...raw64("x") }),
      (e: unknown) => e instanceof PrintpapiError && e.kind === "invalid" && /unknown printer/.test(e.message),
      "another org's printer reads like a missing one",
    );
    assert.equal((await h.client(other.key).jobs.list()).length, 0);
    await assert.rejects(
      c.jobs.wait(999_999, { intervalMs: 20 }),
      (e: unknown) => e instanceof PrintpapiError && e.kind === "not_found",
    );
  });

  it("cancels a queued job, not a claimed one, and never a foreign or unknown one (test 8)", async () => {
    const queued = await c.jobs.submit({ printer_id: zpl, ...raw64("q") });
    assert.deepEqual(await c.jobs.cancel(queued.job_id), { ok: true, state: "cancelled" });
    assert.equal((await c.jobs.get(queued.job_id)).state, "cancelled");

    const taken = await c.jobs.submit({ printer_id: zpl, ...raw64("t") });
    const job = await agent.claim();
    assert.equal(job?.job_id, taken.job_id);
    await assert.rejects(
      c.jobs.cancel(taken.job_id),
      (e: unknown) => e instanceof PrintpapiError && e.kind === "conflict" && e.status === 409 && e.attempts === 1,
    );
    await agent.result(taken.job_id, true);

    await assert.rejects(c.jobs.cancel(424_242), (e: unknown) => e instanceof PrintpapiError && e.kind === "not_found");
    const other = await createOrg(h.server, "other-cancel");
    await assert.rejects(
      h.client(other.key).jobs.cancel(taken.job_id),
      (e: unknown) => e instanceof PrintpapiError && e.kind === "not_found",
      "another org's job reads like a missing one",
    );
  });
});
