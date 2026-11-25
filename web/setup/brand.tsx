// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { createContext, useContext, useEffect, useState } from "react";
import { ExternalLink } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle, DialogTrigger } from "@/components/ui/dialog";
import { ScrollArea } from "@/components/ui/scroll-area";
import { setup, toSetupError, type BrandView } from "./api";
import { errorText, pngDataUri } from "./model";

/**
 * What the window knows about the brand of this build.
 * - `ready`: the shell has answered (a stock build answers `null`). Until then nothing that
 *   starts from the brand (the server field) may be mounted.
 * - `brand`: null for the stock build.
 * - `icon`: the brand's icon as a data address.
 * - `error`: the shell could not say; the window then shows no stock page that looks fine.
 */
export type BrandState = { ready: boolean; brand: BrandView | null; icon: string | null; error: string | null };

const NONE: BrandState = { ready: false, brand: null, icon: null, error: null };
const BrandContext = createContext<BrandState>(NONE);

export function useBrand(): BrandState {
  return useContext(BrandContext);
}

/**
 * Asks the shell for the brand once. Its children are drawn at once with no brand, so the first
 * paint of the stock window is what it always was; a white-label build starts its window hidden
 * and this provider shows it (`show_window`) after the page has rendered the brand, or after the
 * failure notice. The stock build never hides anything and never calls `show_window`.
 */
export function BrandProvider({ children }: { children: React.ReactNode }) {
  const [state, setState] = useState<BrandState>(NONE);

  useEffect(() => {
    let live = true;
    (async () => {
      try {
        const brand = await setup.getBrand();
        const icon = brand ? pngDataUri(await setup.brandIcon()) : null;
        if (live) setState({ ready: true, brand, icon, error: null });
      } catch (e) {
        if (live) setState({ ready: true, brand: null, icon: null, error: errorText(toSetupError(e)) });
      }
    })();
    return () => {
      live = false;
    };
  }, []);

  // after the commit that draws the brand (or the failure): the window has been hidden until now
  useEffect(() => {
    if (state.brand || state.error) void setup.showWindow().catch(() => undefined);
  }, [state.brand, state.error]);

  return <BrandContext.Provider value={state}>{children}</BrandContext.Provider>;
}

/** The icon and the name at the top of the sidebar. The label may take two lines and break inside a word. */
export function BrandMark({ brand, icon }: { brand: BrandView; icon: string | null }) {
  return (
    <div className="flex items-center gap-2.5 px-1">
      {/* a data address from the shell: the framework's image component has nothing to optimise */}
      {/* eslint-disable-next-line @next/next/no-img-element */}
      {icon && <img src={icon} alt="" className="size-7 shrink-0 rounded-md" />}
      <span className="min-w-0 break-words text-sm leading-tight font-bold tracking-tight text-white">{brand.name}</span>
    </div>
  );
}

/**
 * The licence notice at the foot of the sidebar of a white-label build, on every screen. The text
 * is the shell's, not the brand's. It opens About: the notice, the client, the support page and the
 * whole licence.
 */
export function BrandFooter({ brand }: { brand: BrandView }) {
  const [license, setLicense] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);

  async function load(open: boolean) {
    if (!open || license !== null) return;
    try {
      setLicense(await setup.licenseText());
    } catch (e) {
      setProblem(errorText(toSetupError(e)));
    }
  }

  return (
    <Dialog onOpenChange={(open) => void load(open)}>
      <DialogTrigger asChild>
        <button
          type="button"
          className="mt-auto min-w-0 rounded-md px-1 py-1 text-left [overflow-wrap:anywhere] text-[11px] leading-snug text-sidebar-foreground/70 underline-offset-2 outline-none hover:text-sidebar-foreground hover:underline focus-visible:ring-2 focus-visible:ring-sidebar-ring"
        >
          {brand.notice}
        </button>
      </DialogTrigger>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>About {brand.name}</DialogTitle>
          <DialogDescription className="[overflow-wrap:anywhere]">{brand.notice}</DialogDescription>
        </DialogHeader>
        {brand.hasSupport && (
          <div>
            <Button type="button" variant="outline" size="sm" onClick={() => void setup.openSupport().catch(() => undefined)}>
              <ExternalLink />
              Support
            </Button>
          </div>
        )}
        {problem && <p className="text-sm text-state-failed">{problem}</p>}
        <ScrollArea className="h-64 rounded-md border">
          <pre className="whitespace-pre-wrap p-3 font-mono text-xs">{license ?? "Loading the licence…"}</pre>
        </ScrollArea>
      </DialogContent>
    </Dialog>
  );
}
