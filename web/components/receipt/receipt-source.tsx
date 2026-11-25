"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence } from "motion/react";
import { ApiError, getReceiptProfile, previewReceipt, type Printer, type ReceiptProfile } from "@/lib/api";
import { createPreviewer, describeProfile, parseReceiptText, previewPlaceholder, type Previewer } from "@/lib/receipt";
import { Textarea } from "@/components/ui/textarea";
import { FieldLabel, Note } from "@/components/bits";

type Body = { receipt: unknown; printer_id?: number };

/** The Receipt tab of the Print page: the receipt as JSON, and what it will look like. Syntax
 *  errors are found here; everything else is the server's text. */
export function ReceiptSource({
  printer,
  text,
  onText,
}: {
  printer: Printer | undefined;
  text: string;
  onText: (text: string) => void;
}) {
  const parsed = useMemo(() => parseReceiptText(text), [text]);
  const [image, setImage] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [profile, setProfile] = useState<{ id: number; profile: ReceiptProfile } | null>(null);
  const previewer = useRef<Previewer<Body> | null>(null);

  const receipt = "receipt" in parsed ? parsed.receipt : null;
  const syntaxError = "error" in parsed ? parsed.error : null;
  const printerId = printer?.id;

  // the printer's profile, for the one line under the preview; a printer that cannot print
  // receipts (CloudPRNT) answers with its reason, which is shown instead
  useEffect(() => {
    if (printerId === undefined) return;
    let alive = true;
    getReceiptProfile(printerId).then(
      (r) => {
        if (!alive) return;
        setProfile({ id: printerId, profile: r.profile });
        setProblem(null);
      },
      (e: unknown) => {
        if (alive) setProblem(e instanceof Error ? e.message : String(e));
      },
    );
    return () => {
      alive = false;
    };
  }, [printerId]);

  // one request in flight, the newest text waits, a busy server is retried quietly
  useEffect(() => {
    const p = createPreviewer<Body>({
      send: previewReceipt,
      onImage: (url) => {
        setImage(url);
        setProblem(null);
      },
      onError: setProblem,
      isBusy: (e) => e instanceof ApiError && e.status === 503,
    });
    previewer.current = p;
    return () => {
      p.dispose();
      previewer.current = null;
    };
  }, []);

  // 400 ms after the last keystroke (or another printer), ask for a new picture
  useEffect(() => {
    if (receipt === null) return;
    const timer = setTimeout(() => {
      previewer.current?.request(printerId === undefined ? { receipt } : { receipt, printer_id: printerId });
    }, 400);
    return () => clearTimeout(timer);
  }, [receipt, printerId]);

  const shownProfile = printerId !== undefined && profile?.id === printerId ? profile.profile : null;
  const error = syntaxError ?? problem;
  const line = describeProfile(shownProfile, printer?.can_pdf ?? false, printerId !== undefined);

  return (
    <div className="space-y-3">
      <div>
        <FieldLabel htmlFor="receipttext">Receipt (JSON)</FieldLabel>
        <Textarea
          id="receipttext"
          className="mt-1.5 max-h-96 min-h-48 font-mono text-xs"
          spellCheck={false}
          value={text}
          onChange={(e) => onText(e.target.value)}
          aria-invalid={syntaxError !== null}
        />
      </div>
      <AnimatePresence>
        {error && (
          <Note key="problem" tone="bad">
            {error}
          </Note>
        )}
      </AnimatePresence>
      <div className="max-h-96 overflow-auto rounded-lg bg-muted/40 p-3">
        {image ? (
          // a data: URL of the server's PNG: the optimiser has nothing to do
          // eslint-disable-next-line @next/next/no-img-element
          <img src={image} alt="Preview of the receipt" className="mx-auto h-auto max-w-full" />
        ) : (
          <p className="py-8 text-center text-sm text-muted-foreground">
            {previewPlaceholder(receipt !== null, error)}
          </p>
        )}
      </div>
      {line && <p className="text-xs text-muted-foreground">{line}</p>}
    </div>
  );
}
