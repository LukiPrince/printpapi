// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import { LABEL_SIZES, SIZE_GROUPS, searchSizes, sizeLabel } from "./label-sizes.ts";

const byId = (id: string) => {
  const s = LABEL_SIZES.find((x) => x.id === id);
  assert.ok(s, `size ${id} missing`);
  return s;
};

test("searchSizes finds the 4x8 template first", () => {
  const hits = searchSizes("4x8");
  assert.equal(hits[0].id, "Standard:4x8");
  assert.ok(hits.some((s) => s.id === "Shipping:LabelStockSize 4x8"));
});

test("searchSizes takes the multiplication sign and spaces", () => {
  assert.equal(searchSizes("4 × 8")[0].id, "Standard:4x8");
  assert.equal(searchSizes("4 x 6")[0].id, "Standard:4x6");
});

test("searchSizes finds a Brother roll by its product code", () => {
  assert.deepEqual(searchSizes("DK-11209").map((s) => s.id), ["Brother:DK-11209"]);
  assert.deepEqual(searchSizes("dk-11209").map((s) => s.id), ["Brother:DK-11209"]);
});

test("searchSizes matches rounded millimetres: 57 x 32 is the DYMO multi-purpose label", () => {
  const ids = searchSizes("57 x 32").map((s) => s.id);
  assert.ok(ids.includes("DYMO:30334"));
  assert.ok(ids.includes("DYMO:11354"));
  assert.ok(!ids.includes("Standard:4x6"));
});

test("searchSizes ANDs several words and matches the group", () => {
  const ids = searchSizes("dymo address").map((s) => s.id);
  assert.ok(ids.includes("DYMO:30252"));
  assert.ok(ids.every((id) => id.startsWith("DYMO:")));
  assert.ok(searchSizes("zebra").every((s) => s.group === "Zebra"));
});

test("searchSizes finds nothing for nonsense and everything for an empty query", () => {
  assert.deepEqual(searchSizes("zzzz-no-such-label"), []);
  assert.equal(searchSizes("").length, LABEL_SIZES.length);
  assert.equal(searchSizes("   ").length, LABEL_SIZES.length);
});

test("the catalogue has no duplicate ids", () => {
  const ids = LABEL_SIZES.map((s) => s.id);
  assert.equal(new Set(ids).size, ids.length);
});

test("every manufacturer size names its source, the standard sizes have none", () => {
  for (const s of LABEL_SIZES) {
    if (s.group === "Standard") {
      assert.equal(s.source, undefined, `${s.id} is a nominal size and needs no source`);
    } else {
      assert.ok(s.source, `${s.id} has no source`);
      assert.match(s.source.url, /^https:\/\//, `${s.id} source is not an https link`);
      assert.ok(s.source.label.length > 0);
    }
  }
});

test("every size is in a known group and has sane dimensions", () => {
  for (const s of LABEL_SIZES) {
    assert.ok(SIZE_GROUPS.includes(s.group), `${s.id} has an unknown group`);
    assert.ok(s.w > 0, `${s.id} width`);
    assert.ok(s.h >= 0, `${s.id} height`); // 0 = continuous roll
    assert.ok(s.name.length > 0 && s.code.length > 0);
  }
  for (const g of SIZE_GROUPS) assert.ok(LABEL_SIZES.some((s) => s.group === g), `group ${g} is empty`);
  assert.equal(SIZE_GROUPS[0], "Standard");
});

test("the dimensions carried over from the verified catalogue", () => {
  const dims = (id: string) => [byId(id).w, byId(id).h];
  assert.deepEqual(dims("Standard:4x6"), [101.6, 152.4]);
  assert.deepEqual(dims("Standard:4x8"), [101.6, 203.2]);
  assert.deepEqual(dims("DYMO:30334"), [57.2, 31.8]);
  assert.deepEqual(dims("Brother:DK-11209"), [29, 62]);
  assert.deepEqual(dims("Brother:DK-22205"), [62, 0]);
  assert.deepEqual(dims("Shipping:910-300-400"), [103, 150]);
  assert.equal(byId("Brother:DK-11218").round, true);
  assert.equal(byId("Rollo:Round 2in").round, true);
});

test("the sizes from the second research round", () => {
  const dims = (id: string) => [byId(id).w, byId(id).h];
  assert.deepEqual(dims("DYMO:S0722540"), [57, 32]);
  assert.deepEqual(dims("DYMO:S0904980"), [104, 159]);
  assert.deepEqual(dims("Shipping:910-300-610"), [103, 199]);
  assert.deepEqual(dims("Shipping:910-300-700"), [105, 208]);
  assert.deepEqual(dims("MUNBYN:4x5"), [101, 127]);
  assert.deepEqual(dims("MUNBYN:4x6 roll"), [104, 159]);
  // MUNBYN states 50 mm for its 2 in circles, not the nominal 50.8
  assert.deepEqual(dims("MUNBYN:Round 2in"), [50, 50]);
  assert.equal(byId("MUNBYN:Round 2in").round, true);
  assert.equal(searchSizes("S0722540")[0].id, "DYMO:S0722540");
});

test("continuous rolls are the Brother DK-22xxx and DK-N55224 rows", () => {
  const rolls = LABEL_SIZES.filter((s) => s.h === 0).map((s) => s.code);
  assert.deepEqual(rolls.sort(), ["DK-22205", "DK-22210", "DK-22214", "DK-22223", "DK-22225", "DK-N55224"]);
});

test("sizeLabel", () => {
  assert.equal(sizeLabel(byId("Standard:4x6")), "101.6 × 152.4 mm");
  assert.equal(sizeLabel(byId("DYMO:30334")), "57.2 × 31.8 mm");
  assert.equal(sizeLabel(byId("Brother:DK-11218")), "Ø 24 mm");
  assert.equal(sizeLabel(byId("Brother:DK-22205")), "62 mm wide, free length");
});
