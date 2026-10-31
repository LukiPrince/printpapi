// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The shape both builds export as the module specifier "printpapi-demo" (see next.config.ts):
// off.ts for the embedded dashboard, on.tsx for the demo site. Product code imports only this.
import type { ComponentType } from "react";

export type DemoModule = {
  DEMO: boolean;
  DemoBanner: ComponentType | null;
  DemoRail: ComponentType<{ help: string }> | null;
  DemoLanding: ComponentType | null;
  DemoSignOut: ComponentType | null;
  DEMO_SERVER_URL: string | null;
  OVERVIEW_HREF: string;
  /** Public pages that render without the login gate and the dashboard shell (normalized paths). */
  STANDALONE_PATHS: readonly string[];
};
