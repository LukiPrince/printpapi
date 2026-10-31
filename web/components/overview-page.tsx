"use client";

import Link from "next/link";
import { useState } from "react";
import { motion } from "motion/react";
import { ArrowRight, Inbox, Pause, Play, Printer, RefreshCw, Server } from "lucide-react";
import { usePoll } from "@/hooks/use-poll";
import { getMetrics, listJobs, type JobState } from "@/lib/api";
import { JOB_STATES } from "@/lib/job-state";
import { fmtAgo } from "@/lib/format";
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Counter, EmptyState, PulseDot, StatusBadge } from "@/components/bits";
import { OutcomeBar } from "@/components/outcome-bar";

const TILES: JobState[] = ["queued", "claimed", "done", "failed"];

export default function OverviewPage() {
  const [live, setLive] = useState(true);
  const metrics = usePoll(getMetrics, 5000, live);
  const jobs = usePoll(listJobs, 5000, live);

  const counts = metrics.data?.jobs;
  const recent = (jobs.data ?? []).slice(0, 8);
  const total = counts ? Object.values(counts).reduce((a, b) => a + b, 0) : 0;

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <PulseDot live={live} />
          {live ? "Live — refreshing every 5s" : "Paused"}
        </div>
        <div className="flex gap-1.5">
          <Button variant="outline" size="sm" onClick={() => setLive((v) => !v)}>
            {live ? <Pause /> : <Play />}
            {live ? "Pause" : "Resume"}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              metrics.refresh();
              jobs.refresh();
            }}
          >
            <RefreshCw />
            Refresh
          </Button>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        {TILES.map((state, i) => {
          const meta = JOB_STATES[state];
          const Icon = meta.icon;
          return (
            <motion.div
              key={state}
              initial={{ opacity: 0, y: 12 }}
              animate={{ opacity: 1, y: 0 }}
              transition={{ delay: i * 0.06, duration: 0.3 }}
            >
              <Card className="gap-0 py-4">
                <CardContent className="px-4">
                  <div className="flex items-center gap-1.5 text-sm text-muted-foreground">
                    <Icon className="size-3.5" style={{ color: meta.color }} />
                    {meta.label}
                  </div>
                  <div className="mt-2 text-3xl font-bold tracking-tight">
                    {counts ? <Counter value={counts[state]} /> : <Skeleton className="h-8 w-14" />}
                  </div>
                </CardContent>
              </Card>
            </motion.div>
          );
        })}
      </div>

      <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_18rem]">
        <Card>
          <CardHeader>
            <CardTitle>Job outcomes</CardTitle>
            <CardDescription>Every job this server has queued, by state.</CardDescription>
            <CardAction className="text-right">
              <div className="text-2xl font-bold tracking-tight tabular-nums">
                <Counter value={total} />
              </div>
              <div className="font-mono text-[0.6rem] font-semibold tracking-[0.16em] text-muted-foreground uppercase">
                total
              </div>
            </CardAction>
          </CardHeader>
          <CardContent>
            {counts ? <OutcomeBar counts={counts} /> : <Skeleton className="h-3.5 w-full" />}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Fleet</CardTitle>
            <CardDescription>Agents polling this server.</CardDescription>
          </CardHeader>
          <CardContent className="space-y-3">
            <div className="flex items-center justify-between">
              <span className="flex items-center gap-2 text-sm text-muted-foreground">
                <Server className="size-4" /> Agents online
              </span>
              <span className="font-mono font-semibold tabular-nums">
                <Counter value={metrics.data?.agents_online ?? 0} />
                <span className="text-muted-foreground">/{metrics.data?.agents_total ?? 0}</span>
              </span>
            </div>
            <div className="flex items-center justify-between">
              <span className="flex items-center gap-2 text-sm text-muted-foreground">
                <Printer className="size-4" /> Printers
              </span>
              <span className="font-mono font-semibold tabular-nums">
                <Counter value={metrics.data?.printers_total ?? 0} />
              </span>
            </div>
            <Button asChild variant="outline" size="sm" className="w-full">
              <Link href="/devices" prefetch={false}>
                Manage devices <ArrowRight />
              </Link>
            </Button>
          </CardContent>
        </Card>
      </div>

      <Card className="gap-0">
        <CardHeader>
          <CardTitle>Recent activity</CardTitle>
          <CardDescription>The last jobs this server handled.</CardDescription>
        </CardHeader>
        <CardContent className="px-0">
          {jobs.loading && !jobs.data ? (
            <div className="space-y-2 px-6">
              {[0, 1, 2].map((i) => (
                <Skeleton key={i} className="h-10 w-full" />
              ))}
            </div>
          ) : recent.length === 0 ? (
            <EmptyState
              icon={Inbox}
              title="No jobs yet"
              hint="Submit one from Print Something, or fire a test print at a device."
              action={
                <Button asChild variant="brand" size="sm" className="mt-2">
                  <Link href="/print" prefetch={false}>Print something</Link>
                </Button>
              }
            />
          ) : (
            <ul className="divide-y divide-border">
              {recent.map((job, i) => (
                <motion.li
                  key={job.id}
                  initial={{ opacity: 0, x: -8 }}
                  animate={{ opacity: 1, x: 0 }}
                  transition={{ delay: Math.min(i * 0.04, 0.3), duration: 0.25 }}
                  className="flex items-center gap-3 px-6 py-2.5"
                >
                  <span className="w-12 shrink-0 font-mono text-xs text-muted-foreground">
                    #{job.id}
                  </span>
                  <span className="min-w-0 flex-1 truncate text-sm">
                    {job.title || <span className="text-muted-foreground">untitled</span>}
                    <span className="ml-2 text-xs text-muted-foreground">
                      {job.printer_name} · {job.agent_name}
                    </span>
                  </span>
                  <span className="hidden shrink-0 text-xs text-muted-foreground sm:block">
                    {fmtAgo(job.created_at)}
                  </span>
                  <StatusBadge state={job.state} />
                </motion.li>
              ))}
            </ul>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
