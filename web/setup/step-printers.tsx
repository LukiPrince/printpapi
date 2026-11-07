// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useMemo, useState } from "react";
import { CircleAlert, Download, Loader2, Lock, Plus, Printer, TriangleAlert } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { EmptyState, FieldLabel } from "@/components/bits";
import { setup, toSetupError, type FoundPrinter, type Platform, type PrinterChoice } from "./api";
import {
  DEFAULT_RAW_PORT,
  addNetworkPrinter,
  canLeavePrinters,
  canSwitchOn,
  errorText,
  fromFound,
  needsSumatra,
} from "./model";
import { Actions, Notice } from "./bits";

const KINDS = {
  raw: { label: "Raw (ZPL, ESC/POS)", help: "Zebra, receipt printers. Not DYMO or Brother QL." },
  pdf: { label: "PDF through its driver", help: "Laser, inkjet, and the label printers DYMO and Brother QL." },
} as const;

type Row = { name: string; found: FoundPrinter | null; choice: PrinterChoice | null };

/** Step 2. Found printers first, then whatever was already chosen but is not found (network, by hand). */
export function StepPrinters({
  platform,
  found,
  selected,
  sumatra,
  onChange,
  onSumatra,
  onBack,
  onNext,
}: {
  platform: Platform;
  found: FoundPrinter[] | null;
  selected: PrinterChoice[];
  sumatra: boolean;
  onChange: (list: PrinterChoice[]) => void;
  onSumatra: () => void;
  onBack?: () => void;
  onNext: () => void;
}) {
  const [adding, setAdding] = useState(false);
  const [netName, setNetName] = useState("");
  const [netIp, setNetIp] = useState("");
  const [netPort, setNetPort] = useState(String(DEFAULT_RAW_PORT));
  const [netError, setNetError] = useState<string | null>(null);
  const [downloading, setDownloading] = useState(false);
  const [downloadError, setDownloadError] = useState<string | null>(null);

  const rows: Row[] = useMemo(() => {
    const list = found ?? [];
    const names = new Set(list.map((f) => f.name));
    return [
      ...list.map((f) => ({ name: f.name, found: f, choice: selected.find((s) => s.name === f.name) ?? null })),
      ...selected.filter((s) => !names.has(s.name)).map((s) => ({ name: s.name, found: null, choice: s })),
    ];
  }, [found, selected]);

  function toggle(row: Row, on: boolean) {
    if (!on) return onChange(selected.filter((s) => s.name !== row.name));
    if (row.found) onChange([...selected, ...fromFound([row.found])]);
  }

  function setKind(name: string, pdf: boolean) {
    onChange(selected.map((s) => (s.name === name ? { ...s, pdf } : s)));
  }

  function addNetwork(e: React.FormEvent) {
    e.preventDefault();
    try {
      const all = [...fromFound(found ?? []), ...selected.filter((s) => !(found ?? []).some((f) => f.name === s.name))];
      const next = addNetworkPrinter(all, netName, netIp, Number(netPort));
      onChange([...selected, next[next.length - 1]]);
      setAdding(false);
      setNetName("");
      setNetIp("");
      setNetPort(String(DEFAULT_RAW_PORT));
      setNetError(null);
    } catch (err) {
      setNetError(err instanceof Error ? err.message : String(err));
    }
  }

  async function downloadSumatra() {
    setDownloading(true);
    setDownloadError(null);
    try {
      await setup.downloadSumatra();
      onSumatra();
    } catch (e) {
      setDownloadError(errorText(toSetupError(e)));
    } finally {
      setDownloading(false);
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Which printers should printpapi use?</CardTitle>
        <CardDescription>Choose at least one. You can change this later.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {found === null ? (
          <p className="text-sm text-muted-foreground">Looking for printers…</p>
        ) : rows.length === 0 ? (
          <EmptyState
            icon={Printer}
            title="No printers found on this machine"
            hint="Add a network printer by its IP address."
          />
        ) : (
          <ul className="space-y-2">
            {rows.map((row) => (
              <PrinterRow
                key={row.name}
                row={row}
                onToggle={(on) => toggle(row, on)}
                onKind={(pdf) => setKind(row.name, pdf)}
              />
            ))}
          </ul>
        )}

        {adding ? (
          <form onSubmit={addNetwork} className="space-y-3 rounded-lg border p-3">
            <div className="grid gap-3 sm:grid-cols-[1fr_1fr_6rem]">
              <div>
                <FieldLabel htmlFor="net-name">Name</FieldLabel>
                <Input id="net-name" className="mt-1.5" value={netName} onChange={(e) => setNetName(e.target.value)} />
              </div>
              <div>
                <FieldLabel htmlFor="net-ip">IP address</FieldLabel>
                <Input
                  id="net-ip"
                  className="mt-1.5 font-mono"
                  placeholder="192.168.1.50"
                  value={netIp}
                  onChange={(e) => setNetIp(e.target.value)}
                />
              </div>
              <div>
                <FieldLabel htmlFor="net-port">Port</FieldLabel>
                <Input
                  id="net-port"
                  inputMode="numeric"
                  className="mt-1.5 font-mono"
                  value={netPort}
                  onChange={(e) => setNetPort(e.target.value)}
                />
              </div>
            </div>
            <p className="text-xs text-muted-foreground">Network printers take raw ZPL or ESC/POS, never PDF.</p>
            {netError && <Notice tone="error">{netError}</Notice>}
            <div className="flex justify-end gap-2">
              <Button type="button" variant="ghost" size="sm" onClick={() => setAdding(false)}>
                Cancel
              </Button>
              <Button type="submit" size="sm">
                Add
              </Button>
            </div>
          </form>
        ) : (
          <Button type="button" variant="outline" size="sm" onClick={() => setAdding(true)}>
            <Plus /> Add network printer
          </Button>
        )}

        {needsSumatra(platform, selected, sumatra) && (
          <Notice tone="warn">
            <p>PDF jobs fail until SumatraPDF is installed.</p>
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="mt-2"
              disabled={downloading}
              onClick={() => void downloadSumatra()}
            >
              {downloading ? <Loader2 className="animate-spin" /> : <Download />}
              Download SumatraPDF
            </Button>
            {downloadError && <p className="mt-2 text-state-failed">{downloadError}</p>}
          </Notice>
        )}

        <Actions>
          {onBack && (
            <Button type="button" variant="ghost" onClick={onBack}>
              Back
            </Button>
          )}
          <Button type="button" variant="brand" disabled={!canLeavePrinters(selected)} onClick={onNext}>
            Continue
          </Button>
        </Actions>
      </CardContent>
    </Card>
  );
}

function PrinterRow({
  row,
  onToggle,
  onKind,
}: {
  row: Row;
  onToggle: (on: boolean) => void;
  onKind: (pdf: boolean) => void;
}) {
  const { choice, found } = row;
  const locked = choice?.locked ?? false;
  const unusable = found !== null && !found.usable;
  const on = choice !== null;
  const kind = choice?.pdf ? "pdf" : "raw";
  const network = choice?.target?.startsWith("socket://") ?? false;

  return (
    <li className={`rounded-lg border px-3 py-2 ${unusable ? "opacity-60" : ""}`}>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <Switch
          size="sm"
          checked={on}
          disabled={locked || unusable || !canSwitchOn(found, on)}
          onCheckedChange={onToggle}
          aria-label={`Use ${row.name}`}
        />
        <span className="min-w-0 flex-1 truncate font-medium" title={row.name}>
          {row.name}
        </span>
        {network && <Badge variant="secondary">network</Badge>}
        {on && !locked && !network && (
          <Select value={kind} onValueChange={(v) => onKind(v === "pdf")}>
            <SelectTrigger size="sm" aria-label={`Kind of ${row.name}`} className="w-52">
              <SelectValue>{KINDS[kind].label}</SelectValue>
            </SelectTrigger>
            {/* popper: opens below its trigger, not over the next row; the width of the trigger keeps it in the window */}
            <SelectContent position="popper" align="end" className="w-(--radix-select-trigger-width)">
              {(Object.keys(KINDS) as (keyof typeof KINDS)[]).map((k) => (
                <SelectItem key={k} value={k}>
                  <span className="flex flex-col items-start">
                    <span>{KINDS[k].label}</span>
                    <span className="text-xs text-muted-foreground">{KINDS[k].help}</span>
                  </span>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}
      </div>
      {on && !locked && !network && <p className="mt-1 text-xs text-muted-foreground">{KINDS[kind].help}</p>}
      {locked && (
        <p className="mt-1 flex items-center gap-1.5 text-xs text-muted-foreground">
          <Lock className="size-3" /> Set up by hand in agent.ini. Kept as it is.
        </p>
      )}
      {unusable && (
        <p className="mt-1 flex items-start gap-1.5 text-xs text-state-failed">
          <CircleAlert className="mt-0.5 size-3 shrink-0" />
          This name contains &quot;;&quot;, &quot;=&quot; or &quot;|&quot;, which agent.ini cannot hold.
        </p>
      )}
      {found?.per_user && (
        <p className="mt-1 flex items-start gap-1.5 text-xs text-state-queued">
          <TriangleAlert className="mt-0.5 size-3 shrink-0" />
          Connected for this user only. The agent cannot see it.
        </p>
      )}
    </li>
  );
}
