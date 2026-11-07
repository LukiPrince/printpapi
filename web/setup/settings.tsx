// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useCallback, useEffect, useState } from "react";
import {
  Activity,
  FileDown,
  Plug,
  Printer,
  RotateCw,
  ScrollText,
  SlidersHorizontal,
  Trash2,
} from "lucide-react";
import { setup, toSetupError, type AgentStatus, type Existing, type Platform } from "./api";
import { errorText } from "./model";
import { Shell, type NavItem } from "./bits";
import { Wizard } from "./wizard";
import { Overview } from "./settings-overview";
import { PrintersSection } from "./settings-printers";
import { ConnectionSection } from "./settings-connection";
import { AdvancedSection } from "./settings-advanced";
import { LogSection } from "./settings-log";
import { RemoveSection, RestartSection, SumatraSection } from "./settings-actions";

const SECTIONS: (NavItem & { windowsOnly?: boolean })[] = [
  { id: "overview", label: "Overview", icon: Activity },
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

/** What the setup flow is told about the agent that is there now. */
function known(status: AgentStatus | null): Existing {
  return { serverUrl: status?.server ?? "", name: status?.name ?? "", printers: status?.printers ?? [] };
}

type Flow = { kind: "printers" | "connection"; existing: Existing };

/**
 * The window of an agent that is installed: its state and the settings that matter, in sections.
 * Looking needs no administrator rights; each change asks for them once.
 */
export function Settings({ platform, onRemoved }: { platform: Platform; onRemoved: () => void }) {
  const { status, error, refresh } = useStatus();
  const [section, setSection] = useState("overview");
  // the printer steps and the connection step of the setup, started from what is known now
  const [flow, setFlow] = useState<Flow | null>(null);

  if (flow) {
    return (
      <Wizard
        platform={platform}
        existing={flow.kind === "printers" ? flow.existing : null}
        replacing={flow.kind === "connection" ? flow.existing : null}
        onDone={() => {
          setFlow(null);
          void refresh();
        }}
        onCancel={() => setFlow(null)}
      />
    );
  }

  const items = SECTIONS.filter((s) => !s.windowsOnly || platform === "windows");
  return (
    <Shell nav={{ label: "Settings sections", items, current: section, onSelect: setSection }}>
      {section === "overview" && <Overview status={status} error={error} />}
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
