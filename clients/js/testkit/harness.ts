// One server, one recorded client per test file. The recorder checks every exchange against
// crates/server/openapi.json, so a test that passes also proved the document right.
import { readFileSync } from "node:fs";
import { Printpapi } from "../src/index.ts";
import type { ClientOptions } from "../src/index.ts";
import { type Json, Recorder, assertSupported } from "./contract.ts";
import { OPENAPI_FILE, type Running, startServer } from "./server.ts";

export interface Harness {
  server: Running;
  doc: Json;
  recorder: Recorder;
  /** A client of this server whose every call is checked against the document. */
  client(apiKey: string, options?: Partial<ClientOptions>): Printpapi;
  stop(): Promise<void>;
}

export async function harness(extraEnv: Record<string, string> = {}): Promise<Harness> {
  const doc = JSON.parse(readFileSync(OPENAPI_FILE, "utf8")) as Json;
  assertSupported(doc);
  const server = await startServer(extraEnv);
  const recorder = new Recorder(doc, server.url);
  return {
    server, doc, recorder,
    client: (apiKey, options = {}) => new Printpapi({ baseUrl: server.url, apiKey, fetch: recorder.fetch, ...options }),
    async stop() {
      await server.stop();
      recorder.assertClean();
    },
  };
}
