"use client";

import { useMemo, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { toast } from "sonner";
import { ChevronRight, History, Pause, Play, Search, X } from "lucide-react";
import { usePoll } from "@/hooks/use-poll";
import { ApiError, cancelJob, listJobs, type Job, type JobState } from "@/lib/api";
import { JOB_STATES, STATE_ORDER } from "@/lib/job-state";
import { fmtAgo, fmtDuration, fmtTime } from "@/lib/format";
import { Card, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState, PulseDot, StatusBadge } from "@/components/bits";
import { cn } from "@/lib/utils";

export default function HistoryPage() {
  const [live, setLive] = useState(true);
  const { data: jobs, loading, refresh } = usePoll(listJobs, 5000, live);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<JobState | "all">("all");
  const [open, setOpen] = useState<number | null>(null);

  const all = useMemo(() => jobs ?? [], [jobs]);
  const counts = useMemo(() => {
    const c = Object.fromEntries(STATE_ORDER.map((s) => [s, 0])) as Record<JobState, number>;
    for (const j of all) c[j.state] += 1;
    return c;
  }, [all]);

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase();
    return all.filter((j) => {
      if (filter !== "all" && j.state !== filter) return false;
      if (!q) return true;
      return [String(j.id), j.title, j.printer_name, j.agent_name, j.type]
        .filter(Boolean)
        .some((v) => String(v).toLowerCase().includes(q));
    });
  }, [all, query, filter]);

  async function cancel(job: Job) {
    try {
      await cancelJob(job.id);
      toast.success(`Job #${job.id} cancelled`);
    } catch (err) {
      // Only 409 means "an agent claimed it first" — a 401/404/network error is something else
      // entirely and must not be reported as a race. The refresh shows the real state either way.
      const claimed = err instanceof ApiError && err.status === 409;
      toast.warning(`Job #${job.id} could not be cancelled`, {
        description: claimed
          ? "An agent already claimed it."
          : err instanceof Error
            ? err.message
            : String(err),
      });
    } finally {
      refresh();
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <div className="relative min-w-56 flex-1">
          <Search className="absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search id, title, printer, agent…"
            className="pl-8"
            aria-label="Search jobs"
          />
          {query && (
            <Button
              variant="ghost"
              size="icon-xs"
              aria-label="Clear search"
              className="absolute top-1/2 right-1.5 -translate-y-1/2"
              onClick={() => setQuery("")}
            >
              <X />
            </Button>
          )}
        </div>
        <Button variant="outline" size="sm" onClick={() => setLive((v) => !v)}>
          {live ? <Pause /> : <Play />}
          {live ? "Pause" : "Resume"}
        </Button>
      </div>

      <div className="flex flex-wrap gap-1.5">
        <FilterChip active={filter === "all"} onClick={() => setFilter("all")} label="All" count={all.length} />
        {STATE_ORDER.map((s) => (
          <FilterChip
            key={s}
            active={filter === s}
            onClick={() => setFilter(s)}
            label={JOB_STATES[s].label}
            count={counts[s]}
            color={JOB_STATES[s].color}
          />
        ))}
        <span className="ml-auto flex items-center gap-1.5 self-center text-xs text-muted-foreground">
          <PulseDot live={live} />
          {live ? "live" : "paused"}
        </span>
      </div>

      <Card className="gap-0 py-0">
        <CardContent className="px-0">
          {loading && !jobs ? (
            <div className="space-y-2 p-4">
              {[0, 1, 2, 3].map((i) => (
                <Skeleton key={i} className="h-11 w-full" />
              ))}
            </div>
          ) : rows.length === 0 ? (
            <EmptyState
              icon={History}
              title={all.length === 0 ? "No jobs yet" : "Nothing matches that filter"}
              hint={
                all.length === 0
                  ? "Submitted jobs show up here within a second."
                  : "Try a different search term or clear the state filter."
              }
            />
          ) : (
            <ul className="divide-y divide-border">
              {rows.map((job) => (
                <li key={job.id}>
                  <button
                    type="button"
                    onClick={() => setOpen((id) => (id === job.id ? null : job.id))}
                    aria-expanded={open === job.id}
                    className="flex w-full items-center gap-3 px-4 py-2.5 text-left transition-colors hover:bg-brand/6"
                  >
                    <ChevronRight
                      className={cn(
                        "size-3.5 shrink-0 text-muted-foreground transition-transform",
                        open === job.id && "rotate-90",
                      )}
                    />
                    <span className="w-12 shrink-0 font-mono text-xs text-muted-foreground">
                      #{job.id}
                    </span>
                    <span className="min-w-0 flex-1 truncate text-sm">
                      {job.title || <span className="text-muted-foreground">untitled</span>}
                    </span>
                    <span className="hidden w-40 shrink-0 truncate font-mono text-xs text-muted-foreground md:block">
                      {job.printer_name}
                    </span>
                    <span className="hidden w-20 shrink-0 text-xs text-muted-foreground sm:block">
                      {fmtAgo(job.created_at)}
                    </span>
                    <StatusBadge state={job.state} />
                  </button>

                  <AnimatePresence initial={false}>
                    {open === job.id && (
                      <motion.div
                        initial={{ height: 0, opacity: 0 }}
                        animate={{ height: "auto", opacity: 1 }}
                        exit={{ height: 0, opacity: 0 }}
                        transition={{ duration: 0.2 }}
                        className="overflow-hidden"
                      >
                        <dl className="grid gap-x-6 gap-y-1.5 bg-muted/40 px-4 py-3 pl-11 text-sm sm:grid-cols-2">
                          <Detail label="Computer" value={job.agent_name} />
                          <Detail label="Printer" value={`${job.printer_name} (#${job.printer_id})`} />
                          <Detail label="Type" value={`${job.type} · mode ${job.mode}`} />
                          <Detail label="Created" value={fmtTime(job.created_at)} />
                          <Detail label="Finished" value={fmtTime(job.finished_at)} />
                          <Detail
                            label="Duration"
                            value={fmtDuration(job.created_at, job.finished_at)}
                          />
                          {job.error && (
                            <div className="sm:col-span-2">
                              <dt className="font-mono text-[0.68rem] tracking-[0.12em] text-muted-foreground uppercase">
                                Error
                              </dt>
                              <dd
                                className="font-mono text-sm break-words"
                                style={{ color: "var(--state-failed)" }}
                              >
                                {job.error}
                              </dd>
                            </div>
                          )}
                          {job.state === "queued" && (
                            <div className="sm:col-span-2">
                              <Button variant="destructive" size="sm" onClick={() => cancel(job)}>
                                <X /> Cancel this job
                              </Button>
                            </div>
                          )}
                        </dl>
                      </motion.div>
                    )}
                  </AnimatePresence>
                </li>
              ))}
            </ul>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

function Detail({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="font-mono text-[0.68rem] tracking-[0.12em] text-muted-foreground uppercase">
        {label}
      </dt>
      <dd className="font-mono">{value}</dd>
    </div>
  );
}

function FilterChip({
  active,
  onClick,
  label,
  count,
  color,
}: {
  active: boolean;
  onClick: () => void;
  label: string;
  count: number;
  color?: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      className={cn(
        "relative flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs font-medium transition-colors",
        active ? "border-transparent text-primary-foreground" : "border-border hover:bg-muted",
      )}
    >
      {active && (
        <motion.span
          layoutId="filter-chip"
          className="absolute inset-0 rounded-full bg-primary"
          transition={{ type: "spring", stiffness: 500, damping: 40 }}
        />
      )}
      {color && (
        <span
          className="relative size-2 rounded-full"
          style={{ backgroundColor: color }}
          aria-hidden
        />
      )}
      <span className="relative">{label}</span>
      <span className="relative font-mono tabular-nums opacity-70">{count}</span>
    </button>
  );
}
