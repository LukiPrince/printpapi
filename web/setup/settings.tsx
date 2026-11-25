// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useCallback, useEffect, useState } from "react";
import {
  Activity,
  CircleArrowUp,
  FileDown,
  Plug,
  Printer,
  RotateCw,
  ScrollText,
  SlidersHorizontal,
  Trash2,
} from "lucide-react";
import { setup, toSetupError, type AgentStatus, type Existing, type PairingView, type Platform, type UpdateInfo } from "./api";
import { errorText, installHereInfo, olderCardShown, updateAvailable, type Hold } from "./model";
import { Notice, Shell, type NavItem } from "./bits";
import { Wizard } from "./wizard";
import { Overview } from "./settings-overview";
import { PrintersSection } from "./settings-printers";
import { ConnectionSection } from "./settings-connection";
import { AdvancedSection } from "./settings-advanced";
import { LogSection } from "./settings-log";
import { RemoveSection, RestartSection, SumatraSection } from "./settings-actions";
import { UpdatesSection } from "./settings-updates";
import { RunNotice, useApplyPatch } from "./settings-apply";
import { Button } from "@/components/ui/button";
import { OlderCard } from "./older-card";
import { useBrand } from "./brand";
import { usePairingLink } from "./use-pairing-link";

const SECTIONS: (NavItem & { windowsOnly?: boolean })[] = [
  { id: "overview", label: "Overview", icon: Activity },
  { id: "updates", label: "Updates", icon: CircleArrowUp },
  { id: "printers", label: "Printers", icon: Printer },
  { id: "connection", label: "Connection", icon: Plug },
  { id: "advanced", label: "Advanced", icon: SlidersHorizontal },
  { id: "log", label: "Log", icon: ScrollText },
  { id: "sumatra", label: "SumatraPDF", icon: FileDown, windowsOnly: true },
  { id: "restart", label: "Restart", icon: RotateCw },
  { id: "remove", label: "Remove", icon: Trash2 },
];

/** How often the window reads the status file. The file itself changes at most about once a minute. */
const POLL_MS = 5000;

/** The status file, read now and then. A read that fails keeps the last good answer. */
function useStatus() {
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStatus(await setup.readStatus());
      setError(null);
    } catch (e) {
      setError(errorText(toSetupError(e)));
    }
  }, []);

  useEffect(() => {
    // a read on mount and then on a timer; each sets state once the answer is in
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void refresh();
    const timer = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(timer);
  }, [refresh]);

  return { status, error, refresh };
}

/**
 * What the window knows about updates. It is read at the start and then with the status poll (no
 * network: files only), so a window left open overnight notices that an update installed a newer
 * agent and shows the older card. The one look at the signed file is made when the window opens;
 * the cache keeps it to one request a day, and nothing is asked in the mode "off".
 */
function useUpdateInfo() {
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const refresh = useCallback(async () => {
    try {
      setInfo(await setup.updateInfo());
    } catch {
      // the sections still work without it
    }
  }, []);
  useEffect(() => {
    // a read on mount, a look at the signed file once, and a read on a timer; each sets state
    // once the answer is in
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void refresh();
    void setup.checkUpdate(false).then(setInfo, () => undefined);
    const timer = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(timer);
  }, [refresh]);
  return { info, setInfo, refresh };
}

/** What the setup flow is told about the agent that is there now. */
function known(status: AgentStatus | null): Existing {
  return {
    serverUrl: status?.server ?? "",
    name: status?.name ?? "",
    printers: status?.printers ?? [],
    keyUnreadable: status?.keyUnreadable ?? false,
  };
}

type Flow = { kind: "printers" | "connection"; existing: Existing; link?: PairingView };

/**
 * This program is newer than the installed agent: one click installs this program's version over
 * it (the same change as "Install from this setup" in Updates, which a brand does not have).
 */
function InstallHere({
  info,
  platform,
  onStart,
  onChanged,
}: {
  info: UpdateInfo;
  platform: Platform;
  /** The button was clicked: the owner keeps this notice on screen from now on. */
  onStart: () => void;
  onChanged: () => void;
}) {
  const { run, busy, apply, keepWaiting } = useApplyPatch(onChanged);
  return (
    <Notice tone="info">
      <p>
        This program ({info.own}) is newer than the installed agent ({info.installed}).
      </p>
      <RunNotice
        run={run}
        platform={platform}
        onKeepWaiting={() => keepWaiting()}
        doneText={`Version ${info.own} is installed from this setup. The agent is running.`}
      />
      {/* a retry after a failure; not while it runs and not once it is over */}
      {(run.at === "idle" || run.at === "failed") && (
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
          Install {info.own} from this setup
        </Button>
      )}
    </Notice>
  );
}

/**
 * The window of an agent that is installed: its state and the settings that matter, in sections.
 * Looking needs no administrator rights; each change asks for them once.
 */
export function Settings({
  platform,
  onRemoved,
  top,
}: {
  platform: Platform;
  onRemoved: () => void;
  /** Shown above the section, whichever it is (the offer to take over a stock install). */
  top?: React.ReactNode;
}) {
  const { brand } = useBrand();
  // a white-label build has no updates: no section, no line in the overview
  const sections = SECTIONS.filter((s) => (!s.windowsOnly || platform === "windows") && !(brand && s.id === "updates"));
  const { status, error, refresh } = useStatus();
  const { info, setInfo, refresh: refreshInfo } = useUpdateInfo();
  const [section, setSection] = useState("overview");
  // An install started here makes the installed agent newer than this program until the window
  // reopens as the new version: the card for that must not replace the notice of the install.
  const [installedHere, setInstalledHere] = useState(false);
  // What the window keeps on screen while an install it started runs: the poll reads the new
  // marker half way through, and swapping the screen would drop the notice of how it ended.
  const [hold, setHold] = useState<Hold>(null);
  // What the notice of "Install X from this setup" showed when its button was clicked: the install
  // writes its marker half way through, and the notice must outlive the poll that reads it.
  const [hereInfo, setHereInfo] = useState<UpdateInfo | null>(null);
  // the section the command line asked for (the tray's "Updates…"), taken once
  useEffect(() => {
    void setup.startSection().then(
      (s) => {
        if (s && sections.some((x) => x.id === s)) setSection(s);
      },
      () => undefined,
    );
    // read once at the start, with the sections of that moment
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  // the printer steps and the connection step of the setup, started from what is known now
  const [flow, setFlow] = useState<Flow | null>(null);
  // A pairing link opens the connection step with the link in it (nothing is contacted yet). Only
  // once the status is read and while no flow is open: the flow's own first step takes a later one.
  usePairingLink((link) => {
    if (status) {
      setSection("connection");
      setFlow({ kind: "connection", existing: known(status), link });
    }
  }, flow === null && status !== null);

  const shown = installHereInfo(brand !== null, top !== undefined, info, hereInfo);

  // An installed agent that is newer than this program: nothing here may change it by accident.
  if (info && olderCardShown(info, installedHere, hold)) {
    return (
      <Shell>
        <OlderCard
          info={info}
          platform={platform}
          onChanged={() => { void refresh(); void refreshInfo(); }}
          onHold={() => setHold("card")}
          onContinue={() => setHold(null)}
        />
      </Shell>
    );
  }

  if (flow) {
    return (
      <Wizard
        platform={platform}
        existing={flow.kind === "printers" ? flow.existing : null}
        replacing={flow.kind === "connection" ? flow.existing : null}
        link={flow.link ?? null}
        onDone={() => {
          setFlow(null);
          void refresh();
        }}
        onCancel={() => setFlow(null)}
      />
    );
  }

  return (
    <Shell nav={{ label: "Settings sections", items: sections, current: section, onSelect: setSection }}>
      {top}
      {/* A brand has no Updates section, and running a newer build of it is how it is rolled out:
          the one button of that section that is no update check stays. Not next to the take-over
          offer, whose button does the same. */}
      {shown && (
        <InstallHere
          info={shown}
          platform={platform}
          onStart={() => setHereInfo(shown)}
          onChanged={() => {
            void refresh();
            void refreshInfo();
          }}
        />
      )}
      {section === "overview" && (
        <Overview
          status={status}
          error={error}
          update={!brand && info && updateAvailable(info) ? info : null}
          onUpdates={() => setSection("updates")}
        />
      )}
      {section === "updates" && !brand && (
        <UpdatesSection
          info={info}
          status={status}
          platform={platform}
          onChanged={() => { void refresh(); void refreshInfo(); }}
          onInfo={setInfo}
          onInstallStart={() => setHold("section")}
          onInstallEnd={(installed) => {
            if (installed) setInstalledHere(true);
            setHold(null);
          }}
        />
      )}
      {section === "printers" && (
        <PrintersSection status={status} onChange={() => setFlow({ kind: "printers", existing: known(status) })} />
      )}
      {section === "connection" && (
        <ConnectionSection status={status} onChange={() => setFlow({ kind: "connection", existing: known(status) })} />
      )}
      {section === "advanced" && <AdvancedSection status={status} platform={platform} onApplied={() => void refresh()} />}
      {section === "log" && <LogSection platform={platform} />}
      {section === "sumatra" && <SumatraSection platform={platform} onApplied={() => void refresh()} />}
      {section === "restart" && <RestartSection platform={platform} onRestarted={() => void refresh()} />}
      {section === "remove" && <RemoveSection onRemoved={onRemoved} />}
    </Shell>
  );
}
