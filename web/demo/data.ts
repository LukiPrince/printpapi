// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The fictional shop behind the demo: people, products, and the payloads a real shop would send.
// Everything is derived from a seeded rng so the same order number always yields the same order.
export const SHOP = { name: "Nordlicht Coffee Roasters", street: "Hafenstraße 12", city: "20457 Hamburg" };

export function rng(seed: number): () => number {
  let s = (seed >>> 0) || 1;
  return () => (s = (Math.imul(s, 1664525) + 1013904223) >>> 0) / 4294967296;
}

export function shuffle<T>(arr: T[], r: () => number): T[] {
  const a = [...arr];
  for (let i = a.length - 1; i > 0; i--) {
    const j = Math.floor(r() * (i + 1));
    [a[i], a[j]] = [a[j], a[i]];
  }
  return a;
}

/** Base64 of the UTF-8 bytes (btoa alone throws on anything past Latin-1). */
export function b64(s: string): string {
  let bin = "";
  new TextEncoder().encode(s).forEach((b) => (bin += String.fromCharCode(b)));
  return btoa(bin);
}

export const eur = (v: number): string => `${v.toFixed(2)} EUR`;

const PEOPLE: [string, string, string, string][] = [
  ["Mira Jansen", "Lindenallee 8", "50968 Köln", "DE"],
  ["Jonas Albrecht", "Kastanienweg 3", "80331 München", "DE"],
  ["Elif Demir", "Sonnenstraße 21", "04109 Leipzig", "DE"],
  ["Lars Petersen", "Am Deich 5", "25980 Sylt", "DE"],
  ["Hannah Vogel", "Burgstraße 14", "01067 Dresden", "DE"],
  ["Tomás Ruiz", "Kurfürstendamm 90", "10709 Berlin", "DE"],
  ["Sophie Brandt", "Marktplatz 2", "69117 Heidelberg", "DE"],
  ["Noah Keller", "Bahnhofstrasse 47", "8001 Zürich", "CH"],
];

// sku, name, price, kg
const PRODUCTS: [string, string, number, number][] = [
  ["ETH-GUJI-250", "Ethiopia Guji, 250 g", 12.9, 0.25],
  ["ESP-NO4-1K", "Espresso Blend No. 4, 1 kg", 31.5, 1],
  ["COL-HUI-250", "Colombia Huila, 250 g", 11.4, 0.25],
  ["DEC-SWP-500", "Decaf Swiss Water, 500 g", 17.8, 0.5],
  ["FLT-V60-02", "V60 filter papers, 100 pcs", 6.9, 0.1],
  ["KEN-AA-250", "Kenya AA Nyeri, 250 g", 14.2, 0.25],
];

const MENU: [string, number][] = [
  ["Flat white", 3.8],
  ["Filter coffee", 2.9],
  ["Cardamom bun", 3.4],
  ["Espresso", 2.4],
  ["Oat cortado", 3.6],
  ["Ethiopia Guji 250g", 12.9],
];

export type Order = {
  n: number;
  person: [name: string, street: string, city: string, cc: string];
  lines: { qty: number; sku: string; name: string; total: number; kg: number }[];
  ship: number;
  total: number;
  tracking: string;
  kg: string;
  note: string;
};

export function orderData(n: number): Order {
  const r = rng(n * 7919);
  const person = PEOPLE[Math.floor(r() * PEOPLE.length)];
  const lines = shuffle(PRODUCTS, r)
    .slice(0, 1 + Math.floor(r() * 3))
    .map(([sku, name, price, kg]) => {
      const qty = 1 + Math.floor(r() * 2);
      return { qty, sku, name, total: qty * price, kg: qty * kg };
    });
  const sub = lines.reduce((a, l) => a + l.total, 0);
  const ship = sub >= 50 ? 0 : 4.9;
  return {
    n,
    person,
    lines,
    ship,
    total: sub + ship,
    tracking: "00340434" + String(Math.floor(r() * 1e12)).padStart(12, "0"),
    kg: (lines.reduce((a, l) => a + l.kg, 0) + 0.15).toFixed(1),
    note: r() < 0.35 ? "Leave at the door" : "",
  };
}

export type Receipt = { n: number; items: [string, number][]; total: number; time: string };

export function receiptData(n: number): Receipt {
  const r = rng(n * 104729);
  const items = shuffle(MENU, r).slice(0, 2 + Math.floor(r() * 3));
  return {
    n,
    items,
    total: items.reduce((a, [, p]) => a + p, 0),
    time: `${8 + Math.floor(r() * 9)}:${String(Math.floor(r() * 60)).padStart(2, "0")}`,
  };
}

/** The ZPL a shop would send for a shipping label. */
export function zplFor(o: Order): string {
  const [name, street, city, cc] = o.person;
  return [
    "^XA",
    "^CI28",
    `^FO40,30^A0N,22^FDFROM ${SHOP.name}, ${SHOP.street}, ${SHOP.city}^FS`,
    "^FO40,90^A0N,24^FDSHIP TO^FS",
    `^FO40,125^A0N,48^FD${name}^FS`,
    `^FO40,185^A0N,36^FD${street}^FS`,
    `^FO40,230^A0N,36^FD${city} ${cc}^FS`,
    `^FO40,320^BY3^BCN,120,Y,N^FD${o.tracking}^FS`,
    `^FO40,520^A0N,26^FDORDER #${o.n}   ${o.kg} KG   1/1^FS`,
    "^XZ",
  ].join("\n");
}

/** `shown` is the human-readable command listing, `bytes` the real ESC/POS stream (to base64). */
export function escposFor(rc: Receipt): { shown: string; bytes: string } {
  const lines = rc.items.map(([n, v]) => n.padEnd(24).slice(0, 24) + v.toFixed(2).padStart(8));
  return {
    shown: [
      "ESC @",
      "ESC a 1",
      '"NORDLICHT COFFEE\\n"',
      `"${SHOP.street} · Hamburg\\n"`,
      "ESC a 0",
      ...lines.map((l) => `"${l}\\n"`),
      `"${"TOTAL".padEnd(24)}${rc.total.toFixed(2).padStart(8)}\\n"`,
      "GS V 0",
    ].join("\n"),
    bytes: "\x1b@\x1ba\x01NORDLICHT COFFEE\n" + lines.join("\n") + "\n\x1dV\x00",
  };
}
