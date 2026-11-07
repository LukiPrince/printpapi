// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { Check, CircleAlert, CircleCheck, Info, PowerOff, TriangleAlert, type LucideIcon } from "lucide-react";
import { Barcode, Logo } from "@/components/brand";
import { cn } from "@/lib/utils";
import type { AgentStatus } from "./api";
import { statusWord } from "./model";

// The breakpoint is 700px, not Tailwind's md (768px): the setup window is a fixed 760px wide and
// must show the rail.
export type NavItem = { id: string; label: string; icon: LucideIcon };

/** The ink rail on the left with the steps (or, in the settings, the sections), and the content column on the right. */
export function Shell({
  steps,
  current,
  nav,
  children,
}: {
  steps?: string[];
  current?: number;
  /** Sections to choose from, for the settings of an installed agent. */
  nav?: { label: string; items: NavItem[]; current: string; onSelect: (id: string) => void };
  children: React.ReactNode;
}) {
  return (
    <div className="min-h-svh min-[700px]:grid min-[700px]:grid-cols-[13rem_minmax(0,1fr)]">
      <aside className="flex flex-col gap-4 bg-sidebar px-4 py-5 text-sidebar-foreground min-[700px]:sticky min-[700px]:top-0 min-[700px]:h-svh">
        <div className="flex items-center gap-2.5 px-1">
          <Logo className="size-7" />
          <span className="font-mono text-base font-bold tracking-tight text-white">printpapi</span>
        </div>
        <Barcode className="mx-1 h-3.5 text-white/30" />
        {steps && (
          <ol className="mt-2 flex gap-4 min-[700px]:flex-col min-[700px]:gap-3">
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
        {nav && (
          <nav aria-label={nav.label} className="mt-2">
            <ul className="flex flex-wrap gap-1 min-[700px]:flex-col">
              {nav.items.map(({ id, label, icon: Icon }) => {
                const on = id === nav.current;
                return (
                  <li key={id}>
                    <button
                      type="button"
                      aria-current={on ? "page" : undefined}
                      onClick={() => nav.onSelect(id)}
                      className={cn(
                        "flex w-full items-center gap-2 rounded-md border-l-2 border-transparent px-2.5 py-1.5 text-left text-sm transition-colors outline-none hover:bg-sidebar-accent hover:text-sidebar-accent-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring",
                        on && "border-sidebar-primary bg-sidebar-accent font-semibold text-sidebar-accent-foreground",
                      )}
                    >
                      <Icon className="size-4 shrink-0" />
                      {label}
                    </button>
                  </li>
                );
              })}
            </ul>
          </nav>
        )}
      </aside>
      <main className="px-4 py-6 min-[700px]:px-8 min-[700px]:py-8">
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

const STATE_LOOK = {
  online: { icon: CircleCheck, cls: "text-state-done" },
  offline: { icon: CircleAlert, cls: "text-state-failed" },
  not_running: { icon: PowerOff, cls: "text-muted-foreground" },
} as const;

/** The state of the agent as colour + icon + word. */
export function AgentStateLine({ state }: { state: AgentStatus["state"] }) {
  const { icon: Icon, cls } = STATE_LOOK[state];
  return (
    <span className={cn("inline-flex items-center gap-1.5 font-medium", cls)}>
      <Icon className="size-4" />
      {statusWord(state)}
    </span>
  );
}
