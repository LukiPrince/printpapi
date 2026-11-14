// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// Pure formatting for the "Support access" card on the settings page.

export type SupportGrant = {
  id: number;
  reason: string;
  granted_at: number;
  expires_at: number;
  ended_at: number | null;
  active: boolean;
  listed_job_ids: number[];
  opened_job_ids: number[];
};

export type SupportAccess = { enabled: boolean; grants: SupportGrant[] };

/** The window that was granted, in minutes (not how long it was actually used). */
export const grantMinutes = (g: SupportGrant) => Math.round((g.expires_at - g.granted_at) / 60);

export function grantOutcome(g: SupportGrant): "active" | "ended early" | "expired" {
  if (g.active) return "active";
  return g.ended_at !== null ? "ended early" : "expired";
}

export const activeGrant = (grants: SupportGrant[]) => grants.find((g) => g.active) ?? null;

const SHOWN = 5;

/** "#1, #2, #3", or the first few and a count; "none" when empty. */
export function idsSummary(ids: number[]): string {
  if (ids.length === 0) return "none";
  const head = ids.slice(0, SHOWN).map((id) => `#${id}`).join(", ");
  return ids.length > SHOWN ? `${head} and ${ids.length - SHOWN} more` : head;
}

/** Hidden when the server has the feature off and nothing ever happened. */
export const showSupportCard = (s: SupportAccess) => s.enabled || s.grants.length > 0;
