// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { Check, CircleAlert, CircleCheck, Info, TriangleAlert } from "lucide-react";
import { Barcode, Logo } from "@/components/brand";
import { cn } from "@/lib/utils";

/** The ink rail on the left with the steps, and the content column on the right. */
export function Shell({
  steps,
  current,
  children,
}: {
  steps?: string[];
  current?: number;
  children: React.ReactNode;
}) {
  return (
    <div className="min-h-svh md:grid md:grid-cols-[13rem_minmax(0,1fr)]">
      <aside className="flex flex-col gap-4 bg-sidebar px-4 py-5 text-sidebar-foreground md:sticky md:top-0 md:h-svh">
        <div className="flex items-center gap-2.5 px-1">
          <Logo className="size-7" />
          <span className="font-mono text-base font-bold tracking-tight text-white">printpapi</span>
        </div>
        <Barcode className="mx-1 h-3.5 text-white/30" />
        {steps && (
          <ol className="mt-2 flex gap-4 md:flex-col md:gap-3">
            {steps.map((label, n) => {
              const done = n < (current ?? 0);
              const on = n === current;
              return (
                <li
                  key={label}
                  aria-current={on ? "step" : undefined}
                  className={cn("flex items-center gap-2 text-sm", on && "font-semibold text-white")}
                >
                  <span
                    className={cn(
                      "flex size-4 shrink-0 items-center justify-center rounded-full border-[1.5px]",
                      done && "border-sidebar-foreground bg-sidebar-foreground text-sidebar",
                      on && "border-sidebar-primary bg-sidebar-primary",
                    )}
                  >
                    {done && <Check className="size-2.5" strokeWidth={3} />}
                  </span>
                  {label}
                  <span className="sr-only">{done ? "(done)" : on ? "(current step)" : ""}</span>
                </li>
              );
            })}
          </ol>
        )}
      </aside>
      <main className="px-4 py-6 md:px-8 md:py-8">
        <div className="mx-auto max-w-xl space-y-4">{children}</div>
      </main>
    </div>
  );
}

const TONES = {
  info: { icon: Info, cls: "text-muted-foreground border-border" },
  warn: { icon: TriangleAlert, cls: "text-state-queued border-state-queued/40 bg-state-queued/8" },
  error: { icon: CircleAlert, cls: "text-state-failed border-state-failed/40 bg-state-failed/8" },
  ok: { icon: CircleCheck, cls: "text-state-done border-state-done/40 bg-state-done/8" },
} as const;

/** A message with colour, icon and words, never colour alone. */
export function Notice({
  tone,
  children,
  className,
}: {
  tone: keyof typeof TONES;
  children: React.ReactNode;
  className?: string;
}) {
  const { icon: Icon, cls } = TONES[tone];
  return (
    <div
      role={tone === "error" ? "alert" : "status"}
      className={cn("flex items-start gap-2 rounded-lg border px-3 py-2 text-sm", cls, className)}
    >
      <Icon className="mt-0.5 size-4 shrink-0" />
      <div className="min-w-0 text-foreground">{children}</div>
    </div>
  );
}

export function Actions({ children }: { children: React.ReactNode }) {
  return <div className="flex flex-wrap items-center justify-end gap-2 pt-2">{children}</div>;
}
