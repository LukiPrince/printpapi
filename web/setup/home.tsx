// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useCallback, useEffect, useState } from "react";
import { CircleCheck, X } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { setup, toSetupError, type InstallState, type Platform } from "./api";
import { errorText } from "./model";
import { Actions, Notice, Shell } from "./bits";
import { NEW_STEPS, Wizard } from "./wizard";
import { Settings } from "./settings";

type Boot =
  | { at: "loading" }
  | { at: "ready"; platform: Platform; install: InstallState }
  | { at: "broken"; message: string };

type View = "start" | "removed";

/** What the window opens on: the wizard, or, for an agent that is already there, its settings. */
export function Home() {
  const [boot, setBoot] = useState<Boot>({ at: "loading" });
  const [view, setView] = useState<View>("start");

  const load = useCallback(async () => {
    try {
      const [platform, install] = await Promise.all([setup.platform(), setup.detectInstall()]);
      setBoot({ at: "ready", platform, install });
    } catch (e) {
      setBoot({ at: "broken", message: errorText(toSetupError(e)) });
    }
  }, []);

  useEffect(() => {
    // a fetch on mount; it sets state once the answer is in
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void load();
  }, [load]);

  if (boot.at === "loading") {
    return (
      <Shell>
        <Skeleton className="h-48 w-full" />
      </Shell>
    );
  }
  if (boot.at === "broken") {
    return (
      <Shell steps={NEW_STEPS} dimmed>
        <Notice tone="error">{boot.message}</Notice>
      </Shell>
    );
  }

  if (view === "removed") {
    return (
      <Shell>
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <CircleCheck className="size-5 text-state-done" /> The agent is gone
            </CardTitle>
            <CardDescription>
              Removed. Revoke this device&apos;s key on the Keys page of your dashboard.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Actions>
              <CloseButton variant="brand" />
            </Actions>
          </CardContent>
        </Card>
      </Shell>
    );
  }

  if (boot.install === "none") {
    return (
      <Wizard
        platform={boot.platform}
        existing={null}
        onDone={() => {
          setBoot({ at: "loading" });
          void load();
        }}
      />
    );
  }

  if (boot.install === "foreign") {
    return (
      <Shell steps={NEW_STEPS} dimmed>
        <Notice tone="warn">
          This computer already runs an agent that was set up by hand, so this setup does not change or
          remove it. Remove it by hand, the way it was set up: see &quot;Run as a service&quot; in
          docs/agent.md.
        </Notice>
        <Actions>
          <CloseButton />
        </Actions>
      </Shell>
    );
  }
  if (boot.install === "unsupported") {
    return (
      <Shell steps={NEW_STEPS} dimmed>
        <Notice tone="warn">
          This Linux does not use systemd. Set up the agent by hand, see docs/agent.md.
        </Notice>
        <Actions>
          <CloseButton />
        </Actions>
      </Shell>
    );
  }

  return <Settings platform={boot.platform} onRemoved={() => setView("removed")} />;
}

/** Closes the window. An error here has nowhere to go, so it is dropped: the window is still open. */
function CloseButton({ variant = "outline" }: { variant?: "outline" | "brand" }) {
  return (
    <Button type="button" variant={variant} onClick={() => void setup.close().catch(() => {})}>
      <X />
      Close
    </Button>
  );
}
