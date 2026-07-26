import { Download, Gauge, History, KeyRound, Printer, Send, SlidersHorizontal, Users } from "lucide-react";

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
    help: "Every agent from /computers with its printers. Online means the agent polled within the last minute; an offline one shows when it was last seen. Test print sends a sample label or PDF, whichever the printer can handle.",
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
    help: "Per-client API keys — one per integration. The key is shown once, so copy it then. Revoking cuts access instantly. Needs an account login or the bootstrap PRINTAPI_TOKEN.",
  },
  {
    href: "/users",
    label: "Team",
    eyebrow: "Accounts",
    icon: Users,
    help: "The people who can sign in to this org. They get their own e-mail and password instead of sharing the bootstrap token, and can manage the org's keys and people. Changing your own password signs out every browser.",
  },
  {
    href: "/settings",
    label: "Settings",
    eyebrow: "This org",
    icon: SlidersHorizontal,
    help: "Your org's own settings: how many jobs it has printed this month against its quota, where agent online/offline events are POSTed, and the Shopify webhook secret. The quota is the operator's to set — the bootstrap token can change it, an account cannot.",
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
