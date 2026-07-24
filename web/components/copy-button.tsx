"use client";

import { useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { Check, Copy } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

export function CopyButton({
  value,
  label = "Copy",
  className,
  variant = "outline",
  size = "sm",
}: {
  value: string;
  label?: string;
  className?: string;
  variant?: React.ComponentProps<typeof Button>["variant"];
  size?: React.ComponentProps<typeof Button>["size"];
}) {
  const [copied, setCopied] = useState(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
    } catch {
      return; // clipboard blocked (insecure origin) — the text is selectable anyway
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  }

  return (
    <Button type="button" variant={variant} size={size} onClick={copy} className={cn(className)}>
      <AnimatePresence mode="wait" initial={false}>
        <motion.span
          key={String(copied)}
          initial={{ opacity: 0, scale: 0.8 }}
          animate={{ opacity: 1, scale: 1 }}
          exit={{ opacity: 0, scale: 0.8 }}
          transition={{ duration: 0.14 }}
          className="flex items-center gap-1.5"
        >
          {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
          {size !== "icon" && size !== "icon-sm" && size !== "icon-xs" && (copied ? "Copied" : label)}
        </motion.span>
      </AnimatePresence>
    </Button>
  );
}
