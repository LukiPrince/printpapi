// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useEffect, useState } from "react";
import { CircleCheck, Download, Loader2, RotateCw, Trash2 } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
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
import { setup, toSetupError, type Platform } from "./api";
import { errorText } from "./model";
import { Actions, Notice } from "./bits";
import { RunNotice, useApplyPatch } from "./settings-apply";

/**
 * Stops the agent and starts it again. One administrator prompt, then the wait for the agent's next
 * contact after the start, which the elevated run reports (not a time read afterwards).
 */
export function RestartSection({ platform, onRestarted }: { platform: Platform; onRestarted: () => void }) {
  const { run, busy, restart, keepWaiting } = useApplyPatch(onRestarted);

  return (
    <Card>
      <CardHeader>
        <CardTitle>Restart</CardTitle>
        <CardDescription>
          Stops the agent and starts it again, for example after you changed agent.ini by hand. You will
          be asked for administrator rights.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <RunNotice
          run={run}
          platform={platform}
          onKeepWaiting={() => keepWaiting()}
          doneText="Restarted. The agent is connected again."
        />
        <Actions>
          <Button type="button" variant="brand" disabled={busy} onClick={() => void restart()}>
            {busy ? <Loader2 className="animate-spin" /> : <RotateCw />}
            Restart agent
          </Button>
        </Actions>
      </CardContent>
    </Card>
  );
}

/**
 * Windows only. SumatraPDF prints the PDF jobs. A download is put in place by a change with nothing
 * else in it: the install picks the file up from the working folder.
 */
export function SumatraSection({ platform, onApplied }: { platform: Platform; onApplied: () => void }) {
  const [present, setPresent] = useState<boolean | null>(null);
  // downloaded in this window but not installed yet: a second try does not download again
  const [downloaded, setDownloaded] = useState(false);
  const { run, busy, apply, keepWaiting } = useApplyPatch(() => {
    setPresent(true);
    onApplied();
  });

  useEffect(() => {
    let live = true;
    setup.sumatraPresent().then(
      (p) => {
        if (live) setPresent(p);
      },
      () => {},
    );
    return () => {
      live = false;
    };
  }, []);

  const install = () =>
    apply({}, async () => {
      if (downloaded) return;
      await setup.downloadSumatra();
      setDownloaded(true);
    });

  return (
    <Card>
      <CardHeader>
        <CardTitle>SumatraPDF</CardTitle>
        <CardDescription>
          The agent prints PDF jobs with SumatraPDF. Raw printers (ZPL, ESC/POS) do not need it.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {present && (
          <p className="flex items-center gap-2 text-sm">
            <CircleCheck className="size-4 text-state-done" /> SumatraPDF is installed.
          </p>
        )}
        {present === false && (
          <p className="text-sm text-muted-foreground">
            SumatraPDF is not installed, so PDF jobs fail. It is downloaded from the official site and
            checked before it is installed. You will be asked for administrator rights.
          </p>
        )}
        <RunNotice run={run} platform={platform} onKeepWaiting={() => keepWaiting()} />
        {present === false && (
          <Actions>
            <Button type="button" variant="brand" disabled={busy} onClick={() => void install()}>
              {busy ? <Loader2 className="animate-spin" /> : <Download />}
              Download and install
            </Button>
          </Actions>
        )}
      </CardContent>
    </Card>
  );
}

/** Removes the agent, its autostart and the tray icon. */
export function RemoveSection({ onRemoved }: { onRemoved: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function remove() {
    setBusy(true);
    setError(null);
    try {
      await setup.uninstall();
      onRemoved();
    } catch (e) {
      setError(errorText(toSetupError(e)));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Remove</CardTitle>
        <CardDescription>
          The agent stops, no longer starts with this computer, and its folder is deleted. The device stays
          listed on your server until you revoke its key.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <Notice tone="info">
          The tray icon is removed with it. If you opened this window from the tray icon, it closes, too:
          its program is part of what is removed.
        </Notice>
        {error && <Notice tone="error">{error}</Notice>}
        <Actions>
          <AlertDialog>
            <AlertDialogTrigger asChild>
              <Button type="button" variant="destructive" disabled={busy}>
                {busy ? <Loader2 className="animate-spin" /> : <Trash2 />}
                Remove agent
              </Button>
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Remove the agent?</AlertDialogTitle>
                <AlertDialogDescription>
                  It stops, no longer starts with this computer, and its folder and the tray icon are
                  deleted. This window closes if it belongs to the installed program. The device stays
                  listed on your server until you revoke its key.
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
        </Actions>
      </CardContent>
    </Card>
  );
}
