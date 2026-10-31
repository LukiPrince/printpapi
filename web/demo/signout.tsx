// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The sidebar's bottom-right button in the demo: there is no login to leave, so it goes back to the tour.
"use client";

import Link from "next/link";
import { LogOut } from "lucide-react";
import { Button } from "@/components/ui/button";

export function SignOutLink() {
  return (
    <Button
      variant="ghost"
      size="sm"
      asChild
      className="ml-auto font-mono text-[0.7rem] tracking-[0.08em] text-sidebar-foreground uppercase hover:bg-white/7 hover:text-white"
    >
      <Link href="/" prefetch={false}>
        <LogOut />
        Back to the tour
      </Link>
    </Button>
  );
}
