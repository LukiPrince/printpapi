// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
/** The terms a signup must accept, as `/health` reports them (only when the server requires consent). */
export type Terms = {
  version: string;
  terms_url: string;
  dpa_url: string | null;
  privacy_url: string | null;
};

/** The one link rule, twin of `is_terms_url` in crates/server/src/main.rs: `http://` or `https://` (any case), then a host, printable ASCII only (0x21-0x7E). */
export const isHttpUrl = (v: unknown): v is string =>
  typeof v === "string" && /^[!-~]+$/.test(v) && /^https?:\/\/[^/?#]/i.test(v);

const httpUrl = (v: unknown): string | null => (isHttpUrl(v) ? v : null);

/** `/health`'s `terms` key, or null when consent is not required (or the answer is unusable). */
export function parseTerms(raw: unknown): Terms | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const terms_url = httpUrl(r.terms_url);
  if (typeof r.version !== "string" || !r.version || !terms_url) return null;
  return { version: r.version, terms_url, dpa_url: httpUrl(r.dpa_url), privacy_url: httpUrl(r.privacy_url) };
}

/** What the form says for a refused signup: the server's wording, except a stale terms version. */
export function termsMessage(status: number, message: string): string {
  return status === 409 && message === "terms changed, reload"
    ? "The terms changed, please reload."
    : message;
}
