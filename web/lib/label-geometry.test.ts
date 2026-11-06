// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import {
  barWidthDots,
  fit,
  fontDots,
  moveSnapped,
  nativeBox,
  overflowMm,
  printsBlack,
  resizeFrom,
  rotateCrop,
  textClipped,
  textFonts,
  wrapLines,
} from "./label-geometry.ts";
import type { LabelBox } from "./api.ts";

const near = (a: number, b: number, eps = 1e-9) =>
  assert.ok(Math.abs(a - b) < eps, `${a} is not within ${eps} of ${b}`);
const nearBox = (a: LabelBox, b: LabelBox) => {
  near(a.x, b.x);
  near(a.y, b.y);
  near(a.w, b.w);
  near(a.h, b.h);
};

const LABEL = { w: 100, h: 150 };

test("fit contain: a wide image fills the width and is centred vertically", () => {
  const b = fit({ w: 1400, h: 800 }, LABEL, "contain");
  nearBox(b, { x: 0, y: (150 - 100 / 1.75) / 2, w: 100, h: 100 / 1.75 });
});

test("fit contain: a tall image fills the height and is centred horizontally", () => {
  const b = fit({ w: 400, h: 1200 }, LABEL, "contain");
  nearBox(b, { x: (100 - 50) / 2, y: 0, w: 50, h: 150 });
});

test("fit cover: the content covers the whole label and spills over the short side", () => {
  const b = fit({ w: 1400, h: 800 }, LABEL, "cover");
  const w = 150 * 1.75;
  nearBox(b, { x: (100 - w) / 2, y: 0, w, h: 150 });
  assert.ok(b.x < 0 && b.x + b.w > LABEL.w);
});

test("fit keeps the aspect ratio in both modes", () => {
  for (const mode of ["contain", "cover"] as const) {
    const b = fit({ w: 1234, h: 567 }, LABEL, mode);
    near(b.w / b.h, 1234 / 567);
  }
});

test("nativeBox: 203 px at 203 dpi is one inch, centred", () => {
  nearBox(nativeBox({ w: 203, h: 203 }, 203, { w: 101.6, h: 152.4 }), {
    x: (101.6 - 25.4) / 2,
    y: (152.4 - 25.4) / 2,
    w: 25.4,
    h: 25.4,
  });
});

test("nativeBox: a bigger image at a higher dpi keeps its physical size", () => {
  const b = nativeBox({ w: 600, h: 300 }, 300, LABEL);
  near(b.w, 50.8);
  near(b.h, 25.4);
});

test("moveSnapped snaps to the centre and reports the guide", () => {
  const box = { x: 0, y: 0, w: 40, h: 40 };
  const r = moveSnapped(box, 30.4, 30, LABEL, 1);
  near(r.box.x, 30); // (100 - 40) / 2
  assert.equal(r.guideX, 50);
  assert.equal(r.guideY, null);
  near(r.box.y, 30);
});

test("moveSnapped snaps to both edges", () => {
  const box = { x: 5, y: 5, w: 40, h: 40 };
  const left = moveSnapped(box, 0.4, 109.7, LABEL, 1);
  near(left.box.x, 0);
  assert.equal(left.guideX, 0);
  near(left.box.y, 110);
  assert.equal(left.guideY, 150);
  const right = moveSnapped(box, 59.6, 0.3, LABEL, 1);
  near(right.box.x, 60);
  assert.equal(right.guideX, 100);
  assert.equal(right.guideY, 0);
});

test("moveSnapped outside the tolerance leaves the position alone", () => {
  const r = moveSnapped({ x: 0, y: 0, w: 40, h: 40 }, 20, 20, LABEL, 1);
  near(r.box.x, 20);
  near(r.box.y, 20);
  assert.equal(r.guideX, null);
  assert.equal(r.guideY, null);
});

test("moveSnapped keeps 3 mm of the content on the label", () => {
  const box = { x: 0, y: 0, w: 40, h: 40 };
  const far = moveSnapped(box, 500, 500, LABEL, 0.5);
  near(far.box.x, 97);
  near(far.box.y, 147);
  const gone = moveSnapped(box, -500, -500, LABEL, 0.5);
  near(gone.box.x, 3 - 40);
  near(gone.box.y, 3 - 40);
});

const START: LabelBox = { x: 10, y: 10, w: 20, h: 10 };
type Corner = "ne" | "nw" | "se" | "sw";
const opposite = (h: Corner, b: LabelBox) => ({
  x: h.includes("w") ? b.x + b.w : b.x,
  y: h.includes("n") ? b.y + b.h : b.y,
});

test("resizeFrom with a locked ratio keeps the opposite corner fixed from every corner", () => {
  const drags: Record<Corner, [number, number]> = {
    se: [10, 3],
    ne: [10, -3],
    sw: [-10, 3],
    nw: [-10, -3],
  };
  for (const [h, [dx, dy]] of Object.entries(drags) as [Corner, [number, number]][]) {
    const b = resizeFrom(h, START, dx, dy, true);
    near(b.w / b.h, 2);
    near(b.w, 30); // the wider relative change wins: 30/20
    const before = opposite(h, START);
    const after = opposite(h, b);
    near(after.x, before.x);
    near(after.y, before.y);
  }
});

test("resizeFrom unlocked changes one side only", () => {
  nearBox(resizeFrom("e", START, 5, 99, false), { x: 10, y: 10, w: 25, h: 10 });
  nearBox(resizeFrom("s", START, 99, 5, false), { x: 10, y: 10, w: 20, h: 15 });
  nearBox(resizeFrom("w", START, -5, 0, false), { x: 5, y: 10, w: 25, h: 10 });
  nearBox(resizeFrom("n", START, 0, -4, false), { x: 10, y: 6, w: 20, h: 14 });
});

test("resizeFrom never goes below 3 mm", () => {
  const free = resizeFrom("se", START, -100, -100, false);
  assert.equal(free.w, 3);
  assert.equal(free.h, 3);
  const locked = resizeFrom("se", START, -100, -100, true);
  near(locked.h, 3);
  near(locked.w, 6);
  near(locked.w / locked.h, 2);
  const nw = resizeFrom("nw", START, 100, 100, true);
  near(nw.x + nw.w, 30); // bottom-right corner stays put
  near(nw.y + nw.h, 20);
});

test("overflowMm: a box on the label does not overflow", () => {
  assert.equal(overflowMm({ x: 0, y: 0, w: 100, h: 150 }, LABEL, false), 0);
  assert.equal(overflowMm({ x: 10, y: 10, w: 20, h: 20 }, LABEL, false), 0);
});

test("overflowMm: the worst side counts on a rectangular label", () => {
  near(overflowMm({ x: -2, y: 0, w: 50, h: 50 }, LABEL, false), 2);
  near(overflowMm({ x: 60, y: 0, w: 50, h: 50 }, LABEL, false), 10);
  near(overflowMm({ x: 0, y: 120, w: 50, h: 50 }, LABEL, false), 20);
  near(overflowMm({ x: -1, y: -7, w: 50, h: 50 }, LABEL, false), 7);
});

test("overflowMm: round labels measure the corners against the circle", () => {
  const disc = { w: 50, h: 50 };
  // the full bounding square pokes out by the corner distance minus the radius
  near(overflowMm({ x: 0, y: 0, w: 50, h: 50 }, disc, true), Math.hypot(25, 25) - 25);
  // the largest square inside the circle fits
  const side = 50 / Math.SQRT2;
  near(overflowMm({ x: 25 - side / 2, y: 25 - side / 2, w: side, h: side }, disc, true), 0, 1e-9);
  // the same box overflows nothing on a rectangular label
  assert.equal(overflowMm({ x: 0, y: 0, w: 50, h: 50 }, disc, false), 0);
});

test("rotateCrop moves each strip with the content", () => {
  const c = { t: 1, r: 2, b: 3, l: 4 };
  assert.deepEqual(rotateCrop(c, true), { t: 4, r: 1, b: 2, l: 3 }); // left edge becomes the top
  assert.deepEqual(rotateCrop(c, false), { t: 2, r: 3, b: 4, l: 1 });
});

test("rotateCrop round-trips", () => {
  const c = { t: 0, r: 5, b: 14.3, l: 2 };
  assert.deepEqual(rotateCrop(rotateCrop(c, true), false), c);
  let x = c;
  for (let i = 0; i < 4; i++) x = rotateCrop(x, true);
  assert.deepEqual(x, c);
});

test("barWidthDots follows the scale and the printer resolution", () => {
  near(barWidthDots(3, 1, 203, 203), 3);
  near(barWidthDots(3, 0.5, 203, 203), 1.5);
  near(barWidthDots(3, 1, 300, 203), (3 * 300) / 203);
});

test("fontDots is limited by the width of the longest line", () => {
  // 203 x 102 dots, one line of 8 characters at advance 0.42: 203 / (8 * 0.42) = 60.4
  assert.equal(fontDots(["A-104872"], { w: 203, h: 102 }, 0.42), 60);
});

test("fontDots is limited by the height when the line is short", () => {
  // two lines: 100 / (2 * 1.1) = 45.45
  assert.equal(fontDots(["ab", "cd"], { w: 1000, h: 100 }, 0.42), 45);
});

test("fontDots stays between 10 and 400 dots", () => {
  assert.equal(fontDots(["x".repeat(500)], { w: 100, h: 100 }, 0.42), 10);
  assert.equal(fontDots(["x"], { w: 9000, h: 9000 }, 0.42), 400);
});

test("wrapLines is a greedy word wrap", () => {
  assert.deepEqual(wrapLines("Module Replacement needed", 12), ["Module", "Replacement", "needed"]);
  assert.deepEqual(wrapLines("ab cd ef gh", 5), ["ab cd", "ef gh"]);
  assert.deepEqual(wrapLines("Module needed", 13), ["Module needed"]);
});

test("wrapLines: a word longer than the limit stays whole, 0 means one line", () => {
  assert.deepEqual(wrapLines("Replacements ok", 5), ["Replacements", "ok"]);
  assert.deepEqual(wrapLines("one two three four five six", 0), ["one two three four five six"]);
  assert.deepEqual(wrapLines("  spaced   out  ", 0), ["spaced out"]);
  assert.deepEqual(wrapLines("", 12), []);
});

test("fit contain on a round label puts the corners on the circle, not past it", () => {
  const round = { w: 24, h: 24 };
  for (const content of [
    { w: 100, h: 100 },
    { w: 200, h: 100 },
    { w: 100, h: 300 },
  ]) {
    const b = fit(content, round, "contain", true);
    assert.ok(overflowMm(b, round, true) < 1e-9, `${content.w}x${content.h} sticks out`);
    near(b.x + b.w / 2, 12);
    near(b.y + b.h / 2, 12);
    near(b.w / b.h, content.w / content.h);
    near(Math.hypot(b.w, b.h), 24, 1e-9); // the largest rectangle of that shape: its diagonal is the diameter
  }
  // with the round flag off, a rectangular label is fitted as before
  nearBox(fit({ w: 100, h: 100 }, { w: 24, h: 24 }, "contain"), { x: 0, y: 0, w: 24, h: 24 });
});

test("printsBlack uses the luma the server uses (Rec. 709 integer weights, floor, black below 140)", () => {
  assert.equal(printsBlack(0, 0, 0), true);
  assert.equal(printsBlack(255, 255, 255), false);
  assert.equal(printsBlack(139, 139, 139), true);
  assert.equal(printsBlack(140, 140, 140), false);
  // image 0.25 to_luma_alpha8: green (0,200,0) is 143, so it prints white
  assert.equal(printsBlack(0, 200, 0), false);
  // light red (255,102,102) is 134, so it prints black
  assert.equal(printsBlack(255, 102, 102), true);
});

test("textFonts gives the font height for ZPL (advance 0.42) and PDF (0.72)", () => {
  assert.deepEqual(textFonts(["A-104872"], { w: 672, h: 336 }), { zpl: 200, pdf: 117 });
  // short text is limited by the height in both
  assert.deepEqual(textFonts(["B1"], { w: 672, h: 336 }), { zpl: 305, pdf: 305 });
});

test("textClipped is true when the text cannot get small enough for the box", () => {
  // 1 x 0.5 inch at 203 dpi, 60 characters on one line: 170 dots wide, the font would be under 10
  assert.equal(textClipped(["x".repeat(60)], { w: 170, h: 100 }), true);
  assert.equal(textClipped(["AB"], { w: 170, h: 100 }), false);
  // too many lines: 30 lines in 300 dots is under 10 dots a line
  assert.equal(textClipped(Array.from({ length: 30 }, () => "ab"), { w: 400, h: 300 }), true);
  // exactly 10 dots still fits (PDF has the wider advance)
  assert.equal(textClipped(["x".repeat(10)], { w: 10 * 10 * 0.72, h: 100 }), false);
  assert.equal(textClipped([], { w: 100, h: 100 }), false);
});
