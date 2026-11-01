// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// Page metadata of the demo site. Server-only (read by app/layout.tsx at build time, never in a client chunk).
import type { Metadata } from "next";

export const SITE = "https://printpapi.lukaswotzka.com";
const REPO = "https://github.com/LukiPrince/printpapi";

const title = "printpapi live demo: self-hosted print bridge";
const description =
  "Send a print job over HTTP and watch it come out of the printer. A live, in-browser demo of printpapi and its dashboard.";

export const demoMetadata = {
  metadataBase: new URL(SITE),
  title,
  description,
  // "./" resolves against each page's own path (next/dist/lib/metadata/resolvers/resolve-url.js), so
  // every page names itself as canonical, on this host and not on the workers.dev mirror.
  alternates: { canonical: "./" },
  openGraph: {
    type: "website",
    siteName: "printpapi",
    url: "./",
    title,
    description,
    images: [{ url: "/og.png", width: 1200, height: 630, alt: "The printpapi live demo: a print job on its way to a label printer" }],
  },
  twitter: { card: "summary_large_image" },
} satisfies Metadata;

// schema.org description of the software, rendered as JSON-LD by app/layout.tsx in the demo build.
export const demoJsonLd = {
  "@context": "https://schema.org",
  "@type": "SoftwareApplication",
  name: "printpapi",
  url: `${SITE}/`,
  description:
    "Self-hosted, source-available print bridge and PrintNode alternative: an HTTP API for print jobs and an agent that polls it outbound and prints to document, label and receipt printers.",
  applicationCategory: "BusinessApplication",
  operatingSystem: "Windows, Linux, macOS",
  image: `${SITE}/og.png`,
  downloadUrl: `${REPO}/releases/latest`,
  license: `${REPO}/blob/main/LICENSE`,
  sameAs: [REPO],
  author: { "@type": "Person", name: "Lukas Wotzka" },
  offers: { "@type": "Offer", price: "0", priceCurrency: "EUR" },
};
