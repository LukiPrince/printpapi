"use client";

import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { useTheme } from "next-themes";
import { toast } from "sonner";
import { LogOut, Moon, Printer, Sun } from "lucide-react";
import {
  Command,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
} from "@/components/ui/command";
import { NAV } from "@/lib/nav";
import { createJob, listPrinters, type Printer as PrinterT } from "@/lib/api";
import { testJob } from "@/lib/testdoc";
import { useAuth } from "@/components/auth-gate";

/** ⌘K / Ctrl-K: jump between views, flip the theme, or fire a test print. */
export function CommandPalette({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
}) {
  const router = useRouter();
  const { setTheme, resolvedTheme } = useTheme();
  const { signOut } = useAuth();
  const [printers, setPrinters] = useState<PrinterT[]>([]);

  useEffect(() => {
    if (!open) return;
    listPrinters()
      .then(setPrinters)
      .catch(() => setPrinters([]));
  }, [open]);

  const run = (fn: () => void) => {
    onOpenChange(false);
    fn();
  };

  async function testPrint(p: PrinterT) {
    try {
      const { job_id } = await createJob(testJob(p.id, p.can_pdf));
      toast.success(`Test job #${job_id} queued`, { description: p.name });
    } catch (e) {
      toast.error("Test print failed", {
        description: e instanceof Error ? e.message : "",
      });
    }
  }

  return (
    <CommandDialog open={open} onOpenChange={onOpenChange} title="Command palette">
      {/* CommandDialog only supplies the Dialog shell — cmdk's context comes from <Command>. */}
      <Command>
        <CommandInput placeholder="Jump to a view, or type a printer name…" />
        <CommandList>
          <CommandEmpty>Nothing matches.</CommandEmpty>
          <CommandGroup heading="Go to">
            {NAV.map((item) => (
              <CommandItem
                key={item.href}
                value={`${item.label} ${item.eyebrow}`}
                onSelect={() => run(() => router.push(item.href))}
              >
                <item.icon />
                {item.label}
                <span className="ml-auto text-xs text-muted-foreground">{item.eyebrow}</span>
              </CommandItem>
            ))}
          </CommandGroup>

          {printers.length > 0 && (
            <>
              <CommandSeparator />
              <CommandGroup heading="Test print">
                {printers.map((p) => (
                  <CommandItem
                    key={p.id}
                    value={`test ${p.name} ${p.agent_name}`}
                    disabled={!p.online}
                    onSelect={() => run(() => testPrint(p))}
                  >
                    <Printer />
                    {p.name}
                    <span className="ml-auto text-xs text-muted-foreground">
                      {p.online ? p.agent_name : "offline"}
                    </span>
                  </CommandItem>
                ))}
              </CommandGroup>
            </>
          )}

          <CommandSeparator />
          <CommandGroup heading="Session">
            <CommandItem
              value="toggle theme dark light"
              onSelect={() => run(() => setTheme(resolvedTheme === "dark" ? "light" : "dark"))}
            >
              {resolvedTheme === "dark" ? <Sun /> : <Moon />}
              Switch to {resolvedTheme === "dark" ? "light" : "dark"} theme
            </CommandItem>
            <CommandItem value="sign out logout" onSelect={() => run(signOut)}>
              <LogOut />
              Sign out
            </CommandItem>
          </CommandGroup>
        </CommandList>
      </Command>
    </CommandDialog>
  );
}

/** Global ⌘K / Ctrl-K listener. Returns the open state for the palette. */
export function useCommandPalette() {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setOpen((v) => !v);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);
  return { open, setOpen };
}
