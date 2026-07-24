"use client";

import { useMemo, useState } from "react";
import Link from "next/link";
import { AnimatePresence, motion } from "motion/react";
import { toast } from "sonner";
import { AlertTriangle, FileText, Link2, Loader2, Minus, Plus, Printer, Zap } from "lucide-react";
import { usePoll } from "@/hooks/use-poll";
import { createJob, fileToBase64, listPrinters, type NewJob, type Printer as PrinterT } from "@/lib/api";
import { TEST_PDF_B64, TEST_ZPL_B64 } from "@/lib/testdoc";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Dropzone } from "@/components/dropzone";
import { EmptyState, FieldLabel, PulseDot } from "@/components/bits";

type Source = "pdf" | "raw" | "url" | "test";

export default function PrintPage() {
  const { data: printers } = usePoll(listPrinters, 15000);
  const [source, setSource] = useState<Source>("pdf");
  const [printerId, setPrinterId] = useState<string>("");
  const [file, setFile] = useState<File | null>(null);
  const [url, setUrl] = useState("");
  const [urlIsPdf, setUrlIsPdf] = useState(false);
  const [title, setTitle] = useState("");
  const [copies, setCopies] = useState(1);
  const [callback, setCallback] = useState("");
  const [busy, setBusy] = useState(false);

  const list = useMemo(() => printers ?? [], [printers]);
  const printer = useMemo(() => list.find((p) => String(p.id) === printerId), [list, printerId]);

  // gotcha #1: a raw-only (label) printer cannot render a PDF — it prints blanks.
  const wantsPdf = source === "pdf" || (source === "url" && urlIsPdf);
  const pdfMismatch = !!printer && wantsPdf && !printer.can_pdf;

  const byAgent = useMemo(() => {
    const map = new Map<string, PrinterT[]>();
    for (const p of list) map.set(p.agent_name, [...(map.get(p.agent_name) ?? []), p]);
    return [...map.entries()];
  }, [list]);

  async function buildJob(): Promise<NewJob> {
    const id = Number(printerId);
    const base = { printer_id: id, title: title.trim() || null, copies };
    const hook = callback.trim();
    const withHook = hook ? { ...base, callback_url: hook } : base;

    if (source === "pdf" || source === "raw") {
      if (!file) throw new Error("Choose a file first.");
      return {
        ...withHook,
        type: source === "pdf" ? "pdf_base64" : "raw_base64",
        content: await fileToBase64(file),
      };
    }
    if (source === "url") {
      if (!url.trim()) throw new Error("Enter a URL.");
      return { ...withHook, type: urlIsPdf ? "pdf_uri" : "raw_uri", url: url.trim() };
    }
    return printer?.can_pdf
      ? { ...withHook, type: "pdf_base64", content: TEST_PDF_B64, title: base.title || "test page" }
      : { ...withHook, type: "raw_base64", content: TEST_ZPL_B64, title: base.title || "test label" };
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!printer) return toast.error("Select a printer.");
    if (pdfMismatch) return toast.error("That printer is raw-only — send it ZPL/ESC-POS, not a PDF.");
    setBusy(true);
    try {
      const { job_id } = await createJob(await buildJob());
      toast.success(`Job #${job_id} queued`, {
        description: `${printer.name} · ${copies} ${copies === 1 ? "copy" : "copies"}`,
        action: { label: "History", onClick: () => location.assign("/history/") },
      });
      setFile(null);
      setTitle("");
    } catch (err) {
      toast.error("Could not queue the job", {
        description: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setBusy(false);
    }
  }

  if (printers && list.length === 0) {
    return (
      <Card>
        <EmptyState
          icon={Printer}
          title="No printers registered"
          hint="Install the agent on the machine with the printers — it registers them automatically on first poll."
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
    <form onSubmit={submit} className="max-w-2xl space-y-4">
      <Card>
        <CardHeader>
          <CardTitle>Source</CardTitle>
          <CardDescription>What should go on the paper?</CardDescription>
        </CardHeader>
        <CardContent>
          <Tabs value={source} onValueChange={(v) => setSource(v as Source)}>
            <TabsList className="w-full">
              <TabsTrigger value="pdf">
                <FileText /> PDF
              </TabsTrigger>
              <TabsTrigger value="raw">
                <Zap /> Raw / ZPL
              </TabsTrigger>
              <TabsTrigger value="url">
                <Link2 /> URL
              </TabsTrigger>
              <TabsTrigger value="test">
                <Printer /> Test
              </TabsTrigger>
            </TabsList>

            <TabsContent value="pdf" className="mt-4">
              <Dropzone
                file={file}
                onFile={setFile}
                accept="application/pdf,.pdf"
                hint="A PDF for a document printer — the agent renders it via the driver."
              />
            </TabsContent>
            <TabsContent value="raw" className="mt-4">
              <Dropzone
                file={file}
                onFile={setFile}
                hint="Already-rendered bytes (ZPL, ESC-POS) — sent straight to the printer."
              />
            </TabsContent>
            <TabsContent value="url" className="mt-4 space-y-3">
              <div>
                <FieldLabel htmlFor="joburl">URL</FieldLabel>
                <Input
                  id="joburl"
                  className="mt-1.5 font-mono"
                  placeholder="https://example.com/label.zpl"
                  value={url}
                  onChange={(e) => setUrl(e.target.value)}
                />
              </div>
              <label className="flex items-center gap-2.5 text-sm">
                <Switch checked={urlIsPdf} onCheckedChange={setUrlIsPdf} />
                This URL returns a PDF (render it)
              </label>
              <p className="text-xs text-muted-foreground">
                The server fetches the URL with a browser User-Agent, so a WAF in front of it will
                not 403 the request.
              </p>
            </TabsContent>
            <TabsContent value="test" className="mt-4">
              <p className="text-sm text-muted-foreground">
                Sends the built-in sample: a one-page PDF for document printers, a ZPL label for raw
                ones. Picked automatically from the printer&apos;s capabilities.
              </p>
            </TabsContent>
          </Tabs>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Destination</CardTitle>
          <CardDescription>Where it comes out.</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div>
            <FieldLabel>Printer</FieldLabel>
            <Select value={printerId} onValueChange={setPrinterId}>
              <SelectTrigger className="mt-1.5 w-full">
                <SelectValue placeholder="Choose a printer" />
              </SelectTrigger>
              <SelectContent>
                {byAgent.map(([agent, group]) => (
                  <SelectGroup key={agent}>
                    <SelectLabel>{agent}</SelectLabel>
                    {group.map((p) => (
                      <SelectItem key={p.id} value={String(p.id)}>
                        <PulseDot live={p.online} />
                        {p.name}
                        <span className="ml-1 text-xs text-muted-foreground">
                          {p.can_pdf ? "PDF + raw" : "raw only"}
                        </span>
                      </SelectItem>
                    ))}
                  </SelectGroup>
                ))}
              </SelectContent>
            </Select>

            <AnimatePresence>
              {printer && !printer.online && (
                <Note key="offline" tone="warn">
                  This printer&apos;s agent has not polled in the last minute. The job will queue and
                  print as soon as it reconnects.
                </Note>
              )}
              {pdfMismatch && (
                <Note key="mismatch" tone="bad">
                  {printer?.name} is raw-only. Sending it a PDF prints blank labels — upload ZPL or
                  ESC-POS instead.
                </Note>
              )}
            </AnimatePresence>
          </div>

          <div className="grid gap-4 sm:grid-cols-[minmax(0,1fr)_auto]">
            <div>
              <FieldLabel htmlFor="jobtitle">Title (optional)</FieldLabel>
              <Input
                id="jobtitle"
                className="mt-1.5"
                placeholder="e.g. Shipping label #4712"
                value={title}
                onChange={(e) => setTitle(e.target.value)}
              />
            </div>
            <div>
              <FieldLabel htmlFor="copies">Copies</FieldLabel>
              <div className="mt-1.5 flex items-center gap-1">
                <Button
                  type="button"
                  variant="outline"
                  size="icon"
                  aria-label="One fewer copy"
                  onClick={() => setCopies((c) => Math.max(1, c - 1))}
                >
                  <Minus />
                </Button>
                <Input
                  id="copies"
                  type="number"
                  min={1}
                  max={100}
                  value={copies}
                  onChange={(e) =>
                    setCopies(Math.min(100, Math.max(1, Math.trunc(Number(e.target.value)) || 1)))
                  }
                  className="w-16 text-center font-mono tabular-nums"
                />
                <Button
                  type="button"
                  variant="outline"
                  size="icon"
                  aria-label="One more copy"
                  onClick={() => setCopies((c) => Math.min(100, c + 1))}
                >
                  <Plus />
                </Button>
              </div>
            </div>
          </div>

          <div>
            <FieldLabel htmlFor="callback">Callback URL (optional)</FieldLabel>
            <Input
              id="callback"
              className="mt-1.5 font-mono"
              placeholder="https://your-app.example/print-hook"
              value={callback}
              onChange={(e) => setCallback(e.target.value)}
            />
            <p className="mt-1.5 text-xs text-muted-foreground">
              The server POSTs the outcome here once the job reaches done, failed or cancelled.
            </p>
          </div>
        </CardContent>
      </Card>

      <div className="flex items-center gap-3">
        <Button type="submit" variant="brand" size="lg" className="font-mono" disabled={busy}>
          {busy ? <Loader2 className="animate-spin" /> : <Printer />}
          {busy ? "QUEUEING" : "PRINT"}
        </Button>
        <span className="text-sm text-muted-foreground">
          {printer ? `→ ${printer.name}` : "Pick a printer to enable printing."}
        </span>
      </div>
    </form>
  );
}

function Note({ tone, children }: { tone: "warn" | "bad"; children: React.ReactNode }) {
  const color = tone === "bad" ? "var(--state-failed)" : "var(--state-queued)";
  return (
    <motion.p
      initial={{ opacity: 0, height: 0 }}
      animate={{ opacity: 1, height: "auto" }}
      exit={{ opacity: 0, height: 0 }}
      className="mt-2 flex items-start gap-1.5 overflow-hidden text-sm"
      style={{ color }}
    >
      <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
      <span>{children}</span>
    </motion.p>
  );
}
