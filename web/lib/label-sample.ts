// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The sample label the template editor shows until someone drops their own image. It is drawn
// like a carrier GIF: lying on its side, with a notice strip along one edge that should not
// be printed. All data is invented.

/** The sample is drawn for a 203 dpi printer. */
export const SAMPLE_DPI = 203;
/** Narrowest bar of the sample barcode, in image pixels. */
export const SAMPLE_BAR_PX = 3;

const W = 800;
const H = 1200;
const STRIP = 200;

function canvas(w: number, h: number): HTMLCanvasElement {
  const c = document.createElement("canvas");
  c.width = Math.max(1, Math.round(w));
  c.height = Math.max(1, Math.round(h));
  return c;
}

/** 1400 x 800 px: the 800 x 1200 label turned 90 degrees, with a 200 px notice strip at the end. */
export function drawSampleLabel(): HTMLCanvasElement {
  const upright = canvas(W, H + STRIP);
  const c = upright.getContext("2d")!;
  let seed = 20261105;
  const rnd = () => (seed = (seed * 1664525 + 1013904223) >>> 0) / 4294967296; // fixed seed: same label every time
  const text = (s: string, x: number, y: number, size: number, weight = "bold") => {
    c.font = `${weight} ${size}px Arial, Helvetica, sans-serif`;
    c.fillText(s, x, y);
  };
  const rule = (y: number) => c.fillRect(10, y, W - 20, 6);

  c.fillStyle = "#fff";
  c.fillRect(0, 0, W, H + STRIP);
  c.fillStyle = "#000";
  c.strokeStyle = "#000";
  c.lineWidth = 6;
  c.strokeRect(10, 10, W - 20, H - 20);

  text("SAMPLE GMBH", 34, 62, 30);
  text("Sample Street 1", 34, 100, 26, "normal");
  text("12345 Sampletown", 34, 134, 26, "normal");
  text("1.2 KG", 590, 66, 38);
  text("1 / 1", 590, 110, 32, "normal");
  rule(158);
  text("TO:", 34, 205, 26);
  text("JANE EXAMPLE", 34, 256, 42);
  text("EXAMPLE ROAD 42", 34, 306, 36, "normal");
  text("54321 EXAMPLEVILLE", 34, 360, 46);
  text("GERMANY", 34, 408, 32, "normal");
  rule(432);

  // a 2D code lookalike: solid finder edges, random fill
  for (let y = 0; y < 30; y++) {
    for (let x = 0; x < 30; x++) {
      const edge = x === 0 || y === 29 || (x === 29 && y % 2 === 0) || (y === 0 && x % 2 === 0);
      if (edge || rnd() > 0.5) c.fillRect(34 + x * 8, 462 + y * 8, 8, 8);
    }
  }
  text("DE 543", 320, 552, 96);
  text("21-04", 320, 660, 76, "normal");
  rule(718);
  text("SAMPLE POST EXPRESS", 34, 780, 46);
  rule(808);

  // a linear barcode lookalike with a known narrowest bar
  for (let x = 60; x < 740; ) {
    const w = SAMPLE_BAR_PX * (1 + Math.floor(rnd() * 3));
    c.fillRect(x, 836, w, 230);
    x += w + SAMPLE_BAR_PX * (1 + Math.floor(rnd() * 3));
  }
  text("SHIPMENT 00 3401 2345 6789 01", 110, 1104, 32);
  text("Sample label, not a real shipment", 34, 1164, 26, "normal");
  text("NOTICE STRIP", 34, H + 76, 46);
  text("This margin does not belong on the label.", 34, H + 130, 30, "normal");

  const turned = canvas(H + STRIP, W);
  const d = turned.getContext("2d")!;
  d.translate(0, W);
  d.rotate(-Math.PI / 2);
  d.drawImage(upright, 0, 0);
  return turned;
}
