// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// Builds the window of the agent setup into web/out-setup. Same tree as the dashboard,
// PRINTPAPI_SETUP=1 switches next.config.ts to the page.setup.tsx files. Spawns Next's CLI
// directly (no shell) so the exit code passes through.
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const next = require.resolve("next/dist/bin/next");
const r = spawnSync(process.execPath, [next, "build"], {
  stdio: "inherit",
  env: { ...process.env, PRINTPAPI_SETUP: "1" },
});
process.exit(r.status ?? 1);
