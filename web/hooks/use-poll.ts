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
 *
 * `key` names what the fetcher fetches (an org id, say). The fetcher itself is no
 * dependency (see below), so a fetcher that changes with its inputs must pass them
 * as `key`: a new key fetches at once and drops the old data, so one org's answer
 * never shows under another's name.
 */
export function usePoll<T>(
  fetcher: () => Promise<T>,
  intervalMs = 5000,
  enabled = true,
  key: string | number | null = null,
): PollState<T> {
  const [data, setData] = useState<T>();
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);
  const ref = useRef(fetcher);
  const fetchedKey = useRef(key);

  const refresh = useCallback(() => setTick((t) => t + 1), []);

  // Keep the latest fetcher without making it an effect dependency — callers pass
  // an inline closure, so depending on it would restart the interval every render.
  useEffect(() => {
    ref.current = fetcher;
  }, [fetcher]);

  useEffect(() => {
    let alive = true;
    if (fetchedKey.current !== key) {
      fetchedKey.current = key;
      setData(undefined);
      setLoading(true);
    }
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
  }, [intervalMs, enabled, tick, key]);

  return { data, error, loading, refresh };
}
