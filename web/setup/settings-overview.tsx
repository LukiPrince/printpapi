// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { fmtAgo } from "@/lib/format";
import type { AgentStatus } from "./api";
import { AgentStateLine, Notice } from "./bits";

/** The first section: whether the agent runs, and what it last did. Reads the status file only. */
export function Overview({ status, error }: { status: AgentStatus | null; error: string | null }) {
  if (!status) {
    return error ? <Notice tone="error">{error}</Notice> : <Skeleton className="h-48 w-full" />;
  }
  const { lastJob } = status;
  return (
    <Card>
      <CardHeader>
        <CardTitle>Overview</CardTitle>
        <CardDescription>
          <AgentStateLine state={status.state} />
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {status.state === "offline" && (
          <Notice tone="error">{status.error ?? "The agent cannot reach the server."}</Notice>
        )}
        {status.state === "not_running" && (
          <Notice tone="warn">
            The agent is not running, or it has not reported for two minutes. Look at the Log, or use
            Restart.
          </Notice>
        )}
        <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1 text-sm">
          <dt className="text-muted-foreground">Server</dt>
          <dd className="truncate font-mono">{status.server || "—"}</dd>
          <dt className="text-muted-foreground">Device</dt>
          <dd className="truncate">{status.name || "—"}</dd>
          <dt className="text-muted-foreground">Last contact</dt>
          <dd>{fmtAgo(status.lastContact)}</dd>
          <dt className="text-muted-foreground">Last job</dt>
          <dd>{lastJob ? `#${lastJob.id} ${lastJob.ok ? "printed" : "failed"}, ${fmtAgo(lastJob.at)}` : "none yet"}</dd>
          <dt className="text-muted-foreground">Agent version</dt>
          <dd className="font-mono">{status.version || "—"}</dd>
        </dl>
      </CardContent>
    </Card>
  );
}
