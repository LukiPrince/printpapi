"use client";

import { Lightbulb } from "lucide-react";
import { usePoll } from "@/hooks/use-poll";
import { getMetrics } from "@/lib/api";
import { Counter, PulseDot } from "@/components/bits";
import { Card, CardContent } from "@/components/ui/card";

/** Right rail: contextual help plus a small always-on fleet readout. */
export function LiveRail({ help }: { help: string }) {
  const { data } = usePoll(getMetrics, 10000);
  const queued = data?.jobs.queued ?? 0;
  const printing = data?.jobs.claimed ?? 0;

  return (
    <aside className="hidden border-l border-border bg-card px-5 py-6 xl:block">
      <h2 className="font-mono text-[0.66rem] font-semibold tracking-[0.2em] text-brand uppercase">
        Live
      </h2>
      <Card className="mt-3 gap-0 py-4">
        <CardContent className="space-y-3 px-4">
          <Row
            label="Agents online"
            value={data?.agents_online ?? 0}
            suffix={`/ ${data?.agents_total ?? 0}`}
            dot={(data?.agents_online ?? 0) > 0}
          />
          <Row label="Printers" value={data?.printers_total ?? 0} />
          <Row label="In queue" value={queued} />
          <Row label="Printing" value={printing} dot={printing > 0} />
        </CardContent>
      </Card>

      <h2 className="mt-7 flex items-center gap-1.5 font-mono text-[0.66rem] font-semibold tracking-[0.2em] text-brand uppercase">
        <Lightbulb className="size-3.5" />
        Tips
      </h2>
      <p className="mt-3 text-sm leading-relaxed text-muted-foreground">{help}</p>

      <p className="mt-7 text-xs text-muted-foreground">
        Press{" "}
        <kbd className="rounded border border-border bg-muted px-1 py-0.5 font-mono text-[0.7rem]">
          ⌘K
        </kbd>{" "}
        for the command palette.
      </p>
    </aside>
  );
}

function Row({
  label,
  value,
  suffix,
  dot,
}: {
  label: string;
  value: number;
  suffix?: string;
  dot?: boolean;
}) {
  return (
    <div className="flex items-center justify-between gap-3">
      <span className="flex items-center gap-2 text-sm text-muted-foreground">
        {dot !== undefined && <PulseDot live={dot} />}
        {label}
      </span>
      <span className="font-mono text-sm font-semibold tabular-nums">
        <Counter value={value} />
        {suffix && <span className="ml-1 text-muted-foreground">{suffix}</span>}
      </span>
    </div>
  );
}
