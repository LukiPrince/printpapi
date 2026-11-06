// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useEffect, useState } from "react";
import { Loader2, Plug } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { FieldLabel } from "@/components/bits";
import { setup, toSetupError } from "./api";
import { errorText } from "./model";
import { Actions, Notice } from "./bits";

export type Connection = { serverUrl: string; apiKey: string; name: string };

/** Step 1. Checks the address and the key; nothing is created on the server yet. */
export function StepConnect({
  initial,
  onNext,
}: {
  initial: Connection | null;
  onNext: (c: Connection) => void;
}) {
  const [serverUrl, setServerUrl] = useState(initial?.serverUrl ?? "");
  const [apiKey, setApiKey] = useState(initial?.apiKey ?? "");
  const [name, setName] = useState(initial?.name ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // the address the user agreed to send the key to unencrypted
  const [confirmedUrl, setConfirmedUrl] = useState<string | null>(null);
  const [askInsecure, setAskInsecure] = useState(false);
  // a verified connection whose device name already exists: shown with a hint, never blocked
  const [nameTaken, setNameTaken] = useState(false);

  useEffect(() => {
    if (initial) return;
    let live = true;
    setup.defaultName().then(
      (n) => {
        if (live) setName((cur) => cur || n);
      },
      () => {},
    );
    return () => {
      live = false;
    };
  }, [initial]);

  const filled = serverUrl.trim() !== "" && apiKey.trim() !== "" && name.trim() !== "";
  const current = (): Connection => ({
    serverUrl: serverUrl.trim(),
    apiKey: apiKey.trim(),
    name: name.trim(),
  });

  function edit(set: (v: string) => void) {
    return (e: React.ChangeEvent<HTMLInputElement>) => {
      set(e.target.value);
      setNameTaken(false);
      setAskInsecure(false);
      setError(null);
    };
  }

  async function connect(insecureOk: boolean) {
    const c = current();
    setBusy(true);
    setError(null);
    try {
      if (!insecureOk && confirmedUrl !== c.serverUrl && (await setup.needsInsecureConfirm(c.serverUrl))) {
        setAskInsecure(true);
        return;
      }
      setAskInsecure(false);
      const exists = await setup.checkConnection(c.serverUrl, c.apiKey, c.name);
      if (exists) setNameTaken(true);
      else onNext(c);
    } catch (e) {
      setError(errorText(toSetupError(e)));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Connect to your server</CardTitle>
        <CardDescription>
          Create one key per device on the Keys page of your dashboard.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <form
          className="space-y-3"
          onSubmit={(e) => {
            e.preventDefault();
            if (nameTaken) onNext(current());
            else if (filled && !busy) void connect(false);
          }}
        >
          <div>
            <FieldLabel htmlFor="server">Server address</FieldLabel>
            <Input
              id="server"
              className="mt-1.5 font-mono"
              placeholder="https://print.example.com"
              autoComplete="off"
              value={serverUrl}
              onChange={edit(setServerUrl)}
            />
          </div>
          <div>
            <FieldLabel htmlFor="key">Agent key</FieldLabel>
            <Input
              id="key"
              type="password"
              className="mt-1.5 font-mono"
              autoComplete="off"
              value={apiKey}
              onChange={edit(setApiKey)}
            />
          </div>
          <div>
            <FieldLabel htmlFor="name">Name of this device</FieldLabel>
            <Input id="name" className="mt-1.5" autoComplete="off" value={name} onChange={edit(setName)} />
          </div>

          {askInsecure && (
            <Notice tone="warn">
              <p>This address is not encrypted. Your key would travel readable over the network.</p>
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="mt-2"
                onClick={() => {
                  setConfirmedUrl(serverUrl.trim());
                  void connect(true);
                }}
              >
                Use it anyway
              </Button>
            </Notice>
          )}
          {nameTaken && (
            <Notice tone="warn">
              A device with this name already exists. Continue only if this is the same machine.
            </Notice>
          )}
          {error && <Notice tone="error">{error}</Notice>}

          <Actions>
            <Button type="submit" variant="brand" disabled={!filled || busy || askInsecure}>
              {busy ? <Loader2 className="animate-spin" /> : <Plug />}
              {nameTaken ? "Continue" : "Connect"}
            </Button>
          </Actions>
        </form>
      </CardContent>
    </Card>
  );
}
