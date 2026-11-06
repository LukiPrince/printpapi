// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The state of one template while it is being edited, and the edits that change it. Pure:
// millimetres in, millimetres out, no DOM, so every rule here has a test. The page and the
// canvas drawing live in components/labels/.

import type { LabelBox, LabelSpec } from "./api";
import { fit, rotateCrop, wrapLines, type Size } from "./label-geometry.ts";
import { LABEL_SIZES, type LabelSize } from "./label-sizes.ts";

type Crop = LabelSpec["crop_percent"];
type TextSettings = NonNullable<LabelSpec["text"]>;

/** Everything the server stores for a template, with the optional parts always present. */
export type Draft = {
  name: string;
  content: "image" | "text";
  /** Millimetres. */
  label: Size;
  /** Catalogue size the label started from; undefined for a hand-typed one. */
  sizeRef?: string;
  dpi: number;
  rotate: 0 | 90 | 180 | 270;
  crop: Crop;
  place: LabelBox;
  text: TextSettings;
  /** Not editable here; kept so saving an existing template does not drop it. */
  darkness?: number;
};

export const DEFAULT_DPI = 203;
/** The sample label is 1400 x 800 px (see label-sample.ts). */
export const SAMPLE_PX: Size = { w: 1400, h: 800 };
/** Start length of a continuous roll, which has none of its own. */
const ROLL_START_MM = 60;
/** The server refuses labels over this many dots in either direction (label.rs MAX_DOTS). */
const MAX_DOTS = 8000;
const MIN_LABEL_MM = 5;
/** So much of the box stays on the label when a size change would leave it off. */
const KEEP_MM = 3;
const MIN_BOX_MM = 3;
const MAX_STRIP = 95;
const TEXT_INSET = 0.08;
const MM_PER_INCH = 25.4;

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
const r2 = (v: number) => Math.round(v * 100) / 100;
const dots = (mm: number, dpi: number) => Math.round((mm / MM_PER_INCH) * dpi);
const NO_TEXT: TextSettings = { wrap_chars: 0, lines_per_label: 0 };

const catalogue = (ref?: string): LabelSize | undefined => LABEL_SIZES.find((s) => s.id === ref);

/** Size of the content after the turn and the crop, in the same unit as `src`. */
export function preparedSize(src: Size, rotate: Draft["rotate"], crop: Crop): Size {
  const sideways = rotate % 180 !== 0;
  const w = sideways ? src.h : src.w;
  const h = sideways ? src.w : src.h;
  return { w: w * (1 - (crop.l + crop.r) / 100), h: h * (1 - (crop.t + crop.b) / 100) };
}

/** The box that shows `px` pixels one dot each, centred on the label. */
function native(px: Size, dpi: number, label: Size): LabelBox {
  const w = (px.w / dpi) * MM_PER_INCH;
  const h = (px.h / dpi) * MM_PER_INCH;
  return { x: (label.w - w) / 2, y: (label.h - h) / 2, w, h };
}

const SAMPLE_CROP: Crop = { t: 0, r: 0, b: 14.3, l: 0 };
const NO_CROP: Crop = { t: 0, r: 0, b: 0, l: 0 };

/** The built-in sample label set up like a carrier label: turned, bottom cut off,
 *  one pixel per dot. Only for an image template. */
export function forSample(d: Draft): Draft {
  if (d.content !== "image") return d;
  return {
    ...d,
    rotate: 90,
    crop: { ...SAMPLE_CROP },
    place: native(preparedSize(SAMPLE_PX, 90, SAMPLE_CROP), d.dpi, d.label),
  };
}

/** The user's own picture on a template that is not saved yet: upright and whole, fitted into the
 *  label. The turn and crop belong to the sample only. */
export function forPicture(d: Draft, px: Size): Draft {
  if (d.content !== "image") return d;
  return {
    ...d,
    rotate: 0,
    crop: { ...NO_CROP },
    place: fit(px, d.label, "contain", isRound(d)),
  };
}

/** A new template: the sample label set up like a carrier label. */
export function newDraft(): Draft {
  const size = catalogue("Standard:4x6")!;
  return forSample({
    name: "",
    content: "image",
    label: { w: size.w, h: size.h },
    sizeRef: size.id,
    dpi: DEFAULT_DPI,
    rotate: 0,
    crop: { ...NO_CROP },
    place: { x: 0, y: 0, w: size.w, h: size.h },
    text: { ...NO_TEXT },
    // ponytail: no darkness field in the editor yet; an existing value is passed through untouched
  });
}

export function draftFromTemplate(t: LabelSpec): Draft {
  return {
    name: t.name,
    content: t.content,
    label: { ...t.label_mm },
    sizeRef: t.size_ref ?? undefined,
    dpi: t.dpi,
    rotate: t.rotate,
    crop: { ...t.crop_percent },
    place: { ...t.place_mm },
    text: { ...(t.text ?? NO_TEXT) },
    darkness: t.zpl_darkness ?? undefined,
  };
}

const box2 = (b: LabelBox): LabelBox => ({ x: r2(b.x), y: r2(b.y), w: r2(b.w), h: r2(b.h) });

/** The JSON contract. Numbers are rounded to 0.01 mm, which is a fifth of a dot at 300 dpi. */
export function toSpec(d: Draft): LabelSpec {
  return {
    name: d.name.trim(),
    content: d.content,
    label_mm: { w: r2(d.label.w), h: r2(d.label.h) },
    dpi: d.dpi,
    rotate: d.rotate,
    crop_percent: { t: r2(d.crop.t), r: r2(d.crop.r), b: r2(d.crop.b), l: r2(d.crop.l) },
    place_mm: box2(d.place),
    ...(d.content === "text" ? { text: { ...d.text } } : {}),
    ...(typeof d.darkness === "number" ? { zpl_darkness: d.darkness } : {}),
    ...(d.sizeRef ? { size_ref: d.sizeRef } : {}),
  };
}

/** Equal keys mean "nothing to save". */
export const specKey = (d: Draft): string => JSON.stringify(toSpec(d));

export const isRound = (d: Draft): boolean => !!catalogue(d.sizeRef)?.round;

export const labelDots = (d: Draft): Size => ({
  w: dots(d.label.w, d.dpi),
  h: dots(d.label.h, d.dpi),
});

/** Longest label edge in mm that still comes to at most 8000 dots. */
export function maxMm(dpi: number): number {
  return Math.floor(((MAX_DOTS * MM_PER_INCH) / dpi) * 10) / 10;
}

/** Box of the same shape, as large as fits inside the label. */
const refit = (d: Draft, label: Size, round: boolean): LabelBox =>
  fit({ w: d.place.w, h: d.place.h }, label, "contain", round);

/** The text box: the label less a margin on every side. On a round label, the largest box of the
 *  label shape that sits inside the circle. */
export function textBox(label: Size, round = false): LabelBox {
  if (round) return fit(label, label, "contain", true);
  const x = label.w * TEXT_INSET;
  const y = label.h * TEXT_INSET;
  return { x, y, w: label.w - 2 * x, h: label.h - 2 * y };
}

/** Use a catalogue size. The content keeps its shape and is fitted into the new label. */
export function applySize(d: Draft, s: LabelSize): Draft {
  const label = { w: s.w, h: s.h || ROLL_START_MM };
  return {
    ...d,
    label,
    sizeRef: s.id,
    place: d.content === "text" ? textBox(label, !!s.round) : refit(d, label, !!s.round),
  };
}

/** The box after the label changed size by typing: kept where it is while it still fits, so a
 *  correction of a millimetre does not rescale the content. Only a box that is now bigger than the
 *  label is fitted again, and one that is left off the label is moved back onto it. */
function keepBox(d: Draft, label: Size, round: boolean): LabelBox {
  const b = d.place;
  if (b.w > label.w + 1e-9 || b.h > label.h + 1e-9) {
    return d.content === "text" ? textBox(label, round) : refit(d, label, round);
  }
  const off = (pos: number, size: number, extent: number) =>
    pos > extent - KEEP_MM || pos + size < KEEP_MM;
  return {
    ...b,
    x: off(b.x, b.w, label.w) ? clamp(b.x, 0, label.w - b.w) : b.x,
    y: off(b.y, b.h, label.h) ? clamp(b.y, 0, label.h - b.h) : b.y,
  };
}

/** Type a width or length. A hand-typed edge ends the link to the catalogue, except the free
 *  length of a continuous roll. */
export function setLabelDim(d: Draft, key: "w" | "h", mm: number): Draft {
  const value = clamp(mm, MIN_LABEL_MM, maxMm(d.dpi));
  const roll = catalogue(d.sizeRef)?.h === 0 && key === "h";
  const label = { ...d.label, [key]: value };
  const sizeRef = roll ? d.sizeRef : undefined;
  return { ...d, label, sizeRef, place: keepBox(d, label, !!catalogue(sizeRef)?.round) };
}

/**
 * Change one crop strip. What stays behind keeps its place on the label: cropping the left or
 * top moves the box's edge by the strip's share of the uncropped size, cropping right or bottom
 * only makes it smaller.
 */
export function setCrop(d: Draft, key: keyof Crop, percent: number): Draft {
  const opposite = { t: "b", b: "t", l: "r", r: "l" }[key] as keyof Crop;
  const value = clamp(percent, 0, Math.min(MAX_STRIP, 99 - d.crop[opposite]));
  const old = d.crop;
  const crop = { ...old, [key]: value };
  const fullW = d.place.w / (1 - (old.l + old.r) / 100);
  const fullH = d.place.h / (1 - (old.t + old.b) / 100);
  return {
    ...d,
    crop,
    place: {
      x: d.place.x + ((crop.l - old.l) / 100) * fullW,
      y: d.place.y + ((crop.t - old.t) / 100) * fullH,
      w: fullW * (1 - (crop.l + crop.r) / 100),
      h: fullH * (1 - (crop.t + crop.b) / 100),
    },
  };
}

/** Turn the content a quarter. Its centre and scale stay, the crop strips travel with their edges. */
export function turn(d: Draft, clockwise: boolean): Draft {
  const cx = d.place.x + d.place.w / 2;
  const cy = d.place.y + d.place.h / 2;
  const w = d.place.h;
  const h = d.place.w;
  return {
    ...d,
    rotate: ((d.rotate + (clockwise ? 90 : 270)) % 360) as Draft["rotate"],
    crop: rotateCrop(d.crop, clockwise),
    place: { x: cx - w / 2, y: cy - h / 2, w, h },
  };
}

/** Type one of X, Y, W, H. With `lock`, the other size follows so the shape stays. */
export function setPlace(d: Draft, key: keyof LabelBox, value: number, lock: boolean): Draft {
  if (key === "x" || key === "y") return { ...d, place: { ...d.place, [key]: value } };
  let { w, h } = d.place;
  if (lock) {
    const ratio = w / h;
    if (key === "w") {
      w = Math.max(value, MIN_BOX_MM, MIN_BOX_MM * ratio);
      h = w / ratio;
    } else {
      h = Math.max(value, MIN_BOX_MM, MIN_BOX_MM / ratio);
      w = h * ratio;
    }
  } else if (key === "w") w = Math.max(MIN_BOX_MM, value);
  else h = Math.max(MIN_BOX_MM, value);
  return { ...d, place: { ...d.place, w, h } };
}

/** Switch between image and text. `image` is the size of the sample image in pixels. */
export function setContent(d: Draft, content: Draft["content"], image: Size): Draft {
  if (content === d.content) return d;
  const prepared = preparedSize(image, d.rotate, d.crop);
  const round = isRound(d);
  return {
    ...d,
    content,
    place: content === "text" ? textBox(d.label, round) : fit(prepared, d.label, "contain", round),
  };
}

/** The last box of each content kind, with the label it was made for. */
export type BoxMemory = Partial<Record<Draft["content"], { box: LabelBox; label: Size }>>;

/** `setContent` that remembers the box of the kind it leaves and brings back the one of the kind
 *  it enters, as long as the label has the same size. Switching tabs by accident loses nothing. */
export function switchContent(
  d: Draft,
  content: Draft["content"],
  image: Size,
  memory: BoxMemory,
): { draft: Draft; memory: BoxMemory } {
  if (content === d.content) return { draft: d, memory };
  const next: BoxMemory = { ...memory, [d.content]: { box: { ...d.place }, label: { ...d.label } } };
  const kept = memory[content];
  const same = kept && kept.label.w === d.label.w && kept.label.h === d.label.h;
  return {
    draft: same ? { ...d, content, place: { ...kept.box } } : setContent(d, content, image),
    memory: next,
  };
}

type StashRead = Pick<Storage, "getItem">;
type StashWrite = Pick<Storage, "setItem" | "removeItem">;
const stashKey = (id: number | null) => `printpapi.label-draft.${id ?? "new"}`;

const isNum = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);
const hasNums = (v: unknown, keys: string[]): boolean =>
  typeof v === "object" && v !== null && keys.every((k) => isNum((v as Record<string, unknown>)[k]));

function isDraft(v: unknown): v is Draft {
  if (typeof v !== "object" || v === null) return false;
  const d = v as Record<string, unknown>;
  return (
    typeof d.name === "string" &&
    (d.content === "image" || d.content === "text") &&
    hasNums(d.label, ["w", "h"]) &&
    hasNums(d.place, ["x", "y", "w", "h"]) &&
    hasNums(d.crop, ["t", "r", "b", "l"]) &&
    hasNums(d.text, ["wrap_chars", "lines_per_label"]) &&
    isNum(d.dpi) &&
    [0, 90, 180, 270].includes(d.rotate as number) &&
    (d.sizeRef === undefined || typeof d.sizeRef === "string") &&
    (d.darkness === undefined || isNum(d.darkness))
  );
}

/**
 * Keep an unsaved draft in session storage (or drop it with null). A sign-out from a dead
 * session unmounts the editor; this is how the work survives it. Storage can be blocked or full,
 * and then the draft is simply not kept.
 */
export function stashDraft(store: StashWrite | null, id: number | null, d: Draft | null): void {
  try {
    if (!store) return;
    if (d) store.setItem(stashKey(id), JSON.stringify(d));
    else store.removeItem(stashKey(id));
  } catch {
    /* blocked or full: no stash */
  }
}

/** The draft stashDraft kept for this template (null: a new one), or null. */
export function unstashDraft(store: StashRead | null, id: number | null): Draft | null {
  try {
    const raw = store?.getItem(stashKey(id));
    if (!raw) return null;
    const v: unknown = JSON.parse(raw);
    return isDraft(v) ? v : null;
  } catch {
    return null;
  }
}

const NAME_OK = /^[\p{L}\p{N}_. -]+$/u;

/** Why the server would refuse this name, or null (dispatch.rs valid_option_value). */
export function nameProblem(name: string): string | null {
  const n = name.trim();
  if (n === "") return "Give the template a name.";
  if (n.length > 64) return "The name can be at most 64 characters.";
  if (!NAME_OK.test(n)) return "Use letters, digits, spaces and _ . - in the name.";
  return null;
}

/** The lines of the first label for a sample text: the same folding and wrapping as the server. */
export function previewLines(sample: string, text: TextSettings): string[] {
  const clean = sample.replace(/[\^~]/g, " ");
  const lines = wrapLines(clean, text.wrap_chars);
  return text.lines_per_label > 0 ? lines.slice(0, text.lines_per_label) : lines;
}

/** A curl call that prints with this template. */
export function jobExample(d: Draft, origin: string, sampleText = "A-104872"): string {
  const template = d.name.trim() || "my-template";
  const body =
    d.content === "text"
      ? { printer_id: 1, type: "label_text", text: sampleText, template }
      : { printer_id: 1, type: "label_uri", url: "https://example.com/label.gif", template };
  return [
    `curl -X POST ${origin}/jobs \\`,
    `  -H "Authorization: Bearer $PRINTPAPI_TOKEN" \\`,
    `  -H "Content-Type: application/json" \\`,
    `  -d '${JSON.stringify(body, null, 2)}'`,
  ].join("\n");
}
