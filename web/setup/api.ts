// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The contract between the setup window and its Rust shell (crates/setup). Command names are
// snake_case, argument keys camelCase (Tauri's default for the frontend side, see
// https://v2.tauri.app/develop/calling-rust/). This is the only file that imports @tauri-apps/api,
// and nothing outside web/setup/ may import this file.
import { invoke } from "@tauri-apps/api/core";

export type Platform = "windows" | "macos" | "linux";
export type InstallState = "none" | "managed" | "foreign" | "unsupported";
export type FoundPrinter = { name: string; per_user: boolean; usable: boolean };
export type PrinterChoice = { name: string; pdf: boolean; target: string | null; locked: boolean };
export type Existing = { serverUrl: string; name: string; printers: PrinterChoice[] };
export type SetupError = { code: string; message: string };
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
};
