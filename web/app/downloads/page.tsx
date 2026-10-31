"use client";

import { motion } from "motion/react";
import { Apple, MonitorCheck, Terminal } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { CopyButton } from "@/components/copy-button";
import { DEMO_SERVER_URL } from "printpapi-demo";
import { useClientValue } from "@/hooks/use-client-value";

const RELEASES = "https://github.com/LukiPrince/printpapi/releases/latest";

const STEPS = [
  {
    title: "Download the agent",
    body: (
      <>
        Get the archive for your platform from the{" "}
        <a href={RELEASES} className="underline" target="_blank" rel="noreferrer">
          latest release
        </a>
        : <code className="font-mono">printpapi-agent-windows-x86_64.zip</code>,{" "}
        <code className="font-mono">-linux-x86_64</code>,{" "}
        <code className="font-mono">-linux-aarch64</code>,{" "}
        <code className="font-mono">-macos-aarch64</code> or{" "}
        <code className="font-mono">-macos-x86_64</code> (<code className="font-mono">.tar.gz</code>).
        Unpack it on the machine the printers are attached to.
      </>
    ),
  },
  {
    title: "Check the print path",
    body: (
      <>
        Nothing to install for the agent itself. On Windows, put{" "}
        <code className="font-mono">SumatraPDF.exe</code> next to it (or on PATH) to print PDFs.
        On Linux and macOS, CUPS provides <code className="font-mono">lp</code>.
      </>
    ),
  },
  {
    title: "Write agent.ini",
    body: (
      <>
        Save the config below as <code className="font-mono">agent.ini</code> next to the
        executable and fill in your agent key.
      </>
    ),
  },
  {
    title: "Run it",
    body: (
      <>
        Start <code className="font-mono">printpapi-agent</code> (
        <code className="font-mono">printpapi-agent.exe</code> on Windows) — then autostart it via
        Task Scheduler, a systemd unit or launchd.
      </>
    ),
  },
];

export default function DownloadsPage() {
  const origin = useClientValue(() => DEMO_SERVER_URL ?? window.location.origin, "http://localhost:3460");

  const ini = `[agent]
server_url = ${origin}
api_key = <your agent key>
name = office-pc
; printers: semicolon-separated. Append |pdf for document printers.
; A CUPS queue / Windows printer name, socket://IP:PORT for a raw network printer,
; or file:///path/to/dir to write the job to disk instead of printing it.
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
            cannot render a PDF, so send it ZPL or ESC-POS. A{" "}
            <code className="font-mono">file:///dir</code> printer archives the job to that
            directory instead of printing it.
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
                Task Scheduler → <em>At startup</em> → run{" "}
                <code className="font-mono">C:\printpapi\printpapi-agent.exe</code>, or install
                it as a service with nssm. It is a console program; with &quot;run whether user is
                logged on or not&quot; no window shows.
              </p>
              <p>
                Locked-down machines (Smart App Control / WDAC / AppLocker) block the agent
                because it is not code-signed yet. Until signed builds exist, use the Python agent
                (<code className="font-mono">agent/print_agent.py</code> in the repo) through the
                PSF-signed <code className="font-mono">pythonw.exe</code> — the{" "}
                <code className="font-mono">.py</code> file is data, not an executable. It needs
                Python 3.9+ and <code className="font-mono">pywin32</code>, and reads the same{" "}
                <code className="font-mono">agent.ini</code>.
              </p>
            </TabsContent>
            <TabsContent value="linux" className="mt-4 space-y-2 text-sm text-muted-foreground">
              <p>
                A systemd unit with{" "}
                <code className="font-mono">ExecStart=/opt/printpapi/printpapi-agent</code> and{" "}
                <code className="font-mono">Restart=always</code>.
              </p>
              <p>
                Printers are CUPS queue names — check them with{" "}
                <code className="font-mono">lpstat -p</code>.
              </p>
            </TabsContent>
            <TabsContent value="macos" className="mt-4 space-y-2 text-sm text-muted-foreground">
              <p>
                A <code className="font-mono">launchd</code> plist whose{" "}
                <code className="font-mono">ProgramArguments</code> is the binary, with{" "}
                <code className="font-mono">RunAtLoad</code> and{" "}
                <code className="font-mono">KeepAlive</code>.
              </p>
              <p>macOS uses the same CUPS path as Linux, so the printer names match lpstat.</p>
            </TabsContent>
          </Tabs>
        </CardContent>
      </Card>
    </div>
  );
}
