// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The demo build's side of the "printpapi-demo" switch.
import type { DemoModule } from "./surface";
import { installDemo } from "./store";
import { DemoBanner as Banner } from "./banner";
import { DemoRail as Rail } from "./rail";
import { Landing } from "./landing";
import { SignOutLink } from "./signout";

export const DEMO = true;
export const DemoBanner: DemoModule["DemoBanner"] = Banner;
export const DemoRail: DemoModule["DemoRail"] = Rail;
export const DemoLanding: DemoModule["DemoLanding"] = Landing;
export const DemoSignOut: DemoModule["DemoSignOut"] = SignOutLink;
export const OVERVIEW_HREF = "/overview";
export const DEMO_SERVER_URL = "https://print.example.com";
export const STANDALONE_PATHS: DemoModule["STANDALONE_PATHS"] = ["/", "/datenschutz"];

// Written to window so the string survives minification; scripts/check-bundles.mjs greps for it.
export const DEMO_MARKER = "printpapi-demo-build";
if (typeof window !== "undefined") (window as unknown as { __printpapiDemo?: string }).__printpapiDemo = DEMO_MARKER;

// Patches window.fetch before the first request the dashboard makes.
if (typeof window !== "undefined") installDemo();

export default { DEMO, DemoBanner, DemoRail, DemoLanding, DemoSignOut, DEMO_SERVER_URL, OVERVIEW_HREF, STANDALONE_PATHS } satisfies DemoModule;
