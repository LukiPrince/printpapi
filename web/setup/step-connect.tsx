// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useEffect, useRef, useState } from "react";
import { KeyRound, Loader2, Plug } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { FieldLabel } from "@/components/bits";
import { setup, toSetupError, type PairedView, type PairingView } from "./api";
import {
  errorText,
  fixedServerHint,
  formEdited,
  formFromLink,
  formServer,
  installedConnection,
  keepsOldName,
  needsNewName,
  pairingFields,
  pairingResult,
  startServer,
  type PairForm,
} from "./model";
import { Actions, Notice } from "./bits";
import { useBrand } from "./brand";
import { usePairingLink } from "./use-pairing-link";

/** `apiKey` is null for a connection a pairing code made: the key stays in the shell. */
export type Connection = { serverUrl: string; apiKey: string | null; name: string; paired?: PairedView };

/** What the connection of an installed agent is now, when it is being replaced. */
export type Replacing = { serverUrl: string; name: string; keyUnreadable?: boolean };

const KEY_UNREADABLE_NOTICE =
  "The key stored on this computer cannot be read. Enter the key this device already uses and keep its name, or a new key with a new name.";

const SAME_SERVER_WARNING =
  "A new key on the same server needs a new device name. The printers get new IDs, so integrations that use fixed printer IDs must be updated, and the old device stays in your dashboard.";

type StepProps = {
  initial: Connection | null;
  /** Present when an installed agent gets another server or key. */
  replacing?: Replacing | null;
  /** Shown when the install sent the window back here. */
  notice?: string | null;
  /** A link the window was opened with (or that arrived while the settings were open). */
  link?: PairingView | null;
  /** The code of that link was spent: the wizard forgets the link. */
  onLinkUsed?: () => void;
  /** Leaves the flow without a change; given when an installed agent is being changed. */
  onCancel?: () => void;
  onNext: (c: Connection) => void;
};

/**
 * Step 1. A pairing link or code first (the dashboard's Connect device button makes it), or an
 * address and a key typed by hand. Nothing is created on the server before the person clicks.
 */
export function StepConnect(props: StepProps) {
  const [byKey, setByKey] = useState(props.initial !== null && props.initial.apiKey !== null);
  return byKey ? (
    <KeyConnect {...props} onSwitch={() => setByKey(false)} />
  ) : (
    <PairConnect {...props} onSwitch={() => setByKey(true)} />
  );
}

/** One field for a link or a code; the confirmation of what it connects comes before anything changes. */
function PairConnect({
  initial,
  replacing = null,
  notice,
  link = null,
  onLinkUsed,
  onCancel,
  onNext,
  onSwitch,
}: StepProps & { onSwitch: () => void }) {
  const { brand } = useBrand();
  const fixedHint = fixedServerHint(brand);
  // A connection that was already paired shows its card again; the link the window was opened with
  // was spent by it and is not seeded a second time.
  const seed = initial?.paired ? null : link;
  // the field takes a link as its code, the address shows in the notice
  const [form, setForm] = useState<PairForm>({
    text: seed?.code ?? "",
    view: seed,
    fromLink: seed !== null,
    typedServer: startServer(initial, replacing, brand),
  });
  const [name, setName] = useState(initial?.name ?? replacing?.name ?? "");
  const [paired, setPaired] = useState<PairedView | null>(initial?.paired ?? null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(notice ?? null);
  const [confirmedUrl, setConfirmedUrl] = useState<string | null>(null);
  const [askInsecure, setAskInsecure] = useState(false);
  // the answer of the newest read wins
  const reading = useRef(0);
  const installed = installedConnection(replacing);

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

  // A link that arrives while this step is open (a click on the link, the system's own hand-over).
  // Not while a code is being spent: the answer belongs to the address it was sent to.
  usePairingLink(
    (l) => {
      reading.current += 1;
      setForm((f) => formFromLink(f, l));
      setPaired(null);
      setAskInsecure(false);
      setError(null);
    },
    paired === null && !busy,
  );

  const fields = pairingFields(form.view);
  const server = formServer(form);
  const ready = fields !== null && server !== "" && name.trim() !== "";

  function onText(value: string) {
    setForm((f) => formEdited(f, value));
    setPaired(null);
    setAskInsecure(false);
    setError(null);
    const n = ++reading.current;
    if (value.trim() === "") return;
    setup.readPairing(value).then(
      (v) => {
        if (n === reading.current) setForm((f) => ({ ...f, view: v }));
      },
      () => {},
    );
  }

  async function connect(insecureOk: boolean) {
    if (!fields) return;
    // the address and the code this answer belongs to
    const target = server;
    setBusy(true);
    setError(null);
    try {
      if (!insecureOk && confirmedUrl !== target && (await setup.needsInsecureConfirm(target))) {
        setAskInsecure(true);
        return;
      }
      setAskInsecure(false);
      const result = await setup.pair(target, fields.code, name.trim(), insecureOk || confirmedUrl === target);
      setPaired(result);
      // the code of the link is spent: coming back to this step must not fill it in again
      onLinkUsed?.();
    } catch (e) {
      setError(errorText(toSetupError(e), { pairing: true, installed: replacing !== null }));
    } finally {
      setBusy(false);
    }
  }

  if (paired) {
    const summary = pairingResult(paired, replacing);
    return (
      <Card>
        <CardHeader>
          <CardTitle>Connect to {summary.account}?</CardTitle>
          <CardDescription>Nothing has changed on this computer or for any device yet.</CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          {/* The whole text, never cut: the end of an address is what tells servers apart. */}
          <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1 text-sm">
            <dt className="text-muted-foreground">Account</dt>
            <dd className="break-all">{summary.account}</dd>
            <dt className="text-muted-foreground">Created by</dt>
            <dd className="break-all">{summary.createdBy}</dd>
            <dt className="text-muted-foreground">Device</dt>
            <dd className="break-all">{summary.device}</dd>
            <dt className="text-muted-foreground">Server</dt>
            <dd className="break-all font-mono">{paired.serverUrl}</dd>
          </dl>
          {summary.createdByWarning && <Notice tone="warn">{summary.createdByWarning}</Notice>}
          {summary.takeover && <Notice tone="warn">{summary.takeover}</Notice>}
          {summary.leaving && <Notice tone="warn">{summary.leaving}</Notice>}
          <Actions>
            <Button
              type="button"
              variant="ghost"
              onClick={() => {
                // the code is spent: the field asks for a new one
                setPaired(null);
                setForm((f) => ({
                  ...f,
                  text: "",
                  view: null,
                  fromLink: false,
                  typedServer: startServer(initial, replacing, brand),
                }));
                setError(null);
              }}
            >
              Use another code
            </Button>
            <Button
              type="button"
              variant="brand"
              onClick={() => onNext({ serverUrl: paired.serverUrl, apiKey: null, name: paired.name, paired })}
            >
              Continue
            </Button>
          </Actions>
        </CardContent>
      </Card>
    );
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Connect to your server</CardTitle>
        <CardDescription>
          On the Devices page of your dashboard, click Connect device and paste the link here.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <form
          className="space-y-3"
          onSubmit={(e) => {
            e.preventDefault();
            if (ready && !busy) void connect(false);
          }}
        >
          <div>
            <FieldLabel htmlFor="pairing">Pairing link or code</FieldLabel>
            <Input
              id="pairing"
              className="mt-1.5 font-mono"
              placeholder="printpapi://pair?… or 7K3M-Q2ZD-9XHT"
              autoComplete="off"
              spellCheck={false}
              disabled={busy}
              value={form.text}
              onChange={(e) => onText(e.target.value)}
            />
          </div>
          {fields?.askServer && (
            <div>
              <FieldLabel htmlFor="server">Server address</FieldLabel>
              <Input
                id="server"
                className="mt-1.5 font-mono"
                placeholder="https://print.example.com"
                autoComplete="off"
                disabled={busy}
                readOnly={fixedHint !== null}
                value={form.typedServer}
                onChange={(e) => {
                  setForm((f) => ({ ...f, typedServer: e.target.value }));
                  setAskInsecure(false);
                  setError(null);
                }}
              />
              {fixedHint && <p className="mt-1 text-xs text-muted-foreground">{fixedHint}</p>}
            </div>
          )}
          <div>
            <FieldLabel htmlFor="name">Name of this device</FieldLabel>
            <Input
              id="name"
              className="mt-1.5"
              autoComplete="off"
              disabled={busy}
              value={name}
              onChange={(e) => {
                setName(e.target.value);
                setError(null);
              }}
            />
          </div>

          {ready && form.fromLink && (
            <Notice tone="warn">
              <p>
                A link wants to connect this computer to <strong className="break-all font-mono">{server}</strong>.
                Continue only if you created it yourself in that dashboard.
              </p>
              {installed && <p className="mt-1 break-all">It is connected to {installed.serverUrl} now.</p>}
            </Notice>
          )}
          {ready && !form.fromLink && (
            <Notice tone="info">
              <p>
                This connects the computer to <strong className="break-all font-mono">{server}</strong>.
              </p>
              {installed && <p className="mt-1 break-all">It is connected to {installed.serverUrl} now.</p>}
            </Notice>
          )}
          {askInsecure && (
            <Notice tone="warn">
              <p>This address is not encrypted. The code and the key would travel readable over the network.</p>
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="mt-2"
                onClick={() => {
                  setConfirmedUrl(server);
                  void connect(true);
                }}
              >
                Use it anyway
              </Button>
            </Notice>
          )}
          {error && <Notice tone="error">{error}</Notice>}

          <Actions>
            {onCancel && (
              <Button type="button" variant="ghost" disabled={busy} onClick={onCancel}>
                Cancel
              </Button>
            )}
            <Button type="button" variant="ghost" disabled={busy} onClick={onSwitch}>
              <KeyRound />
              Use an address and a key
            </Button>
            <Button type="submit" variant="brand" disabled={!ready || busy || askInsecure}>
              {busy ? <Loader2 className="animate-spin" /> : <Plug />}
              Connect
            </Button>
          </Actions>
        </form>
      </CardContent>
    </Card>
  );
}

/** The address and key form: a key made on the Keys page, typed here. */
function KeyConnect({
  initial,
  replacing = null,
  notice,
  onCancel,
  onNext,
  onSwitch,
}: StepProps & { onSwitch: () => void }) {
  const { brand } = useBrand();
  const fixedHint = fixedServerHint(brand);
  const [serverUrl, setServerUrl] = useState(startServer(initial, replacing, brand));
  const [apiKey, setApiKey] = useState(initial?.apiKey ?? "");
  const [name, setName] = useState(initial?.name ?? replacing?.name ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(notice ?? null);
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

  // the server ties a name to one key: the same server needs another name
  const sameServer = replacing !== null && needsNewName(replacing.serverUrl, serverUrl);
  const keepsName = keepsOldName(replacing, serverUrl, name);
  const filled = serverUrl.trim() !== "" && apiKey.trim() !== "" && name.trim() !== "" && !keepsName;
  const current = (): Connection & { apiKey: string } => ({
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
          Create one key per device on the Keys page of your dashboard, with the rights Agent only if it offers them.
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
              readOnly={fixedHint !== null}
              value={serverUrl}
              onChange={edit(setServerUrl)}
            />
            {fixedHint && <p className="mt-1 text-xs text-muted-foreground">{fixedHint}</p>}
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

          {sameServer && replacing.keyUnreadable && (
            <Notice tone="warn">
              <p>{KEY_UNREADABLE_NOTICE}</p>
            </Notice>
          )}
          {sameServer && !replacing.keyUnreadable && (
            <Notice tone="warn">
              <p>{SAME_SERVER_WARNING}</p>
              {keepsName && <p className="mt-1 font-medium">Pick a name other than {replacing.name}.</p>}
            </Notice>
          )}
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
            {onCancel && (
              <Button type="button" variant="ghost" disabled={busy} onClick={onCancel}>
                Cancel
              </Button>
            )}
            <Button type="button" variant="ghost" disabled={busy} onClick={onSwitch}>
              Use a pairing link or code
            </Button>
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
