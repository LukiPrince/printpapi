"use client";

import OverviewPage from "@/components/overview-page";
import { DemoLanding } from "printpapi-demo";

// The demo build opens on the start page and keeps the dashboard at /overview; the embedded
// dashboard has no start page and stays the overview at /.
export default function Page() {
  return DemoLanding ? <DemoLanding /> : <OverviewPage />;
}
