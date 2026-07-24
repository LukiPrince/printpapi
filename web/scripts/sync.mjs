// Copy the static export (web/out) into app/web, where the Python server serves it from.
import { cpSync, rmSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const web = dirname(dirname(fileURLToPath(import.meta.url)));
const out = join(web, "out");
const dest = join(web, "..", "app", "web");

if (!existsSync(out)) {
  console.error("web/out missing — run `npm run build` first");
  process.exit(1);
}
rmSync(dest, { recursive: true, force: true });
cpSync(out, dest, { recursive: true });
console.log(`synced ${out} -> ${dest}`);
