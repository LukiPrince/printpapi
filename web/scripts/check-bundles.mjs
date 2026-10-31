// Guards the two builds against each other: the production bundle (out/, and the copy embedded in
// the server binary, ../crates/server/web) must hold no demo code or sample data, and the demo
// bundle (out-demo/) must hold the demo marker, or the module switch silently picked the wrong build.
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

const PROD = ["out", "../crates/server/web"];
const NEEDLES = ["printpapi-demo-build", "Nordlicht", "Back to the tour", "print.example.com", "/overview", "Shop traffic", "Paper tray", "Datenschutz", "/datenschutz"];
// binary assets (fonts, images, icons) cannot hold the strings and may not decode as text
const BINARY = /\.(woff2?|ttf|otf|eot|png|jpe?g|gif|webp|avif|ico|pdf|zip|gz|br|mp4|webm)$/i;

function* files(dir) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) yield* files(p);
    else if (!BINARY.test(p)) yield p;
  }
}

function find(dir, needle) {
  for (const p of files(dir)) if (readFileSync(p, "utf8").includes(needle)) return p;
  return null;
}

function fail(why) {
  console.error(`check-bundles: ${why}`);
  process.exit(1);
}

for (const dir of PROD) {
  if (!existsSync(dir)) fail(`${dir} missing`);
  for (const needle of NEEDLES) {
    const hit = find(dir, needle);
    if (hit) fail(`${dir} contains "${needle}" (${hit})`);
  }
}
if (!existsSync("out-demo")) fail("out-demo missing");
if (!find("out-demo", "printpapi-demo-build")) fail("out-demo lacks printpapi-demo-build");
console.log("check-bundles: ok");
