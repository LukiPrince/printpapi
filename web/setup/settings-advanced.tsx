// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useState } from "react";
import { Eye, Loader2, Plus, Rocket, X } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { FieldLabel } from "@/components/bits";
import { setup, toSetupError, type AgentStatus, type Platform } from "./api";
import { errorText, patchFor, timeoutProblem } from "./model";
import { Actions, Notice } from "./bits";
import { RunNotice, useApplyPatch } from "./settings-apply";

type Headers = [string, string | null][];

/** `timeout` and `[headers]` of the agent. Header values are secrets: hidden until the admin prompt. */
export function AdvancedSection({
  status,
  platform,
  onApplied,
}: {
  status: AgentStatus | null;
  platform: Platform;
  onApplied: () => void;
}) {
  if (!status || status.name === "") {
    return (
      <Notice tone="warn">
        The agent&apos;s settings are not known while it is not running. Use Restart first.
      </Notice>
    );
  }
  return <AdvancedForm status={status} platform={platform} onApplied={onApplied} />;
}

function AdvancedForm({
  status,
  platform,
  onApplied,
}: {
  status: AgentStatus;
  platform: Platform;
  onApplied: () => void;
}) {
  // what the agent has now (`saved`) and what the form says; the patch is the difference
  const initial: Headers = status.headerNames.map((n) => [n, null]);
  const [saved, setSaved] = useState({ timeout: status.timeout, headers: initial });
  const [timeoutText, setTimeoutText] = useState(String(status.timeout));
  const [headers, setHeaders] = useState<Headers>(initial);
  const [newName, setNewName] = useState("");
  const [newValue, setNewValue] = useState("");
  const [revealing, setRevealing] = useState(false);
  const [revealError, setRevealError] = useState<string | null>(null);

  const problem = timeoutText.trim() === "" ? null : timeoutProblem(timeoutText);
  const timeoutOk = timeoutProblem(timeoutText) === null;
  const patch = patchFor(
    { timeout: saved.timeout, headers: saved.headers },
    { timeout: timeoutOk ? Number(timeoutText) : saved.timeout, headers },
  );
  // the agent has the form's state once the run is through
  const { run, busy, apply, keepWaiting } = useApplyPatch(() => {
    setSaved({ timeout: timeoutOk ? Number(timeoutText) : saved.timeout, headers });
    onApplied();
  });
  const hidden = headers.some(([, v]) => v === null);

  async function reveal() {
    setRevealing(true);
    setRevealError(null);
    try {
      const values = new Map((await setup.headerValues()).map(([n, v]) => [n.toLowerCase(), v]));
      const fill = (list: Headers): Headers => list.map(([n, v]) => [n, v ?? values.get(n.toLowerCase()) ?? null]);
      setSaved((s) => ({ ...s, headers: fill(s.headers) }));
      setHeaders(fill);
    } catch (e) {
      setRevealError(errorText(toSetupError(e)));
    } finally {
      setRevealing(false);
    }
  }

  function addHeader(e: React.FormEvent) {
    e.preventDefault();
    const name = newName.trim();
    if (name === "" || newValue === "") return;
    setHeaders((list) => [...list.filter(([n]) => n.toLowerCase() !== name.toLowerCase()), [name, newValue]]);
    setNewName("");
    setNewValue("");
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Advanced</CardTitle>
        <CardDescription>
          Most setups never need these. You will be asked for administrator rights once, and the agent
          restarts.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div>
          <FieldLabel htmlFor="timeout">Timeout (seconds)</FieldLabel>
          <div className="mt-1.5 w-32">
            <Input
              id="timeout"
              inputMode="decimal"
              className="font-mono"
              autoComplete="off"
              aria-invalid={problem !== null}
              value={timeoutText}
              onChange={(e) => setTimeoutText(e.target.value)}
            />
          </div>
          {problem ? (
            <p className="mt-1 text-xs text-state-failed">{problem}</p>
          ) : (
            <p className="mt-1 text-xs text-muted-foreground">
              How long one request to the server may take. The default is 60.
            </p>
          )}
        </div>

        <div className="space-y-2">
          <FieldLabel htmlFor="header-name">Headers</FieldLabel>
          <p className="text-xs text-muted-foreground">
            Extra headers for every request to the server, for example the token of a proxy in front of it.
          </p>
          {headers.length === 0 ? (
            <p className="text-sm text-muted-foreground">No headers set.</p>
          ) : (
            <ul className="space-y-1.5">
              {headers.map(([name, value]) => (
                <li key={name} className="flex items-center gap-2 rounded-lg border px-3 py-1.5 text-sm">
                  <span className="shrink-0 font-mono">{name}</span>
                  {value === null ? (
                    <span className="min-w-0 flex-1 truncate font-mono text-muted-foreground" aria-label="value hidden">
                      ••••••••
                    </span>
                  ) : (
                    <Input
                      aria-label={`Value of ${name}`}
                      className="h-8 min-w-0 flex-1 font-mono"
                      autoComplete="off"
                      value={value}
                      onChange={(e) =>
                        setHeaders((list) => list.map(([n, v]) => (n === name ? [n, e.target.value] : [n, v])))
                      }
                    />
                  )}
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label={`Remove header ${name}`}
                    onClick={() => setHeaders((list) => list.filter(([n]) => n !== name))}
                  >
                    <X />
                  </Button>
                </li>
              ))}
            </ul>
          )}
          {hidden && (
            <div>
              <Button type="button" variant="outline" size="sm" disabled={revealing} onClick={() => void reveal()}>
                {revealing ? <Loader2 className="animate-spin" /> : <Eye />}
                Show values
              </Button>
              <p className="mt-1 text-xs text-muted-foreground">Asks for administrator rights.</p>
            </div>
          )}
          {revealError && <Notice tone="error">{revealError}</Notice>}
          <form onSubmit={addHeader} className="grid gap-2 sm:grid-cols-[1fr_1fr_auto]">
            <Input
              id="header-name"
              aria-label="Header name"
              placeholder="Header name"
              className="font-mono"
              autoComplete="off"
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
            />
            <Input
              aria-label="Header value"
              placeholder="Value"
              className="font-mono"
              autoComplete="off"
              value={newValue}
              onChange={(e) => setNewValue(e.target.value)}
            />
            <Button type="submit" variant="outline" disabled={newName.trim() === "" || newValue === ""}>
              <Plus /> Add
            </Button>
          </form>
        </div>

        <RunNotice run={run} platform={platform} onKeepWaiting={() => keepWaiting()} />
        <Actions>
          <Button
            type="button"
            variant="brand"
            disabled={busy || !timeoutOk || Object.keys(patch).length === 0}
            onClick={() => void apply(patch)}
          >
            {busy ? <Loader2 className="animate-spin" /> : <Rocket />}
            Apply changes
          </Button>
        </Actions>
      </CardContent>
    </Card>
  );
}
