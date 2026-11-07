// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useState } from "react";
import { setup, toSetupError, type ConfigPatch, type Platform } from "./api";
import { errorText, logLocation } from "./model";
import { Button } from "@/components/ui/button";
import { Notice } from "./bits";

export type Run =
  | { at: "idle" }
  | { at: "applying" }
  | { at: "waiting" }
  | { at: "done" }
  | { at: "slow" }
  | { at: "failed"; message: string };

/**
 * A change of the installed agent: one administrator prompt, then the wait for the agent's next
 * contact. The settings that change something share it (Advanced, SumatraPDF).
 */
export function useApplyPatch(onDone: () => void) {
  const [run, setRun] = useState<Run>({ at: "idle" });
  const [baseline, setBaseline] = useState<number | null>(null);

  async function wait(since: number) {
    setRun({ at: "waiting" });
    try {
      const online = await setup.waitOnline(since);
      setRun(online ? { at: "done" } : { at: "slow" });
      if (online) onDone();
    } catch (e) {
      setRun({ at: "failed", message: errorText(toSetupError(e)) });
    }
  }

  /** `before` runs first (a download), inside the same busy state; its failure ends the run. */
  async function apply(patch: ConfigPatch, before?: () => Promise<void>) {
    setRun({ at: "applying" });
    try {
      await before?.();
      const since = await setup.applyPatch(patch);
      setBaseline(since);
      await wait(since);
    } catch (e) {
      setRun({ at: "failed", message: errorText(toSetupError(e)) });
    }
  }

  return {
    run,
    busy: run.at === "applying" || run.at === "waiting",
    apply,
    keepWaiting: () => (baseline === null ? undefined : void wait(baseline)),
  };
}

/** What a run says, in words. Nothing while idle. */
export function RunNotice({ run, platform, onKeepWaiting }: { run: Run; platform: Platform; onKeepWaiting: () => void }) {
  switch (run.at) {
    case "idle":
      return null;
    case "applying":
      return (
        <p role="status" className="text-sm text-muted-foreground">
          Confirm the administrator prompt.
        </p>
      );
    case "waiting":
      return (
        <p role="status" className="text-sm text-muted-foreground">
          The agent has to report in to the server. This takes up to 60 seconds.
        </p>
      );
    case "done":
      return <Notice tone="ok">Applied. The agent is running with the new settings.</Notice>;
    case "slow":
      return (
        <Notice tone="error">
          <p>
            The agent did not report in within 60 seconds. Look for the reason in the log:{" "}
            <span className="break-all font-mono text-xs">{logLocation(platform)}</span>
          </p>
          <Button type="button" variant="outline" size="sm" className="mt-2" onClick={onKeepWaiting}>
            Keep waiting
          </Button>
        </Notice>
      );
    case "failed":
      return <Notice tone="error">{run.message}</Notice>;
  }
}
