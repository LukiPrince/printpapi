// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The setup window's rules, free of React and of Tauri so they can be tested.
import type { FoundPrinter, Platform, PrinterChoice, SetupError } from "./api";

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
  return !(p.pdf && platform === "windows" && !sumatraPresent);
}

/** The text for a failed command. A proxy refusal gets the way around it, the rest speak for themselves. */
export function errorText(e: SetupError): string {
  if (e.code === "proxy") {
    return "A proxy in front of the server refused this. Set up the agent by hand with [headers], see docs/agent.md.";
  }
  return e.message;
}
