"use client";

import { useMemo, useState } from "react";
import { useRouter } from "next/navigation";
import { toast } from "sonner";
import { AlertTriangle, FileText, Loader2, Printer, Zap } from "lucide-react";
import { usePoll } from "@/hooks/use-poll";
import { createJob, listPrinters, type LabelTemplate, type Printer as PrinterT } from "@/lib/api";
import { canvasToBase64 } from "@/lib/label-render";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { FieldLabel, PulseDot } from "@/components/bits";

/** Prints the template's sample (the image or the text from the editor) on a real printer. */
export function TestPrintDialog({
  open,
  onOpenChange,
  content,
  sampleText,
  image,
  needsSave,
  ensureSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  content: "image" | "text";
  sampleText: string;
  image: HTMLCanvasElement;
  /** True when the template is new or has unsaved changes. */
  needsSave: boolean;
  /** Saves the template if needed and returns it, or null when saving failed. */
  ensureSaved: () => Promise<LabelTemplate | null>;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Test print</DialogTitle>
          <DialogDescription>
            Renders the sample from the editor with this template and sends it to a printer.
          </DialogDescription>
        </DialogHeader>
        {/* Unmounts on close: the printer list is fetched fresh each time the dialog opens. */}
        <Body
          content={content}
          sampleText={sampleText}
          image={image}
          needsSave={needsSave}
          ensureSaved={ensureSaved}
          onDone={() => onOpenChange(false)}
        />
      </DialogContent>
    </Dialog>
  );
}

function Body({
  content,
  sampleText,
  image,
  needsSave,
  ensureSaved,
  onDone,
}: {
  content: "image" | "text";
  sampleText: string;
  image: HTMLCanvasElement;
  needsSave: boolean;
  ensureSaved: () => Promise<LabelTemplate | null>;
  onDone: () => void;
}) {
  const router = useRouter();
  const { data: printers } = usePoll(listPrinters, 0);
  const [printerId, setPrinterId] = useState("");
  const [busy, setBusy] = useState(false);

  const online = useMemo(() => (printers ?? []).filter((p) => p.online), [printers]);
  const printer = online.find((p) => String(p.id) === printerId);
  const byAgent = useMemo(() => {
    const map = new Map<string, PrinterT[]>();
    for (const p of online) map.set(p.agent_name, [...(map.get(p.agent_name) ?? []), p]);
    return [...map.entries()];
  }, [online]);

  async function print() {
    if (!printer) return;
    if (content === "text" && sampleText.trim() === "") {
      toast.error("Enter a sample text first.");
      return;
    }
    setBusy(true);
    try {
      const saved = await ensureSaved();
      if (!saved) return;
      const { job_id } = await createJob({
        printer_id: printer.id,
        title: `test: ${saved.name}`,
        template: saved.id,
        ...(content === "text"
          ? { type: "label_text" as const, text: sampleText }
          : { type: "label_base64" as const, content: canvasToBase64(image) }),
      });
      toast.success(`Job #${job_id} queued`, {
        description: `${printer.name} · ${saved.name}`,
        action: { label: "History", onClick: () => router.push("/history/") },
      });
      onDone();
    } catch (err) {
      toast.error("Could not queue the test print", {
        description: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <div>
        <FieldLabel>Printer</FieldLabel>
        <Select value={printerId} onValueChange={setPrinterId}>
          <SelectTrigger className="mt-1.5 w-full" disabled={online.length === 0}>
            <SelectValue placeholder={printers ? "Choose an online printer" : "Loading printers"} />
          </SelectTrigger>
          <SelectContent>
            {byAgent.map(([agent, group]) => (
              <SelectGroup key={agent}>
                <SelectLabel>{agent}</SelectLabel>
                {group.map((p) => (
                  <SelectItem key={p.id} value={String(p.id)}>
                    <PulseDot live />
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

        {printers && online.length === 0 && (
          <p className="mt-2 flex items-start gap-1.5 text-sm" style={{ color: "var(--state-queued)" }}>
            <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
            <span>No printer is online. Start the agent, then open this again.</span>
          </p>
        )}
        {printer && (
          <p className="mt-2 flex items-center gap-1.5 text-sm text-muted-foreground">
            {printer.can_pdf ? <FileText className="size-3.5" /> : <Zap className="size-3.5" />}
            {printer.can_pdf ? "Sends a PDF." : "Sends ZPL, because this printer is raw only."}
          </p>
        )}
      </div>

      <div className="-mx-4 -mb-4 flex flex-col-reverse gap-2 rounded-b-xl border-t bg-muted/50 p-4 sm:flex-row sm:justify-end">
        <DialogClose asChild>
          <Button variant="outline">Cancel</Button>
        </DialogClose>
        <Button variant="brand" disabled={!printer || busy} onClick={print}>
          {busy ? <Loader2 className="animate-spin" /> : <Printer />}
          {needsSave ? "Save and print" : "Print"}
        </Button>
      </div>
    </>
  );
}
