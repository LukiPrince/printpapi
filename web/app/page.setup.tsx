// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { Home } from "@/setup/home";

// Marker for npm run check:bundles: the setup bundle holds it, the dashboard and the demo never do.
const SETUP_MARKER = "printpapi-setup-build";

export default function Page() {
  return (
    <div data-build={SETUP_MARKER}>
      <Home />
    </div>
  );
}
