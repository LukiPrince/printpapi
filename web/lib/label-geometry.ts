// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// Placement maths for the label editor. Pure: millimetres in, millimetres out, no DOM.
// The geometry (snapping, the 3 mm floor, the corner test for round labels) follows the
// design prototype; fontDots and wrapLines mirror crates/server/src/label.rs so the preview
// and the printed label agree.

import type { LabelBox, LabelSpec } from "./api";

export type Size = { w: number; h: number };
type Handle = "n" | "s" | "e" | "w" | "ne" | "nw" | "se" | "sw";

const MM_PER_INCH = 25.4;
/** Smallest edge the content may be resized to. */
const MIN_MM = 3;
/** So much of the content stays on the label when it is dragged away. */
const KEEP_MM = 3;
/** Text block height per line, relative to the font height (label.rs: box_h / (lines * 1.1)). */
const LINE_HEIGHT = 1.1;
const FONT_MIN = 10;
const FONT_MAX = 400;
/** Glyph width relative to the font height: the ZPL font (label.rs zpl_text) and the PDF font
 *  (Helvetica Bold, label.rs pdf_text). */
export const ZPL_ADVANCE = 0.42;
export const PDF_ADVANCE = 0.72;
/** Printer threshold, label.rs: a pixel prints black when its luma is under this. */
const BLACK_BELOW = 140;

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/** Scale `content` into `label` keeping its ratio, centred. `contain` fits inside, `cover` fills.
 *  On a `round` label, `contain` makes the diagonal of the box the diameter, so the corners sit on
 *  the circle instead of past it. */
export function fit(
  content: Size,
  label: Size,
  mode: "contain" | "cover",
  round = false,
): LabelBox {
  const a = content.w / content.h;
  if (round && mode === "contain") {
    const h = Math.min(label.w, label.h) / Math.sqrt(1 + a * a);
    const w = a * h;
    return { x: (label.w - w) / 2, y: (label.h - h) / 2, w, h };
  }
  const labelIsWider = a > label.w / label.h;
  // contain: a wide content is limited by the width. cover: by the height.
  const byWidth = (mode === "contain") === labelIsWider;
  const w = byWidth ? label.w : label.h * a;
  const h = byWidth ? label.w / a : label.h;
  return { x: (label.w - w) / 2, y: (label.h - h) / 2, w, h };
}

/** The content at its own pixel size for the printer resolution, centred. */
export function nativeBox(contentPx: Size, dpi: number, label: Size): LabelBox {
  const w = (contentPx.w / dpi) * MM_PER_INCH;
  const h = (contentPx.h / dpi) * MM_PER_INCH;
  return { x: (label.w - w) / 2, y: (label.h - h) / 2, w, h };
}

/**
 * Move the box to (x, y), snapping its edges and centre to the label's within `tolMm`.
 * Returns the guide positions (label coordinates) so the stage can draw a line.
 */
export function moveSnapped(
  box: LabelBox,
  x: number,
  y: number,
  label: Size,
  tolMm: number,
): { box: LabelBox; guideX: number | null; guideY: number | null } {
  const snap = (v: number, size: number, extent: number) => {
    const targets: [number, number][] = [
      [0, 0],
      [(extent - size) / 2, extent / 2],
      [extent - size, extent],
    ];
    for (const [pos, guide] of targets) if (Math.abs(v - pos) < tolMm) return { v: pos, guide };
    return { v, guide: null };
  };
  const sx = snap(x, box.w, label.w);
  const sy = snap(y, box.h, label.h);
  return {
    box: {
      ...box,
      x: clamp(sx.v, KEEP_MM - box.w, label.w - KEEP_MM),
      y: clamp(sy.v, KEEP_MM - box.h, label.h - KEEP_MM),
    },
    guideX: sx.guide,
    guideY: sy.guide,
  };
}

/**
 * Resize from a handle by the drag (dx, dy) in mm. The edge or corner opposite the handle stays
 * where it is. `lock` keeps the ratio; a corner follows whichever axis grew more.
 */
export function resizeFrom(
  handle: Handle,
  start: LabelBox,
  dx: number,
  dy: number,
  lock: boolean,
): LabelBox {
  const sx = handle.includes("e") ? 1 : handle.includes("w") ? -1 : 0;
  const sy = handle.includes("s") ? 1 : handle.includes("n") ? -1 : 0;
  let w = start.w + sx * dx;
  let h = start.h + sy * dy;
  if (lock) {
    const k = Math.max(w / start.w, h / start.h, MIN_MM / Math.min(start.w, start.h));
    w = start.w * k;
    h = start.h * k;
  }
  w = Math.max(MIN_MM, w);
  h = Math.max(MIN_MM, h);
  return {
    x: sx < 0 ? start.x + start.w - w : start.x,
    y: sy < 0 ? start.y + start.h - h : start.y,
    w,
    h,
  };
}

/**
 * How far the box sticks out past the label edge, in mm (0 when it fits). On a round label the
 * four corners are measured against the circle, since a rectangle is farthest out at a corner.
 */
export function overflowMm(box: LabelBox, label: Size, round: boolean): number {
  if (round) {
    const r = Math.min(label.w, label.h) / 2;
    const corners = [
      [box.x, box.y],
      [box.x + box.w, box.y],
      [box.x, box.y + box.h],
      [box.x + box.w, box.y + box.h],
    ];
    return Math.max(
      0,
      ...corners.map(([cx, cy]) => Math.hypot(cx - label.w / 2, cy - label.h / 2) - r),
    );
  }
  return Math.max(0, -box.x, -box.y, box.x + box.w - label.w, box.y + box.h - label.h);
}

/** The crop strips after turning the content 90 degrees: each strip travels with its edge. */
export function rotateCrop(
  c: LabelSpec["crop_percent"],
  clockwise: boolean,
): LabelSpec["crop_percent"] {
  return clockwise ? { t: c.l, r: c.t, b: c.r, l: c.b } : { t: c.r, r: c.b, b: c.l, l: c.t };
}

/**
 * Width in printer dots of the narrowest barcode bar. `narrowPx` is measured in the source image
 * (drawn for `sourceDpi`), `scale` is how much the content is enlarged relative to its own size.
 */
export function barWidthDots(narrowPx: number, scale: number, dpi: number, sourceDpi: number): number {
  return (narrowPx * scale * dpi) / sourceDpi;
}

/**
 * Font height in dots for a block of lines inside the box: the smaller of "all lines fit the
 * height" and "the longest line fits the width", limited to 10..400. `advance` is the glyph width
 * relative to the height (0.42 for the ZPL font, 0.72 for the PDF font). Same formula as label.rs.
 */
export function fontDots(lines: string[], boxDots: Size, advance: number): number {
  const longest = Math.max(1, ...lines.map((l) => l.length));
  const byHeight = boxDots.h / (Math.max(1, lines.length) * LINE_HEIGHT);
  const byWidth = boxDots.w / (longest * advance);
  return Math.round(clamp(Math.min(byHeight, byWidth), FONT_MIN, FONT_MAX));
}

/** Font heights the server picks for both outputs, so the editor can name each. */
export function textFonts(lines: string[], boxDots: Size): { zpl: number; pdf: number } {
  return {
    zpl: fontDots(lines, boxDots, ZPL_ADVANCE),
    pdf: fontDots(lines, boxDots, PDF_ADVANCE),
  };
}

/**
 * True when the text needs a font under the 10 dot minimum to fit the box, so the server's
 * clamp makes it run out of the box (and maybe the label). Checked for both outputs.
 */
export function textClipped(lines: string[], boxDots: Size): boolean {
  if (lines.length === 0) return false;
  const longest = Math.max(1, ...lines.map((l) => l.length));
  const byHeight = boxDots.h / (lines.length * LINE_HEIGHT);
  return [ZPL_ADVANCE, PDF_ADVANCE].some(
    (advance) => Math.min(byHeight, boxDots.w / (longest * advance)) < FONT_MIN - 1e-6,
  );
}

/**
 * Whether a pixel prints black. Same luma as the server: image 0.25 `to_luma_alpha8` uses the
 * integer weights 2126/7152/722 over 10000 (Rec. 709, src/color.rs SRGB_LUMA), rounded down,
 * and label.rs prints black below 140.
 */
export function printsBlack(r: number, g: number, b: number): boolean {
  return Math.floor((2126 * r + 7152 * g + 722 * b) / 10000) < BLACK_BELOW;
}

/**
 * Greedy word wrap like label.rs: a word moves to the next line when it would push
 * the line past `wrapChars`. A word longer than the limit stays whole. 0 means one line.
 */
export function wrapLines(text: string, wrapChars: number): string[] {
  const words = text.split(/\s+/).filter(Boolean);
  if (words.length === 0) return [];
  if (wrapChars <= 0) return [words.join(" ")];
  const lines: string[] = [];
  let line = "";
  for (const word of words) {
    if (line === "") line = word;
    else if (line.length + 1 + word.length <= wrapChars) line += ` ${word}`;
    else {
      lines.push(line);
      line = word;
    }
  }
  lines.push(line);
  return lines;
}
