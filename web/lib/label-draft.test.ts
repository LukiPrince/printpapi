// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import {
  applySize,
  draftFromTemplate,
  forPicture,
  forSample,
  isRound,
  jobExample,
  labelDots,
  maxMm,
  nameProblem,
  newDraft,
  preparedSize,
  previewLines,
  setContent,
  setCrop,
  setLabelDim,
  setPlace,
  specKey,
  stashDraft,
  switchContent,
  textBox,
  toSpec,
  turn,
  unstashDraft,
  type BoxMemory,
  type Draft,
} from "./label-draft.ts";
import { overflowMm } from "./label-geometry.ts";
import { LABEL_SIZES } from "./label-sizes.ts";
import type { LabelSpec } from "./api.ts";

const near = (a: number, b: number, eps = 1e-6) =>
  assert.ok(Math.abs(a - b) < eps, `${a} is not within ${eps} of ${b}`);
const size = (id: string) => {
  const s = LABEL_SIZES.find((x) => x.id === id);
  assert.ok(s, `size ${id} missing`);
  return s;
};

test("a new draft is the carrier label setup on a 4x6 label", () => {
  const spec = toSpec(newDraft());
  assert.equal(spec.content, "image");
  assert.deepEqual(spec.label_mm, { w: 101.6, h: 152.4 });
  assert.equal(spec.dpi, 203);
  assert.equal(spec.rotate, 90);
  assert.equal(spec.crop_percent.b, 14.3);
  assert.equal(spec.size_ref, "Standard:4x6");
  // the sample is 1400 x 800 px: turned and cropped it is 800 x 1200 px, one dot per pixel
  near(spec.place_mm.w, (800 / 203) * 25.4, 0.1);
  near(spec.place_mm.h, (1200 / 203) * 25.4, 0.2);
  assert.equal("text" in spec, false, "an image template carries no text settings");
});

test("a stored template survives the round trip through a draft", () => {
  const stored: LabelSpec = {
    name: "order-no",
    content: "text",
    label_mm: { w: 25.4, h: 12.7 },
    dpi: 300,
    rotate: 0,
    crop_percent: { t: 0, r: 0, b: 0, l: 0 },
    place_mm: { x: 1.5, y: 1, w: 22.4, h: 10.7 },
    text: { wrap_chars: 12, lines_per_label: 2 },
    zpl_darkness: 30,
    size_ref: "Standard:1x0.5",
  };
  assert.deepEqual(toSpec(draftFromTemplate(stored)), stored);
});

test("optional fields that are null or unset are left out, the server refuses null", () => {
  const stored = {
    name: "plain",
    content: "image",
    label_mm: { w: 100, h: 50 },
    dpi: 203,
    rotate: 0,
    crop_percent: { t: 0, r: 0, b: 0, l: 0 },
    place_mm: { x: 0, y: 0, w: 100, h: 50 },
    zpl_darkness: null,
    size_ref: null,
  } as unknown as LabelSpec;
  const spec = toSpec(draftFromTemplate(stored));
  assert.equal("zpl_darkness" in spec, false);
  assert.equal("size_ref" in spec, false);
  assert.equal(JSON.stringify(spec).includes("null"), false);
});

test("toSpec rounds to two decimals so the dirty check does not flicker", () => {
  const d = newDraft();
  const a = specKey({ ...d, place: { ...d.place, x: 1.00001 } });
  const b = specKey({ ...d, place: { ...d.place, x: 1.00004 } });
  assert.equal(a, b);
});

test("preparedSize applies the turn first, then the crop", () => {
  const p = preparedSize({ w: 1400, h: 800 }, 90, { t: 0, r: 0, b: 14.3, l: 0 });
  near(p.w, 800);
  near(p.h, 1400 * (1 - 0.143), 1e-6);
  assert.deepEqual(preparedSize({ w: 100, h: 50 }, 0, { t: 0, r: 0, b: 0, l: 0 }), { w: 100, h: 50 });
  assert.deepEqual(preparedSize({ w: 100, h: 50 }, 180, { t: 0, r: 0, b: 0, l: 0 }), { w: 100, h: 50 });
});

test("cropping the bottom keeps the top edge where it was", () => {
  const d = { ...newDraft(), crop: { t: 0, r: 0, b: 0, l: 0 }, place: { x: 5, y: 10, w: 80, h: 100 } };
  const c = setCrop(d, "b", 20);
  near(c.place.y, 10);
  near(c.place.x, 5);
  near(c.place.h, 80);
  near(c.place.w, 80);
  assert.equal(c.crop.b, 20);
});

test("cropping the left moves what is left to the same spot on the label", () => {
  const d = { ...newDraft(), crop: { t: 0, r: 0, b: 0, l: 0 }, place: { x: 5, y: 10, w: 80, h: 100 } };
  const c = setCrop(d, "l", 25);
  near(c.place.x, 5 + 20); // a quarter of 80 mm is gone from the left
  near(c.place.w, 60);
  near(c.place.y, 10);
});

test("cropping again after a crop works from the uncropped size", () => {
  const once = setCrop(
    { ...newDraft(), crop: { t: 0, r: 0, b: 0, l: 0 }, place: { x: 0, y: 0, w: 100, h: 100 } },
    "t",
    10,
  );
  const twice = setCrop(once, "t", 20);
  near(twice.place.y, 20);
  near(twice.place.h, 80);
});

test("a crop never takes everything", () => {
  const d = { ...newDraft(), crop: { t: 0, r: 0, b: 60, l: 0 } };
  assert.equal(setCrop(d, "t", 90).crop.t, 39, "t + b stays under 100");
  assert.equal(setCrop(d, "l", 99).crop.l, 95, "one strip is at most 95");
  assert.equal(setCrop(d, "l", -4).crop.l, 0);
});

test("turning keeps the centre and swaps the box, and the crop travels with its edge", () => {
  const d = {
    ...newDraft(),
    rotate: 0 as const,
    crop: { t: 1, r: 2, b: 3, l: 4 },
    place: { x: 10, y: 20, w: 40, h: 60 },
  };
  const cw = turn(d, true);
  assert.equal(cw.rotate, 90);
  assert.deepEqual(cw.crop, { t: 4, r: 1, b: 2, l: 3 });
  near(cw.place.x + cw.place.w / 2, 30);
  near(cw.place.y + cw.place.h / 2, 50);
  near(cw.place.w, 60);
  near(cw.place.h, 40);
  const back = turn(cw, false);
  assert.equal(back.rotate, 0);
  assert.deepEqual(back.crop, d.crop);
  near(back.place.x, 10);
  near(back.place.h, 60);
  assert.equal(turn(turn(turn(turn(d, false), false), false), false).rotate, 0);
  assert.equal(turn(d, false).rotate, 270);
});

test("a locked width keeps the ratio, an unlocked one does not", () => {
  const d = { ...newDraft(), place: { x: 0, y: 0, w: 40, h: 20 } };
  const locked = setPlace(d, "w", 80, true);
  near(locked.place.w, 80);
  near(locked.place.h, 40);
  const free = setPlace(d, "w", 80, false);
  near(free.place.h, 20);
  const y = setPlace(d, "y", -7, true);
  near(y.place.y, -7);
  near(y.place.h, 20);
});

test("the content is never smaller than 3 mm", () => {
  const d = { ...newDraft(), place: { x: 0, y: 0, w: 40, h: 20 } };
  assert.equal(setPlace(d, "w", 0.5, false).place.w, 3);
  const l = setPlace(d, "h", 0.5, true);
  assert.ok(l.place.h >= 3 && l.place.w >= 3);
});

test("picking a catalogue size refits the content and remembers the size", () => {
  const d = applySize(newDraft(), size("Standard:4x8"));
  assert.deepEqual(d.label, { w: 101.6, h: 203.2 });
  assert.equal(d.sizeRef, "Standard:4x8");
  assert.equal(isRound(d), false);
  // the content keeps its ratio, centred
  near(d.place.w / d.place.h, newDraft().place.w / newDraft().place.h, 1e-6);
  near(d.place.x + d.place.w / 2, 101.6 / 2);
  near(d.place.y + d.place.h / 2, 203.2 / 2);
  assert.ok(d.place.w <= 101.6 + 1e-9 && d.place.h <= 203.2 + 1e-9);
});

test("a continuous roll starts at 60 mm and a round label is round", () => {
  const roll = applySize(newDraft(), size("Brother:DK-22205"));
  assert.deepEqual(roll.label, { w: 62, h: 60 });
  const round = applySize(newDraft(), size("Brother:DK-11218"));
  assert.equal(isRound(round), true);
});

test("typing a custom size drops the catalogue reference and the round flag", () => {
  const round = applySize(newDraft(), size("Brother:DK-11218"));
  const custom = setLabelDim(round, "w", 30);
  assert.equal(custom.sizeRef, undefined);
  assert.equal(isRound(custom), false);
  assert.equal(custom.label.w, 30);
});

test("typing the length of a continuous roll keeps its reference", () => {
  const roll = applySize(newDraft(), size("Brother:DK-22205"));
  const longer = setLabelDim(roll, "h", 90);
  assert.equal(longer.sizeRef, "Brother:DK-22205");
  assert.equal(longer.label.h, 90);
  const wider = setLabelDim(roll, "w", 40);
  assert.equal(wider.sizeRef, undefined);
});

test("the label stays within the 8000 dot limit of the server", () => {
  for (const dpi of [100, 203, 300, 600]) {
    const mm = maxMm(dpi);
    assert.ok(Math.round((mm / 25.4) * dpi) <= 8000, `${mm} mm at ${dpi} dpi`);
    assert.ok(Math.round(((mm + 1) / 25.4) * dpi) > 8000, `${mm} mm is not the limit at ${dpi} dpi`);
  }
  const d = setLabelDim(newDraft(), "w", 5000);
  assert.equal(d.label.w, maxMm(203));
  assert.equal(setLabelDim(newDraft(), "h", 0).label.h, 5, "never below 5 mm");
});

test("labelDots follows the printer resolution", () => {
  assert.deepEqual(labelDots(newDraft()), { w: 812, h: 1218 });
  assert.deepEqual(labelDots({ ...newDraft(), dpi: 300 }), { w: 1200, h: 1800 });
});

test("switching to text insets the box, switching back refits the image", () => {
  const upright = { ...newDraft(), rotate: 0 as const, crop: { t: 0, r: 0, b: 0, l: 0 } };
  const text = setContent(upright, "text", { w: 800, h: 1200 });
  assert.equal(text.content, "text");
  assert.ok(text.place.x > 0 && text.place.y > 0);
  near(text.place.x + text.place.w, 101.6 - text.place.x);
  near(text.place.y + text.place.h, 152.4 - text.place.y);
  const image = setContent(text, "image", { w: 800, h: 1200 });
  assert.equal(image.content, "image");
  near(image.place.w / image.place.h, 800 / 1200);
  assert.ok(image.place.h <= 152.4 + 1e-9);
});

test("the template name follows the server's rule", () => {
  assert.equal(nameProblem("shipping-4x6"), null);
  assert.equal(nameProblem("Größe 4.5_a"), null);
  assert.match(nameProblem("")!, /name/i);
  assert.match(nameProblem("   ")!, /name/i);
  assert.match(nameProblem("a".repeat(65))!, /64/);
  assert.match(nameProblem("a/b")!, /letters/i);
  assert.match(nameProblem("a^b")!, /letters/i);
  assert.equal(nameProblem("a".repeat(64)), null);
});

test("the preview shows the first label's lines, with ZPL control characters gone", () => {
  assert.deepEqual(previewLines("Module Replacement needed", { wrap_chars: 12, lines_per_label: 0 }), [
    "Module",
    "Replacement",
    "needed",
  ]);
  assert.deepEqual(previewLines("Module Replacement needed", { wrap_chars: 12, lines_per_label: 2 }), [
    "Module",
    "Replacement",
  ]);
  assert.deepEqual(previewLines("B1^XZ^XA^PQ9999", { wrap_chars: 0, lines_per_label: 0 }), ["B1 XZ XA PQ9999"]);
  assert.deepEqual(previewLines("   ", { wrap_chars: 0, lines_per_label: 0 }), []);
});

test("the job example names the template and uses the matching job type", () => {
  const image = jobExample({ ...newDraft(), name: "shipping-4x6" }, "https://print.example.org");
  assert.match(image, /curl -X POST https:\/\/print\.example\.org\/jobs/);
  assert.match(image, /"type": "label_uri"/);
  assert.match(image, /"template": "shipping-4x6"/);
  assert.match(image, /Authorization: Bearer \$PRINTPAPI_TOKEN/);
  const text = jobExample({ ...newDraft(), name: "order no", content: "text" }, "https://x.test", "A-1");
  assert.match(text, /"type": "label_text"/);
  assert.match(text, /"text": "A-1"/);
  assert.match(text, /"template": "order no"/);
  assert.match(jobExample({ ...newDraft(), name: "  " }, "https://x.test"), /"template": "my-template"/);
});

test("a new template with the user's own upright picture starts unturned and uncropped", () => {
  const d = forPicture(newDraft(), { w: 812, h: 1218 });
  assert.equal(d.rotate, 0);
  assert.deepEqual(d.crop, { t: 0, r: 0, b: 0, l: 0 });
  // the picture fills the 4x6 label
  near(d.place.w, 101.6, 1e-6);
  near(d.place.h, 152.4, 1e-6);
  assert.equal(overflowMm(d.place, d.label, false), 0);
});

test("going back to the built-in sample restores the carrier label setup", () => {
  const own = forPicture(newDraft(), { w: 812, h: 1218 });
  assert.deepEqual(toSpec(forSample(own)), toSpec(newDraft()));
});

test("a text template is not touched by picking a picture", () => {
  const t = setContent(newDraft(), "text", { w: 800, h: 1200 });
  assert.equal(forPicture(t, { w: 10, h: 10 }), t);
});

const plain = (over: Partial<Draft>): Draft => ({
  ...newDraft(),
  rotate: 0,
  crop: { t: 0, r: 0, b: 0, l: 0 },
  label: { w: 100, h: 100 },
  sizeRef: undefined,
  ...over,
});

test("typing a size keeps a box that still fits, so the content does not jump", () => {
  const d = plain({ place: { x: 5, y: 5, w: 20, h: 20 } });
  const taller = setLabelDim(d, "h", 101);
  assert.deepEqual(taller.place, { x: 5, y: 5, w: 20, h: 20 });
  assert.equal(taller.label.h, 101);
  assert.deepEqual(setLabelDim(d, "w", 120).place, d.place);
});

test("typing a size that makes the box too big shrinks it into the label, keeping its shape", () => {
  const d = plain({ place: { x: 0, y: 0, w: 100, h: 50 } });
  const narrow = setLabelDim(d, "w", 40);
  near(narrow.place.w, 40);
  near(narrow.place.h, 20);
  near(narrow.place.x, 0);
  near(narrow.place.y, 40); // centred again
});

test("typing a size that leaves the box off the label brings it back", () => {
  const d = plain({ place: { x: 80, y: 80, w: 20, h: 20 } });
  const small = setLabelDim(setLabelDim(d, "w", 50), "h", 50);
  assert.ok(small.place.x + small.place.w <= 50 + 1e-9 && small.place.x >= 0);
  assert.ok(small.place.y + small.place.h <= 50 + 1e-9 && small.place.y >= 0);
});

test("typing a size keeps a text box the user placed while it fits", () => {
  const t: Draft = {
    ...setContent(newDraft(), "text", { w: 800, h: 1200 }),
    place: { x: 10, y: 10, w: 50, h: 30 },
  };
  assert.deepEqual(setLabelDim(t, "h", 160).place, t.place);
  const tiny = setLabelDim(t, "w", 20);
  assert.deepEqual(tiny.place, textBox(tiny.label));
});

test("a catalogue size still refits the content", () => {
  const d = { ...newDraft(), place: { x: 1, y: 1, w: 10, h: 10 } };
  const a = applySize(d, size("Standard:4x8"));
  near(a.place.x + a.place.w / 2, 101.6 / 2);
});

test("on a round label Fit and the text box stay inside the circle", () => {
  const round = size("Brother:DK-11218");
  const image = applySize(newDraft(), round);
  assert.ok(overflowMm(image.place, image.label, true) < 0.05, "image");
  const text = applySize(setContent(newDraft(), "text", { w: 800, h: 1200 }), round);
  assert.ok(overflowMm(text.place, text.label, true) < 0.05, "text");
  assert.ok(overflowMm(textBox({ w: 24, h: 24 }, true), { w: 24, h: 24 }, true) < 0.05);
  // on a rectangular label the text box is unchanged
  assert.deepEqual(textBox({ w: 100, h: 50 }), { x: 8, y: 4, w: 84, h: 42 });
});

test("switching to Text and back restores the box the image had", () => {
  const image = { ...newDraft(), place: { x: 10, y: 20, w: 40, h: 60 } };
  const a = switchContent(image, "text", { w: 800, h: 1200 }, {});
  assert.equal(a.draft.content, "text");
  assert.deepEqual(a.draft.place, textBox(a.draft.label));
  const b = switchContent(a.draft, "image", { w: 800, h: 1200 }, a.memory);
  assert.equal(b.draft.content, "image");
  assert.deepEqual(b.draft.place, { x: 10, y: 20, w: 40, h: 60 });
  // a text box the user moved comes back too
  const moved = switchContent({ ...a.draft, place: { x: 1, y: 2, w: 3, h: 4 } }, "image", { w: 800, h: 1200 }, b.memory);
  const again = switchContent(moved.draft, "text", { w: 800, h: 1200 }, moved.memory);
  assert.deepEqual(again.draft.place, { x: 1, y: 2, w: 3, h: 4 });
});

test("a remembered box is dropped when the label changed in between", () => {
  const image = { ...newDraft(), place: { x: 10, y: 20, w: 40, h: 60 } };
  const a = switchContent(image, "text", { w: 800, h: 1200 }, {});
  const resized = setLabelDim(a.draft, "w", 50);
  const b = switchContent(resized, "image", { w: 800, h: 1200 }, a.memory as BoxMemory);
  assert.notDeepEqual(b.draft.place, { x: 10, y: 20, w: 40, h: 60 });
  assert.ok(b.draft.place.w <= 50 + 1e-9);
});

test("switching to the content it already has changes nothing", () => {
  const d = newDraft();
  assert.equal(switchContent(d, "image", { w: 1, h: 1 }, {}).draft, d);
});

class MemoryStorage {
  data = new Map<string, string>();
  getItem(k: string) {
    return this.data.get(k) ?? null;
  }
  setItem(k: string, v: string) {
    this.data.set(k, v);
  }
  removeItem(k: string) {
    this.data.delete(k);
  }
}

test("an unsaved draft survives in session storage and comes back as it was", () => {
  const store = new MemoryStorage();
  const d = { ...newDraft(), name: "half-done", place: { x: 3, y: 4, w: 50, h: 60 } };
  stashDraft(store, null, d);
  assert.deepEqual(unstashDraft(store, null), d);
  assert.equal(unstashDraft(store, 7), null, "another template has its own slot");
  stashDraft(store, null, null);
  assert.equal(unstashDraft(store, null), null);
});

test("a damaged or foreign stash is ignored, and a broken storage does not throw", () => {
  const store = new MemoryStorage();
  store.setItem("printpapi.label-draft.new", "{not json");
  assert.equal(unstashDraft(store, null), null);
  store.setItem("printpapi.label-draft.new", JSON.stringify({ name: 3 }));
  assert.equal(unstashDraft(store, null), null);
  const blocked = () => {
    throw new Error("blocked");
  };
  const broken = { getItem: blocked, setItem: blocked, removeItem: blocked };
  assert.equal(unstashDraft(broken, null), null);
  assert.doesNotThrow(() => stashDraft(broken, null, newDraft()));
  assert.equal(unstashDraft(null, null), null);
});
