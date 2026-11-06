// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The agents and the shop, simulated from timestamps only: no timers live in the state, so a reload
// or a background tab that was throttled for hours still lands on a consistent picture.
import type { DemoJob, DemoState } from "./seed.ts";

export type JobSpec = Pick<DemoJob, "printer_id" | "title" | "type" | "kind" | "ref" | "file" | "copies">;
export type Timing = { claimAfter: number; printFor: number }; // seconds
export const TRAFFIC_EVERY_S = 13;

const OUTPUTS_MAX = 8;
export const JOBS_MAX = 300;
// ponytail: the seed ends at job #1291, so "created since the seed" is "id above that".
export const SEED_LAST_JOB = 1291;

// Newest first. Drops the oldest finished jobs that outputs does not reference; queued and
// claimed jobs always stay.
export function trim(s: DemoState): void {
  let extra = s.jobs.length - JOBS_MAX;
  if (extra <= 0) return;
  const keep = new Set(s.outputs);
  for (let i = s.jobs.length - 1; i >= 0 && extra > 0; i--) {
    const j = s.jobs[i];
    if (j.state === "queued" || j.state === "claimed" || keep.has(j.id)) continue;
    s.jobs.splice(i, 1);
    if (j.state === "done" || j.state === "failed" || j.state === "cancelled") s.trimmed[j.state]++;
    if (j.id > SEED_LAST_JOB && j.state !== "cancelled") s.trimmed.since_seed++;
    extra--;
  }
}

export function enqueue(s: DemoState, spec: JobSpec, now: number, timing?: Timing, rand: () => number = Math.random): DemoJob {
  const t = timing ?? { claimAfter: 0.25 + rand() * 0.45, printFor: 1.1 + rand() * 0.9 };
  const printer = s.printers.find((p) => p.id === spec.printer_id);
  const online = printer ? s.agents.find((a) => a.id === printer.agent_id)?.online === true : false;
  const claim_at = online ? now + t.claimAfter : null;
  const job: DemoJob = {
    ...spec,
    id: s.next.job++,
    state: "queued",
    error: null,
    created_at: now,
    finished_at: null,
    claim_at,
    done_at: claim_at === null ? null : claim_at + t.printFor,
  };
  s.jobs.unshift(job);
  return job;
}

export function simulateOrder(s: DemoState, now: number, rand: () => number = Math.random): number {
  const n = s.next.order++;
  enqueue(s, { printer_id: 3, title: `Packing slip · order #${n}`, type: "pdf_base64", kind: "slip", ref: n, file: null, copies: 1 }, now, undefined, rand);
  // the label follows the slip a moment later, like the shop plugin sends them
  enqueue(
    s,
    { printer_id: 1, title: `Shipping label · order #${n}`, type: "raw_base64", kind: "label", ref: n, file: null, copies: 1 },
    now,
    { claimAfter: 0.7, printFor: 1.1 + rand() * 0.9 },
    rand,
  );
  return n;
}

// Moves every due job forward and returns the ids that became done. Jobs only ever step
// queued -> claimed -> done, each once, however long ago it was due.
export function advance(s: DemoState, now: number): number[] {
  const finished: number[] = [];
  // oldest first, so outputs keep their print order
  for (let i = s.jobs.length - 1; i >= 0; i--) {
    const j = s.jobs[i];
    if (j.state === "queued" && j.claim_at !== null && now >= j.claim_at) j.state = "claimed";
    if (j.state === "claimed" && j.done_at !== null && now >= j.done_at) {
      j.state = "done";
      j.finished_at = j.done_at;
      s.outputs.push(j.id);
      finished.push(j.id);
    }
  }
  if (s.outputs.length > OUTPUTS_MAX) s.outputs.splice(0, s.outputs.length - OUTPUTS_MAX);

  return finished;
}

// Every job's lifecycle step summed (queued 0, claimed 1, finished 2). advance() only moves jobs
// forward, so a caller compares this before and after to see whether anything moved.
export function progress(s: DemoState): number {
  return s.jobs.reduce((n, j) => n + (j.state === "queued" ? 0 : j.state === "claimed" ? 1 : 2), 0);
}

// One shop event when due, however long we were away: no flood of orders after a night in a
// background tab. Returns whether anything was created.
export function trafficTick(s: DemoState, now: number, rand: () => number = Math.random): boolean {
  if (!s.traffic || now < s.traffic_next_at) return false;
  if (rand() < 0.7) simulateOrder(s, now, rand);
  else {
    const n = s.next.receipt++;
    enqueue(s, { printer_id: 4, title: `Receipt #A-${n}`, type: "raw_base64", kind: "receipt", ref: n, file: null, copies: 1 }, now, undefined, rand);
  }
  s.traffic_next_at = now + TRAFFIC_EVERY_S;
  trim(s);
  return true;
}

export function cancel(s: DemoState, id: number, now: number): "ok" | "not-found" | "not-cancellable" {
  const j = s.jobs.find((x) => x.id === id);
  if (!j) return "not-found";
  if (j.state !== "queued") return "not-cancellable";
  j.state = "cancelled";
  j.finished_at = now;
  return "ok";
}

// A label_* job is rendered for its printer: PDF where it takes PDF, ZPL (raw) otherwise.
export function mode(type: string, canPdf = false): "pdf" | "raw" {
  if (type === "label_uri" || type === "label_base64" || type === "label_text") return canPdf ? "pdf" : "raw";
  return type === "pdf_base64" || type === "pdf_uri" || type === "pdf_uri_post" ? "pdf" : "raw";
}
