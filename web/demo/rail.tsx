// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The demo's right rail, in place of live-rail.tsx: the Live readout, the paper tray the virtual
// printers print into, a "Try this" checklist and the page's tips.
"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { usePathname } from "next/navigation";
import { useReducedMotion } from "motion/react";
import { toast } from "sonner";
import { Archive, Check, FileText, Lightbulb, Printer, Receipt, ShoppingBag, Tag } from "lucide-react";
import { Counter, PulseDot } from "@/components/bits";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { fmtAgo } from "@/lib/format";
import { cn } from "@/lib/utils";
import type { DemoJob, DemoPrinter } from "./seed.ts";
import { Paper, paperStyles } from "./paper.tsx";
import { demoActions, useDemo } from "./store.ts";

const H2 = "flex items-center gap-1.5 font-mono text-[0.66rem] font-semibold tracking-[0.2em] text-brand uppercase";

function iconFor(p: DemoPrinter | undefined) {
  if (p?.paper === "file") return Archive;
  if (p?.paper === "receipt") return Receipt;
  return p?.can_pdf ? FileText : Tag;
}

export function DemoRail({ help }: { help: string }) {
  const state = useDemo((s) => s);
  const pathname = usePathname();
  const still = useReducedMotion();
  // Outputs that finish while this rail is mounted feed out of the slot; the ones already there sit.
  const [since] = useState(() => Date.now() / 1000);

  useEffect(() => {
    // not normalizePath(): lib/nav imports this module's entry, which would be an import cycle
    if (pathname.replace(/\/$/, "") === "/history") demoActions.markHistorySeen();
  }, [pathname]);

  // Shop traffic is announced here. The newest slip's order number is the marker. A visitor's own
  // "Simulate order" click raises it first (mutedOrder), so that order is toasted once, by the button.
  const seenOrder = useRef<number | null>(null);
  const mutedOrder = useRef<number | null>(null);
  const newestSlip = state.jobs.reduce((m, j) => (j.kind === "slip" && j.ref !== null && j.ref > m ? j.ref : m), 0);
  useEffect(() => {
    const prev = seenOrder.current;
    seenOrder.current = newestSlip;
    if (prev === null || newestSlip <= prev || newestSlip === mutedOrder.current) return;
    toast(`WooCommerce order #${newestSlip}`, { description: "Packing slip + shipping label queued." });
  }, [newestSlip]);

  const outs = [...state.outputs]
    .reverse()
    .map((id) => state.jobs.find((j) => j.id === id))
    .filter((j): j is DemoJob => !!j);
  const top = outs[0];
  const topPrinter = top && state.printers.find((p) => p.id === top.printer_id);

  const online = state.agents.filter((a) => a.online).length;
  const queued = state.jobs.filter((j) => j.state === "queued").length;
  const printing = state.jobs.filter((j) => j.state === "claimed").length;

  const T = state.todo;
  const items: { ok: boolean; title: string; body: string; action: React.ReactNode }[] = [
    {
      ok: T.test,
      title: "Send a test print",
      body: "Devices → Test print on the Zebra GK420d. Watch the paper tray.",
      action: <GoButton href="/devices">Go to Devices</GoButton>,
    },
    {
      ok: T.history,
      title: "Find it in Print History",
      body: "Open the job to see its type and how long it took.",
      action: <GoButton href="/history">Open history</GoButton>,
    },
    {
      ok: T.order,
      title: "Simulate a shop order",
      body: "A Shopify order arrives: packing slip on the laser, label on the Zebra.",
      action: (
        <Button
          variant="brand"
          size="sm"
          onClick={() => {
            const n = demoActions.simulateOrder();
            mutedOrder.current = n;
            toast(`Shopify order #${n} received`, { description: "Packing slip and shipping label queued." });
          }}
        >
          <ShoppingBag />
          Simulate order
        </Button>
      ),
    },
    {
      ok: T.cancel,
      title: "Cancel a stuck job",
      body: "packing-station-2 is offline. Its return label waits in the queue until you cancel it.",
      action: <GoButton href="/history">Show queued</GoButton>,
    },
  ];

  return (
    <aside className="hidden space-y-7 border-l border-border bg-card px-5 py-6 xl:block">
      <section>
        <h2 className={H2}>Live</h2>
        <Card className="mt-3 gap-0 py-4">
          <CardContent className="space-y-3 px-4">
            <Row label="Agents online" value={online} suffix={`/ ${state.agents.length}`} dot={online > 0} />
            <Row label="Printers" value={state.printers.length} />
            <Row label="In queue" value={queued} />
            <Row label="Printing" value={printing} dot={printing > 0} />
          </CardContent>
        </Card>
      </section>

      <section>
        <h2 className={H2}>
          <Printer className="size-3.5" />
          Paper tray
        </h2>
        <div className={cn(paperStyles.tray, "mt-3")}>
          <div className={paperStyles.slot} />
          {top ? (
            <>
              <div className={paperStyles.trayBig}>
                {/* keyed by job so each new output feeds out of the slot again */}
                <Paper key={top.id} job={top} state={state} feeding={!still && (top.finished_at ?? 0) > since} />
              </div>
              <p className="px-3.5 pb-3 text-center text-xs text-muted-foreground">
                <b className="font-semibold text-foreground">#{top.id}</b> · {topPrinter?.name} · {fmtAgo(top.finished_at)}
              </p>
            </>
          ) : (
            <p className="px-4 py-6 text-center text-[0.8125rem] text-muted-foreground">
              Nothing printed in this session yet. Send a test print from Devices.
            </p>
          )}
        </div>
        {outs.length > 1 && (
          <ul className="mt-2.5">
            {outs.slice(1, 5).map((j) => {
              const Icon = iconFor(state.printers.find((p) => p.id === j.printer_id));
              return (
                <li key={j.id} className="flex min-w-0 items-center gap-2 border-b border-border py-1.5 text-[0.78rem] last:border-0">
                  <Icon className="size-3.5 shrink-0 text-muted-foreground" />
                  <span className="min-w-0 flex-1 truncate">{j.title}</span>
                  <small className="shrink-0 text-muted-foreground">{fmtAgo(j.finished_at)}</small>
                </li>
              );
            })}
          </ul>
        )}
      </section>

      <section>
        <h2 className={H2}>Try this</h2>
        <ol className="mt-3 space-y-3">
          {items.map((it, i) => (
            <li key={it.title} className="grid grid-cols-[1.4rem_minmax(0,1fr)] items-start gap-2 text-[0.84rem]">
              <span
                className={cn(
                  "grid size-[1.4rem] place-items-center rounded-full border font-mono text-[0.7rem]",
                  it.ok ? "border-state-done bg-state-done text-white" : "border-border text-muted-foreground",
                )}
              >
                {it.ok ? <Check className="size-3.5" aria-label="done" /> : i + 1}
              </span>
              <div>
                <b className={cn("block font-semibold", it.ok && "text-muted-foreground line-through")}>{it.title}</b>
                <small className="text-[0.76rem] text-muted-foreground">{it.body}</small>
                {!it.ok && <div className="mt-1.5">{it.action}</div>}
              </div>
            </li>
          ))}
        </ol>
      </section>

      <section>
        <h2 className={H2}>
          <Lightbulb className="size-3.5" />
          Tips
        </h2>
        <p className="mt-3 text-sm leading-relaxed text-muted-foreground">{help}</p>
      </section>
    </aside>
  );
}

function GoButton({ href, children }: { href: string; children: React.ReactNode }) {
  return (
    <Button asChild variant="outline" size="sm">
      <Link href={href} prefetch={false}>
        {children}
      </Link>
    </Button>
  );
}

function Row({ label, value, suffix, dot }: { label: string; value: number; suffix?: string; dot?: boolean }) {
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
