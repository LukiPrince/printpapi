import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, it } from "node:test";
import { ROOT } from "../testkit/server.ts";

const CLIENT = join(ROOT, "clients/js");

describe("the package (test 21)", () => {
  it("npm pack lists the built sources, the README, the LICENSE and package.json, and nothing else", () => {
    // shell: on Windows npm is npm.cmd
    const r = spawnSync("npm pack --dry-run --json", { cwd: CLIENT, shell: true, encoding: "utf8" });
    assert.equal(r.status, 0, r.stderr);
    const [pack] = JSON.parse(r.stdout) as { files: { path: string }[] }[];
    const files = (pack?.files ?? []).map((f) => f.path.replace(/\\/g, "/")).sort();
    const own = new Set(["LICENSE", "README.md", "package.json"]);
    for (const f of files) {
      assert.ok(own.has(f) || /^build\/src\/[^/]+\.(js|d\.ts)$/.test(f), "unexpected file in the package: " + f);
    }
    for (const f of [...own, "build/src/index.js", "build/src/index.d.ts", "build/src/client.js", "build/src/types.d.ts"]) {
      assert.ok(files.includes(f), "missing from the package: " + f);
    }
  });

  it("has no runtime dependency, is ESM and names its entry points", () => {
    const pkg = JSON.parse(readFileSync(join(CLIENT, "package.json"), "utf8")) as Record<string, unknown>;
    assert.equal(pkg.dependencies, undefined);
    assert.equal(pkg.type, "module");
    assert.deepEqual(Object.keys(pkg.devDependencies as object).sort(), ["@types/node", "typescript"]);
    assert.deepEqual(pkg.files, ["build/src", "README.md", "LICENSE"]);
  });
});
