"use client";

import { useCallback, useEffect, useRef, useState } from "react";

type PollState<T> = {
  data: T | undefined;
  error: string | null;
  loading: boolean;
  refresh: () => void;
};

/**
 * Fetch on mount, then re-fetch every `intervalMs` while `enabled`. Errors are
 * kept but stale data stays on screen, so a blip does not blank the dashboard.
 */
export function usePoll<T>(fetcher: () => Promise<T>, intervalMs = 5000, enabled = true): PollState<T> {
  const [data, setData] = useState<T>();
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);
  const ref = useRef(fetcher);

  const refresh = useCallback(() => setTick((t) => t + 1), []);

  // Keep the latest fetcher without making it an effect dependency — callers pass
  // an inline closure, so depending on it would restart the interval every render.
  useEffect(() => {
    ref.current = fetcher;
  }, [fetcher]);

  useEffect(() => {
    let alive = true;
    const run = async () => {
      try {
        const next = await ref.current();
        if (alive) {
          setData(next);
          setError(null);
        }
      } catch (e) {
        if (alive) setError(e instanceof Error ? e.message : String(e));
      } finally {
        if (alive) setLoading(false);
      }
    };
    run();
    if (!enabled || intervalMs <= 0) return () => void (alive = false);
    const id = setInterval(run, intervalMs);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, [intervalMs, enabled, tick]);

  return { data, error, loading, refresh };
}
