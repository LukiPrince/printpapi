// The harness: the real server binary on a loopback port, a fake agent, throwaway HTTP servers and
// a fault proxy. No HTTP mocks: the client under test talks to the same server a user runs.
import { type ChildProcess, spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import http from "node:http";
import net from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

/** The repository root, from build/testkit/server.js. */
export const ROOT = fileURLToPath(new URL("../../../../", import.meta.url));
export const OPENAPI_FILE = join(ROOT, "crates/server/openapi.json");

function findBinary(): string {
  const exe = process.platform === "win32" ? "printpapi.exe" : "printpapi";
  const fromEnv = process.env.PRINTAPI_BIN;
  const candidates = fromEnv
    ? [fromEnv, fromEnv + ".exe"]
    : [join(ROOT, "target/debug", exe), join(ROOT, "target/release", exe)];
  const found = candidates.find((c) => existsSync(c));
  if (!found) {
    throw new Error("no printpapi binary found (looked for " + candidates.join(", ") + "): run \"cargo build -p printpapi-server\" or set PRINTAPI_BIN");
  }
  return found;
}

/** A free loopback port: bind 0, read the number, release it. */
export function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.once("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address() as net.AddressInfo;
      s.close(() => resolve(port));
    });
  });
}

/** Binds a free loopback port and says which: no gap between choosing and binding it. */
async function listenOnAny(server: http.Server): Promise<number> {
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  return (server.address() as net.AddressInfo).port;
}

export interface Running {
  url: string;
  port: number;
  /** The bootstrap token (root). */
  token: string;
  stop(): Promise<void>;
}

/**
 * Starts the binary. The environment is explicit and not inherited: the server under test must not
 * change with the shell it is started from (docs/server.md lists what else it reads).
 */
export async function startServer(extraEnv: Record<string, string> = {}): Promise<Running> {
  const bin = findBinary();
  const dir = mkdtempSync(join(tmpdir(), "printpapi-client-"));
  const port = await freePort();
  const token = "root-" + Math.random().toString(36).slice(2);
  const env: Record<string, string> = {};
  for (const k of ["PATH", "SystemRoot", "TEMP", "TMP"]) {
    const v = process.env[k];
    if (v !== undefined) env[k] = v;
  }
  Object.assign(env, { PRINTAPI_TOKEN: token, PRINT_DB: join(dir, "p.db"), PRINT_PORT: String(port) }, extraEnv);
  const child: ChildProcess = spawn(bin, [], { env, stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
  let stderr = "";
  child.stderr?.on("data", (d: Buffer) => (stderr += d.toString()));
  const exited = new Promise<void>((resolve) => child.once("exit", () => resolve()));
  await new Promise<void>((resolve, reject) => {
    let out = "";
    const timer = setTimeout(() => reject(new Error("the server did not start within 10 s: " + stderr)), 10_000);
    child.stdout?.on("data", (d: Buffer) => {
      out += d.toString();
      if (out.includes("printpapi listening on :" + port)) {
        clearTimeout(timer);
        resolve();
      }
    });
    child.once("exit", (code) => {
      clearTimeout(timer);
      reject(new Error("the server exited with " + code + ": " + stderr));
    });
  });
  const url = "http://127.0.0.1:" + port;
  const running: Running = {
    url, port, token,
    async stop() {
      child.kill();
      await exited; // Windows keeps the database file locked until the process is gone
      for (let i = 0; ; i++) {
        try {
          rmSync(dir, { recursive: true, force: true });
          return;
        } catch (e) {
          if (i >= 20) throw e;
          await new Promise((r) => setTimeout(r, 100));
        }
      }
    },
  };
  // a binary older than the checked-out document would be tested silently: refuse
  const served = await (await fetch(url + "/openapi.json")).text();
  if (served !== readFileSync(OPENAPI_FILE, "utf8")) {
    await running.stop();
    throw new Error("stale binary: it serves another openapi.json than crates/server/openapi.json, rebuild it (cargo build -p printpapi-server)");
  }
  return running;
}

export interface Reply {
  status: number;
  json: Json;
}
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type Json = any;

/** A raw call: what the harness uses where the client is not under test. */
export async function raw(
  base: string, token: string | undefined, method: string, path: string, body?: unknown, f: typeof fetch = fetch,
): Promise<Reply> {
  const headers: Record<string, string> = {};
  if (token) headers.Authorization = "Bearer " + token;
  const init: RequestInit = { method, headers };
  if (body !== undefined) init.body = JSON.stringify(body);
  const res = await f(base + path, init);
  const text = await res.text();
  return { status: res.status, json: text ? JSON.parse(text) : undefined };
}

export interface Org {
  orgId: number;
  /** A full client key of the org. */
  key: string;
  /** An agent-only key of the org. */
  agentKey: string;
  /** A key with the rights of a name: print, read, ... */
  keyWith(scope: string): Promise<string>;
}

/** A fresh org with its keys: every test works in its own org, so no test sees another's jobs. */
export async function createOrg(s: Running, name: string): Promise<Org> {
  const org = await raw(s.url, s.token, "POST", "/orgs", { name });
  const orgId = org.json.id as number;
  const issue = async (scope: string): Promise<string> => {
    const k = await raw(s.url, s.token, "POST", "/apikeys", { label: scope, org_id: orgId, scope });
    return k.json.key as string;
  };
  return { orgId, key: await issue("full"), agentKey: await issue("agent"), keyWith: issue };
}

export interface AgentJob {
  job_id: number;
  printer_id: number;
  mode: string;
  copies: number;
  options: Json;
  job_uid?: string;
}

/** What the agent next to the printers does, by hand: register, claim, fetch, report. */
export class FakeAgent {
  readonly #url: string;
  readonly #key: string;
  readonly #f: typeof fetch;
  printers: Record<string, number> = {};
  computerId = 0;

  constructor(url: string, key: string, f: typeof fetch = fetch) {
    this.#url = url;
    this.#key = key;
    this.#f = f;
  }

  async register(name: string, printers: { name: string; can_pdf?: boolean; capabilities?: unknown }[]): Promise<this> {
    const r = await raw(this.#url, this.#key, "POST", "/agent/register", { name, printers }, this.#f);
    if (r.status !== 200) throw new Error("register: " + JSON.stringify(r));
    this.computerId = r.json.computer_id;
    this.printers = r.json.printer_ids;
    return this;
  }

  /** The id of a printer by the name it was registered under. */
  printer(name: string): number {
    const id = this.printers[name];
    if (id === undefined) throw new Error("no printer " + name);
    return id;
  }

  /** The next job, or null. Only call it with a job queued: an empty queue is a long poll. */
  async claim(): Promise<AgentJob | null> {
    const res = await this.#f(this.#url + "/agent/jobs", {
      headers: { Authorization: "Bearer " + this.#key, "X-Printapi-Features": "job-uid" },
      signal: AbortSignal.timeout(10_000),
    });
    if (res.status === 204) return null;
    if (res.status !== 200) throw new Error("claim: " + res.status);
    return (await res.json()) as AgentJob;
  }

  async payload(jobId: number): Promise<Uint8Array> {
    const res = await this.#f(this.#url + "/agent/jobs/" + jobId + "/payload", {
      headers: { Authorization: "Bearer " + this.#key },
    });
    if (res.status !== 200) throw new Error("payload: " + res.status);
    return new Uint8Array(await res.arrayBuffer());
  }

  async result(jobId: number, ok: boolean, error?: string): Promise<Reply> {
    return raw(this.#url, this.#key, "POST", "/agent/jobs/" + jobId + "/result", { ok, error }, this.#f);
  }

  /** Claim, fetch the bytes and report the print as done. */
  async print(): Promise<{ job: AgentJob; bytes: Uint8Array }> {
    const job = await this.claim();
    if (!job) throw new Error("nothing to claim");
    const bytes = await this.payload(job.job_id);
    await this.result(job.job_id, true);
    return { job, bytes };
  }
}

// --- throwaway servers ----------------------------------------------------------------------------

export interface Seen {
  method: string;
  path: string;
  headers: http.IncomingHttpHeaders;
  body: string;
}

export interface Throwaway {
  url: string;
  port: number;
  seen: Seen[];
  close(): Promise<void>;
}

/** A loopback HTTP server with a handler of your own that records every request. */
export async function throwaway(
  handler: (req: http.IncomingMessage, res: http.ServerResponse, seen: Seen) => void,
): Promise<Throwaway> {
  const seen: Seen[] = [];
  const sockets = new Set<net.Socket>();
  const server = http.createServer((req, res) => {
    const chunks: Buffer[] = [];
    req.on("data", (c: Buffer) => chunks.push(c));
    req.on("end", () => {
      const s: Seen = { method: req.method ?? "", path: req.url ?? "", headers: req.headers, body: Buffer.concat(chunks).toString() };
      seen.push(s);
      handler(req, res, s);
    });
  });
  server.on("connection", (s) => {
    sockets.add(s);
    s.once("close", () => sockets.delete(s));
  });
  const port = await listenOnAny(server);
  return {
    url: "http://127.0.0.1:" + port, port, seen,
    close: () => new Promise<void>((r) => {
      for (const s of sockets) s.destroy();
      server.close(() => r());
    }),
  };
}

/** A payload server that holds its answer until release(): a slow download the server is in the middle of. */
export async function slowSource(): Promise<Throwaway & { arrived: Promise<void>; release(body: Uint8Array): void }> {
  let arrive!: () => void;
  const arrived = new Promise<void>((r) => (arrive = r));
  const waiting: http.ServerResponse[] = [];
  let released: Uint8Array | undefined;
  const t = await throwaway((_req, res) => {
    arrive();
    if (released) res.end(released);
    else waiting.push(res);
  });
  return Object.assign(t, {
    arrived,
    release(body: Uint8Array) {
      released = body;
      for (const r of waiting.splice(0)) r.end(body);
    },
  });
}

/** Resolves when `cond` holds, polling; rejects after `ms`. */
export async function until(cond: () => boolean, ms = 10_000, what = "condition"): Promise<void> {
  const end = Date.now() + ms;
  while (!cond()) {
    if (Date.now() > end) throw new Error("timed out waiting for " + what);
    await new Promise((r) => setTimeout(r, 10));
  }
}

// --- fault proxy ------------------------------------------------------------------------------------

/** What the proxy does with the n-th request (counted from 1). */
export type Action =
  | "forward"
  | "drop" // the server answers, the answer never arrives: the connection is cut
  | "hang" // the request is swallowed and never answered
  | { status: number; body: string; type?: string }; // answered by the proxy, the server sees nothing

export interface ProxyRequest {
  n: number;
  method: string;
  path: string;
  body: string;
  /** The status the server answered, when the request was forwarded. */
  upstream?: number;
}

export interface FaultProxy {
  url: string;
  requests: ProxyRequest[];
  rule: (r: ProxyRequest) => Action;
  close(): Promise<void>;
}

/** A proxy in front of `target` that breaks requests as told, to test what a client does about it. */
export async function faultProxy(target: string, rule: (r: ProxyRequest) => Action = () => "forward"): Promise<FaultProxy> {
  const to = new URL(target);
  const requests: ProxyRequest[] = [];
  const sockets = new Set<net.Socket>();
  const proxy: FaultProxy = { url: "", requests, rule, close: async () => {} };
  const server = http.createServer((req, res) => {
    const chunks: Buffer[] = [];
    req.on("data", (c: Buffer) => chunks.push(c));
    req.on("end", () => {
      const r: ProxyRequest = { n: requests.length + 1, method: req.method ?? "", path: req.url ?? "", body: Buffer.concat(chunks).toString() };
      requests.push(r);
      const action = proxy.rule(r);
      if (action === "hang") return;
      if (typeof action === "object") {
        res.writeHead(action.status, { "Content-Type": action.type ?? "text/html" });
        res.end(action.body);
        return;
      }
      const headers = { ...req.headers };
      delete headers.host;
      const up = http.request({ host: to.hostname, port: to.port, method: r.method, path: r.path, headers }, (ur) => {
        const out: Buffer[] = [];
        ur.on("data", (c: Buffer) => out.push(c));
        ur.on("end", () => {
          r.upstream = ur.statusCode;
          if (action === "drop") return void req.socket.destroy();
          const h = { ...ur.headers };
          delete h["transfer-encoding"];
          delete h.connection;
          res.writeHead(ur.statusCode ?? 502, h);
          res.end(Buffer.concat(out));
        });
      });
      up.on("error", () => req.socket.destroy());
      up.end(Buffer.concat(chunks));
    });
  });
  server.on("connection", (s) => {
    sockets.add(s);
    s.once("close", () => sockets.delete(s));
  });
  const port = await listenOnAny(server);
  proxy.url = "http://127.0.0.1:" + port;
  proxy.close = () => new Promise<void>((r) => {
    for (const s of sockets) s.destroy();
    server.close(() => r());
  });
  return proxy;
}

/** A tiny valid PNG (a 2x2 image), made here so no binary fixture is kept. */
export async function tinyPng(): Promise<Uint8Array> {
  const { deflateSync } = await import("node:zlib");
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc = (b: Uint8Array) => {
    let c = 0xffffffff;
    for (const x of b) c = crcTable[(c ^ x) & 0xff]! ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const chunk = (type: string, data: Uint8Array) => {
    const body = Buffer.concat([Buffer.from(type), data]);
    const out = Buffer.alloc(8 + data.length + 4);
    out.writeUInt32BE(data.length, 0);
    body.copy(out, 4);
    out.writeUInt32BE(crc(body), 8 + data.length);
    return out;
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(2, 0);
  ihdr.writeUInt32BE(2, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 0; // grayscale
  const rows = Buffer.from([0, 0, 255, 0, 255, 0]);
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr), chunk("IDAT", deflateSync(rows)), chunk("IEND", Buffer.alloc(0)),
  ]);
}

export const PDF_BYTES = new TextEncoder().encode("%PDF-1.4\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF\n");
