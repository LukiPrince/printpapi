// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useState } from "react";
import { CircleCheck, Loader2, Rocket } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { setup, toSetupError, type Platform, type PrinterChoice } from "./api";
import { connectMayHelp, errorText, logLocation, patchFor, returnsToConnect } from "./model";
import { Actions, Notice } from "./bits";
import type { Connection } from "./step-connect";

type Phase =
  | { at: "idle" }
  | { at: "installing" }
  | { at: "waiting"; baseline: number }
  | { at: "offline"; baseline: number }
  | { at: "online" }
  | { at: "failed"; message: string; code: string };

/** What the main button says: the action before and after a run, what is going on during one. */
function buttonLabel(at: Phase["at"], changing: boolean): string {
  if (at === "installing") return changing ? "Applying…" : "Installing…";
  if (at === "waiting") return "Waiting for the agent…";
  if (at === "failed") return "Try again";
  return changing ? "Apply changes" : "Install and start";
}

/**
 * Step 4. Asks for admin rights, installs, then only reports success once the server has seen the
 * agent after the install: the install's own registration would make the device look online.
 * The key goes to `apply` and nowhere else; it is never shown.
 */
export function StepFinish({
  platform,
  connection,
  deviceName,
  selected,
  changeFrom,
  onBack,
  onBackToConnect,
  onOnline,
  onDone,
}: {
  platform: Platform;
  connection: Connection | null;
  deviceName: string;
  selected: PrinterChoice[];
  /** The printers of the installed agent, when only they are being changed. */
  changeFrom: PrinterChoice[];
  onBack: () => void;
  /** Back to step 1, with the reason when a refused key or name sent it there. */
  onBackToConnect?: (message?: string) => void;
  /** The agent reported in: the whole run is finished, whatever the person does next. */
  onOnline: () => void;
  onDone: () => void;
}) {
  const [phase, setPhase] = useState<Phase>({ at: "idle" });
  const changing = connection === null;

  async function wait(baseline: number) {
    setPhase({ at: "waiting", baseline });
    try {
      const online = await setup.waitOnline(baseline);
      setPhase(online ? { at: "online" } : { at: "offline", baseline });
      if (online) onOnline();
    } catch (e) {
      const err = toSetupError(e);
      setPhase({ at: "failed", message: errorText(err), code: err.code });
    }
  }

  async function run() {
    setPhase({ at: "installing" });
    let baseline: number;
    try {
      // a change of the installed agent names only what differs; a new connection is a new install
      baseline = connection
        ? await setup.apply({
            serverUrl: connection.serverUrl,
            apiKey: connection.apiKey,
            name: connection.name,
            printers: selected,
          })
        : await setup.applyPatch(patchFor({ printers: changeFrom }, { printers: selected }));
    } catch (e) {
      const err = toSetupError(e);
      if (onBackToConnect && returnsToConnect(err.code)) {
        onBackToConnect(errorText(err));
        return;
      }
      setPhase({ at: "failed", message: errorText(err), code: err.code });
      return;
    }
    await wait(baseline);
  }

  const busy = phase.at === "installing" || phase.at === "waiting";

  if (phase.at === "online") {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <CircleCheck className="size-5 text-state-done" /> The agent is running
          </CardTitle>
          <CardDescription>
            {deviceName} is connected and starts with this computer. Its printers are listed in your
            dashboard.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Actions>
            <Button type="button" variant="brand" onClick={onDone}>
              Done
            </Button>
          </Actions>
        </CardContent>
      </Card>
    );
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{changing ? "Apply your changes" : "Install the agent"}</CardTitle>
        <CardDescription>
          You will be asked for administrator rights once. The agent then runs in the background and
          starts with this computer.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1 text-sm">
          {connection && (
            <>
              <dt className="text-muted-foreground">Server</dt>
              <dd className="truncate font-mono">{connection.serverUrl}</dd>
            </>
          )}
          <dt className="text-muted-foreground">Device</dt>
          <dd className="truncate">{deviceName}</dd>
          <dt className="text-muted-foreground">Printers</dt>
          <dd className="break-words">{selected.map((p) => p.name).join(", ")}</dd>
        </dl>

        {phase.at === "installing" && (
          <p role="status" className="text-sm text-muted-foreground">
            Confirm the administrator prompt.
          </p>
        )}
        {phase.at === "waiting" && (
          <p role="status" className="text-sm text-muted-foreground">
            The agent has to report in to the server. This takes up to 60 seconds.
          </p>
        )}
        {phase.at === "offline" && (
          <Notice tone="error">
            The agent did not report in within 60 seconds. Look for the reason in the log:{" "}
            <span className="break-all font-mono text-xs">{logLocation(platform)}</span>
          </Notice>
        )}
        {phase.at === "failed" && <Notice tone="error">{phase.message}</Notice>}

        <Actions>
          {!busy && (
            <Button type="button" variant="ghost" onClick={onBack}>
              Back
            </Button>
          )}
          {phase.at === "failed" && onBackToConnect && connectMayHelp(phase.code) && (
            <Button type="button" variant="outline" onClick={() => onBackToConnect()}>
              Change server or key
            </Button>
          )}
          {phase.at === "offline" ? (
            <Button type="button" variant="brand" onClick={() => void wait(phase.baseline)}>
              Keep waiting
            </Button>
          ) : (
            <Button type="button" variant="brand" disabled={busy} onClick={() => void run()}>
              {busy ? <Loader2 className="animate-spin" /> : <Rocket />}
              {buttonLabel(phase.at, changing)}
            </Button>
          )}
        </Actions>
      </CardContent>
    </Card>
  );
}
