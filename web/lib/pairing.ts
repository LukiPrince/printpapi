// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The rules behind the Connect device dialog, free of React so they can be tested.
import type { PairingInfo, PairingState } from "./api";

/**
 * The link the setup app opens with (`printpapi://`, registered by its install). Parsed only by
 * `pair::parse` in crates/agent/src/pair.rs: both tests hold the same golden value.
 */
export function pairingLink(server: string, code: string): string {
  return `printpapi://pair?server=${encodeURIComponent(server)}&code=${encodeURIComponent(code)}`;
}

/** An address that only reaches this computer: another computer cannot use it to connect. */
export function isLocalOrigin(origin: string): boolean {
  let host: string;
  try {
    host = new URL(origin).hostname.toLowerCase();
  } catch {
    return true; // no address at all: nothing to hand out
  }
  host = host.replace(/^\[|\]$/g, "");
  return (
    host === "localhost" ||
    host.endsWith(".localhost") ||
    host === "::1" ||
    host === "0.0.0.0" ||
    /^127\.\d+\.\d+\.\d+$/.test(host)
  );
}

/**
 * The address a pairing link names: the one the dashboard is opened under (what the Downloads page
 * writes into agent.ini), and only when that is a local address, the server's own PUBLIC_URL.
 * `null`: no address that works for another computer, the dialog says so.
 */
export function pairingServer(origin: string, serverUrl: string | null): string | null {
  return isLocalOrigin(origin) ? serverUrl : origin;
}

/** The word of each state: shown with an icon and a colour, never colour alone. */
export const PAIRING_WORD: Record<PairingState, string> = {
  open: "Waiting",
  expired: "Expired",
  fetched: "Key fetched",
  registered: "Ready",
  connected: "Connected",
  revoked: "Revoked",
};

/** Rows the page keeps above the device list: an open code, a key somebody may have fetched, a device waiting for its agent, an old key that still works. */
export function needsAttention(p: Pick<PairingInfo, "state" | "old_key">): boolean {
  return p.state === "open" || p.state === "fetched" || p.state === "registered" || (p.state === "connected" && p.old_key !== null);
}

/**
 * A code the dialog showed that the list no longer holds, after it had held it: a newer code for
 * the same device replaced it, somebody cancelled it elsewhere, or a newer takeover ended it. Before
 * the first listing it is not gone, only not listed yet.
 */
export function codeWentAway(rows: Pick<PairingInfo, "id">[] | undefined, id: number, wasListed: boolean): boolean {
  return wasListed && rows !== undefined && !rows.some((r) => r.id === id);
}

/** The commands of the Terminal tab: the agent's folder, with administrator rights where it needs them. */
export const TERMINAL_COMMANDS = {
  unix: "./printpapi-agent pair",
  windows: ".\\printpapi-agent.exe pair",
} as const;
