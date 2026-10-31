// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The production build's side of the "printpapi-demo" switch: nothing demo-related.
import type { DemoModule } from "./surface";

export const DEMO = false;
export const DemoBanner: DemoModule["DemoBanner"] = null;
export const DemoRail: DemoModule["DemoRail"] = null;
export const DemoLanding: DemoModule["DemoLanding"] = null;
export const DemoSignOut: DemoModule["DemoSignOut"] = null;
export const OVERVIEW_HREF: DemoModule["OVERVIEW_HREF"] = "/";
export const DEMO_SERVER_URL: DemoModule["DEMO_SERVER_URL"] = null;
export const STANDALONE_PATHS: DemoModule["STANDALONE_PATHS"] = [];

export default { DEMO, DemoBanner, DemoRail, DemoLanding, DemoSignOut, DEMO_SERVER_URL, OVERVIEW_HREF, STANDALONE_PATHS } satisfies DemoModule;
