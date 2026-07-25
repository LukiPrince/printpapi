"use client";

import { useMemo, useState } from "react";
import Link from "next/link";
import { AnimatePresence, motion } from "motion/react";
import { toast } from "sonner";
import { Check, FileText, Loader2, Printer, Server, Zap } from "lucide-react";
import { usePoll } from "@/hooks/use-poll";
import { createJob, listComputers, listPrinters, type Printer as PrinterT } from "@/lib/api";
import { fmtAgo } from "@/lib/format";
import { testJob } from "@/lib/testdoc";
import { Card, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState, PulseDot } from "@/components/bits";

type AgentGroup = {
  id: number;
  name: string;
  online: boolean;
  last_seen_at: number | null;
  printers: PrinterT[];
};

export default function DevicesPage() {
  const { data: printers, loading, refresh } = usePoll(listPrinters, 5000);
  const { data: computers } = usePoll(listComputers, 5000);

  const agents: AgentGroup[] = useMemo(() => {
    const byAgent = new Map<number, { name: string; printers: PrinterT[] }>();
    for (const p of printers ?? []) {
      const g = byAgent.get(p.agent_id) ?? { name: p.agent_name, printers: [] };
      g.printers.push(p);
      byAgent.set(p.agent_id, g);
    }
    // /computers is the authority on which agents exist — one that reported no printers still
    // shows up. If that call is unavailable, fall back to what /printers says about its agents.
    if (!computers) {
      return [...byAgent].map(([id, g]) => ({
        id,
        name: g.name,
        online: g.printers.some((p) => p.online),
        last_seen_at: null,
        printers: g.printers,
      }));
    }
    return computers.map((c) => ({
      id: c.id,
      name: c.name,
      online: c.online,
      last_seen_at: c.last_seen_at,
      printers: byAgent.get(c.id)?.printers ?? [],
    }));
  }, [printers, computers]);

  if (loading && !printers) {
    return (
      <div className="space-y-3">
        {[0, 1].map((i) => (
          <Skeleton key={i} className="h-32 w-full" />
        ))}
      </div>
    );
  }

  if (agents.length === 0) {
    return (
      <Card>
        <EmptyState
          icon={Server}
          title="No agents have checked in"
          hint="Run the agent on the machine with the printers. It registers itself and its printers on the first poll — no inbound ports needed."
          action={
            <Button asChild variant="brand" size="sm" className="mt-2">
              <Link href="/downloads" prefetch={false}>Set up an agent</Link>
            </Button>
          }
        />
      </Card>
    );
  }

  return (
    <div className="space-y-6">
      {agents.map((agent, gi) => (
        <motion.section
          key={agent.id}
          initial={{ opacity: 0, y: 10 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ delay: gi * 0.05, duration: 0.28 }}
        >
          <div className="mb-2.5 flex items-center gap-2">
            <PulseDot live={agent.online} />
            <h2 className="font-mono text-sm font-semibold">{agent.name}</h2>
            <span className="text-xs text-muted-foreground">
              {agent.online
                ? "polling"
                : agent.last_seen_at
                  ? `offline · last seen ${fmtAgo(agent.last_seen_at)}`
                  : "not seen in the last minute"}{" "}
              · {agent.printers.length}{" "}
              {agent.printers.length === 1 ? "printer" : "printers"}
            </span>
          </div>
          {agent.printers.length === 0 ? (
            <Card>
              <CardContent className="px-4 text-sm text-muted-foreground">
                Registered, but reported no printers. Check the <code>printers</code> line in the
                agent&apos;s <code>agent.ini</code>.
              </CardContent>
            </Card>
          ) : (
            <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
              {agent.printers.map((p) => (
                <PrinterCard key={p.id} printer={p} onPrinted={refresh} />
              ))}
            </div>
          )}
        </motion.section>
      ))}
    </div>
  );
}

function PrinterCard({ printer, onPrinted }: { printer: PrinterT; onPrinted: () => void }) {
  const [state, setState] = useState<"idle" | "sending" | "sent">("idle");

  async function test() {
    setState("sending");
    try {
      const { job_id } = await createJob(testJob(printer.id, printer.can_pdf));
      setState("sent");
      toast.success(`Test job #${job_id} queued`, { description: printer.name });
      onPrinted();
      setTimeout(() => setState("idle"), 1800);
    } catch (e) {
      setState("idle");
      toast.error("Test print failed", { description: e instanceof Error ? e.message : "" });
    }
  }

  return (
    <Card className="gap-0 py-4 transition-colors hover:border-brand/50">
      <CardContent className="px-4">
        <div className="flex items-start gap-2">
          <Printer className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
          <div className="min-w-0 flex-1">
            <p className="truncate font-medium" title={printer.name}>
              {printer.name}
            </p>
            <p className="font-mono text-xs text-muted-foreground">#{printer.id}</p>
          </div>
          <PulseDot live={printer.online} className="mt-1.5" />
        </div>

        <div className="mt-3 flex items-center gap-1.5">
          <Badge variant="outline" className="gap-1">
            {printer.can_pdf ? <FileText /> : <Zap />}
            {printer.can_pdf ? "PDF + raw" : "raw only"}
          </Badge>
          <Badge variant={printer.online ? "secondary" : "outline"}>
            {printer.online ? "online" : "offline"}
          </Badge>
        </div>

        <Button
          variant="outline"
          size="sm"
          className="mt-3 w-full"
          disabled={state !== "idle"}
          onClick={test}
        >
          <AnimatePresence mode="wait" initial={false}>
            <motion.span
              key={state}
              initial={{ opacity: 0, y: 4 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -4 }}
              transition={{ duration: 0.15 }}
              className="flex items-center gap-1.5"
            >
              {state === "sending" && <Loader2 className="size-3.5 animate-spin" />}
              {state === "sent" && <Check className="size-3.5" />}
              {state === "idle" ? "Test print" : state === "sending" ? "Queueing" : "Queued"}
            </motion.span>
          </AnimatePresence>
        </Button>
      </CardContent>
    </Card>
  );
}
