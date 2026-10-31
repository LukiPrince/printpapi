import type { Metadata } from "next";
import { Geist, Geist_Mono } from "next/font/google";
import "./globals.css";
import { Providers } from "@/components/providers";
import { demoMetadata } from "@/demo/meta";

const geistSans = Geist({ variable: "--font-sans", subsets: ["latin"] });
const geistMono = Geist_Mono({ variable: "--font-geist-mono", subsets: ["latin"] });

// The demo build (npm run build:demo) is a public site of its own and says so in its metadata.
const demo = process.env.PRINTPAPI_DEMO === "1";

export const metadata: Metadata = demo
  ? demoMetadata
  : {
      title: "printpapi — print bridge",
      description: "Self-hosted print bridge: submit jobs over HTTP, print them anywhere.",
    };

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html
      lang="en"
      suppressHydrationWarning
      className={`${geistSans.variable} ${geistMono.variable} h-full antialiased`}
    >
      <body className="min-h-full">
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}
