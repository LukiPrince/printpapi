"use client";

import { motion, useReducedMotion } from "motion/react";
import { cn } from "@/lib/utils";

/** printpapi mark: ink block, amber spool, a label feeding out. */
export function Logo({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 32 32" className={cn("shrink-0", className)} aria-hidden="true">
      <rect width="32" height="32" rx="8" fill="#141b22" />
      <rect x="9.5" y="4" width="13" height="9" rx="1.5" fill="#e8e6df" />
      <rect x="5.5" y="11.5" width="21" height="8.5" rx="2.5" fill="#f59e0b" />
      <circle cx="22.5" cy="15.75" r="1.15" fill="#231503" />
      <rect x="9.5" y="18.5" width="13" height="9.5" rx="1.5" fill="#fbfaf7" />
      <g fill="#141b22">
        <rect x="11.5" y="21" width="1" height="5" />
        <rect x="13.5" y="21" width="2" height="5" />
        <rect x="16.5" y="21" width="1" height="5" />
        <rect x="18.5" y="21" width="2.5" height="5" />
      </g>
    </svg>
  );
}

/** The barcode strip. Animates its width in on mount, like a strip being fed out. */
export function Barcode({ className }: { className?: string }) {
  const still = useReducedMotion();
  return (
    <div className={cn("overflow-hidden", className)} aria-hidden="true">
      <motion.div
        className="barcode h-full w-full"
        initial={still ? false : { scaleX: 0, opacity: 0 }}
        animate={{ scaleX: 1, opacity: 0.85 }}
        transition={{ duration: 0.7, ease: [0.2, 0.8, 0.3, 1] }}
        style={{ originX: 0 }}
      />
    </div>
  );
}
