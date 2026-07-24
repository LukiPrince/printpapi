"use client";

import { useTheme } from "next-themes";
import { AnimatePresence, motion } from "motion/react";
import { Monitor, Moon, Sun } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useHydrated } from "@/hooks/use-client-value";

const ORDER = ["light", "dark", "system"] as const;
const ICONS = { light: Sun, dark: Moon, system: Monitor };

export function ThemeToggle({ className }: { className?: string }) {
  const { theme, setTheme } = useTheme();
  // next-themes only knows the stored theme client-side; render "system" until then.
  const hydrated = useHydrated();

  const current = (hydrated && theme && theme in ICONS ? theme : "system") as keyof typeof ICONS;
  const Icon = ICONS[current];

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className={className}
          aria-label={`Theme: ${current}. Switch.`}
          onClick={() => setTheme(ORDER[(ORDER.indexOf(current) + 1) % ORDER.length])}
        >
          <AnimatePresence mode="wait" initial={false}>
            <motion.span
              key={current}
              initial={{ rotate: -90, opacity: 0, scale: 0.6 }}
              animate={{ rotate: 0, opacity: 1, scale: 1 }}
              exit={{ rotate: 90, opacity: 0, scale: 0.6 }}
              transition={{ duration: 0.18 }}
              className="flex"
            >
              <Icon className="size-4" />
            </motion.span>
          </AnimatePresence>
        </Button>
      </TooltipTrigger>
      <TooltipContent>Theme: {current}</TooltipContent>
    </Tooltip>
  );
}
