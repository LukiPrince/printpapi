"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { motion } from "motion/react";
import { Command, LogOut } from "lucide-react";
import { NAV, navFor, normalizePath } from "@/lib/nav";
import { Barcode, Logo } from "@/components/brand";
import { ThemeToggle } from "@/components/theme-toggle";
import { CommandPalette, useCommandPalette } from "@/components/command-palette";
import { LiveRail } from "@/components/live-rail";
import { Button } from "@/components/ui/button";
import { useAuth } from "@/components/auth-gate";
import { cn } from "@/lib/utils";

export function AppShell({ children }: { children: React.ReactNode }) {
  const pathname = usePathname();
  const active = normalizePath(pathname);
  const current = navFor(pathname);
  const { signOut } = useAuth();
  const palette = useCommandPalette();

  return (
    <div className="min-h-svh md:grid md:grid-cols-[15rem_minmax(0,1fr)] xl:grid-cols-[15rem_minmax(0,1fr)_19rem]">
      {/* sidebar — the ink block */}
      <nav className="flex gap-1 overflow-x-auto bg-sidebar px-3 py-2 text-sidebar-foreground md:sticky md:top-0 md:h-svh md:flex-col md:overflow-y-auto md:px-3 md:py-5">
        <Link href="/" prefetch={false} className="flex shrink-0 items-center gap-2.5 px-2 md:pb-3">
          <Logo className="size-7" />
          <span className="font-mono text-base font-bold tracking-tight text-white">printpapi</span>
        </Link>
        <Barcode className="mx-2 hidden h-3.5 text-white/30 md:block" />

        <div className="flex shrink-0 gap-1 md:mt-3 md:flex-col">
          {NAV.map((item) => {
            const isActive = active === item.href;
            return (
              <Link
                key={item.href}
                href={item.href}
                // Static export: every route ships in the same bundle, so prefetching only
                // adds RSC segment requests a plain file server has no file for.
                prefetch={false}
                aria-current={isActive ? "page" : undefined}
                className={cn(
                  "relative flex items-center gap-2.5 rounded-md px-2.5 py-2 text-sm whitespace-nowrap transition-colors",
                  isActive
                    ? "text-sidebar-primary-foreground"
                    : "hover:bg-white/7 hover:text-white",
                )}
              >
                {isActive && (
                  <motion.span
                    layoutId="nav-active"
                    className="absolute inset-0 rounded-md bg-sidebar-primary"
                    transition={{ type: "spring", stiffness: 480, damping: 38 }}
                  />
                )}
                <item.icon className="relative size-4" />
                <span className="relative font-medium">{item.label}</span>
              </Link>
            );
          })}
        </div>

        <div className="hidden flex-1 md:block" />

        <div className="flex shrink-0 items-center gap-1 md:mt-3 md:border-t md:border-sidebar-border md:pt-3">
          <ThemeToggle className="text-sidebar-foreground hover:bg-white/7 hover:text-white" />
          <Button
            variant="ghost"
            size="icon"
            aria-label="Command palette"
            onClick={() => palette.setOpen(true)}
            className="text-sidebar-foreground hover:bg-white/7 hover:text-white"
          >
            <Command />
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={signOut}
            className="ml-auto font-mono text-[0.7rem] tracking-[0.08em] text-sidebar-foreground uppercase hover:bg-white/7 hover:text-white"
          >
            <LogOut />
            Sign out
          </Button>
        </div>
      </nav>

      {/* main column */}
      <main className="min-w-0 px-5 py-6 md:px-8">
        <header className="mb-6">
          <div className="font-mono text-[0.66rem] font-semibold tracking-[0.22em] text-muted-foreground uppercase">
            {current.eyebrow}
          </div>
          <h1 className="mt-1 text-2xl font-bold tracking-tight">{current.label}</h1>
        </header>

        <motion.div
          key={active}
          initial={{ opacity: 0, y: 10 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.28, ease: [0.2, 0.8, 0.3, 1] }}
        >
          {children}
        </motion.div>
      </main>

      <LiveRail help={current.help} />

      <CommandPalette open={palette.open} onOpenChange={palette.setOpen} />
    </div>
  );
}
