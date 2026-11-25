// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useState } from "react";
import { CircleArrowUp, ExternalLink, Loader2, RefreshCw } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
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
import { fmtAgo } from "@/lib/format";
import { setup, toSetupError, type AgentStatus, type Platform, type UpdateInfo, type UpdateMode } from "./api";
import {
  MODES,
  NEEDS_CONNECTED,
  autoLine,
  errorText,
  modePatch,
  goBackNote,
  modeStillOff,
  ownIsNewer,
  updateAvailable,
  updateErrorText,
  updateLine,
  UPDATE_URL_TEXT,
} from "./model";
import { Actions, Notice } from "./bits";
import { RunNotice, useApplyPatch } from "./settings-apply";

type Install =
  | { at: "idle" }
  | { at: "downloading" }
  | { at: "confirming" }
  | { at: "installing" }
  | { at: "done"; version: string; openError?: string }
  | { at: "failed"; message: string };

/**
 * Updates: whether a newer version exists, the three modes, and the one-click install. The window
 * only fetches and shows; the installed agent checks every file again and swaps itself and the
 * tray program after one administrator prompt, or puts the old version back.
 */
export function UpdatesSection({
  info,
  status,
  platform,
  onChanged,
  onInfo,
  onInstallStart,
  onInstallEnd,
}: {
  info: UpdateInfo | null;
  status: AgentStatus | null;
  platform: Platform;
  onChanged: () => void;
  onInfo: (info: UpdateInfo) => void;
  /**
   * An install from this window starts and ends. The marker names the new version before the
   * install has finished (and this window is the old program for a moment longer after it): the
   * window keeps this section until `onInstallEnd`, `true` when it worked.
   */
  onInstallStart: () => void;
  onInstallEnd: (installed: boolean) => void;
}) {
  const [install, setInstall] = useState<Install>({ at: "idle" });
  const [checking, setChecking] = useState(false);
  const [checkError, setCheckError] = useState<string | null>(null);
  // what the last change was, so that the line after it names it
  const [lastChange, setLastChange] = useState<"mode" | "setup">("mode");
  // a mode that looks was saved and the agent still says off: its update_url is no folder
  const [stillOff, setStillOff] = useState(false);
  const { run, busy, apply, keepWaiting } = useApplyPatch(onChanged);

  if (!info) return <Notice tone="warn">The state of the updates is not known yet.</Notice>;

  const online = status?.state === "online";
  const busyNow = busy || install.at === "downloading" || install.at === "confirming" || install.at === "installing";
  const mode: UpdateInfo["mode"] = info.mode;

  async function check() {
    setChecking(true);
    setCheckError(null);
    try {
      onInfo(await setup.checkUpdate(true));
    } catch (e) {
      setCheckError(updateErrorText(toSetupError(e)));
    } finally {
      setChecking(false);
    }
  }

  async function installLatest() {
    setInstall({ at: "downloading" });
    onInstallStart();
    try {
      await setup.downloadUpdate();
      setInstall({ at: "confirming" });
      const version = await setup.installUpdate();
      setInstall({ at: "done", version });
      onInstallEnd(true);
      onChanged();
      // the window reopens as the version that was just installed
      setTimeout(
        () =>
          void setup.openInstalled().catch((e) =>
            setInstall({ at: "done", version, openError: errorText(toSetupError(e)) }),
          ),
        2500,
      );
    } catch (e) {
      setInstall({ at: "failed", message: updateErrorText(toSetupError(e)) });
      onInstallEnd(false);
      // the last attempt may have been written (a put-back): read it again
      onChanged();
    }
  }

  const change = (next: UpdateMode) => {
    const patch = modePatch(mode, next);
    if (patch) {
      setLastChange("mode");
      setStillOff(false);
      void (async () => {
        if (!(await apply(patch))) return;
        // the agent writes "off" for an update_url it cannot use, whatever updates says
        try {
          const fresh = await setup.updateInfo();
          onInfo(fresh);
          setStillOff(modeStillOff(next, fresh.mode));
        } catch {
          // the status poll shows the mode soon enough
        }
      })();
    }
  };

  const canInstall = info.supported && updateAvailable(info) && info.installed !== null;
  const line = updateLine(info, (ts) => fmtAgo(ts));
  const auto = autoLine(mode);

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <CircleArrowUp className="size-5" /> Updates
        </CardTitle>
        <CardDescription>
          The agent and this program, in one step. A new version that does not connect is replaced by
          the old one.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <p role="status" className="text-sm">
          {line}
          {auto && <span className="block text-muted-foreground">{auto}</span>}
        </p>
        {info.supported && (
          <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1 text-sm">
            <dt className="text-muted-foreground">Installed</dt>
            <dd className="font-mono">{info.installed ?? "unknown"}</dd>
            <dt className="text-muted-foreground">This program</dt>
            <dd className="font-mono">{info.own}</dd>
          </dl>
        )}
        {checkError && <Notice tone="error">{checkError}</Notice>}
        {canInstall && !online && <Notice tone="warn">{NEEDS_CONNECTED}</Notice>}
        {install.at === "downloading" && (
          <p role="status" className="text-sm text-muted-foreground">
            Downloading…
          </p>
        )}
        {install.at === "confirming" && (
          <p role="status" className="text-sm text-muted-foreground">
            Confirm the administrator prompt. Installing and checking that the new version connects.
            Usually under a minute, up to 15 minutes if it has to be put back.
          </p>
        )}
        {install.at === "done" && (
          <Notice tone="ok">
            Version {install.version} is installed.{" "}
            {install.openError
              ? `This window could not open the installed program (${install.openError}). Open it from the menu or the icon.`
              : "This window reopens."}
          </Notice>
        )}
        {install.at === "failed" && <Notice tone="error">{install.message}</Notice>}

        {ownIsNewer(info) && info.installed !== null && (
          <Notice tone="info">
            This program ({info.own}) is newer than the installed agent ({info.installed}).
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="ml-2"
              disabled={busyNow}
              onClick={() => {
                setLastChange("setup");
                void apply({});
              }}
            >
              Install {info.own} from this setup
            </Button>
          </Notice>
        )}

        {info.supported && mode !== "unknown" && (
          <div className="flex flex-wrap items-center justify-between gap-2">
            <span className="text-sm text-muted-foreground" id="update-mode-label">
              When a new version is out
            </span>
            <Select value={mode} onValueChange={(v) => change(v as UpdateMode)} disabled={busyNow}>
              <SelectTrigger size="sm" aria-labelledby="update-mode-label" className="w-56">
                <SelectValue>{MODES.find((m) => m.value === mode)?.label}</SelectValue>
              </SelectTrigger>
              <SelectContent position="popper" align="end" className="w-(--radix-select-trigger-width)">
                {MODES.map((m) => (
                  <SelectItem key={m.value} value={m.value}>
                    {m.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        )}
        <RunNotice
          run={run}
          platform={platform}
          onKeepWaiting={() => keepWaiting()}
          doneTone={lastChange === "mode" && stillOff ? "warn" : "ok"}
          doneText={
            lastChange === "setup"
              ? `Version ${info.own} is installed from this setup. The agent is running.`
              : stillOff
                ? UPDATE_URL_TEXT
                : "Saved. The agent is running with the new setting."
          }
        />

        <Actions>
          {info.latest && (
            <Button
              type="button"
              variant="outline"
              onClick={() => void setup.openReleaseNotes(info.latest ?? "").catch(() => undefined)}
            >
              <ExternalLink /> Release notes
            </Button>
          )}
          {info.supported && mode !== "off" && mode !== "unknown" && (
            <Button type="button" variant="outline" disabled={checking || busyNow} onClick={() => void check()}>
              {checking ? <Loader2 className="animate-spin" /> : <RefreshCw />}
              Check now
            </Button>
          )}
          {canInstall && (
            <Button
              type="button"
              variant="brand"
              disabled={busyNow || !online}
              title={online ? undefined : NEEDS_CONNECTED}
              onClick={() => void installLatest()}
            >
              {busyNow ? <Loader2 className="animate-spin" /> : <CircleArrowUp />}
              Install {info.latest}
            </Button>
          )}
        </Actions>
      </CardContent>
    </Card>
  );
}

/** The confirmation of going back: an AlertDialog around the button that does it. */
export function GoBackButton({
  own,
  mode,
  disabled,
  onConfirm,
}: {
  own: string;
  mode: UpdateInfo["mode"];
  disabled: boolean;
  onConfirm: () => void;
}) {
  return (
    <AlertDialog>
      <AlertDialogTrigger asChild>
        <Button type="button" variant="outline" disabled={disabled}>
          Go back to {own}
        </Button>
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Go back to {own}?</AlertDialogTitle>
          <AlertDialogDescription>
            {["The installed agent is replaced by the one in this program.", goBackNote(mode), "You will be asked for administrator rights."]
              .filter(Boolean)
              .join(" ")}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction onClick={onConfirm}>Go back to {own}</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
