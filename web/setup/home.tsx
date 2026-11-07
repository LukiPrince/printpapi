// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useCallback, useEffect, useState } from "react";
import { CircleCheck, Loader2, Pencil, Trash2, X } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { setup, toSetupError, type Existing, type InstallState, type Platform } from "./api";
import { errorText } from "./model";
import { Actions, Notice, Shell } from "./bits";
import { Wizard } from "./wizard";

type Boot =
  | { at: "loading" }
  | { at: "ready"; platform: Platform; install: InstallState }
  | { at: "broken"; message: string };

type View = "start" | "change" | "removed";

/** What the window opens on: the wizard, or, for an agent that is already there, what can be done. */
export function Home() {
  const [boot, setBoot] = useState<Boot>({ at: "loading" });
  const [view, setView] = useState<View>("start");
  const [existing, setExisting] = useState<Existing | null>(null);

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
      <Shell>
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

  if (view === "change" && existing) {
    return (
      <Wizard
        platform={boot.platform}
        existing={existing}
        onDone={() => {
          setView("start");
          setExisting(null);
          void load();
        }}
      />
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
      <Shell>
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
      <Shell>
        <Notice tone="warn">
          This Linux does not use systemd. Set up the agent by hand, see docs/agent.md.
        </Notice>
        <Actions>
          <CloseButton />
        </Actions>
      </Shell>
    );
  }

  return (
    <Shell>
      <Installed
        onChange={(e) => {
          setExisting(e);
          setView("change");
        }}
        onRemoved={() => setView("removed")}
      />
    </Shell>
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

/** The start screen of a machine that already has the agent from this setup. */
function Installed({ onChange, onRemoved }: { onChange: (e: Existing) => void; onRemoved: () => void }) {
  const [busy, setBusy] = useState<"read" | "remove" | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function change() {
    setBusy("read");
    setError(null);
    try {
      onChange(await setup.readExisting());
    } catch (e) {
      setError(errorText(toSetupError(e)));
    } finally {
      setBusy(null);
    }
  }

  async function remove() {
    setBusy("remove");
    setError(null);
    try {
      await setup.uninstall();
      onRemoved();
    } catch (e) {
      setError(errorText(toSetupError(e)));
    } finally {
      setBusy(null);
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <CircleCheck className="size-5 text-state-done" /> The agent is installed
        </CardTitle>
        <CardDescription>
          It runs in the background and starts with this computer. You will be asked for administrator
          rights. A newer setup file updates the agent when you use Change printers.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {error && <Notice tone="error">{error}</Notice>}
        <Actions>
          <AlertDialog>
            <AlertDialogTrigger asChild>
              <Button type="button" variant="destructive" disabled={busy !== null}>
                {busy === "remove" ? <Loader2 className="animate-spin" /> : <Trash2 />}
                Remove agent
              </Button>
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Remove the agent?</AlertDialogTitle>
                <AlertDialogDescription>
                  It stops, no longer starts with this computer, and its folder is deleted. The device
                  stays listed on your server until you revoke its key.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Keep it</AlertDialogCancel>
                <AlertDialogAction variant="destructive" onClick={() => void remove()}>
                  Remove
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
          <Button type="button" variant="brand" disabled={busy !== null} onClick={() => void change()}>
            {busy === "read" ? <Loader2 className="animate-spin" /> : <Pencil />}
            Change printers
          </Button>
        </Actions>
      </CardContent>
    </Card>
  );
}
