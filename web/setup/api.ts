// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The contract between the setup window and its Rust shell (crates/setup). Command names are
// snake_case, argument keys camelCase (Tauri's default for the frontend side, see
// https://v2.tauri.app/develop/calling-rust/). This is the only file that imports @tauri-apps/api,
// and nothing outside web/setup/ may import this file.
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

export type Platform = "windows" | "macos" | "linux";
export type InstallState = "none" | "managed" | "foreign" | "unsupported";
export type FoundPrinter = { name: string; per_user: boolean; usable: boolean };
export type PrinterChoice = { name: string; pdf: boolean; target: string | null; locked: boolean };
export type Existing = { serverUrl: string; name: string; printers: PrinterChoice[] };
export type SetupError = { code: string; message: string };
/** What the agent says about itself (its status file, readable without administrator rights). */
export type AgentStatus = {
  state: "online" | "offline" | "not_running";
  name: string;
  server: string;
  version: string;
  /** Seconds since the epoch of this computer. */
  lastContact: number | null;
  lastJob: { id: number; ok: boolean; at: number } | null;
  printers: PrinterChoice[];
  timeout: number;
  /** The names only: the values are secrets and come with headerValues. */
  headerNames: string[];
  error: string | null;
};
/** A change of the installed agent.ini: only what is named is touched. A header value of null removes it. */
export type ConfigPatch = { printers?: PrinterChoice[]; timeout?: number; headers?: [string, string | null][] };
export type ApplyRequest = {
  serverUrl: string | null;
  apiKey: string | null;
  name: string | null;
  printers: PrinterChoice[];
};

/** What a rejected command becomes: Tauri passes the command's serialized error through as is. */
export function toSetupError(e: unknown): SetupError {
  if (e && typeof e === "object" && "message" in e) {
    const { code, message } = e as { code?: unknown; message: unknown };
    return { code: typeof code === "string" ? code : "error", message: String(message) };
  }
  return { code: "error", message: typeof e === "string" ? e : "Something went wrong." };
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (e) {
    throw toSetupError(e);
  }
}

export const setup = {
  platform: () => call<Platform>("get_platform"),
  detectInstall: () => call<InstallState>("detect_install"),
  defaultName: () => call<string>("default_name"),
  needsInsecureConfirm: (url: string) => call<boolean>("needs_insecure_confirm", { url }),
  /** true: a device with this name already exists on the server. */
  checkConnection: (serverUrl: string, apiKey: string, name: string) =>
    call<boolean>("check_connection", { serverUrl, apiKey, name }),
  listPrinters: () => call<FoundPrinter[]>("list_printers"),
  readExisting: () => call<Existing>("read_existing"),
  sumatraPresent: () => call<boolean>("sumatra_present"),
  downloadSumatra: () => call<void>("download_sumatra"),
  testPrint: (printer: PrinterChoice) => call<void>("test_print", { printer }),
  /** Returns the baseline `last_seen_at` that waitOnline compares against. */
  apply: (request: ApplyRequest) => call<number>("apply", { request }),
  /** Waits up to 60 s for the agent to check in after the baseline. */
  waitOnline: (baseline: number) => call<boolean>("wait_online", { baseline }),
  uninstall: () => call<void>("uninstall"),
  /** Reads the agent's status file; no administrator rights. */
  readStatus: () => call<AgentStatus>("read_status"),
  /** The last lines of the agent's log; no administrator rights. Empty when nothing was logged. */
  readLog: (lines: number) => call<string>("read_log", { lines }),
  /**
   * Applies a change to the installed agent: one administrator prompt. Returns the time on this
   * computer when it was done; waitOnline then waits for the agent's next contact after it.
   */
  applyPatch: (patch: ConfigPatch) => call<number>("apply_patch", { patch }),
  /** Stops the agent and starts it again: one administrator prompt. */
  restartAgent: () => call<void>("restart_agent"),
  /** The values of the installed [headers] (they are secrets): one administrator prompt. */
  headerValues: () => call<[string, string][]>("header_values"),
  /**
   * Closes the window, and with it the program. Needs `core:window:allow-close` in
   * crates/setup/capabilities/default.json (https://v2.tauri.app/learn/window-customization/).
   */
  close: () => getCurrentWindow().close(),
};
