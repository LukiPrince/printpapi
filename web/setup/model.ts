// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The setup window's rules, free of React and of Tauri so they can be tested.
import type { AgentStatus, ConfigPatch, FoundPrinter, PairedView, PairingView, Platform, PrinterChoice, SetupError, UpdateInfo, UpdateMode } from "./api";

/** `printers` in agent.ini is `name[=target][|pdf]` joined by `;`, so these cannot appear in a name. */
const NAME_FORBIDDEN = /[;=|]/;
const HOST = /^[A-Za-z0-9.-]+$/;

export const DEFAULT_RAW_PORT = 9100;

export function fromFound(found: FoundPrinter[]): PrinterChoice[] {
  return found.map((f) => ({ name: f.name, pdf: false, target: null, locked: false }));
}

/** At least one printer must stay chosen. A machine with none found can add a network printer. */
export function canLeavePrinters(list: PrinterChoice[]): boolean {
  return list.length > 0;
}

/** A network printer is `name = socket://ip:port` and always raw. Throws a readable Error. */
export function addNetworkPrinter(
  list: PrinterChoice[],
  name: string,
  ip: string,
  port: number,
): PrinterChoice[] {
  const n = name.trim();
  const host = ip.trim();
  if (!n) throw new Error("Enter a printer name.");
  if (NAME_FORBIDDEN.test(n)) throw new Error('A printer name cannot contain ";", "=" or "|".');
  if (list.some((p) => p.name === n)) throw new Error(`A printer named "${n}" is already in the list.`);
  if (!HOST.test(host)) throw new Error("Enter the printer's IP address or host name.");
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error("The port must be a number from 1 to 65535.");
  }
  return [...list, { name: n, pdf: false, target: `socket://${host}:${port}`, locked: false }];
}

/** SumatraPDF prints PDFs on Windows only. */
export function needsSumatra(platform: Platform, list: PrinterChoice[], present: boolean): boolean {
  return platform === "windows" && !present && list.some((p) => p.pdf);
}

export function canTest(p: PrinterChoice, platform: Platform, sumatraPresent: boolean): boolean {
  if (p.locked) return false;
  return !(p.pdf && platform === "windows" && !sumatraPresent);
}

/**
 * The text for a failed command. A proxy refusal gets the way around it, the rest speak for
 * themselves. With a pairing and an installed agent the way around is the command line, which
 * sends the headers of the installed agent (the window never holds their values).
 */
export function errorText(e: SetupError, context: { pairing?: boolean; installed?: boolean } = {}): string {
  if (e.code === "proxy") {
    if (context.pairing && context.installed) {
      return "A proxy in front of the server refused this. Run printpapi-agent pair on this computer; it sends the headers of the installed agent.";
    }
    return "A proxy in front of the server refused this. Set up the agent by hand with [headers], see docs/agent.md.";
  }
  return e.message;
}

/**
 * What the pairing field shows for what the shell read. A link names its server, so the Server
 * address field is hidden; a bare code needs the address typed. `null`: not a link or code.
 */
export function pairingFields(view: PairingView | null): { code: string; server: string | null; askServer: boolean } | null {
  if (view === null) return null;
  return { code: view.code, server: view.serverUrl, askServer: view.serverUrl === null };
}

/**
 * What the Server address field starts with: what the person typed before, else the installed
 * agent's address. The connection a pairing made names the server of a link, so it never seeds a
 * field: coming back to the step and typing another code must not inherit that address.
 */
export function startServer(
  initial: { serverUrl: string; paired?: unknown } | null,
  replacing: { serverUrl: string } | null,
): string {
  if (initial && !initial.paired) return initial.serverUrl;
  return replacing?.serverUrl ?? "";
}

/** What the second confirmation shows, from what the server said about the code. */
export type PairingSummary = {
  account: string;
  createdBy: string;
  /** Set when the address is not a bare e-mail address (punctuation around it, characters outside plain ASCII): it can look like another one. */
  createdByWarning: string | null;
  device: string;
  /** Set when the code takes an existing device over. */
  takeover: string | null;
  /** Set when this computer is connected to something else now and would leave it. */
  leaving: string | null;
};

/**
 * The lines of the confirmation after a code was redeemed and before anything changes. `replacing`
 * is the connection of the installed agent, when there is one: leaving it is a warning, unless this
 * is the same device on the same server (a Reconnect of this very computer).
 */
export function pairingResult(
  paired: PairedView,
  replacing: { serverUrl: string; name: string } | null,
): PairingSummary {
  // An agent that has not run for two minutes has no status file: its server and name read as "".
  // Nothing may be said about what is not known.
  const installed = installedConnection(replacing);
  const reconnectOfThis =
    paired.replaces &&
    (replacing === null ||
      installed === null ||
      (paired.name === installed.name && sameOrigin(installed.serverUrl, paired.serverUrl)));
  return {
    account: paired.org,
    createdBy: paired.createdBy ?? "the server administrator",
    createdByWarning: paired.createdByEscaped
      ? "This is not a plain e-mail address. It has punctuation around it or characters outside plain ASCII (those are spelled out as \\u{...}), so it can imitate another address. Continue only if it is the address you expect."
      : null,
    device: paired.name,
    takeover: paired.replaces
      ? `This takes over the device ${paired.name}. If it runs on another computer, that one stops printing when this one starts.`
      : null,
    leaving: leavingText(paired, installed, replacing !== null, reconnectOfThis),
  };
}

function leavingText(
  paired: PairedView,
  installed: { serverUrl: string; name: string } | null,
  agentIsInstalled: boolean,
  reconnectOfThis: boolean,
): string | null {
  if (!agentIsInstalled || reconnectOfThis) return null;
  if (installed !== null) {
    return `This computer leaves ${installed.serverUrl}, device ${installed.name}. Its printers get new IDs, and the old device stays in that dashboard.`;
  }
  // an agent is installed but nothing is known of its connection: a Reconnect of it says nothing
  // more, a new device cannot keep the printer IDs of whatever it was
  return paired.replaces
    ? null
    : "This computer leaves the connection it has now. Its printers get new IDs, and the old device stays in that dashboard.";
}

/** The connection of an installed agent, or null when its server or its name is not known. */
export function installedConnection(
  replacing: { serverUrl: string; name: string } | null,
): { serverUrl: string; name: string } | null {
  if (replacing === null || replacing.serverUrl.trim() === "" || replacing.name.trim() === "") return null;
  return replacing;
}

/**
 * The same server the way the agent compares them (`setup::same_origin`): scheme, host in lower
 * case and port, with the default port of the scheme, whatever the path. Anything unreadable is not
 * the same.
 */
export function sameOrigin(a: string, b: string): boolean {
  try {
    const [x, y] = [new URL(a.trim()), new URL(b.trim())];
    return (x.protocol === "http:" || x.protocol === "https:") && x.origin === y.origin;
  } catch {
    return false;
  }
}

/** What the pairing form holds: the field's text, what the shell read from it, and the typed address. */
export type PairForm = { text: string; view: PairingView | null; fromLink: boolean; typedServer: string };

/**
 * A link arrives. It carries its own server in `view`; the field the person types an address
 * into is not touched, so a bare code typed afterwards never inherits the address of a link.
 */
export function formFromLink(form: PairForm, link: PairingView): PairForm {
  return { ...form, text: link.code, view: link, fromLink: true };
}

/** The text is edited: what was read from the old text is gone until the new text is read. */
export function formEdited(form: PairForm, text: string): PairForm {
  return { ...form, text, view: null, fromLink: false };
}

/** The address Connect would use: the link's own, else the typed one. */
export function formServer(form: PairForm): string {
  return (pairingFields(form.view)?.server ?? form.typedServer).trim();
}

/**
 * Failures of the install's own register call that only another key or name fixes. The window goes
 * back to step 1 for them (a proxy, the network or the machine are not fixed there).
 */
export function returnsToConnect(code: string): boolean {
  return code === "key_in_use" || code === "name_taken";
}

/**
 * Whether the finish step offers "Change server or key". Only a failure that another address, key
 * or name fixes does: an unreachable server, a key that is taken, a name that belongs to another key.
 */
export function connectMayHelp(code: string): boolean {
  return code === "unreachable" || returnsToConnect(code);
}

/**
 * A printer connected for one user only is invisible to the agent (a service), so it cannot be
 * switched on. One that is already on can still be switched off.
 */
export function canSwitchOn(found: FoundPrinter | null, on: boolean): boolean {
  if (on) return true;
  return found === null || (found.usable && !found.per_user);
}

/** Where the installed agent writes its crash log, next to its status file; mirrors `service_log_path` in crates/agent/src/install.rs. */
export function logLocation(platform: Platform): string {
  switch (platform) {
    case "windows":
      return "C:\\ProgramData\\printpapi-agent\\print_agent-error.log";
    case "macos":
      return "/Library/Application Support/printpapi-agent/print_agent-error.log";
    case "linux":
      return "/var/lib/printpapi-agent/print_agent-error.log";
  }
}

export function statusWord(state: AgentStatus["state"]): string {
  switch (state) {
    case "online":
      return "Connected";
    case "offline":
      return "Offline";
    case "not_running":
      return "Not running";
  }
}

/**
 * The "Last job" line of the overview. A missing job reads "none yet" only when the agent runs; an
 * agent that is not running has reported nothing, so the value is "—" like the other fields.
 */
export function lastJobText(
  state: AgentStatus["state"],
  job: AgentStatus["lastJob"],
  ago: (ts: number) => string,
): string {
  if (job) return `#${job.id} ${job.ok ? "printed" : "failed"}, ${ago(job.at)}`;
  return state === "not_running" ? "—" : "none yet";
}

/** A path in pieces that may be broken between, each ending in its separator, so a file name stays whole. */
export function pathParts(path: string): string[] {
  return path.match(/[^\\/]+[\\/]?|[\\/]/g) ?? [];
}

/** An address the way the agent reads it: no space, no slash at the end, the host in lower case. */
function normalizedServer(url: string): string {
  return url.trim().replace(/\/+$/, "").toLowerCase();
}

/**
 * The server binds a device name and a key to each other, so a new key on the same server cannot
 * keep the name. On another server the device is new there and the name can stay. With no old
 * server known (no status file yet) there is nothing to compare.
 */
export function needsNewName(oldServer: string, newServer: string): boolean {
  const old = normalizedServer(oldServer);
  return old !== "" && old === normalizedServer(newServer);
}

/**
 * Does the person keep the name of the device they are replacing the connection of, on the same
 * server? The server binds the name to the old key, so that needs a new name, unless the stored key
 * cannot be read: then the same key and name again is the repair, and the server stays the judge.
 */
export function keepsOldName(
  replacing: { serverUrl: string; name: string; keyUnreadable?: boolean } | null,
  serverUrl: string,
  name: string,
): boolean {
  if (replacing === null || replacing.keyUnreadable) return false;
  return needsNewName(replacing.serverUrl, serverUrl) && name.trim() === replacing.name;
}

/** The timeout the agent accepts: at least `MIN_TIMEOUT` of crates/agent/src/config.rs, in seconds. */
const MIN_TIMEOUT = 30;

/** What is wrong with the text of a timeout field, or null. */
export function timeoutProblem(text: string): string | null {
  const t = text.trim();
  if (!/^\d+(\.\d+)?$/.test(t)) return "Enter the timeout in seconds, for example 60.";
  if (Number(t) < MIN_TIMEOUT) {
    return `The timeout must be at least ${MIN_TIMEOUT} seconds: the server holds a request for up to 25.`;
  }
  return null;
}

/** What can be changed in an installed agent. A header value of null is one that is not known. */
export type Settings = {
  printers: PrinterChoice[];
  timeout: number;
  headers: [string, string | null][];
};

const printerKey = (list: PrinterChoice[]) => JSON.stringify(list.map((p) => [p.name, p.pdf, p.target]));

/**
 * What differs between two states, as the patch the agent applies to its `agent.ini`: only the
 * fields that both sides know and that changed. A header is set when it is new or has another value
 * and removed (null) when it is gone; one whose value was never shown (null) stays as it is unless
 * a value was typed for it. Header names do not differ by case.
 */
export function patchFor(before: Partial<Settings>, after: Partial<Settings>): ConfigPatch {
  const patch: ConfigPatch = {};
  if (before.printers && after.printers && printerKey(before.printers) !== printerKey(after.printers)) {
    patch.printers = after.printers;
  }
  if (before.timeout !== undefined && after.timeout !== undefined && before.timeout !== after.timeout) {
    patch.timeout = after.timeout;
  }
  if (before.headers && after.headers) {
    const old = new Map(before.headers.map(([n, v]) => [n.toLowerCase(), v] as const));
    const now = new Set(after.headers.map(([n]) => n.toLowerCase()));
    const changes: [string, string | null][] = [];
    for (const [name, value] of after.headers) {
      if (value !== null && old.get(name.toLowerCase()) !== value) changes.push([name, value]);
    }
    for (const [name] of before.headers) {
      if (!now.has(name.toLowerCase())) changes.push([name, null]);
    }
    if (changes.length > 0) patch.headers = changes;
  }
  return patch;
}

/** `a` is a higher N.N.N than `b` (3.10.0 is newer than 3.9.9). Anything that is no version is never newer. */
export function isNewer(a: string | null | undefined, b: string | null | undefined): boolean {
  const parse = (v: string | null | undefined) => {
    const m = /^(\d+)\.(\d+)\.(\d+)$/.exec(v ?? "");
    return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : null;
  };
  const x = parse(a);
  const y = parse(b);
  if (!x || !y) return false;
  for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] > y[i];
  return false;
}

/** The version that counts as "installed" for comparing: the agent's, else this program's. */
const current = (info: UpdateInfo) => info.installed ?? info.own;

/** A newer version than the installed one was found. */
export function updateAvailable(info: UpdateInfo): boolean {
  return isNewer(info.latest, current(info));
}

/** The installed agent is newer than this program: the window offers the installed one instead of its sections. */
export function installedIsNewer(info: UpdateInfo): boolean {
  return isNewer(info.installed, info.own);
}

/** The installed agent is older than this program: "Install X from this setup" is offered. */
export function ownIsNewer(info: UpdateInfo): boolean {
  return isNewer(info.own, info.installed);
}

/** The one sentence under the title of Updates. */
export function updateLine(info: UpdateInfo, ago: (ts: number | null) => string): string {
  if (!info.supported) return "This build has no update key, so it cannot check for updates.";
  if (info.mode === "off") {
    return "Checking is switched off in agent.ini (updates = off, or an update_url that is not a usable folder).";
  }
  if (info.mode === "unknown") {
    return "The agent has not said how it is set up: it is stopped, or older than this program. Start it, or install this program's version of it.";
  }
  const tried = info.lastAttempt;
  if (tried && !tried.ok && tried.version === info.latest && updateAvailable(info)) {
    // the stored sentence is the whole story ("Version 3.8.0 did not connect, so 3.7.0 was put back. ...")
    return tried.message.startsWith(`Version ${tried.version}`)
      ? tried.message
      : `Version ${tried.version} was not installed: ${tried.message}`;
  }
  if (updateAvailable(info) && info.latest) return `Version ${info.latest} is available.`;
  if (info.error) return info.error;
  if (info.checkedAt !== null) return `Up to date. Checked ${ago(info.checkedAt)}.`;
  return "Not checked yet.";
}

/** The second line, for the mode that installs on its own. */
export function autoLine(mode: UpdateInfo["mode"]): string | null {
  return mode === "auto" ? "Installed automatically, usually at night." : null;
}

/** The words of the three modes, as the select shows them. */
export const MODES: { value: UpdateMode; label: string }[] = [
  { value: "notify", label: "Tell me" },
  { value: "auto", label: "Install automatically" },
  { value: "off", label: "Off" },
];

/** The patch of a change of mode: nothing when it is the mode already. */
export function modePatch(before: UpdateInfo["mode"], after: UpdateMode): ConfigPatch | null {
  return before === after ? null : { updates: after };
}

/**
 * "Go back to X" leaves the unattended mode, or the nightly update would install the newer version
 * again at once. A stopped agent does not say its mode (the file may hold auto, or off, which a
 * company chose): the install reads the file and lowers only an automatic setting.
 */
export function goBackPatch(mode: UpdateInfo["mode"]): ConfigPatch {
  if (mode === "auto") return { updates: "notify", goBack: true };
  if (mode === "unknown") return { updates: "notify-if-auto", goBack: true };
  return { goBack: true };
}

/** The sentence of the confirmation that says what happens to the update mode, if anything. */
export function goBackNote(mode: UpdateInfo["mode"]): string | null {
  const why = "or the agent would install the newer version again at night.";
  if (mode === "auto") return `Updates are switched to "Tell me", ${why}`;
  if (mode === "unknown") {
    return `If updates are set to "Install automatically", they are switched to "Tell me", ${why} Any other setting stays as it is.`;
  }
  return null;
}

/** What keeps one screen of the settings window in place while an install that it started runs. */
export type Hold = "card" | "section" | null;

/**
 * Whether the card for a newer installed agent replaces the sections. While Go back runs (the card
 * is held) it stays, whatever the marker says half way through: its notice is the only place that
 * says how it ended. While an install from Updates runs the sections stay: the marker is new
 * before that install has finished.
 */
export function olderCardShown(info: UpdateInfo | null, installedHere: boolean, hold: Hold): boolean {
  if (hold === "card") return true;
  if (hold === "section") return false;
  return info !== null && installedIsNewer(info) && !installedHere;
}

/** The mode was changed to one that looks, and the agent still says off: its update_url is no folder. */
export function modeStillOff(asked: UpdateMode, now: UpdateInfo["mode"]): boolean {
  return asked !== "off" && now === "off";
}

export const UPDATE_URL_TEXT =
  "Saved, but the agent still reads updates as off. update_url in agent.ini has to be an http:// or https:// folder (no user name, no password, no ? and no #): correct it there, or remove the line.";

/** The sentence for the agent that must be connected before an update (also the reason the button is off). */
export const NEEDS_CONNECTED =
  "The agent has to be connected, so that the update can check the new version. Otherwise run the setup of the new version.";

/** The text of a failed download or install; the rest speak for themselves. */
export function updateErrorText(e: SetupError): string {
  switch (e.code) {
    case "not_published":
      return "No signed update has been published yet.";
    case "bad_signature":
      return "The update is not signed with a key this program knows. It was not installed.";
    case "bad_file":
      return "A downloaded file is not the one the release lists. Try again in a few minutes.";
    case "wont_start":
      return "The new version cannot be started on this computer (it may be blocked). Nothing was changed.";
    case "no_target":
      return "This release has no file for this installation. Use the setup program.";
    case "offline":
      return NEEDS_CONNECTED;
    default:
      return errorText(e);
  }
}
