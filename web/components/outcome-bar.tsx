"use client";

import { motion } from "motion/react";
import { JOB_STATES, STATE_ORDER } from "@/lib/job-state";
import type { JobState } from "@/lib/api";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

/**
 * One stacked bar of job outcomes. Segments carry a 2px surface gap and rounded
 * data-ends; identity is legend + direct value + icon, never colour alone.
 */
export function OutcomeBar({ counts }: { counts: Record<JobState, number> }) {
  const present = STATE_ORDER.filter((s) => counts[s] > 0);
  const total = present.reduce((sum, s) => sum + counts[s], 0);

  if (total === 0) {
    return (
      <div className="flex h-3.5 w-full items-center rounded-full bg-muted">
        <span className="sr-only">No jobs yet</span>
      </div>
    );
  }

  return (
    <div>
      <div className="flex h-3.5 w-full gap-0.5" role="img" aria-label={ariaLabel(counts, total)}>
        {present.map((state, i) => {
          const meta = JOB_STATES[state];
          const pct = (counts[state] / total) * 100;
          return (
            <Tooltip key={state}>
              <TooltipTrigger asChild>
                <motion.div
                  initial={{ flexGrow: 0 }}
                  animate={{ flexGrow: counts[state] }}
                  transition={{ duration: 0.6, ease: [0.2, 0.8, 0.3, 1] }}
                  className="min-w-1 cursor-default"
                  style={{
                    backgroundColor: meta.color,
                    flexBasis: 0,
                    borderStartStartRadius: i === 0 ? 4 : 0,
                    borderEndStartRadius: i === 0 ? 4 : 0,
                    borderStartEndRadius: i === present.length - 1 ? 4 : 0,
                    borderEndEndRadius: i === present.length - 1 ? 4 : 0,
                  }}
                />
              </TooltipTrigger>
              <TooltipContent>
                {meta.label}: {counts[state]} ({pct.toFixed(0)}%)
              </TooltipContent>
            </Tooltip>
          );
        })}
      </div>

      <ul className="mt-3 flex flex-wrap gap-x-5 gap-y-1.5">
        {present.map((state) => {
          const meta = JOB_STATES[state];
          const Icon = meta.icon;
          return (
            <li key={state} className="flex items-center gap-1.5 text-sm">
              <Icon className="size-3.5" style={{ color: meta.color }} />
              <span className="text-muted-foreground">{meta.label}</span>
              <span className="font-mono font-semibold tabular-nums">{counts[state]}</span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

function ariaLabel(counts: Record<JobState, number>, total: number) {
  const parts = STATE_ORDER.filter((s) => counts[s] > 0).map(
    (s) => `${JOB_STATES[s].label} ${counts[s]}`,
  );
  return `Job outcomes, ${total} total: ${parts.join(", ")}`;
}
