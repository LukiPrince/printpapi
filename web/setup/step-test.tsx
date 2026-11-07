// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useState } from "react";
import { CircleAlert, CircleCheck, Loader2, Printer } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { setup, toSetupError, type Platform, type PrinterChoice } from "./api";
import { canTest, errorText } from "./model";
import { Actions } from "./bits";

type Result = { state: "sending" } | { state: "sent" } | { state: "failed"; message: string };

/** Step 3. Optional: one button per chosen printer, straight at the device, the server is not involved. */
export function StepTest({
  platform,
  selected,
  sumatra,
  onBack,
  onNext,
}: {
  platform: Platform;
  selected: PrinterChoice[];
  sumatra: boolean;
  onBack: () => void;
  onNext: () => void;
}) {
  const [results, setResults] = useState<Record<string, Result>>({});

  async function test(p: PrinterChoice) {
    setResults((r) => ({ ...r, [p.name]: { state: "sending" } }));
    try {
      await setup.testPrint(p);
      setResults((r) => ({ ...r, [p.name]: { state: "sent" } }));
    } catch (e) {
      setResults((r) => ({ ...r, [p.name]: { state: "failed", message: errorText(toSetupError(e)) } }));
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Does something come out of the printer?</CardTitle>
        <CardDescription>
          Optional. The test goes straight to the printer from this machine.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <ul className="space-y-2">
          {selected.map((p) => {
            const r = results[p.name];
            const allowed = canTest(p, platform, sumatra);
            return (
              <li key={p.name} className="rounded-lg border px-3 py-2">
                <div className="flex items-center gap-3">
                  <Printer className="size-4 shrink-0 text-muted-foreground" />
                  <span className="min-w-0 flex-1 truncate font-medium" title={p.name}>
                    {p.name}
                  </span>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    disabled={!allowed || r?.state === "sending"}
                    onClick={() => void test(p)}
                  >
                    {r?.state === "sending" && <Loader2 className="animate-spin" />}
                    {p.pdf ? "Print test page" : "Print test label"}
                  </Button>
                </div>
                {p.locked && (
                  <p className="mt-1 text-xs text-muted-foreground">
                    Set up by hand in agent.ini. It is kept as it is and not tested here.
                  </p>
                )}
                {!allowed && !p.locked && (
                  <p className="mt-1 text-xs text-muted-foreground">
                    PDF test prints need SumatraPDF. Go back and download it first.
                  </p>
                )}
                {r?.state === "sent" && (
                  <p className="mt-1 flex items-center gap-1.5 text-xs text-state-done">
                    <CircleCheck className="size-3" /> Sent to the printer
                  </p>
                )}
                {r?.state === "failed" && (
                  <p role="alert" className="mt-1 flex items-start gap-1.5 text-xs text-state-failed">
                    <CircleAlert className="mt-0.5 size-3 shrink-0" /> Not printed: {r.message}
                  </p>
                )}
              </li>
            );
          })}
        </ul>
        <Actions>
          <Button type="button" variant="ghost" onClick={onBack}>
            Back
          </Button>
          <Button type="button" variant="brand" onClick={onNext}>
            Continue
          </Button>
        </Actions>
      </CardContent>
    </Card>
  );
}
