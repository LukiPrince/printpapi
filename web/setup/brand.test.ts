// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The white-label parts of the window. They are React components that a plain test run cannot draw,
// so what must hold is pinned in the source, like step-connect.test.ts does.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const read = (name: string) => readFileSync(new URL(name, import.meta.url), "utf8");
const brand = read("./brand.tsx");
const bits = read("./bits.tsx");
const home = read("./home.tsx");
const settings = read("./settings.tsx");
const api = read("./api.ts");

test("the provider draws its children at once and never gates them on the brand", () => {
  const provider = brand.slice(brand.indexOf("export function BrandProvider"), brand.indexOf("export function BrandMark"));
  // the stock first paint is what it always was: the children are rendered with no brand first
  assert.match(provider, /<BrandContext\.Provider value=\{state\}>\{children\}<\/BrandContext\.Provider>/);
  assert.ok(!/state\.ready\s*\?/.test(provider) && !/if \(!state\.ready\)/.test(provider), "no early return before the answer");
});

test("a branded window is shown after the brand was drawn, a stock window never asks", () => {
  const provider = brand.slice(brand.indexOf("export function BrandProvider"), brand.indexOf("export function BrandMark"));
  assert.match(provider, /if \(state\.brand \|\| state\.error\) void setup\.showWindow\(\)/);
  assert.match(provider, /\[state\.brand, state\.error\]/, "after the commit that holds the brand or the failure");
  // a failure is shown, not turned into a stock window
  assert.match(provider, /error: errorText\(toSetupError\(e\)\)/);
  assert.match(home, /if \(brandError\)/);
});

test("the icon travels as raw bytes and becomes a data address in the window", () => {
  assert.match(brand, /pngDataUri\(await setup\.brandIcon\(\)\)/);
  assert.match(api, /brandIcon: \(\) => call<ArrayBuffer>\("brand_icon"\)/);
  assert.ok(!/blob:|URL\.createObjectURL/.test(brand), "blob: is outside the content security policy");
});

test("the sidebar label of a brand fits a 20 character single word and wraps inside it", () => {
  assert.match(brand, /min-w-0 break-words text-sm leading-tight/);
  assert.match(bits, /\{brand \? \(\s*<BrandMark/);
  assert.match(bits, /\{brand && <BrandFooter brand=\{brand\} \/>\}/, "the footer is on every screen the shell draws");
});

test("the notice is the shell's text, shown once in About, and a long client name wraps", () => {
  assert.match(brand, /\{brand\.notice\}/);
  assert.equal((brand.match(/\{brand\.notice\}/g) ?? []).length, 2, "the footer and About, nowhere else");
  assert.ok(!/Operated for/.test(brand), "the client is in the notice the shell wrote, not in a second sentence");
  assert.match(brand, /min-w-0 rounded-md[^"]*\[overflow-wrap:anywhere\]/, "the footer button");
  assert.match(brand, /<DialogDescription className="\[overflow-wrap:anywhere\]">\{brand\.notice\}/, "About");
  assert.match(brand, /setup\.licenseText\(\)/);
  assert.match(brand, /ScrollArea/);
  assert.ok(!/open source/i.test(brand), "source available, never open source");
});

test("a brand has no updates section and no update line in the overview", () => {
  assert.match(settings, /!\(brand && s\.id === "updates"\)/);
  assert.match(settings, /update=\{!brand && info && updateAvailable\(info\) \? info : null\}/);
  assert.match(settings, /section === "updates" && !brand/);
});

test("a brand keeps the one button that installs a newer build of itself", () => {
  // no Updates section, but running the build of a newer version of the brand must still update
  assert.match(settings, /installHereInfo\(brand !== null, top !== undefined, info, hereInfo\)/);
  const here = settings.slice(settings.indexOf("function InstallHere"), settings.indexOf("export function Settings"));
  assert.match(here, /void apply\(\{\}\)/);
  assert.match(here, /Install \{info\.own\} from this setup/);
});

test("the notice of an install from this setup, and of a take-over, stays until the run is over", () => {
  const here = settings.slice(settings.indexOf("function InstallHere"), settings.indexOf("export function Settings"));
  // the click keeps what the notice showed, so the poll that reads the new marker cannot unmount it
  assert.match(here, /onStart\(\);\s*void apply\(\{\}\)/);
  assert.match(settings, /onStart=\{\(\) => setHereInfo\(shown\)\}/);
  // the button is a retry after a failure, and gone while a run is going or has ended
  assert.match(here, /run\.at === "idle" \|\| run\.at === "failed"/);
  const take = home.slice(home.indexOf("function TakeOver"), home.indexOf("function CloseButton"));
  assert.match(home, /top=\{\s*boot\.install === "adoptable" \|\| takingOver \?/, "kept mounted from the click on");
  assert.match(take, /onStart\(\);\s*void apply\(\{\}\)/);
  assert.match(take, /adoptable && \(run\.at === "idle" \|\| run\.at === "failed"\)/, "no offer once the install is managed");
  assert.match(take, /<RunNotice/);
  assert.ok(!/does not wait for the agent/.test(home), "the comment says what it does");
});

test("every change reaches Home, so the take-over offer ends with the first change of any kind", () => {
  const apply = read("./settings-apply.tsx");
  assert.match(apply, /const since = await setup\.applyPatch\(patch\);\s*setBaseline\(since\);\s*installChanged\(\);/, "right after the install returned, not when the agent is online");
  const finish = read("./step-finish.tsx");
  assert.match(finish, /: await setup\.applyPatch\(patchFor\(\{ printers: changeFrom \}, \{ printers: selected \}\)\);\s*\/\/[^\n]*\s*installChanged\(\);/, "the wizard of printers and connection, as soon as the install returned");
  assert.ok(!/installChanged/.test(settings), "not only when the wizard's Done is clicked");
  assert.match(home, /<InstallChangedProvider value=\{\(\) => void load\(\)\}>/);
  assert.ok(!/setBoot\(\{ at: "loading" \}\);\s*void load\(\);\s*\}\}\s*\/>\s*\) : undefined/.test(home), "Take over does not blank the window");
});

test("the other_brand notice names no maker that cannot occur", () => {
  const other = home.slice(home.indexOf('boot.install === "other_brand"'), home.indexOf('boot.install === "unsupported"'));
  assert.ok(!/standard printpapi setup/.test(other), "an install of the stock setup is adoptable or managed, never other_brand");
  assert.match(other, /another white-label build, or its marker file cannot be\s+read/);
});

test("other_brand offers the notice and the removal, adoptable the one button", () => {
  const other = home.slice(home.indexOf('boot.install === "other_brand"'), home.indexOf('boot.install === "unsupported"'));
  assert.match(other, /<Notice tone="warn">/);
  assert.match(other, /<RemoveSection onRemoved=\{\(\) => setView\("removed"\)\} \/>/);
  assert.match(other, /<CloseButton \/>/);
  assert.match(home, /boot\.install === "adoptable"/);
  assert.match(home, /void apply\(\{\}\)/, "the same empty change as the SumatraPDF section makes");
  assert.match(home, /Take over/);
});

test("the server field of a fixed brand cannot be edited and says why", () => {
  const connect = read("./step-connect.tsx");
  assert.equal((connect.match(/readOnly=\{fixedHint !== null\}/g) ?? []).length, 2, "both forms");
  assert.equal((connect.match(/startServer\(initial, replacing, brand\)/g) ?? []).length, 3, "every start of the field");
});

test("only api.ts talks to the shell", () => {
  assert.ok(!/@tauri-apps/.test(brand) && !/@tauri-apps/.test(home) && !/@tauri-apps/.test(bits));
});
