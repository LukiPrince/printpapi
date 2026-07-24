/** Unix seconds -> local datetime string. */
export function fmtTime(ts: number | null | undefined): string {
  return ts ? new Date(ts * 1000).toLocaleString() : "—";
}

/** Unix seconds -> "just now" / "4m ago" / "3h ago" / "2d ago". */
export function fmtAgo(ts: number | null | undefined, now = Date.now()): string {
  if (!ts) return "—";
  const s = Math.max(0, Math.round(now / 1000 - ts));
  if (s < 45) return "just now";
  const units: [number, string][] = [
    [60, "m"],
    [3600, "h"],
    [86400, "d"],
  ];
  if (s < 3600) return `${Math.round(s / units[0][0])}m ago`;
  if (s < 86400) return `${Math.round(s / units[1][0])}h ago`;
  return `${Math.round(s / units[2][0])}d ago`;
}

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** Duration between two unix timestamps, for finished jobs. */
export function fmtDuration(from: number, to: number | null | undefined): string {
  if (!to) return "—";
  const s = Math.max(0, to - from);
  return s < 1 ? "<1s" : s < 60 ? `${s.toFixed(1)}s` : `${Math.round(s / 60)}m`;
}
