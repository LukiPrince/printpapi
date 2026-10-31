// Builds the static demo into web/out-demo. Same app, PRINTPAPI_DEMO=1 flips the module switch
// in next.config.ts. Spawns Next's CLI directly (no shell) so the exit code passes through.
// demo/static/* (the Cloudflare _headers file) is copied into the output afterwards.
import { spawnSync } from "node:child_process";
import { cpSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const next = require.resolve("next/dist/bin/next");
const r = spawnSync(process.execPath, [next, "build"], {
  stdio: "inherit",
  env: { ...process.env, PRINTPAPI_DEMO: "1" },
});
if (r.status === 0) cpSync(new URL("../demo/static", import.meta.url), new URL("../out-demo", import.meta.url), { recursive: true });
process.exit(r.status ?? 1);
