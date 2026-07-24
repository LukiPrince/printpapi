"use client";

import { motion } from "motion/react";
import { Apple, MonitorCheck, Terminal } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { CopyButton } from "@/components/copy-button";
import { useClientValue } from "@/hooks/use-client-value";

const STEPS = [
  {
    title: "Grab the agent",
    body: (
      <>
        Copy <code className="font-mono">agent/print_agent.py</code> from the printpapi repo onto the
        machine the printers are attached to.
      </>
    ),
  },
  {
    title: "Install the prerequisites",
    body: (
      <>
        Python 3.9+. On Windows also <code className="font-mono">pywin32</code> plus SumatraPDF for
        PDF rendering; on Linux, CUPS provides <code className="font-mono">lp</code>.
      </>
    ),
  },
  {
    title: "Write agent.ini",
    body: <>Drop the config below next to the script and fill in your agent key.</>,
  },
  {
    title: "Run it",
    body: (
      <>
        <code className="font-mono">python print_agent.py</code> — then autostart it via Task
        Scheduler or a systemd unit.
      </>
    ),
  },
];

export default function DownloadsPage() {
  const origin = useClientValue(() => window.location.origin, "http://localhost:3460");

  const ini = `[agent]
server_url = ${origin}
api_key = <your agent key>
name = office-pc
; printers: semicolon-separated. Append |pdf for document printers.
; A CUPS queue / Windows printer name, or socket://IP:PORT for a raw network printer.
printers = Zebra GK420d ; HP LaserJet|pdf ; netz-bixolon = socket://192.168.1.50:9100`;

  return (
    <div className="max-w-3xl space-y-4">
      <Card>
        <CardHeader>
          <CardTitle>Install the agent</CardTitle>
          <CardDescription>
            It runs on the machine with the printers and polls this server outbound over HTTP(S) —
            no inbound ports, no port forwarding, works behind NAT.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <ol className="space-y-3">
            {STEPS.map((step, i) => (
              <motion.li
                key={step.title}
                initial={{ opacity: 0, x: -8 }}
                animate={{ opacity: 1, x: 0 }}
                transition={{ delay: i * 0.07, duration: 0.25 }}
                className="flex gap-3"
              >
                <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-brand font-mono text-xs font-bold text-brand-foreground">
                  {i + 1}
                </span>
                <div>
                  <p className="font-medium">{step.title}</p>
                  <p className="text-sm text-muted-foreground">{step.body}</p>
                </div>
              </motion.li>
            ))}
          </ol>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>agent.ini</CardTitle>
          <CardDescription>Pre-filled with this server&apos;s address.</CardDescription>
        </CardHeader>
        <CardContent>
          <div className="relative">
            <pre className="overflow-x-auto rounded-lg bg-sidebar p-4 font-mono text-xs leading-relaxed text-[#dfe5ea]">
              {ini}
            </pre>
            <CopyButton value={ini} className="absolute top-2 right-2" />
          </div>
          <p className="mt-3 text-sm text-muted-foreground">
            <code className="font-mono">socket://</code> printers are raw-only — a network printer
            cannot render a PDF, so send it ZPL or ESC-POS.
          </p>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Autostart</CardTitle>
          <CardDescription>Keep it running across reboots.</CardDescription>
        </CardHeader>
        <CardContent>
          <Tabs defaultValue="windows">
            <TabsList>
              <TabsTrigger value="windows">
                <MonitorCheck /> Windows
              </TabsTrigger>
              <TabsTrigger value="linux">
                <Terminal /> Linux
              </TabsTrigger>
              <TabsTrigger value="macos">
                <Apple /> macOS
              </TabsTrigger>
            </TabsList>
            <TabsContent value="windows" className="mt-4 space-y-2 text-sm text-muted-foreground">
              <p>
                Task Scheduler → <em>At log on</em> → run{" "}
                <code className="font-mono">pythonw.exe print_agent.py</code>.
              </p>
              <p>
                Locked-down machines (Smart App Control / WDAC) block unsigned executables. Running
                via the PSF-signed <code className="font-mono">pythonw.exe</code> sidesteps this —
                the <code className="font-mono">.py</code> file is data, not an executable.
              </p>
            </TabsContent>
            <TabsContent value="linux" className="mt-4 space-y-2 text-sm text-muted-foreground">
              <p>
                A user systemd unit with{" "}
                <code className="font-mono">ExecStart=/usr/bin/python3 print_agent.py</code> and{" "}
                <code className="font-mono">Restart=always</code>.
              </p>
              <p>
                Printers are CUPS queue names — check them with{" "}
                <code className="font-mono">lpstat -p</code>.
              </p>
            </TabsContent>
            <TabsContent value="macos" className="mt-4 space-y-2 text-sm text-muted-foreground">
              <p>
                A <code className="font-mono">launchd</code> plist in{" "}
                <code className="font-mono">~/Library/LaunchAgents</code> with{" "}
                <code className="font-mono">RunAtLoad</code>.
              </p>
              <p>macOS uses the same CUPS path as Linux, so the printer names match lpstat.</p>
            </TabsContent>
          </Tabs>
        </CardContent>
      </Card>
    </div>
  );
}
