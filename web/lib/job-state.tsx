import { CheckCircle2, CircleDashed, CircleSlash, Loader2, XCircle } from "lucide-react";
import type { JobState } from "@/lib/api";

/**
 * Job states are the dataviz *status* slots: colour is never the only channel —
 * every rendering pairs the swatch with this icon and the label.
 */
export const JOB_STATES: Record<
  JobState,
  { label: string; color: string; icon: typeof CheckCircle2; spin?: boolean }
> = {
  queued: { label: "Queued", color: "var(--state-queued)", icon: CircleDashed },
  claimed: { label: "Printing", color: "var(--state-claimed)", icon: Loader2, spin: true },
  done: { label: "Done", color: "var(--state-done)", icon: CheckCircle2 },
  failed: { label: "Failed", color: "var(--state-failed)", icon: XCircle },
  cancelled: { label: "Cancelled", color: "var(--state-cancelled)", icon: CircleSlash },
};

export const STATE_ORDER: JobState[] = ["queued", "claimed", "done", "failed", "cancelled"];
