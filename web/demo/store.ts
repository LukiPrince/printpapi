// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The demo's browser side: one state object in sessionStorage, the fake API behind window.fetch,
// and a ticker that lets the simulated agents work. Everything else lives in the pure modules.
import { useSyncExternalStore } from "react";
import { getToken, setToken } from "../lib/api";
import { handle, isFakeRoute } from "./fake-api.ts";
import { loadState, saveState, STORAGE_KEY } from "./persist.ts";
import { seedState } from "./seed.ts";
import type { DemoJob, DemoState } from "./seed.ts";
import { advance, enqueue, progress, simulateOrder as simOrder, trafficTick } from "./sim.ts";
import type { JobSpec, Timing } from "./sim.ts";

const TICK_MS = 500;

let state: DemoState | null = null;
let version = 0;
let installed = false;
let serverSeed: DemoState | null = null; // prerender only: no storage, no clock
const listeners = new Set<() => void>();

const nowS = () => Date.now() / 1000;

function persist() {
  if (!state) return;
  try {
    window.sessionStorage.setItem(STORAGE_KEY, saveState(state));
  } catch {
    // storage full or blocked: the demo keeps running from memory
  }
}

function changed() {
  version++;
  persist();
  listeners.forEach((l) => l());
}

function current(): DemoState {
  if (!state) {
    let raw: string | null = null;
    try {
      raw = window.sessionStorage.getItem(STORAGE_KEY);
    } catch {
      // sessionStorage can throw (blocked site data): start from the seed
    }
    state = loadState(raw, nowS());
  }
  return state;
}

function tick() {
  const s = current();
  const before = progress(s);
  advance(s, nowS());
  // shop traffic only where someone watches the dashboard: not in a hidden tab, not on the start page
  const watching = document.visibilityState === "visible" && window.location.pathname !== "/";
  const created = watching && trafficTick(s, nowS());
  if (created || progress(s) !== before) changed();
}

export function installDemo(): void {
  if (installed || typeof window === "undefined") return;
  installed = true;
  current();
  if (!getToken()) setToken("demo");

  const realFetch = window.fetch.bind(window);
  window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const req = input instanceof Request ? input : null;
    const url = new URL(req ? req.url : String(input instanceof URL ? input.href : input), window.location.href);
    const method = (init?.method ?? req?.method ?? "GET").toUpperCase();
    if (url.origin !== window.location.origin || !isFakeRoute(method, url.pathname)) return realFetch(input, init);

    let body: unknown = null;
    if (typeof init?.body === "string") {
      try {
        body = JSON.parse(init.body);
      } catch {
        body = null;
      }
    }
    const s = current();
    const before = progress(s); // a GET only changes state through handle()'s advance()
    const res = handle(s, { method, path: url.pathname, body }, nowS());
    if (method !== "GET" || progress(s) !== before) changed();
    if (res.json !== undefined) {
      return new Response(JSON.stringify(res.json), { status: res.status, headers: { "content-type": "application/json" } });
    }
    return new Response(res.text ?? "", { status: res.status, headers: { "content-type": "text/plain" } });
  };

  window.setInterval(() => {
    if (document.visibilityState === "visible") tick();
  }, TICK_MS);
}

function subscribe(l: () => void) {
  listeners.add(l);
  return () => listeners.delete(l);
}

export function useDemo<T>(select: (s: DemoState) => T): T {
  // the version counter is the snapshot; the state is only read through select
  useSyncExternalStore(subscribe, () => version, () => 0);
  return select(typeof window === "undefined" ? (serverSeed ??= seedState(0)) : current());
}

export const demoActions = {
  reset(): void {
    state = seedState(nowS());
    changed();
  },
  setTraffic(on: boolean): void {
    const s = current();
    s.traffic = on;
    if (on) s.traffic_next_at = nowS();
    changed();
  },
  simulateOrder(): number {
    const s = current();
    const n = simOrder(s, nowS());
    s.todo.order = true;
    changed();
    return n;
  },
  markHistorySeen(): void {
    const s = current();
    if (s.todo.test && !s.todo.history) {
      s.todo.history = true;
      changed();
    }
  },
  enqueue(spec: JobSpec, timing?: Timing): DemoJob {
    const s = current();
    const job = enqueue(s, spec, nowS(), timing);
    // the start page prints with the next order/receipt number; keep the counters ahead of it
    if (spec.ref !== null && spec.kind === "receipt") s.next.receipt = Math.max(s.next.receipt, spec.ref + 1);
    else if (spec.ref !== null && (spec.kind === "slip" || spec.kind === "label")) s.next.order = Math.max(s.next.order, spec.ref + 1);
    changed();
    return job;
  },
};
