"use client";

import { useEffect, useState } from "react";
import { motion, useReducedMotion, useSpring } from "motion/react";
import { JOB_STATES } from "@/lib/job-state";
import type { JobState } from "@/lib/api";
import { cn } from "@/lib/utils";

/** Number that springs to its new value. Falls back to a plain number when the
 *  user asked for reduced motion. */
export function Counter({ value, className }: { value: number; className?: string }) {
  const still = useReducedMotion();
  const spring = useSpring(0, { stiffness: 130, damping: 22 });
  const [shown, setShown] = useState(0);

  useEffect(() => {
    spring.set(value);
  }, [value, spring]);
  useEffect(() => spring.on("change", (v) => setShown(Math.round(v))), [spring]);

  return <span className={className}>{still ? value : shown}</span>;
}

/** Job state as colour + icon + word. Never colour alone. */
export function StatusBadge({ state, className }: { state: JobState; className?: string }) {
  const meta = JOB_STATES[state];
  const Icon = meta.icon;
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 font-mono text-[0.7rem] font-semibold tracking-wide uppercase",
        className,
      )}
      style={{
        color: meta.color,
        backgroundColor: `color-mix(in oklch, ${meta.color}, transparent 88%)`,
      }}
    >
      <Icon className={cn("size-3", meta.spin && "animate-spin")} />
      {meta.label}
    </span>
  );
}

/** Live/offline dot. The ring only animates for the live case. */
export function PulseDot({ live, className }: { live: boolean; className?: string }) {
  const still = useReducedMotion();
  const color = live ? "var(--state-done)" : "var(--state-cancelled)";
  return (
    <span className={cn("relative flex size-2.5 shrink-0", className)}>
      {live && !still && (
        <motion.span
          className="absolute inset-0 rounded-full"
          style={{ backgroundColor: color }}
          animate={{ scale: [1, 2.2], opacity: [0.5, 0] }}
          transition={{ duration: 1.8, repeat: Infinity, ease: "easeOut" }}
        />
      )}
      <span className="relative size-2.5 rounded-full" style={{ backgroundColor: color }} />
    </span>
  );
}

export function EmptyState({
  icon: Icon,
  title,
  hint,
  action,
}: {
  icon: React.ComponentType<{ className?: string }>;
  title: string;
  hint?: string;
  action?: React.ReactNode;
}) {
  return (
    <div className="flex flex-col items-center gap-2 px-6 py-14 text-center">
      <Icon className="size-7 text-muted-foreground/60" />
      <p className="font-medium">{title}</p>
      {hint && <p className="max-w-sm text-sm text-muted-foreground">{hint}</p>}
      {action}
    </div>
  );
}

/** Uppercase mono field label — the printed-form look used across the forms. */
export function FieldLabel({ children, htmlFor }: { children: React.ReactNode; htmlFor?: string }) {
  return (
    <label
      htmlFor={htmlFor}
      className="font-mono text-[0.68rem] font-semibold tracking-[0.14em] text-muted-foreground uppercase"
    >
      {children}
    </label>
  );
}
