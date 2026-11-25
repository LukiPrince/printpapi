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
import { useBrand } from "./brand";
import { NEW_STEPS, Wizard } from "./wizard";
import { Settings } from "./settings";
import { RemoveSection } from "./settings-actions";
import { InstallChangedProvider, RunNotice, useApplyPatch } from "./settings-apply";

type Boot =
  | { at: "loading" }
  | { at: "ready"; platform: Platform; install: InstallState }
  | { at: "broken"; message: string };

type View = "start" | "removed";

/** What the window opens on: the wizard, or, for an agent that is already there, its settings. */
export function Home() {
  const [boot, setBoot] = useState<Boot>({ at: "loading" });
  const [view, setView] = useState<View>("start");
  // From the click on "Take over" until the window is closed: the install writes its marker half
  // way through, the next read says "managed", and the notice of how the run ended must stay.
  const [takingOver, setTakingOver] = useState(false);
  const { ready, error: brandError } = useBrand();

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

  // The shell's answer about the brand comes first (a stock build answers "none" at once): nothing
  // that starts from the brand is drawn before it. The first paint is the skeleton, as it always was.
  if (brandError) {
    return (
      <Shell>
        <Notice tone="error">{brandError}</Notice>
      </Shell>
    );
  }
  if (boot.at === "loading" || !ready) {
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
  if (boot.install === "other_brand") {
    // Remove asks for administrator rights and a confirmation as always, and works for any marker
    // (even one nobody can read), so a machine can be cleaned whatever is on it.
    return (
      <Shell steps={NEW_STEPS} dimmed>
        <Notice tone="warn">
          The agent on this computer was set up by another white-label build, or its marker file cannot be
          read, so this window does not change it. You can remove it here and set it up again with this
          build, or open the setup it came from.
        </Notice>
        <RemoveSection onRemoved={() => setView("removed")} />
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

  // Every change this window makes stages its brand, so the first one of any kind takes a stock
  // install over. The provider has the state read again after each, which ends the offer to do it.
  return (
    <InstallChangedProvider value={() => void load()}>
      <Settings
        platform={boot.platform}
        onRemoved={() => setView("removed")}
        top={
          boot.install === "adoptable" || takingOver ? (
            <TakeOver
              platform={boot.platform}
              adoptable={boot.install === "adoptable"}
              onStart={() => setTakingOver(true)}
            />
          ) : undefined
        }
      />
    </InstallChangedProvider>
  );
}

/**
 * A white-label build over an agent that the standard setup installed. One button runs the change
 * with nothing in it: the install rewrites the tray, the menu entry and the marker with this
 * build's name, and the settings of the agent stay as they are. The window reads the state again
 * as soon as the install has returned (see `InstallChangedProvider`) and finds an install of this
 * brand: the sentence and the button go, because the offer is over, but the notice stays mounted
 * (`onStart`) and shows the wait for the agent, the log path when it does not report in, and a
 * failure that came after the marker was written.
 */
function TakeOver({
  platform,
  adoptable,
  onStart,
}: {
  platform: Platform;
  adoptable: boolean;
  onStart: () => void;
}) {
  const { run, busy, apply, keepWaiting } = useApplyPatch(() => undefined);
  // the offer: before the click, and again after a failure that left the install as it was
  const offer = adoptable && (run.at === "idle" || run.at === "failed");
  // a notice that was left by a screen of the window (a wizard) and comes back with nothing to say
  if (!offer && run.at === "idle") return null;
  return (
    <Notice tone="warn">
      {offer && (
        <p>
          This computer&apos;s agent was set up with the standard printpapi setup. Take it over so that the
          icon, name and notices match.
        </p>
      )}
      <RunNotice run={run} platform={platform} onKeepWaiting={() => keepWaiting()} />
      {offer && (
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="mt-2"
          disabled={busy}
          onClick={() => {
            onStart();
            void apply({});
          }}
        >
          Take over
        </Button>
      )}
    </Notice>
  );
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
