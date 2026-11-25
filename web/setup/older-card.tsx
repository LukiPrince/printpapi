// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useState } from "react";
import { Loader2, TriangleAlert } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { setup, toSetupError, type Platform, type UpdateInfo } from "./api";
import { errorText, goBackPatch, installedIsNewer } from "./model";
import { Actions, Notice } from "./bits";
import { RunNotice, useApplyPatch } from "./settings-apply";
import { GoBackButton } from "./settings-updates";

/**
 * Shown instead of every section when the installed agent is newer than this program (a download
 * that was kept, the .deb's program, a window left open). Changing anything from here would put
 * the old agent back, so the window offers the installed version, and, unless it is the installed
 * program itself, a deliberate way back. Works with a stopped agent: the marker names the version.
 */
export function OlderCard({
  info,
  platform,
  onChanged,
  onHold,
  onContinue,
}: {
  info: UpdateInfo;
  platform: Platform;
  onChanged: () => void;
  /** Go back starts: the window keeps this card, with its notice, whatever the marker says half way. */
  onHold: () => void;
  /** The way back is over and read: on to the sections. */
  onContinue: () => void;
}) {
  const [opening, setOpening] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { run, busy, apply, keepWaiting } = useApplyPatch(onChanged);
  // After the marker names this program's version the installed agent is no longer newer; the card
  // stays only to say how the way back ended, until the person goes on.
  const newer = installedIsNewer(info);

  async function openInstalled() {
    setOpening(true);
    setError(null);
    try {
      await setup.openInstalled();
    } catch (e) {
      setError(errorText(toSetupError(e)));
      setOpening(false);
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <TriangleAlert className="size-5 text-state-queued" />{" "}
          {newer ? "A newer version is installed" : `Going back to ${info.own}`}
        </CardTitle>
        {newer && (
          <CardDescription>
            The installed agent ({info.installed}) is newer than this program ({info.own}).
          </CardDescription>
        )}
      </CardHeader>
      <CardContent className="space-y-3">
        {error && <Notice tone="error">{error}</Notice>}
        <RunNotice
          run={run}
          platform={platform}
          onKeepWaiting={() => keepWaiting()}
          doneText={`Version ${info.own} is installed again.`}
        />
        <Actions>
          {newer && !info.fromInstalled && (
            <GoBackButton
              own={info.own}
              mode={info.mode}
              disabled={busy || opening}
              onConfirm={() => {
                onHold();
                void apply(goBackPatch(info.mode));
              }}
            />
          )}
          {newer ? (
            <Button type="button" variant="brand" disabled={busy || opening} onClick={() => void openInstalled()}>
              {opening && <Loader2 className="animate-spin" />}
              Open the installed version
            </Button>
          ) : (
            <Button type="button" variant="brand" disabled={busy} onClick={onContinue}>
              Continue to the settings
            </Button>
          )}
        </Actions>
      </CardContent>
    </Card>
  );
}
