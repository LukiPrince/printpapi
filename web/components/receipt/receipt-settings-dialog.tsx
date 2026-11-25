"use client";

import { useEffect, useState } from "react";
import { AnimatePresence } from "motion/react";
import { toast } from "sonner";
import { Loader2 } from "lucide-react";
import { createJob, getReceiptProfile, putReceiptProfile, type Printer, type ReceiptProfile } from "@/lib/api";
import sample from "@/lib/receipt-sample.json";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { FieldLabel, Note } from "@/components/bits";

/** The usual print width of each paper, in dots: what a paper change starts from. */
const USUAL_WIDTH = { 58: 384, 80: 576 } as const;

/** Paper, print width, text mode and cutter of one receipt printer, and a button that prints the
 *  sample receipt with them. The content unmounts on close, so every open starts from the server. */
export function ReceiptSettingsDialog({
  printer,
  open,
  onOpenChange,
}: {
  printer: Printer;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Receipt settings</DialogTitle>
          <DialogDescription>
            How receipts are laid out for {printer.name}. A change applies to the next receipt. Only for a
            receipt printer (ESC/POS) or a printer that takes PDF: the test receipt is no label, and a label
            printer needs ZPL.
          </DialogDescription>
        </DialogHeader>
        <Settings printer={printer} onDone={() => onOpenChange(false)} />
      </DialogContent>
    </Dialog>
  );
}

function Settings({ printer, onDone }: { printer: Printer; onDone: () => void }) {
  // null until loaded; the loaded profile is spread into every save, so a field this form does
  // not know (codepage_number) is kept
  const [profile, setProfile] = useState<ReceiptProfile | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState<"save" | "test" | null>(null);

  useEffect(() => {
    let alive = true;
    getReceiptProfile(printer.id).then(
      (r) => alive && setProfile(r.profile),
      (e: unknown) => alive && setProblem(e instanceof Error ? e.message : String(e)),
    );
    return () => {
      alive = false;
    };
  }, [printer.id]);

  const set = (patch: Partial<ReceiptProfile>) => setProfile((p) => (p ? { ...p, ...patch } : p));
  // the most the server takes: 8 dots a millimetre of paper (heads of 416 or 640 dots exist)
  const max = (profile?.paper_mm ?? 80) * 8;

  async function save(thenTest: boolean) {
    if (!profile) return;
    setProblem(null);
    setBusy(thenTest ? "test" : "save");
    try {
      await putReceiptProfile(printer.id, profile);
      if (thenTest) {
        const { job_id } = await createJob({
          printer_id: printer.id,
          type: "receipt",
          receipt: sample,
          title: "Test receipt",
        });
        toast.success(`Test receipt #${job_id} queued`, { description: printer.name });
      } else {
        toast.success("Receipt settings saved", { description: printer.name });
        onDone();
      }
    } catch (e) {
      setProblem(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <>
      <div className="space-y-4">
        <div className="grid grid-cols-2 gap-3">
          <div>
            <FieldLabel>Paper</FieldLabel>
            <Select
              value={profile ? String(profile.paper_mm) : ""}
              onValueChange={(v) => {
                const paper = Number(v) as 58 | 80;
                // another paper, another width: start from its usual print width
                set({ paper_mm: paper, width_dots: USUAL_WIDTH[paper] });
              }}
              disabled={!profile}
            >
              <SelectTrigger className="mt-1.5 w-full" aria-label="Paper width">
                <SelectValue placeholder="…" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="58">58 mm</SelectItem>
                <SelectItem value="80">80 mm</SelectItem>
              </SelectContent>
            </Select>
          </div>
          <div>
            <FieldLabel htmlFor="rcwidth">Print width (dots)</FieldLabel>
            <Input
              id="rcwidth"
              type="number"
              step={8}
              min={192}
              max={max}
              className="mt-1.5 font-mono tabular-nums"
              disabled={!profile}
              value={profile?.width_dots ?? ""}
              onChange={(e) => set({ width_dots: Math.trunc(Number(e.target.value)) || 0 })}
            />
          </div>
        </div>
        <p className="-mt-2 text-xs text-muted-foreground">
          A multiple of 8, 192 to {max}. {profile ? `That is ${Math.floor(profile.width_dots / 12)} characters a line.` : ""}
        </p>

        <div>
          <FieldLabel>Text</FieldLabel>
          <Select
            value={profile?.codepage ?? ""}
            onValueChange={(v) => set({ codepage: v as ReceiptProfile["codepage"] })}
            disabled={!profile}
          >
            <SelectTrigger className="mt-1.5 w-full" aria-label="Text mode">
              <SelectValue placeholder="…" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="raster">Drawn by printpapi</SelectItem>
              <SelectItem value="cp1252">Printer&apos;s own font (Windows-1252)</SelectItem>
            </SelectContent>
          </Select>
          <p className="mt-1.5 text-xs text-muted-foreground">
            Umlauts wrong or prices on the next line? Choose drawn text.
          </p>
        </div>

        <div>
          <FieldLabel>Cutter</FieldLabel>
          <Select
            value={profile?.cut ?? ""}
            onValueChange={(v) => set({ cut: v as ReceiptProfile["cut"] })}
            disabled={!profile}
          >
            <SelectTrigger className="mt-1.5 w-full" aria-label="Cutter">
              <SelectValue placeholder="…" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="partial">Partial cut</SelectItem>
              <SelectItem value="full">Full cut</SelectItem>
              <SelectItem value="none">No cutter (feed instead)</SelectItem>
            </SelectContent>
          </Select>
        </div>

        {printer.can_pdf && (
          <label className="flex items-center gap-2.5 text-sm">
            <Switch
              checked={profile?.output === "escpos"}
              onCheckedChange={(on) => set({ output: on ? "escpos" : "auto" })}
              disabled={!profile}
            />
            Send ESC/POS instead of a PDF
          </label>
        )}

        <AnimatePresence>
          {problem && (
            <Note key="problem" tone="bad">
              {problem}
            </Note>
          )}
        </AnimatePresence>
      </div>
      <DialogFooter className="mt-2">
        <Button variant="outline" disabled={!profile || busy !== null} onClick={() => save(true)}>
          {busy === "test" && <Loader2 className="animate-spin" />}
          Test receipt
        </Button>
        <Button variant="brand" disabled={!profile || busy !== null} onClick={() => save(false)}>
          {busy === "save" && <Loader2 className="animate-spin" />}
          Save
        </Button>
      </DialogFooter>
    </>
  );
}
