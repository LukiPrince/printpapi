"use client";

import { createContext, useContext, useState } from "react";
import { toast } from "sonner";
import { CircleCheck, Clock, KeyRound, Loader2, Plug, ShieldOff, TriangleAlert, XCircle } from "lucide-react";
import { usePoll } from "@/hooks/use-poll";
import { useClientValue } from "@/hooks/use-client-value";
import {
  cancelPairing,
  createPairing,
  listPairings,
  revokeApiKey,
  type NewPairing,
  type PairingInfo,
  type PairingState,
} from "@/lib/api";
import { fmtTime } from "@/lib/format";
import { PAIRING_WORD, TERMINAL_COMMANDS, codeWentAway, needsAttention, pairingLink, pairingServer } from "@/lib/pairing";
import { DEMO_SERVER_URL } from "printpapi-demo";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
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
import { CopyButton } from "@/components/copy-button";

type Target = { id: number; name: string };
type Connect = { start: (device?: Target) => void; creating: boolean };

const ConnectContext = createContext<Connect | null>(null);

/** The way to start a code from anywhere on the page; `null` for a login that may not (a key). */
export function useConnectDevice(): Connect | null {
  return useContext(ConnectContext);
}

const LOOK: Record<PairingState, { icon: typeof Clock; color: string }> = {
  open: { icon: Clock, color: "var(--state-claimed)" },
  expired: { icon: XCircle, color: "var(--muted-foreground)" },
  fetched: { icon: TriangleAlert, color: "var(--state-queued)" },
  registered: { icon: CircleCheck, color: "var(--state-claimed)" },
  connected: { icon: CircleCheck, color: "var(--state-done)" },
  revoked: { icon: XCircle, color: "var(--state-failed)" },
};

/** A state as colour, icon and word. */
function StateWord({ state }: { state: PairingState }) {
  const { icon: Icon, color } = LOOK[state];
  return (
    <span className="inline-flex items-center gap-1.5 text-sm font-medium" style={{ color }}>
      <Icon className="size-4 shrink-0" />
      {PAIRING_WORD[state]}
    </span>
  );
}

/** A key's revocation, asked once. */
function RevokeButton({
  keyId,
  label,
  onDone,
  children,
  what,
}: {
  keyId: number;
  label: string;
  onDone: () => void;
  children: React.ReactNode;
  what: string;
}) {
  async function revoke() {
    try {
      await revokeApiKey(keyId);
      toast.success(`Revoked "${label}"`);
      onDone();
    } catch (err) {
      toast.error("Could not revoke", { description: err instanceof Error ? err.message : String(err) });
    }
  }
  return (
    <AlertDialog>
      <AlertDialogTrigger asChild>
        <Button variant="destructive" size="xs">
          <ShieldOff /> {children}
        </Button>
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Revoke “{label}”?</AlertDialogTitle>
          <AlertDialogDescription>
            {what} Anything using this key stops working immediately. This cannot be undone.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Keep it</AlertDialogCancel>
          <AlertDialogAction onClick={revoke}>Revoke</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

/** The key of a fetched or registered code is named "Device <name>" by the server. */
const deviceLabel = (p: PairingInfo) => `Device ${p.device_name ?? p.computer_name ?? ""}`.trim();

/** What a row says, and the action that goes with it. */
function RowStatus({ row, onChanged }: { row: PairingInfo; onChanged: () => void }) {
  const name = row.device_name ?? row.computer_name ?? "the device";
  return (
    <div className="space-y-2">
      <StateWord state={row.state} />
      {row.state === "fetched" && (
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <span>
            A machine fetched the key for <span className="font-mono">{name}</span> at {fmtTime(row.used_at)}. Not you?
          </span>
          {row.key_id !== null && (
            <RevokeButton keyId={row.key_id} label={deviceLabel(row)} onDone={onChanged} what="The key it fetched stops working.">
              Revoke
            </RevokeButton>
          )}
        </div>
      )}
      {row.state === "registered" && (
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <span>Ready. It takes over when its agent starts.</span>
          {row.key_id !== null && (
            <RevokeButton keyId={row.key_id} label={deviceLabel(row)} onDone={onChanged} what="The new computer will not take over this device.">
              Revoke
            </RevokeButton>
          )}
        </div>
      )}
      {row.state === "connected" && (
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <span>
            <span className="font-mono">{name}</span> is connected.
          </span>
          {row.old_key && (
            <RevokeButton keyId={row.old_key.id} label={row.old_key.label} onDone={onChanged} what="It was the key of this device before.">
              Revoke old key
            </RevokeButton>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * The Connect device button, the dialog it opens and the one line per code that needs attention.
 * Rendered only for a login that may make codes (root or an account): a machine key gets a 401
 * from /pairing, and the poll inside would ask at once.
 */
export function ConnectDeviceProvider({ children }: { children: React.ReactNode }) {
  const [shown, setShown] = useState<{ code: NewPairing; target: Target | null } | null>(null);
  const [creating, setCreating] = useState(false);
  const { data: rows, refresh } = usePoll(listPairings, shown ? 3000 : 8000);

  async function start(device?: Target) {
    setCreating(true);
    try {
      const code = await createPairing(device?.id);
      setShown({ code, target: device ?? null });
      refresh();
    } catch (err) {
      toast.error("Could not make a pairing code", { description: err instanceof Error ? err.message : String(err) });
    } finally {
      setCreating(false);
    }
  }

  const current = shown ? rows?.find((r) => r.id === shown.code.id) : undefined;
  // Once the list has held the code, a missing row means a newer code replaced it or it was
  // cancelled elsewhere: the dialog must not go on saying "Waiting".
  const [listedId, setListedId] = useState<number | null>(null);
  if (shown && current && listedId !== shown.code.id) setListedId(shown.code.id);
  const gone = shown ? codeWentAway(rows, shown.code.id, listedId === shown.code.id) : false;
  const attention = (rows ?? []).filter(needsAttention);

  return (
    <ConnectContext.Provider value={{ start: (d) => void start(d), creating }}>
      {!shown && attention.length > 0 && (
        <ul className="mb-4 space-y-2">
          {attention.map((row) => (
            <li key={row.id} className="rounded-lg border bg-card px-3 py-2">
              {row.state === "open" ? (
                <div className="flex flex-wrap items-center gap-2 text-sm">
                  <StateWord state={row.state} />
                  <span>
                    Code for <span className="font-mono">{row.computer_name ?? "a new device"}</span> valid until{" "}
                    {fmtTime(row.expires_at)}.
                  </span>
                  <Button
                    variant="ghost"
                    size="xs"
                    onClick={async () => {
                      try {
                        await cancelPairing(row.id);
                      } catch {
                        // already gone: the list shows it
                      }
                      refresh();
                    }}
                  >
                    Cancel
                  </Button>
                </div>
              ) : (
                <RowStatus row={row} onChanged={refresh} />
              )}
            </li>
          ))}
        </ul>
      )}
      {children}
      {shown && (
        <ConnectDialog
          code={shown.code}
          target={shown.target}
          row={current}
          gone={gone}
          onChanged={refresh}
          onClose={() => setShown(null)}
          onNew={() => void start(shown.target ?? undefined)}
        />
      )}
    </ConnectContext.Provider>
  );
}

function ConnectDialog({
  code,
  target,
  row,
  gone,
  onChanged,
  onClose,
  onNew,
}: {
  code: NewPairing;
  target: Target | null;
  /** The code as the server lists it now; absent for the first moment. */
  row: PairingInfo | undefined;
  /** The list held the code and holds it no more: replaced or cancelled elsewhere. */
  gone: boolean;
  onChanged: () => void;
  onClose: () => void;
  onNew: () => void;
}) {
  const origin = useClientValue(() => DEMO_SERVER_URL ?? window.location.origin, "http://localhost:3460");
  const server = pairingServer(origin, code.server_url);
  const link = server ? pairingLink(server, code.code) : null;
  // the server's own word: its list says expired as soon as the code has run out
  const state = gone ? "gone" : (row?.state ?? "open");

  // Closing the dialog (Escape, the cross, a click outside) leaves the code alive: it shows above
  // the list with its own Cancel. Only the button cancels.
  async function cancel() {
    try {
      if (state === "open") await cancelPairing(code.id);
    } catch {
      // gone already
    }
    onChanged();
    onClose();
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{target ? `Reconnect ${target.name}` : "Connect a device"}</DialogTitle>
          <DialogDescription>Valid until {fmtTime(code.expires_at)}. Works once.</DialogDescription>
        </DialogHeader>

        {target && (
          <p className="text-sm text-muted-foreground">
            The device keeps its name and printers. The new key takes over when the new agent starts; until then
            nothing changes.{" "}
            {code.old_key
              ? `Its current key "${code.old_key.label}" can also print and read jobs and keeps working for that. Revoke it once the device is connected.`
              : "The key it uses now stops working then."}
          </p>
        )}

        {server === null ? (
          <p className="rounded-lg border border-dashed p-3 text-sm">
            This address only works on this computer. Open the dashboard under its network address, or set
            PUBLIC_URL.
          </p>
        ) : (
          <Tabs defaultValue="app" className="min-w-0">
            <TabsList>
              <TabsTrigger value="app">Setup app</TabsTrigger>
              <TabsTrigger value="terminal">Terminal</TabsTrigger>
            </TabsList>
            <TabsContent value="app" className="min-w-0 space-y-3 pt-2">
              <p className="text-sm text-muted-foreground">
                Paste this link into the setup app, or open it on the computer.
              </p>
              <div className="flex items-center gap-2">
                <code className="min-w-0 flex-1 rounded bg-muted px-2 py-1.5 font-mono text-xs break-all">{link}</code>
                <CopyButton value={link ?? ""} />
              </div>
              <p className="text-sm">
                Or enter the code with this address.
                <span className="mt-1 block font-mono text-base tracking-wider">{code.code}</span>
                <span className="block font-mono text-xs text-muted-foreground">{server}</span>
              </p>
              <div className="flex flex-wrap items-center gap-2">
                <Button asChild variant="outline" size="sm">
                  <a href={link ?? undefined}>
                    <Plug /> Open on this computer
                  </a>
                </Button>
                <span className="text-xs text-muted-foreground">Works where printpapi is already installed.</span>
              </div>
            </TabsContent>
            <TabsContent value="terminal" className="min-w-0 space-y-3 pt-2">
              <pre className="max-w-full overflow-x-auto rounded bg-muted p-3 font-mono text-xs">
                {TERMINAL_COMMANDS.unix}
                {"  # Linux, macOS\n"}
                {TERMINAL_COMMANDS.windows}
                {"  # Windows, administrator prompt"}
              </pre>
              <p className="text-sm text-muted-foreground">
                Run it in the agent&apos;s folder (the setup&apos;s install: /opt/printpapi-agent,
                C:\Program Files\printpapi-agent), with sudo where that folder is not yours. Paste the link when it
                asks. It needs an agent as new as this server; an older one just starts.
              </p>
              <div className="flex items-center gap-2">
                <code className="min-w-0 flex-1 rounded bg-muted px-2 py-1.5 font-mono text-xs break-all">{link}</code>
                <CopyButton value={link ?? ""} />
              </div>
            </TabsContent>
          </Tabs>
        )}

        <div className="rounded-lg border px-3 py-2">
          {gone ? (
            <>
              <span
                className="inline-flex items-center gap-1.5 text-sm font-medium"
                style={{ color: "var(--muted-foreground)" }}
              >
                <XCircle className="size-4 shrink-0" />
                Replaced or cancelled
              </span>
              <p className="mt-1 text-xs text-muted-foreground">
                A newer code for this device replaced it, or it was cancelled elsewhere. It no longer works.
              </p>
            </>
          ) : row ? (
            <RowStatus row={row} onChanged={onChanged} />
          ) : (
            <StateWord state="open" />
          )}
          {state === "open" && (
            <p className="mt-1 text-xs text-muted-foreground">Waiting for a computer to use the code.</p>
          )}
          {(state === "expired" || state === "gone") && (
            <Button variant="outline" size="xs" className="mt-2" onClick={onNew}>
              <KeyRound /> New code
            </Button>
          )}
        </div>

        <DialogFooter>
          <Button variant="ghost" onClick={() => void cancel()}>
            {state === "open" ? "Cancel code" : "Close"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** The button above the list and in the empty state. */
export function ConnectDeviceButton({ className }: { className?: string }) {
  const connect = useConnectDevice();
  if (!connect) return null;
  return (
    <Button variant="brand" size="sm" className={cn(className)} disabled={connect.creating} onClick={() => connect.start()}>
      {connect.creating ? <Loader2 className="animate-spin" /> : <Plug />}
      Connect device
    </Button>
  );
}

/** Reconnect on a device section: a code that keeps its name and printers. */
export function ReconnectButton({ device }: { device: Target }) {
  const connect = useConnectDevice();
  if (!connect) return null;
  return (
    <Button
      variant="ghost"
      size="xs"
      className="ml-auto"
      disabled={connect.creating}
      onClick={() => connect.start(device)}
    >
      <Plug /> Reconnect
    </Button>
  );
}
