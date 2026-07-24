"use client";

import { useRef, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { FileText, Upload, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { fmtBytes } from "@/lib/format";
import { cn } from "@/lib/utils";

export function Dropzone({
  file,
  onFile,
  accept,
  hint,
}: {
  file: File | null;
  onFile: (f: File | null) => void;
  accept?: string;
  hint: string;
}) {
  const input = useRef<HTMLInputElement>(null);
  const [hot, setHot] = useState(false);

  return (
    <div>
      <input
        ref={input}
        type="file"
        accept={accept}
        className="sr-only"
        onChange={(e) => {
          onFile(e.target.files?.[0] ?? null);
          // Clear it right away: the browser suppresses `change` when the same file is picked
          // twice in a row, so without this you cannot re-print the file you just printed.
          e.target.value = "";
        }}
      />
      <button
        type="button"
        onClick={() => input.current?.click()}
        onDragOver={(e) => {
          e.preventDefault();
          setHot(true);
        }}
        onDragLeave={() => setHot(false)}
        onDrop={(e) => {
          e.preventDefault();
          setHot(false);
          onFile(e.dataTransfer.files?.[0] ?? null);
        }}
        className={cn(
          "flex w-full flex-col items-center gap-1.5 rounded-lg border-2 border-dashed px-6 py-8 text-center transition-colors",
          hot ? "border-brand bg-brand/8 text-foreground" : "border-border text-muted-foreground hover:border-brand/60",
        )}
      >
        <motion.span animate={hot ? { y: -3 } : { y: 0 }}>
          <Upload className="size-5" />
        </motion.span>
        <span className="font-mono text-sm">Drag &amp; drop, or click to choose</span>
        <span className="text-xs">{hint}</span>
      </button>

      <AnimatePresence>
        {file && (
          <motion.div
            initial={{ opacity: 0, height: 0 }}
            animate={{ opacity: 1, height: "auto" }}
            exit={{ opacity: 0, height: 0 }}
            className="overflow-hidden"
          >
            <div className="mt-2 flex items-center gap-2 rounded-md border border-border bg-muted/60 px-3 py-2">
              <FileText className="size-4 shrink-0 text-brand" />
              <span className="min-w-0 flex-1 truncate font-mono text-sm">{file.name}</span>
              <span className="shrink-0 text-xs text-muted-foreground">{fmtBytes(file.size)}</span>
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                aria-label="Remove file"
                onClick={() => {
                  onFile(null);
                  if (input.current) input.current.value = "";
                }}
              >
                <X />
              </Button>
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}
