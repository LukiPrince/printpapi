import { Download, Gauge, History, KeyRound, Printer, Send } from "lucide-react";

export type NavItem = {
  href: string;
  label: string;
  eyebrow: string;
  icon: typeof Gauge;
  help: string;
};

export const NAV: NavItem[] = [
  {
    href: "/",
    label: "Overview",
    eyebrow: "Fleet status",
    icon: Gauge,
    help: "Live snapshot of the queue and the fleet. Counters come from /metrics, the activity feed from /jobs. Auto-refreshes every 5 seconds.",
  },
  {
    href: "/print",
    label: "Print Something",
    eyebrow: "Submit a job",
    icon: Send,
    help: "Pick a source and a printer, then hit PRINT. Label printers are raw-only — they cannot render a PDF, so send them ZPL/ESC-POS. A callback URL gets a POST when the job finishes.",
  },
  {
    href: "/devices",
    label: "Devices",
    eyebrow: "Agents & printers",
    icon: Printer,
    help: "Printers registered by your agents. Online means the agent polled within the last minute. Test print sends a sample label or PDF, whichever the printer can handle.",
  },
  {
    href: "/history",
    label: "Print History",
    eyebrow: "Job queue",
    icon: History,
    help: "The last 50 jobs. State runs queued → claimed → done/failed. Queued jobs can still be cancelled; once an agent claims one it is on its way to the printer.",
  },
  {
    href: "/keys",
    label: "API Keys",
    eyebrow: "Access",
    icon: KeyRound,
    help: "Per-client API keys — one per integration. The key is shown once, so copy it then. Revoking cuts access instantly. Needs the bootstrap PRINTAPI_TOKEN.",
  },
  {
    href: "/downloads",
    label: "Downloads",
    eyebrow: "Agent setup",
    icon: Download,
    help: "Install the agent on the machine with the printers. It polls this server outbound, so no inbound ports or port forwarding are needed.",
  },
];

/** usePathname() returns "/print/" with trailingSlash, so compare on a bare form. */
export const normalizePath = (p: string) => (p !== "/" && p.endsWith("/") ? p.slice(0, -1) : p);

export const navFor = (pathname: string) =>
  NAV.find((n) => n.href === normalizePath(pathname)) ?? NAV[0];
