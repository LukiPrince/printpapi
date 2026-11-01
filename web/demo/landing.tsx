// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The start page: a sandboxed printpapi in the browser. Pick a job, press PRINT and watch it travel
// app -> server -> agent -> printer. Port of the mockup's landing script, but the stations, packets
// and steps are derived from the job's state and its age instead of hand-set timeouts.
"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import { useReducedMotion } from "motion/react";
import { ArrowRight, Monitor, Printer, Server, Terminal } from "lucide-react";
import { Logo } from "@/components/brand";
import { CopyButton } from "@/components/copy-button";
import { Button } from "@/components/ui/button";
import { useHydrated } from "@/hooks/use-client-value";
import { fmtAgo } from "@/lib/format";
import { JOB_STATES } from "@/lib/job-state";
import { cn } from "@/lib/utils";
import { b64, eur, escposFor, orderData, receiptData, zplFor } from "./data.ts";
import { Paper } from "./paper.tsx";
import { seedState } from "./seed.ts";
import type { DemoJob, DemoState } from "./seed.ts";
import { demoActions, useDemo } from "./store.ts";
import type { JobSpec, Timing } from "./sim.ts";
import styles from "./demo.module.css";

type Kind = "label" | "slip" | "receipt";
type Tab = "req" | "pay";

const HOST = "https://print.example.com"; // never the demo's own origin: nobody should point an agent here
const STEP_TIMING: Timing = { claimAfter: 1.6, printFor: 2.4 };
const FAST_TIMING: Timing = { claimAfter: 0.1, printFor: 0.1 }; // prefers-reduced-motion: no theatre
const PACKET_S = 0.56; // a packet crossing a link; the CSS animation is 0.55 s
const SETTLE_S = 0.7; // how long the callback step stays lit before everything rests

const KINDS: Record<Kind, { printer: number; pick: string; via: string; agentSub: string; steps: [string, string][] }> = {
  label: {
    printer: 1,
    pick: "raw ZPL → Zebra GK420d",
    agentSub: "Windows · polls outbound, no open ports",
    via: "Windows spooler, RAW datatype",
    steps: [
      ["POST /jobs", "Your app sends base64 ZPL. The server stores the job and answers with its id."],
      ["Agent claims it", "The agent was already waiting on a long-poll, so it gets the job right away."],
      ["Printer prints", "Raw ZPL goes straight to the label printer. Nothing is rendered."],
      ["Callback", "The server POSTs the outcome to your callback URL."],
    ],
  },
  slip: {
    printer: 3,
    pick: "shop order → HP LaserJet",
    agentSub: "Windows · polls outbound, no open ports",
    via: "SumatraPDF renders the PDF",
    steps: [
      ["POST /orders", "Your shop sends the order. The server renders it as a PDF packing slip."],
      ["Agent claims it", "The agent was already waiting on a long-poll, so it gets the job right away."],
      ["Printer prints", "A document printer gets the PDF through its driver."],
      ["Callback", "The server POSTs the outcome to your callback URL."],
    ],
  },
  receipt: {
    printer: 4,
    pick: "ESC/POS → Epson TM-m30III",
    agentSub: "macOS · polls outbound, no open ports",
    via: "CUPS lp -o raw",
    steps: [
      ["POST /jobs", "Your till sends base64 ESC/POS. The server stores the job and answers with its id."],
      ["Agent claims it", "The agent on the front-desk Mac was waiting on a long-poll and gets it right away."],
      ["Printer prints", "ESC/POS bytes go straight to the receipt printer, cut included."],
      ["Callback", "The server POSTs the outcome to your callback URL."],
    ],
  },
};
const KIND_ORDER: Kind[] = ["label", "slip", "receipt"];
const PICK_TITLE: Record<Kind, string> = { label: "Shipping label", slip: "Packing slip", receipt: "Receipt" };
const PAYLOAD_TAB: Record<Kind, string> = { label: "Payload (ZPL)", slip: "Order (JSON)", receipt: "Payload (ESC/POS)" };

// Plain answers to what people ask a search engine or a chatbot about printpapi, in the prerendered HTML.
const FAQ: [string, string][] = [
  [
    "What is printpapi?",
    "A self-hosted print bridge. Your application sends a print job to its HTTP API, and an agent on a computer next to the printers picks the job up and prints it. Office printers and label or receipt printers work alike.",
  ],
  [
    "How does it reach printers behind a firewall?",
    "The agent only opens outbound connections. It keeps a long-poll request open to the server and gets each job the moment it is queued, so the site needs no open port and no VPN.",
  ],
  [
    "Which printers does it support?",
    "Any printer the operating system can print to. PDFs go to document printers through SumatraPDF on Windows or CUPS on Linux and macOS. ZPL labels and ESC/POS receipts are sent as raw bytes, so a label printer never gets a PDF.",
  ],
  [
    "Is it an alternative to PrintNode?",
    "Yes. It follows the same model, an agent polling from behind NAT, but runs on your own server and has no per-printer fee. Software written against the PrintNode API can switch if its server address is configurable: point it at your server, and the HTTP Basic auth it already sends selects the compatible API.",
  ],
  [
    "Can it print shop orders automatically?",
    "Yes. The WooCommerce plugin and a Shopify webhook send new orders to the server, which renders each one as a PDF packing slip. Any other system can do the same with POST /orders.",
  ],
  [
    "How do I install it?",
    "The server is one Rust binary for Windows, Linux and macOS, or the Docker image ghcr.io/lukiprince/printpapi. The agent is a second small binary on the machine with the printers, configured in agent.ini.",
  ],
  [
    "What does it cost?",
    "Self-hosting is free, commercial use included. printpapi is source available under the Elastic License 2.0: you may run, modify and redistribute it, but not offer it to the public as a hosted print service.",
  ],
];

// What the prerender and the first client render show, whatever sessionStorage holds.
const SEED = seedState(0);

type Payload = { title: string; spec: JobSpec; payload: string; b64?: string; customer?: string; orderNo?: number };

function payloadFor(kind: Kind, n: number, rn: number): Payload {
  const printer_id = KINDS[kind].printer;
  if (kind === "receipt") {
    const rc = receiptData(rn);
    const e = escposFor(rc);
    const title = `Receipt #A-${rc.n}`;
    return { title, spec: { printer_id, title, type: "raw_base64", kind, ref: rc.n, file: null, copies: 1 }, payload: e.shown, b64: b64(e.bytes) };
  }
  const o = orderData(n);
  if (kind === "label") {
    const title = `Shipping label · order #${o.n}`;
    const zpl = zplFor(o);
    return { title, spec: { printer_id, title, type: "raw_base64", kind, ref: o.n, file: null, copies: 1 }, payload: zpl, b64: b64(zpl), customer: o.person[0], orderNo: o.n };
  }
  const title = `Packing slip · order #${o.n}`;
  const order = {
    number: String(o.n),
    customer: o.person[0],
    address: [o.person[1], o.person[2], o.person[3]],
    lines: o.lines.map((l) => ({ qty: l.qty, sku: l.sku, name: l.name, total: eur(l.total) })),
    totals: [...(o.ship ? [["Shipping", eur(o.ship)]] : []), ["Total", eur(o.total)]],
    ...(o.note ? { note: o.note } : {}),
  };
  return {
    title,
    spec: { printer_id, title, type: "pdf_base64", kind, ref: o.n, file: null, copies: 1 },
    payload: JSON.stringify({ printer_id, order }, null, 2),
    customer: o.person[0],
    orderNo: o.n,
  };
}

// The curl as [highlight, text] runs: rendered with colour, joined for the Copy button.
type Run = ["d" | "h" | "", string];
function curlFor(kind: Kind, p: Payload, jobId: number): Run[] {
  const auth = '"Authorization: Bearer pp_demo_7f3c9a…"';
  const head: Run[] = [
    ["d", "$ "],
    ["", `curl -X POST ${HOST}${kind === "slip" ? "/orders" : "/jobs"} \\\n    -H `],
    ["h", auth],
    ["", ' \\\n    -H "Content-Type: application/json" \\\n    -d \''],
  ];
  const body =
    kind === "slip"
      ? `{"printer_id": 3,\n        "order": {"number": "${p.orderNo}", "customer": "${p.customer}", …},\n        "idempotency_key": "woo-${p.orderNo}-processing",\n        "callback_url": "https://shop.example/print-hook"}'`
      : `{"printer_id": ${KINDS[kind].printer}, "type": "raw_base64",\n        "content": "${(p.b64 ?? "").slice(0, 26)}…",\n        "title": "${p.title}",\n        "callback_url": "https://shop.example/print-hook"}'`;
  return [...head, ["", body], ["d", `\n\n# → {"job_id": ${jobId}}`]];
}

type StationId = "app" | "srv" | "agt" | "prn";
type View = {
  hot: StationId | null;
  step: number; // lit step, -1 = none yet
  go: 0 | 1 | 2 | 3; // link a packet is crossing right now
  reached: number; // furthest station the job has reached: 0 app, 1 server, 2 agent, 3 printer
  printing: boolean;
  finished: boolean; // the printer is done (callback step and after)
  settled: boolean; // the run is over, everything rests
};

// A job's age says where it is. The job's own claim_at / done_at (set by enqueue) are the anchors,
// so a reload mid-run, or the 500 ms store ticker, cannot disagree with what is drawn.
function viewAt(job: DemoJob, now: number, reduced: boolean): View {
  const t = now - job.created_at;
  const claim = job.claim_at === null ? Infinity : job.claim_at - job.created_at;
  const done = job.done_at === null ? Infinity : job.done_at - job.created_at;
  const base: View = { hot: null, step: 0, go: 0, reached: 0, printing: false, finished: false, settled: false };
  if (reduced) {
    if (t >= done) return { ...base, step: 3, reached: 3, finished: true, settled: true };
    if (t >= claim) return { ...base, hot: "prn", step: 2, reached: 3, printing: true };
    return { ...base, hot: "srv", step: 0, reached: 1 };
  }
  if (t >= done + SETTLE_S) return { ...base, step: 3, reached: 3, finished: true, settled: true };
  if (t >= done) return { ...base, hot: "srv", step: 3, reached: 3, finished: true };
  if (t >= claim + 0.7) return { ...base, hot: "prn", step: 2, reached: 3, printing: true };
  if (t >= claim + 0.14) return { ...base, hot: "agt", step: 1, go: t < claim + 0.14 + PACKET_S ? 3 : 0, reached: 2 };
  if (t >= claim) return { ...base, hot: "agt", step: 1, reached: 2 };
  if (t >= claim - PACKET_S) return { ...base, hot: "srv", step: 1, go: 2, reached: 1 };
  if (t >= 0.35 + PACKET_S) return { ...base, hot: "srv", step: 0, reached: 1 };
  if (t >= 0.35) return { ...base, hot: "srv", step: 0, go: 1, reached: 1 };
  return { ...base, hot: "app", step: 0, reached: 0 };
}

const RANK = { queued: 0, claimed: 1, done: 2 } as const;

export function Landing() {
  const hydrated = useHydrated();
  const live = useDemo((s) => s);
  const state: DemoState = hydrated ? live : SEED;
  const reduced = useReducedMotion() ?? false;

  const [kind, setKind] = useState<Kind>("label");
  const [tab, setTab] = useState<Tab>("req");
  const [jobId, setJobId] = useState<number | null>(null); // the run on screen
  const [lastId, setLastId] = useState<number | null>(null); // the run before a kind switch
  const [now, setNow] = useState(0);

  const k = KINDS[kind];
  const printer = state.printers.find((p) => p.id === k.printer);
  const agent = printer && state.agents.find((a) => a.id === printer.agent_id);
  const job = jobId === null ? undefined : state.jobs.find((j) => j.id === jobId);
  const view = job ? viewAt(job, now, reduced) : null;
  const running = !!job && !view?.settled;

  // A local clock for the run only; the store's own ticker is slower than the packets.
  useEffect(() => {
    if (!running) return;
    const id = window.setInterval(() => setNow(Date.now() / 1000), 100);
    return () => window.clearInterval(id);
  }, [running]);

  // While a run is on screen the request shows what was sent; afterwards, what the next press sends.
  const p = payloadFor(kind, running && job?.ref ? job.ref : state.next.order, running && job?.ref ? job.ref : state.next.receipt);
  const runs = curlFor(kind, p, running && job ? job.id : state.next.job);
  const curlText = runs.map((r) => r[1]).join("");

  function press() {
    if (running) return;
    const sent = payloadFor(kind, state.next.order, state.next.receipt);
    const j = demoActions.enqueue(sent.spec, reduced ? FAST_TIMING : STEP_TIMING);
    setNow(Date.now() / 1000);
    setJobId(j.id);
  }

  function pickKind(next: Kind) {
    if (running || next === kind) return;
    if (jobId !== null) setLastId(jobId);
    setJobId(null);
    setKind(next);
  }

  // The state the server box shows: the job's own, pushed forward by its age so the badge does not
  // lag behind the animation by a store tick.
  const aged = view?.finished ? "done" : view && view.reached >= 2 ? "claimed" : "queued";
  const shownState = job && RANK[aged] > (RANK[job.state as keyof typeof RANK] ?? 0) ? aged : (job?.state ?? "queued");
  const badge = JOB_STATES[shownState];
  const BadgeIcon = badge.icon;

  const lastJob = (lastId !== null && state.jobs.find((j) => j.id === lastId)) || state.jobs.find((j) => j.state === "done");
  const sample = job && (view?.printing || view?.finished) ? job : state.jobs.find((j) => j.printer_id === k.printer && j.state === "done");
  const feeding = !!job && !!view?.printing && !reduced;
  const settled = !!view?.settled;
  const linkId = settled ? jobId : lastId;

  const hot = (id: StationId) => (view?.hot === id ? styles.hot : "");
  const go = (n: number) => (view?.go === n ? styles.go : "");
  const polled = !view || view.settled || view.reached < 2;

  return (
    <div className={styles.landing}>
      <header className={styles.siteHead}>
        <Link prefetch={false} href="/" className={styles.siteBrand}>
          <Logo className="size-7" />
          <b>printpapi</b>
        </Link>
        <span className={styles.siteBadge}>Live demo</span>
        <nav className={styles.siteNav} aria-label="Site">
          <a className={styles.navLink} href="https://github.com/LukiPrince/printpapi/tree/main/docs">
            Docs
          </a>
          <a className={styles.navLink} href="https://github.com/LukiPrince/printpapi">
            GitHub
          </a>
          <Button asChild variant="outline" size="sm">
            <a href="https://github.com/LukiPrince/printpapi#get-started">Self-host it</a>
          </Button>
          <Button asChild variant="brand" size="sm">
            <Link prefetch={false} href="/overview/">Open the dashboard</Link>
          </Button>
        </nav>
      </header>

      <section className={styles.hero}>
        <div className={styles.eyebrow}>Print bridge · live demo · nothing to install</div>
        <h1>
          Send a print job over <em>HTTP</em>. Watch it come out of the printer.
        </h1>
        <p className={styles.lede}>
          This page runs a sandboxed printpapi in your browser: the API, an agent on a warehouse PC, and its printers. Pick a job,
          press Print and follow it through the queue. No real paper is used.
        </p>
        <div className={styles.heroStrip} aria-hidden="true" />
      </section>

      <section className={styles.demo} aria-label="Interactive demo">
        <div className={styles.demoTop}>
          <div className={styles.seg} role="group" aria-label="What to print">
            {KIND_ORDER.map((id) => (
              <button key={id} type="button" aria-pressed={kind === id} disabled={running} onClick={() => pickKind(id)}>
                <b>{PICK_TITLE[id]}</b>
                <small>{KINDS[id].pick}</small>
              </button>
            ))}
          </div>
          <Button type="button" variant="brand" size="lg" className={cn("h-11 px-6", styles.send)} disabled={running} onClick={press}>
            <Printer />
            <span>{running ? "PRINTING" : job ? "PRINT ANOTHER" : "PRINT"}</span>
          </Button>
        </div>

        <div className={styles.pipe}>
          <div className={cn(styles.st, styles.stApp, hot("app"))}>
            <div className={styles.stH}>
              <Terminal />
              Your app<span className={styles.mono}>any HTTP client</span>
            </div>
            <div className={styles.codeTabs} role="tablist" aria-label="Request view">
              <button type="button" role="tab" aria-selected={tab === "req"} onClick={() => setTab("req")}>
                Request
              </button>
              <button type="button" role="tab" aria-selected={tab === "pay"} onClick={() => setTab("pay")}>
                {PAYLOAD_TAB[kind]}
              </button>
            </div>
            <div className={styles.codeWrap}>
              <pre className={styles.code}>
                {tab === "req"
                  ? runs.map(([c, text], i) => (c ? <span key={i} className={styles[c]}>{text}</span> : text))
                  : p.payload}
              </pre>
              <CopyButton value={tab === "req" ? curlText : p.payload} className={styles.copy} />
            </div>
          </div>

          <div className={cn(styles.link, styles.l1, go(1))} aria-hidden="true">
            <span className={styles.pkt} />
          </div>

          <div className={cn(styles.st, styles.stSrv, hot("srv"))}>
            <div className={styles.stH}>
              <Server />
              printpapi server
            </div>
            <div className={styles.stSub}>HTTP API · SQLite job queue</div>
            {job && view && view.reached >= 1 ? (
              <div className={cn(styles.qbox, styles.qboxHas)}>
                <span className={styles.jid}>
                  #{job.id} · {kind === "slip" ? "rendered as PDF" : "stored"}
                </span>
                <span className={styles.qt}>{job.title}</span>
                <span className="inline-flex w-fit items-center gap-1.5 rounded-full px-2 py-0.5 font-mono text-[0.68rem] font-semibold tracking-wide uppercase"
                  style={{ color: badge.color, background: `color-mix(in oklch, ${badge.color}, transparent 88%)` }}>
                  <BadgeIcon className={cn("size-3", badge.spin && "animate-spin")} />
                  {badge.label}
                </span>
              </div>
            ) : (
              <div className={styles.qbox}>
                <span className={styles.stSmall}>Queue empty</span>
                {lastJob && (
                  <span className={styles.stSmall}>
                    Last job <span className={styles.mono}>#{lastJob.id}</span>
                    {hydrated && ` · done ${fmtAgo(lastJob.finished_at ?? lastJob.created_at)}`}
                  </span>
                )}
              </div>
            )}
            <div className={styles.factsMini}>
              <div>
                <span>Binary</span>
                <b>~8 MB</b>
              </div>
              <div>
                <span>Memory, idle</span>
                <b>6.6 MB</b>
              </div>
            </div>
          </div>

          <div className={cn(styles.link, styles.l2, go(2))} aria-hidden="true">
            <span className={styles.pkt} />
          </div>

          <div className={cn(styles.st, styles.stAgt, hot("agt"))}>
            <div className={styles.stH}>
              <Monitor />
              Agent<span className={styles.mono}>{agent?.name}</span>
            </div>
            <div className={styles.stSub}>{k.agentSub}</div>
            <div className={styles.poll}>
              <LiveDot />
              <span>{polled || !job ? "waiting on GET /agent/jobs" : `got job #${job.id} from the long-poll`}</span>
            </div>
            <div className={styles.alog}>
              {!job || !view || view.reached < 2 ? (
                <>
                  <span>registered 2 label printers, 1 PDF printer</span>
                  <span>long-poll open · 25 s timeout</span>
                </>
              ) : view.finished ? (
                <>
                  <span className={styles.on}>#{job.id} done · reported to the server</span>
                  <span>long-poll open again</span>
                </>
              ) : (
                <>
                  <span className={styles.on}>claimed #{job.id}</span>
                  <span className={styles.on}>
                    → {printer?.name} via {k.via}
                  </span>
                </>
              )}
            </div>
          </div>

          <div className={cn(styles.link, styles.l3, go(3))} aria-hidden="true">
            <span className={styles.pkt} />
          </div>

          <div className={cn(styles.st, styles.stPrn, hot("prn"))}>
            <div className={cn(styles.printer, view?.printing && styles.busy)}>
              <div className={styles.prBody}>
                <div className={styles.prStripe} />
                <div className={styles.prLed} />
                <div className={styles.prName}>{printer?.name}</div>
                <div className={styles.prSlot} />
              </div>
              <div className={styles.prOut}>{sample && <Paper key={sample.id} job={sample} state={state} feeding={feeding} />}</div>
            </div>
          </div>
        </div>

        <div className={styles.demoFoot}>
          <ol className={styles.steps}>
            {k.steps.map(([title, text], i) => {
              const lit = view ? (settled ? "done" : i === view.step ? "on" : i < view.step ? "done" : "") : "";
              return (
                <li key={title} className={cn(lit === "on" && styles.stepOn, lit === "done" && styles.stepDone)}>
                  <b>{title}</b>
                  {text}
                </li>
              );
            })}
          </ol>
          <div className={styles.after}>
            <p>
              A waiting agent gets the job about 19 ms after it is queued (median in the v3 benchmark). The animation is slowed down
              so you can follow it.
            </p>
            {linkId !== null && (
              <p>
                <Link className={styles.goDash} prefetch={false} href="/history/">
                  Job #{linkId} is now in the dashboard&apos;s history <ArrowRight />
                </Link>
              </p>
            )}
          </div>
        </div>
      </section>

      <section className={styles.next}>
        <div>
          <h2>The dashboard behind this demo</h2>
          <p>
            It is filled with a print day at a fictional coffee roaster: seven printers on four machines, a WooCommerce shop sending
            orders, and one packing station that went offline. Send test prints, cancel a queued job, issue an API key. Whatever you
            print lands in the paper tray.
          </p>
          <Button asChild variant="brand" className={styles.nextCta}>
            <Link prefetch={false} href="/overview/">
              Open the demo dashboard <ArrowRight />
            </Link>
          </Button>
        </div>
        <ul className={styles.facts}>
          <li>
            <b>Agents poll outbound</b>Printers behind NAT or a firewall work without opening a port.
          </li>
          <li>
            <b>Label printers get raw bytes</b>ZPL and ESC/POS go straight through. PDFs go to document printers.
          </li>
          <li>
            <b>Shop orders print themselves</b>WooCommerce plugin, Shopify webhook, or <code>POST /orders</code>.
          </li>
          <li>
            <b>PrintNode-compatible API</b>Point an existing PrintNode client at your own server.
          </li>
        </ul>
      </section>

      <section className={styles.faq} aria-labelledby="faq">
        <h2 id="faq">Questions</h2>
        <div className={styles.faqList}>
          {FAQ.map(([q, a]) => (
            <div key={q}>
              <h3>{q}</h3>
              <p>{a}</p>
            </div>
          ))}
        </div>
        <p className={styles.faqNote}>
          PrintNode is a trademark of PrintNode Ltd. printpapi is not affiliated with, endorsed by, or sponsored by PrintNode.
        </p>
      </section>

      <footer className={styles.siteFoot}>
        <span>printpapi is source available under the Elastic License 2.0. Self-hosting is free.</span>
        <a href="https://github.com/LukiPrince/printpapi">GitHub</a>
        <a href="https://github.com/LukiPrince/printpapi/releases/latest">Latest release</a>
        <a href="https://github.com/LukiPrince/printpapi/blob/main/docs/api.md">HTTP API</a>
        <Link prefetch={false} href="/datenschutz/">
          Datenschutz
        </Link>
      </footer>
    </div>
  );
}

function LiveDot() {
  return (
    <span className="relative flex size-2.5 shrink-0">
      <span className="absolute inset-0 rounded-full bg-state-done opacity-50 motion-safe:animate-ping" />
      <span className="relative size-2.5 rounded-full bg-state-done" />
    </span>
  );
}
