// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// Canvas drawing for the label editor: the turned and cropped content, the text block, the
// black-and-white print preview. Browser only. The numbers behind it (box, crop, font size)
// come from label-draft.ts and label-geometry.ts, which are tested; this file only paints them.

import type { LabelBox, LabelSpec } from "./api";
import { fontDots, printsBlack, ZPL_ADVANCE, type Size } from "./label-geometry";

const MM_PER_INCH = 25.4;
/** The preview canvases are drawn at most this large; the browser scales them to the stage. */
const MAX_PREVIEW_PX = 1600;
/** ZPL sends ^A0N,h,0.72h: the glyph is h tall and 0.72h wide (label.rs zpl_text). */
const GLYPH_WIDTH = 0.72;
const LINE_HEIGHT = 1.1;
/** Arial's capital letters are this tall relative to the font size. */
const ARIAL_CAP = 0.716;

export function makeCanvas(w: number, h: number): HTMLCanvasElement {
  const c = document.createElement("canvas");
  c.width = Math.max(1, Math.round(w));
  c.height = Math.max(1, Math.round(h));
  return c;
}

/** The image as the server sees it: turned clockwise first, then cropped by percent. */
export function turnedAndCropped(
  src: HTMLCanvasElement,
  rotate: 0 | 90 | 180 | 270,
  crop: LabelSpec["crop_percent"],
): HTMLCanvasElement {
  const sideways = rotate % 180 !== 0;
  const rw = sideways ? src.height : src.width;
  const rh = sideways ? src.width : src.height;
  const turned = makeCanvas(rw, rh);
  const t = turned.getContext("2d")!;
  t.translate(rw / 2, rh / 2);
  t.rotate((rotate * Math.PI) / 180);
  t.drawImage(src, -src.width / 2, -src.height / 2);

  const out = makeCanvas(rw * (1 - (crop.l + crop.r) / 100), rh * (1 - (crop.t + crop.b) / 100));
  out.getContext("2d")!.drawImage(turned, -(rw * crop.l) / 100, -(rh * crop.t) / 100);
  return out;
}

/** A picture file on white. Transparent pixels print white, so they are painted white here too.
 *  An animated GIF gives its first frame, like the server. */
export async function canvasFromFile(file: File): Promise<HTMLCanvasElement> {
  const bitmap = await createImageBitmap(file);
  const c = makeCanvas(bitmap.width, bitmap.height);
  const g = c.getContext("2d")!;
  g.fillStyle = "#fff";
  g.fillRect(0, 0, c.width, c.height);
  g.drawImage(bitmap, 0, 0);
  bitmap.close();
  return c;
}

/**
 * The text as ZPL prints it in its box: left-aligned, vertically centred, as large as the box
 * allows. The font is h tall and 0.72h wide, so Arial Bold is drawn at h and squeezed to 0.72 of
 * its width; its average advance is then about the 0.42h that label.rs plans with. A PDF printer
 * sets the same text at the same height but unsqueezed (advance 0.72h), see the Font status.
 * ponytail: the browser's Arial Bold stands in for the printer's own font, so the line breaks and
 * the size match the printed label but the letter shapes do not. The test print shows the real one.
 */
export function textCanvas(lines: string[], boxMm: Size, dpi: number): HTMLCanvasElement {
  const dotsW = (boxMm.w / MM_PER_INCH) * dpi;
  const dotsH = (boxMm.h / MM_PER_INCH) * dpi;
  const k = Math.min(1, MAX_PREVIEW_PX / Math.max(dotsW, dotsH));
  const c = makeCanvas(dotsW * k, dotsH * k);
  const g = c.getContext("2d")!;
  g.fillStyle = "#fff";
  g.fillRect(0, 0, c.width, c.height);
  if (lines.length === 0) return c;

  const font = fontDots(lines, { w: dotsW, h: dotsH }, ZPL_ADVANCE) * k;
  const pitch = font * LINE_HEIGHT;
  const top = (c.height - lines.length * pitch) / 2;
  const capHeight = font * ARIAL_CAP;
  g.fillStyle = "#000";
  g.font = `bold ${font}px Arial, Helvetica, sans-serif`;
  g.textBaseline = "alphabetic";
  g.scale(GLYPH_WIDTH, 1);
  lines.forEach((line, i) => g.fillText(line, 0, top + i * pitch + pitch / 2 + capHeight / 2));
  return c;
}

/**
 * The label as the printer will make it: white label, the content scaled into its box and cut off
 * at the edges, every pixel pure black or white. Drawn at printer resolution, or smaller when
 * that would be huge.
 */
export function printPreview(
  content: HTMLCanvasElement,
  box: LabelBox,
  label: Size,
  dpi: number,
): HTMLCanvasElement {
  const dotsW = (label.w / MM_PER_INCH) * dpi;
  const dotsH = (label.h / MM_PER_INCH) * dpi;
  const k = Math.min(1, MAX_PREVIEW_PX / Math.max(dotsW, dotsH));
  const c = makeCanvas(dotsW * k, dotsH * k);
  const g = c.getContext("2d", { willReadFrequently: true })!;
  g.fillStyle = "#fff";
  g.fillRect(0, 0, c.width, c.height);
  g.imageSmoothingQuality = "high";
  const mm = (dpi / MM_PER_INCH) * k;
  g.drawImage(content, box.x * mm, box.y * mm, box.w * mm, box.h * mm);

  const image = g.getImageData(0, 0, c.width, c.height);
  const a = image.data;
  for (let i = 0; i < a.length; i += 4) {
    const v = printsBlack(a[i], a[i + 1], a[i + 2]) ? 0 : 255;
    a[i] = a[i + 1] = a[i + 2] = v;
    a[i + 3] = 255;
  }
  g.putImageData(image, 0, 0);
  return c;
}

/** Bare base64 of the canvas as PNG, for the label_base64 job type. */
export function canvasToBase64(c: HTMLCanvasElement): string {
  return c.toDataURL("image/png").split(",", 2)[1];
}
