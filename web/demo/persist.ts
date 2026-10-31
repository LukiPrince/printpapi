// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// Turning the demo state into a string and back. Anything old, corrupt or foreign becomes a fresh seed.
import { seedState } from "./seed.ts";
import type { DemoState } from "./seed.ts";

export const STORAGE_KEY = "printpapi-demo-v1";

const ARRAYS = ["agents", "printers", "jobs", "outputs", "keys", "users"] as const;

export function loadState(raw: string | null, now: number): DemoState {
  if (raw === null) return seedState(now);
  try {
    const s = JSON.parse(raw) as Record<string, unknown> | null;
    if (!s || typeof s !== "object" || s.v !== 1) return seedState(now);
    if (!ARRAYS.every((k) => Array.isArray(s[k]))) return seedState(now);
    if (!s.org || !s.me || !s.next || !s.todo) return seedState(now);
    s.trimmed ??= { done: 0, failed: 0, cancelled: 0, since_seed: 0 }; // states stored before trimmed existed
    return s as unknown as DemoState;
  } catch {
    return seedState(now);
  }
}

export function saveState(s: DemoState): string {
  return JSON.stringify(s);
}
