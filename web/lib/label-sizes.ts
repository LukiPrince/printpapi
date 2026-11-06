// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// Catalogue of common label sizes for the template editor. Dimensions and sources were checked
// against the linked pages; only the Standard group is derived from nominal sizes and cites none.
// DYMO, Zebra, Brother, Rollo, MUNBYN, UPS, DHL, FedEx, GLS and DPD are trademarks of their respective
// owners. The names here only say which stock a size belongs to; printpapi is not affiliated with
// any of them.

export type LabelSize = {
  /** `${group}:${code}`, unique. Stored in a template's size_ref. */
  id: string;
  group: string;
  /** Manufacturer product code, or a short code for the Standard sizes. */
  code: string;
  name: string;
  /** Millimetres. h = 0 is a continuous roll: the length is free. */
  w: number;
  h: number;
  round?: boolean;
  note?: string;
  source?: { label: string; url: string };
};

const SRC = {
  dymo: {
    label: "DYMO Technical Reference, Appendix E",
    url: "https://download.dymo.com/UserManuals/labelwriter%20user%20guides/LWSE450_Tech_Ref/Content/AppendixE.htm",
  },
  zsb: { label: "Zebra ZSB labels", url: "https://www.zebra.com/content/smb/gb/en/labels.html" },
  zp46: { label: "Zebra Supplies Store", url: "https://zip.zebra.com/xxx/10000301/product/1-156I-772" },
  zp42: { label: "Zebra Supplies Store", url: "https://zip.zebra.com/xxx/10026372/product/1-2L6BZ1E" },
  zs46: { label: "Zebra Supplies Store", url: "https://emeazip.zebra.com/xxx/3003074/product/1-F8R-791" },
  zs102: { label: "Zebra Supplies Store", url: "https://emeazip.zebra.com/xxx/800264-255/product/1-F8T-4053" },
  brother: {
    label: "Brother QL-820NWB supplies",
    url: "https://support.brother.com/g/b/colist.aspx?c=gb&lang=en&prod=lpql820nwbeuk&cao=roll",
  },
  rolloRoll: { label: "Rollo product page", url: "https://www.rollo.com/product/roll-label/" },
  rolloStack: { label: "Rollo product page", url: "https://www.rollo.com/product/stack-label/" },
  rolloBar: { label: "Rollo product page", url: "https://www.rollo.com/product/2x1-barcode-label/" },
  rolloRound: { label: "Rollo product page", url: "https://www.rollo.com/product/2-in-circle-label/" },
  ups: {
    label: "UPS Shipping API",
    url: "https://github.com/UPS-API/api-documentation/blob/main/Shipping.yaml",
  },
  dhl: {
    label: "DHL Parcel DE Shipping API 2.4",
    url: "https://developer.dhl.com/sites/default/files/2026-10/parcel-de-shipping-v2_4.yaml",
  },
  dhlPage: {
    label: "DHL Parcel DE Shipping API",
    url: "https://developer.dhl.com/api-reference/parcel-de-shipping-post-parcel-germany-v2",
  },
  fedex: {
    label: "FedEx Ship API",
    url: "https://developer.fedex.com/api/en-us/catalog/ship/v1/docs.html",
  },
  gls: {
    label: "GLS Czech Republic",
    url: "https://gls-group.com/CZ/en/senders/useful-information/parcel-labels/",
  },
  dhlDe: { label: "DHL label formats for business customers", url: "https://www.dhl.de/de/geschaeftskunden/paket/rund-um-den-versand/verpackung-versand/dhl-verpackungen/versandlabel.html" },
  dpd: {
    label: "DPD Parcel Label Specification 2.4.1",
    url: "https://esolutions.dpd.com/dokumente/DPD_Parcel_Label_Specification_2.4.1_EN.pdf",
  },
};

type Src = keyof typeof SRC;
type Row = [
  code: string,
  name: string,
  w: number,
  h: number,
  src?: Src | LabelSize["source"],
  note?: string,
  extra?: { round: true },
];

const group = (g: string, rows: Row[]): LabelSize[] =>
  rows.map(([code, name, w, h, src, note, extra]) => ({
    id: `${g}:${code}`,
    group: g,
    code,
    name,
    w,
    h,
    ...(extra ?? {}),
    ...(note ? { note } : {}),
    ...(src ? { source: typeof src === "string" ? SRC[src] : src } : {}),
  }));

const ROUND = { round: true } as const;

export const SIZE_GROUPS: string[] = ["Standard", "Shipping", "DYMO", "Zebra", "Brother", "Rollo", "MUNBYN"];

export const LABEL_SIZES: LabelSize[] = [
  ...group("Standard", [
    ["4x8", "4 × 8 in", 101.6, 203.2],
    ["4x6", "4 × 6 in", 101.6, 152.4],
    ["4x4", "4 × 4 in", 101.6, 101.6],
    ["4x3", "4 × 3 in", 101.6, 76.2],
    ["4x2", "4 × 2 in", 101.6, 50.8],
    ["4x1", "4 × 1 in", 101.6, 25.4],
    ["3x2", "3 × 2 in", 76.2, 50.8],
    ["3x1", "3 × 1 in", 76.2, 25.4],
    ["2x1", "2 × 1 in", 50.8, 25.4],
    ["1x0.5", "1 × 0.5 in", 25.4, 12.7],
    ["100x150", "100 × 150 mm", 100, 150],
    ["100x100", "100 × 100 mm", 100, 100],
    ["100x70", "100 × 70 mm", 100, 70],
    ["100x50", "100 × 50 mm", 100, 50],
    ["60x40", "60 × 40 mm", 60, 40],
    ["50x30", "50 × 30 mm", 50, 30],
    ["50x25", "50 × 25 mm", 50, 25],
    ["A6", "A6", 105, 148],
    ["A5", "A5", 148, 210],
  ]),
  ...group("Shipping", [
    ["LabelStockSize 4x6", "UPS 4 × 6 in", 101.6, 152.4, "ups"],
    [
      "LabelStockSize 4x8",
      "UPS 4 × 8 in",
      101.6,
      203.2,
      "ups",
      "UPS scales the image on this size only up to 4 × 6 in.",
    ],
    ["100x70mm", "DHL Kleinpaket", 100, 70, "dhlPage", "Only for Kleinpaket and Warenpost International."],
    ["910-300-400", "DHL thermal", 103, 150, "dhl"],
    [
      "910-300-600",
      "DHL thermal, long",
      103,
      199,
      "dhl",
      "As a laser format the same code measures 105 × 205 mm.",
    ],
    ["910-300-610", "DHL thermal roll, long", 103, 199, "dhlDe"],
    ["910-300-300", "DHL laser", 105, 148, "dhl"],
    ["910-300-700", "DHL laser, long", 105, 208, "dhlDe"],
    ["910-300-700-oz", "DHL laser, long, no extra labels", 105, 208, "dhlPage"],
    [
      "910-300-710",
      "DHL laser, separate sheet",
      103,
      208,
      "dhlDe",
      "The API reference gives 105 × 209 mm, probably the sheet around the label.",
    ],
    ["PAPER_4X6", "FedEx laser 4 × 6 in", 101.6, 152.4, "fedex", "Size derived from the API name."],
    ["PAPER_4X675", "FedEx laser 4 × 6.75 in", 101.6, 171.5, "fedex", "Size derived from the API name."],
    ["PAPER_4X8", "FedEx laser 4 × 8 in", 101.6, 203.2, "fedex", "Size derived from the API name."],
    ["PAPER_4X9", "FedEx laser 4 × 9 in", 101.6, 228.6, "fedex", "Size derived from the API name."],
    ["Thermo 97x147", "GLS thermal roll", 97, 147, "gls", "Only documented on the Czech GLS page."],
    ["Mindestformat A6", "DPD minimum format", 105, 148, "dpd", "Minimum size from the 2021 specification."],
  ]),
  ...group("DYMO", [
    ["30334", "Multi-purpose", 57.2, 31.8, "dymo"],
    [
      "11354",
      "Multi-purpose (EU number)",
      57.2,
      31.8,
      "dymo",
      "The source documents this size for 30334; 11354 matches it.",
    ],
    ["30252", "Address", 28.6, 88.9, "dymo"],
    ["30321", "Large address", 35.6, 88.9, "dymo"],
    ["30323", "Shipping", 54, 101.6, "dymo"],
    ["30256", "Large shipping", 58.7, 101.6, "dymo"],
    ["30327", "File folder", 14.3, 87.3, "dymo"],
    ["30376", "Suspension file", 14.3, 50.8, "dymo"],
    ["30332", "Multi-purpose", 25.4, 25.4, "dymo"],
    ["30333", "Multi-purpose", 12.7, 25.4, "dymo"],
    ["30335", "Multi-purpose", 12.7, 12.7, "dymo"],
    ["30336", "Multi-purpose", 25.4, 54, "dymo"],
    ["30364", "Name badge", 58.7, 101.6, "dymo"],
    ["30365", "Name badge, non-adhesive", 57.2, 88.9, "dymo"],
    ["30374", "Appointment card", 63.5, 88.9, "dymo"],
    ["S0722370", "Address", 28, 89, { label: "DYMO product page", url: "https://www.dymo.de/etiketten-u.-schriftb%C3%A4nder/labelwriter-etiketten/dymo-original-etiketten-f%C3%BCr-labelwriter-adressetiketten-weiss-permanent-haftend-2-x-130-etiketten/SAP_S0722370.html" }],
    ["S0722430", "Shipping", 54, 101, { label: "DYMO product page", url: "https://www.dymo.de/etiketten-u.-schriftb%C3%A4nder/labelwriter-etiketten/dymo-original-etiketten-f%C3%BCr-labelwriter-versandetiketten-weiss-permanent-haftend-1-x-220-etiketten/SAP_S0722430.html" }],
    ["S0722540", "Multi-purpose", 57, 32, { label: "DYMO product page", url: "https://www.dymo.co.uk/labels-tapes/labelwriter-labels/dymo-labelwriter-multi-purpose-labels-1-roll-of-1000/SAP_S0722540.html" }],
    ["S0722530", "Multi-purpose, removable", 13, 25, { label: "DYMO product page", url: "https://www.dymo.de/etiketten-u.-schriftb%C3%A4nder/labelwriter-etiketten/dymo-original-etiketten-f%C3%BCr-labelwriter-vielzweck-weiss-abl%C3%B6sbar-1-x-1.000-etiketten/SAP_S0722530.html" }],
    ["S0722550", "Multi-purpose, removable", 19, 51, { label: "DYMO product page", url: "https://www.dymo.de/etiketten-u.-schriftb%C3%A4nder/labelwriter-etiketten/dymo-original-etiketten-f%C3%BCr-labelwriter-vielzweck-weiss-abl%C3%B6sbar-1-x-500-etiketten/SAP_S0722550.html" }],
    ["S0722520", "Return address", 25, 54, { label: "DYMO product page", url: "https://www.dymo.de/etiketten-u.-schriftb%C3%A4nder/labelwriter-etiketten/dymo-original-etiketten-f%C3%BCr-labelwriter-r%C3%BCcksendung-weiss-permanent-haftend-1-x-500-etiketten/SAP_S0722520.html" }],
    ["S0929120", "Multi-purpose, removable", 25, 25, { label: "DYMO product page", url: "https://www.dymo.de/etiketten-u.-schriftb%C3%A4nder/labelwriter-etiketten/dymo-original-etiketten-f%C3%BCr-labelwriter-vielzweck-weiss-abl%C3%B6sbar-1-x-750-etiketten/SAP_S0929120.html" }],
    ["S0722560", "Name badge, removable", 41, 89, { label: "DYMO product page", url: "https://www.dymo.de/etiketten-u.-schriftb%C3%A4nder/labelwriter-etiketten/dymo-original-etiketten-f%C3%BCr-labelwriter-namensschild-weiss-abl%C3%B6sbar-1-x-300-etiketten/SAP_S0722560.html" }],
    ["S0722460", "Suspension file", 12, 50, { label: "DYMO product page", url: "https://www.dymo.de/etiketten-u.-schriftb%C3%A4nder/labelwriter-etiketten/dymo-original-etiketten-f%C3%BCr-labelwriter-h%C3%A4ngeablage-weiss-permanent-haftend-1-x-220-etiketten/SAP_S0722460.html" }],
    ["S0722470", "Lever arch, narrow", 38, 190, { label: "DYMO product page", url: "https://www.dymo.de/etiketten-u.-schriftb%C3%A4nder/labelwriter-etiketten/dymo-original-etiketten-f%C3%BCr-labelwriter-ordner-schmal-weiss-permanent-haftend-1-x-110-etiketten/SAP_S0722470.html" }],
    ["S0722480", "Lever arch, wide", 59, 190, { label: "DYMO product page", url: "https://www.dymo.de/etiketten-u.-schriftb%C3%A4nder/labelwriter-etiketten/dymo-original-etiketten-f%C3%BCr-labelwriter-ordner-breit-weiss-permanent-haftend-1-x-110-etiketten/SAP_S0722480.html" }],
    ["1933081", "Durable", 25, 89, { label: "DYMO product page", url: "https://www.dymo.de/etiketten-u.-schriftb%C3%A4nder/labelwriter-etiketten/dymo-labelwriter-hochleistungs-etiketten/SAP_1933081.html" }],
    ["1744907", "Extra large shipping, 4 × 6 in", 104, 159, { label: "DYMO product page", url: "https://www.dymo.com/labels-tapes/labelwriter-labels/dymo-labelwriter-extra-large-shipping-labels/SP_95629.html" }, "DYMO states 104 × 159 mm for its 4 × 6 in label."],
    ["S0904980", "Extra large shipping, 4 × 6 in", 104, 159, { label: "DYMO product page", url: "https://www.dymo.de/etiketten-u.-schriftb%C3%A4nder/labelwriter-etiketten/dymo-original-etiketten-f%C3%BCr-labelwriter-versandetikettenaufkleber-weiss-permanent-1-x-220-etiketten-f%C3%BCr-lw-4xl-5xl/SAP_S0904980.html" }, "DYMO states 104 × 159 mm for its 4 × 6 in label."],
    ["S0947420", "Name badge, 4XL/5XL", 59, 102, { label: "DYMO product page", url: "https://www.dymo.de/etiketten-u.-schriftb%C3%A4nder/labelwriter-etiketten/dymo-original-etiketten-f%C3%BCr-labelwriter-namensschild-weiss-permanent-2-x-575-etiketten-f%C3%BCr-lw-4xl-5xl/SAP_S0947420.html" }],
    ["2166659", "Shipping, DHL format, 4XL/5XL", 102, 210, { label: "DYMO product page", url: "https://www.dymo.de/etiketten-u.-schriftb%C3%A4nder/labelwriter-etiketten/dymo-original-etiketten-f%C3%BCr-labelwriter-versandetikettenaufkleber-weiss-permanent-f%C3%BCr-lw-4xl-5xl-und-dhl-format-1-x-140-etiketten/SAP_2166659.html" }],
  ]),
  ...group("Zebra", [
    ["ZSB-LC1", "ZSB shipping", 101.6, 152.4, "zsb"],
    ["ZSB-LC2", "ZSB address", 31.8, 88.9, "zsb"],
    ["ZSB-LC5", "ZSB name badge", 57.2, 88.9, "zsb"],
    ["ZSB-LC6", "ZSB barcode", 57.2, 25.4, "zsb"],
    ["ZSB-LC7", "ZSB small multi-purpose", 31.8, 31.8, "zsb"],
    ["ZSB-LC8", "ZSB jewelry price tag", 57.2, 12.7, "zsb"],
    ["10000301", "Z-Perform 1000D", 101.6, 152.4, "zp46"],
    ["10026372", "Z-Perform 1000D", 101.6, 50.8, "zp42"],
    ["3003074", "Z-Select 2000D", 101.6, 152.4, "zs46"],
    ["800264-255", "Z-Select 2000D", 102, 64, "zs102"],
  ]),
  ...group("Brother", [
    ["DK-11201", "Address", 29, 90, "brother"],
    ["DK-11202", "Shipping", 62, 100, "brother"],
    ["DK-11203", "File folder", 17, 87, "brother"],
    ["DK-11204", "Multi-purpose", 17, 54, "brother"],
    ["DK-11208", "Large address", 38, 90, "brother"],
    [
      "DK-11209",
      "Small address",
      29,
      62,
      "brother",
      "The roll is 62 mm wide, the label sits sideways on it.",
    ],
    ["DK-11221", "Square", 23, 23, "brother"],
    ["DK-11234", "Name badge", 60, 86, "brother"],
    ["DK-11218", "Round", 24, 24, "brother", undefined, ROUND],
    ["DK-11219", "Round", 12, 12, "brother", undefined, ROUND],
    ["DK-22205", "Continuous roll", 62, 0, "brother"],
    ["DK-22223", "Continuous roll", 50, 0, "brother"],
    ["DK-N55224", "Continuous roll, non-adhesive", 54, 0, "brother"],
    ["DK-22225", "Continuous roll", 38, 0, "brother"],
    ["DK-22210", "Continuous roll", 29, 0, "brother"],
    ["DK-22214", "Continuous roll", 12, 0, "brother"],
  ]),
  ...group("Rollo", [
    ["4x6 roll", "4 × 6 in roll", 101.6, 152.4, "rolloRoll"],
    ["4x6 stack", "4 × 6 in stack", 101.6, 152.4, "rolloStack"],
    ["2x1", "2 × 1 in barcode", 50.8, 25.4, "rolloBar"],
    ["Round 2in", "Round 2 in", 50.8, 50.8, "rolloRound", undefined, ROUND],
  ]),
  ...group("MUNBYN", [
    ["4x6 fan-fold", "4 × 6 in fan-fold", 101.6, 152.4, { label: "MUNBYN shop", url: "https://munbyn.com/products/4x6-fanfold-shipping-labels" }],
    ["4x6 roll", "4 × 6 in roll", 104, 159, { label: "MUNBYN UK shop", url: "https://uk.munbyn.com/products/4x6-thermal-shipping-label-roll-220-labels" }, "MUNBYN states 103 or 104 mm wide, depending on the shop."],
    ["4x5", "4 × 5 in", 101, 127, { label: "MUNBYN shop", url: "https://munbyn.com/products/munbyn-4x5-rectangle-thermal-labels-for-shipping-price-tag-stickers" }],
    ["4x3", "4 × 3 in", 101.6, 76.2, { label: "MUNBYN shop", url: "https://munbyn.com/products/munbyn-4x3-generic-rectangle-series-thermal-labels-500-roll" }],
    ["4x2", "4 × 2 in", 101.6, 50.8, { label: "MUNBYN shop", url: "https://munbyn.com/products/munbyn-4x2-generic-rectangle-series-thermal-labels-500-roll" }],
    ["3x5 fan-fold", "3 × 5 in fan-fold", 76, 127, { label: "MUNBYN shop", url: "https://munbyn.com/products/munbyn-3x5-inch-white-fanfold-thermal-shipping-labels" }],
    ["3x3", "3 × 3 in", 76.2, 76.2, { label: "MUNBYN shop", url: "https://munbyn.com/products/munbyn-3x3-generic-square-series-thermal-labels-500-roll" }],
    ["3x2", "3 × 2 in", 76, 51, { label: "MUNBYN shop", url: "https://munbyn.com/products/munbyn-3x2-inch-white-thermal-labels" }],
    ["3x1", "3 × 1 in", 76, 25, { label: "MUNBYN shop", url: "https://munbyn.com/products/munbyn-3x1-rectangle-thermal-labels" }],
    ["2.25x1.25", "2.25 × 1.25 in", 57.2, 31.8, { label: "MUNBYN shop", url: "https://munbyn.com/products/rectangle-labels" }, "MUNBYN states inches only."],
    ["2x2", "2 × 2 in", 50.8, 50.8, { label: "MUNBYN shop", url: "https://munbyn.com/products/munbyn-2x2-square-thermal-labels-750-roll" }],
    ["2x1", "2 × 1 in", 50.8, 25.4, { label: "MUNBYN shop", url: "https://munbyn.com/products/munbyn-white-2x1-inch-thermal-sticker-labels-1000-labels-1-roll" }],
    ["Round 2in", "Round 2 in", 50, 50, { label: "MUNBYN shop", url: "https://munbyn.com/products/munbyn-2-circle-thermal-sticker-labels-150-roll" }, undefined, ROUND],
  ]),
];

/** Lower-case, one multiplication sign, decimal point. Spaces only collapse around a digit x digit. */
function norm(s: string): string {
  return s
    .toLowerCase()
    .replace(/[×*]/g, "x")
    .replace(/(\d)\s*x\s*(\d)/g, "$1x$2")
    .replace(/(\d),(\d)/g, "$1.$2")
    .trim();
}

// ponytail: a plain substring match over a prebuilt string per size. Fine for ~80 rows; a fuzzy
// ranker would only be worth it past a few hundred.
const HAYSTACK = LABEL_SIZES.map((s) =>
  norm(`${s.group} ${s.code} ${s.name} ${s.w}x${s.h} ${Math.round(s.w)}x${Math.round(s.h)}`),
);

/** Sizes whose group, code, name or dimensions contain every word of the query, in catalogue order. */
export function searchSizes(query: string): LabelSize[] {
  const terms = norm(query).split(/\s+/).filter(Boolean);
  if (terms.length === 0) return LABEL_SIZES;
  return LABEL_SIZES.filter((_, i) => terms.every((t) => HAYSTACK[i].includes(t)));
}

const mm = (v: number) => String(Math.round(v * 10) / 10);

/** "101.6 × 152.4 mm", "Ø 24 mm" or "62 mm wide, free length". */
export function sizeLabel(s: LabelSize): string {
  if (s.round) return `Ø ${mm(s.w)} mm`;
  if (s.h === 0) return `${mm(s.w)} mm wide, free length`;
  return `${mm(s.w)} × ${mm(s.h)} mm`;
}
