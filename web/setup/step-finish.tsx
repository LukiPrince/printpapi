// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useState } from "react";
import { CircleCheck, Loader2, Rocket } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { setup, toSetupError, type Platform, type PrinterChoice } from "./api";
import { errorText, logLocation } from "./model";
import { Actions, Notice } from "./bits";
import type { Connection } from "./step-connect";

type Phase =
  | { at: "idle" }
  | { at: "installing" }
  | { at: "waiting"; baseline: number }
  | { at: "offline"; baseline: number }
  | { at: "online" }
  | { at: "failed"; message: string };

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
  onBack,
  onBackToConnect,
  onDone,
}: {
  platform: Platform;
  connection: Connection | null;
  deviceName: string;
  selected: PrinterChoice[];
  onBack: () => void;
  onBackToConnect?: () => void;
  onDone: () => void;
}) {
  const [phase, setPhase] = useState<Phase>({ at: "idle" });
  const changing = connection === null;

  async function wait(baseline: number) {
    setPhase({ at: "waiting", baseline });
    try {
      setPhase((await setup.waitOnline(baseline)) ? { at: "online" } : { at: "offline", baseline });
    } catch (e) {
      setPhase({ at: "failed", message: errorText(toSetupError(e)) });
    }
  }

  async function run() {
    setPhase({ at: "installing" });
    let baseline: number;
    try {
      baseline = await setup.apply({
        serverUrl: connection?.serverUrl ?? null,
        apiKey: connection?.apiKey ?? null,
        name: connection?.name ?? null,
        printers: selected,
      });
    } catch (e) {
      setPhase({ at: "failed", message: errorText(toSetupError(e)) });
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
          <dd className="truncate">{selected.map((p) => p.name).join(", ")}</dd>
        </dl>

        {phase.at === "installing" && (
          <Notice tone="info">
            <span className="flex items-center gap-2">
              <Loader2 className="size-3.5 animate-spin" /> Installing. Confirm the administrator prompt.
            </span>
          </Notice>
        )}
        {phase.at === "waiting" && (
          <Notice tone="info">
            <span className="flex items-center gap-2">
              <Loader2 className="size-3.5 animate-spin" /> Waiting for the agent to report in, up to 60
              seconds.
            </span>
          </Notice>
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
          {phase.at === "failed" && onBackToConnect && (
            <Button type="button" variant="outline" onClick={onBackToConnect}>
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
              {phase.at === "failed" ? "Try again" : changing ? "Apply changes" : "Install and start"}
            </Button>
          )}
        </Actions>
      </CardContent>
    </Card>
  );
}
