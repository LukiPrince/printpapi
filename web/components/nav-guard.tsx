"use client";

import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import { usePathname, useRouter } from "next/navigation";
import { normalizePath } from "@/lib/nav";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";

/** Sent when the nav entry of the page you are on is clicked, so a page with an inner view (the
 *  label editor) can go back to its start. The page asks the guard before it does. */
export const NAV_RESELECT_EVENT = "printpapi:nav-reselect";

type Guard = {
  dirty: boolean;
  /** A page with unsaved work says so here. `discard` runs when the user lets it go. */
  setDirty: (dirty: boolean, discard?: () => void) => void;
  /** Run `go`, after the user agreed to throw the unsaved work away if there is any. */
  request: (go: () => void) => void;
  /** Go to a dashboard route the same way, for the sidebar and the command palette. */
  navigate: (href: string) => void;
};

const NO_GUARD: Guard = {
  dirty: false,
  setDirty: () => {},
  request: (go) => go(),
  navigate: () => {},
};

const GuardContext = createContext<Guard>(NO_GUARD);
export const useNavGuard = () => useContext(GuardContext);

/**
 * Keeps unsaved work from being thrown away by a click elsewhere: the sidebar, the command
 * palette, Sign out, and a reload or closing the tab. (The browser back button and a dead session
 * are not caught here; the editor keeps its draft in session storage for the latter.)
 */
export function NavGuardProvider({ children }: { children: React.ReactNode }) {
  const router = useRouter();
  const pathname = usePathname();
  const [dirty, setDirtyState] = useState(false);
  const [pending, setPending] = useState<{ go: () => void } | null>(null);
  const discard = useRef<(() => void) | null>(null);

  const setDirty = useCallback((value: boolean, onDiscard?: () => void) => {
    setDirtyState(value);
    discard.current = value ? (onDiscard ?? null) : null;
  }, []);

  const request = useCallback(
    (go: () => void) => {
      if (dirty) setPending({ go });
      else go();
    },
    [dirty],
  );

  const navigate = useCallback(
    (href: string) => {
      if (normalizePath(href) === normalizePath(pathname)) {
        window.dispatchEvent(new Event(NAV_RESELECT_EVENT));
      } else {
        request(() => router.push(href));
      }
    },
    [pathname, request, router],
  );

  useEffect(() => {
    if (!dirty) return;
    const warn = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [dirty]);

  const value = useMemo(
    () => ({ dirty, setDirty, request, navigate }),
    [dirty, setDirty, request, navigate],
  );

  function leave() {
    const go = pending?.go;
    discard.current?.();
    setDirtyState(false);
    setPending(null);
    go?.();
  }

  return (
    <GuardContext.Provider value={value}>
      {children}
      <AlertDialog open={pending !== null} onOpenChange={(open) => !open && setPending(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Discard your changes?</AlertDialogTitle>
            <AlertDialogDescription>
              This page has changes that are not saved. Leaving it throws them away.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Keep editing</AlertDialogCancel>
            <AlertDialogAction onClick={leave}>Discard</AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </GuardContext.Provider>
  );
}
