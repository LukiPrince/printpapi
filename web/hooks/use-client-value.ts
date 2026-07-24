"use client";

import { useSyncExternalStore } from "react";

const NEVER_CHANGES = () => () => {};

/**
 * Read a browser-only value (window.location, "am I hydrated") without a
 * setState-in-effect. The server snapshot renders first, then React re-renders
 * with the real one right after hydration.
 */
export function useClientValue<T>(read: () => T, fallback: T): T {
  return useSyncExternalStore(NEVER_CHANGES, read, () => fallback);
}

const TRUE = () => true;
const FALSE = () => false;

/** False during SSR/hydration, true afterwards. */
export function useHydrated(): boolean {
  return useSyncExternalStore(NEVER_CHANGES, TRUE, FALSE);
}
