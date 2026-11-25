// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { createContext, useContext, useState } from "react";
import { setup, toSetupError, type ConfigPatch, type Platform } from "./api";
import { errorText, logLocation } from "./model";
import { Button } from "@/components/ui/button";
import { Notice, Path } from "./bits";

export type Run =
  | { at: "idle" }
  | { at: "applying" }
  | { at: "waiting" }
  | { at: "done" }
  | { at: "slow" }
  | { at: "failed"; message: string };

/**
 * Called when a change reached the installed agent, whatever happens after it (the wait for the
 * agent's next contact may run out). Home reads who installed the agent again: every install this
 * window starts stages its brand, so a take-over may have happened without the button for it.
 */
const InstallChanged = createContext<() => void>(() => undefined);
export const InstallChangedProvider = InstallChanged.Provider;
export const useInstallChanged = () => useContext(InstallChanged);

/**
 * A change of the installed agent: one administrator prompt, then the wait for the agent's next
 * contact. The settings that change something share it (Advanced, SumatraPDF).
 */
export function useApplyPatch(onDone: () => void) {
  const [run, setRun] = useState<Run>({ at: "idle" });
  const [baseline, setBaseline] = useState<number | null>(null);
  const installChanged = useInstallChanged();

  /** True when the agent reported in again. */
  async function wait(since: number): Promise<boolean> {
    setRun({ at: "waiting" });
    try {
      const online = await setup.waitOnline(since);
      setRun(online ? { at: "done" } : { at: "slow" });
      if (online) onDone();
      return online;
    } catch (e) {
      setRun({ at: "failed", message: errorText(toSetupError(e)) });
      return false;
    }
  }

  /**
   * `before` runs first (a download), inside the same busy state; its failure ends the run. True
   * when the change was made and the agent reported in again.
   */
  async function apply(patch: ConfigPatch, before?: () => Promise<void>): Promise<boolean> {
    setRun({ at: "applying" });
    try {
      await before?.();
      const since = await setup.applyPatch(patch);
      setBaseline(since);
      installChanged();
      return await wait(since);
    } catch (e) {
      setRun({ at: "failed", message: errorText(toSetupError(e)) });
      return false;
    }
  }

  /** Restarts the agent, then waits for its next contact like a change does. */
  async function restart() {
    setRun({ at: "applying" });
    try {
      const since = await setup.restartAgent();
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
    restart,
    keepWaiting: () => (baseline === null ? undefined : void wait(baseline)),
  };
}

/** What a run says, in words. Nothing while idle. */
export function RunNotice({
  run,
  platform,
  onKeepWaiting,
  doneText = "Applied. The agent is running with the new settings.",
  doneTone = "ok",
}: {
  run: Run;
  platform: Platform;
  onKeepWaiting: () => void;
  doneText?: string;
  doneTone?: "ok" | "warn";
}) {
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
      return <Notice tone={doneTone}>{doneText}</Notice>;
    case "slow":
      return (
        <Notice tone="error">
          <p>
            The agent did not report in within 60 seconds. Look for the reason in the log:{" "}
            <Path path={logLocation(platform)} />
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
