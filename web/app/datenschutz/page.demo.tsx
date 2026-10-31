// Demo build only ("demo.tsx" is a page extension there): the demo site's privacy notice.
// Production never sees this file.
import type { Metadata } from "next";

export { default } from "@/demo/privacy";

export const metadata: Metadata = { title: "Datenschutz · printpapi live demo" };
