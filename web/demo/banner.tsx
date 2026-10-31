// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The amber strip above the dashboard in the demo build: what this is, plus the demo controls.
"use client";

import Link from "next/link";
import { ArrowRight, RotateCcw } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { demoActions, useDemo } from "./store.ts";

// Buttons sit on amber, so they get a translucent white fill instead of the page's outline look.
const ON_AMBER = "border-brand-foreground/20 bg-white/35 text-brand-foreground hover:bg-white/55 hover:text-brand-foreground";

export function DemoBanner() {
  const traffic = useDemo((s) => s.traffic);
  return (
    <div className="bg-brand text-brand-foreground">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2 px-4 py-2 text-[0.8125rem] md:px-5">
        <span className="rounded-sm bg-brand-foreground px-1.5 py-0.5 font-mono text-[0.66rem] font-bold tracking-[0.18em] text-brand">
          DEMO
        </span>
        <span className="min-w-56 flex-1">
          Sample data for a fictional coffee roaster. What you do stays in this browser tab. No real printer is
          attached.{" "}
          <Link prefetch={false} href="/datenschutz/" className="underline underline-offset-2">
            Datenschutz
          </Link>
        </span>
        <div className="flex flex-wrap items-center gap-2">
          <label className="flex cursor-pointer items-center gap-2 font-medium">
            <Switch
              size="sm"
              checked={traffic}
              onCheckedChange={(on) => {
                demoActions.setTraffic(on);
                toast(on ? "Shop traffic on" : "Shop traffic paused");
              }}
              className="data-checked:bg-brand-foreground data-unchecked:bg-brand-foreground/25 dark:data-unchecked:bg-brand-foreground/25"
            />
            Shop traffic
          </label>
          <Button
            variant="outline"
            size="sm"
            className={ON_AMBER}
            onClick={() => {
              demoActions.reset();
              toast("Demo reset", { description: "Back to the sample data." });
            }}
          >
            <RotateCcw />
            Reset
          </Button>
          <Button asChild variant="outline" size="sm" className={ON_AMBER}>
            <a href="https://github.com/LukiPrince/printpapi#get-started">
              Self-host printpapi <ArrowRight />
            </a>
          </Button>
        </div>
      </div>
    </div>
  );
}
