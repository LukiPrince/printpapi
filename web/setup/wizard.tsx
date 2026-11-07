// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useEffect, useState } from "react";
import { setup, type Existing, type FoundPrinter, type Platform, type PrinterChoice } from "./api";
import { Shell } from "./bits";
import { StepConnect, type Connection } from "./step-connect";
import { StepPrinters } from "./step-printers";
import { StepTest } from "./step-test";
import { StepFinish } from "./step-finish";

export const NEW_STEPS = ["Connect", "Printers", "Test", "Finish"];
const CHANGE_STEPS = ["Printers", "Test", "Finish"];

/**
 * The four steps (three when changing printers: server, key and name stay as they are).
 * It only holds what the steps share; every rule lives in model.ts, every call in api.ts.
 */
export function Wizard({
  platform,
  existing,
  replacing = null,
  onDone,
  onCancel,
}: {
  platform: Platform;
  /** Present when the printers of an installed agent are being changed. */
  existing: Existing | null;
  /** Present when an installed agent gets another server or key: the four steps, from what is known. */
  replacing?: Existing | null;
  onDone: () => void;
  /** Leaves a change of an installed agent at its first step, with nothing changed. */
  onCancel?: () => void;
}) {
  const steps = existing ? CHANGE_STEPS : NEW_STEPS;
  const [i, setI] = useState(0);
  // the key is never known here: the person types the new one
  const [connection, setConnection] = useState<Connection | null>(
    replacing ? { serverUrl: replacing.serverUrl, apiKey: "", name: replacing.name } : null,
  );
  const [selected, setSelected] = useState<PrinterChoice[]>((existing ?? replacing)?.printers ?? []);
  const [found, setFound] = useState<FoundPrinter[] | null>(null);
  const [sumatra, setSumatra] = useState(false);
  // the agent reported in: every step is done, none is current
  const [finished, setFinished] = useState(false);
  // why step 1 is shown again: the install found the key taken, revoked or the name another key's
  const [connectNotice, setConnectNotice] = useState<string | null>(null);

  // A machine without printers, or without lpstat, comes back as an empty list, not an error.
  useEffect(() => {
    let live = true;
    setup.listPrinters().then(
      (list) => {
        if (live) setFound(list);
      },
      () => {
        if (live) setFound([]);
      },
    );
    return () => {
      live = false;
    };
  }, []);

  useEffect(() => {
    if (platform !== "windows") return;
    let live = true;
    setup.sumatraPresent().then(
      (present) => {
        if (live) setSumatra(present);
      },
      () => {},
    );
    return () => {
      live = false;
    };
  }, [platform]);

  const step = steps[i];
  const back = () => setI((n) => Math.max(0, n - 1));
  const next = () => setI((n) => Math.min(steps.length - 1, n + 1));

  return (
    <Shell steps={steps} current={finished ? steps.length : i}>
      {step === "Connect" && (
        <StepConnect
          initial={connection}
          replacing={replacing}
          notice={connectNotice}
          onCancel={replacing ? onCancel : undefined}
          onNext={(c) => {
            setConnection(c);
            setConnectNotice(null);
            next();
          }}
        />
      )}
      {step === "Printers" && (
        <StepPrinters
          platform={platform}
          found={found}
          selected={selected}
          sumatra={sumatra}
          onChange={setSelected}
          onSumatra={() => setSumatra(true)}
          onBack={existing ? undefined : back}
          onCancel={existing ? onCancel : undefined}
          onNext={next}
        />
      )}
      {step === "Test" && (
        <StepTest platform={platform} selected={selected} sumatra={sumatra} onBack={back} onNext={next} />
      )}
      {step === "Finish" && (
        <StepFinish
          platform={platform}
          connection={existing ? null : connection}
          deviceName={existing ? existing.name : (connection?.name ?? "")}
          selected={selected}
          changeFrom={existing?.printers ?? []}
          onBack={back}
          onBackToConnect={
            existing
              ? undefined
              : (message) => {
                  setConnectNotice(message ?? null);
                  setI(0);
                }
          }
          onOnline={() => setFinished(true)}
          onDone={onDone}
        />
      )}
    </Shell>
  );
}
