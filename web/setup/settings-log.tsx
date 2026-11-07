// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useCallback, useEffect, useState } from "react";
import { Loader2, RefreshCw } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { setup, toSetupError, type Platform } from "./api";
import { errorText, logLocation } from "./model";
import { Actions, Notice, Path } from "./bits";

/** How many of the last lines the window shows. */
const LINES = 200;

/** The agent's log, so that nobody has to find the file. Reads it without administrator rights. */
export function LogSection({ platform }: { platform: Platform }) {
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    setBusy(true);
    try {
      setText(await setup.readLog(LINES));
      setError(null);
    } catch (e) {
      setError(errorText(toSetupError(e)));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    // a read on mount; it sets state once the answer is in
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void load();
  }, [load]);

  return (
    <Card>
      <CardHeader>
        <CardTitle>Log</CardTitle>
        <CardDescription>
          {`The last ${LINES} lines of the agent's log file:`}
          <Path path={logLocation(platform)} className="mt-0.5 block" />
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {error && <Notice tone="error">{error}</Notice>}
        {text !== null &&
          (text === "" ? (
            <p className="text-sm text-muted-foreground">Nothing has been logged yet. That is a good sign.</p>
          ) : (
            <pre
              tabIndex={0}
              aria-label="Agent log"
              className="max-h-72 overflow-auto rounded-lg border bg-muted/40 p-3 font-mono text-xs leading-relaxed whitespace-pre-wrap break-words"
            >
              {text}
            </pre>
          ))}
        <Actions>
          <Button type="button" variant="outline" disabled={busy} onClick={() => void load()}>
            {busy ? <Loader2 className="animate-spin" /> : <RefreshCw />}
            Refresh
          </Button>
        </Actions>
      </CardContent>
    </Card>
  );
}
