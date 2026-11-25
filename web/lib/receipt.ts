// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// Receipt helpers of the dashboard. Pure (no api.ts, no DOM), so node --test can run them.

export type ParsedReceipt = { receipt: Record<string, unknown> } | { error: string };

/** The text of the Receipt tab as a receipt object. Only the JSON syntax and the outer shape are
 *  checked here; every other problem is the server's text (it knows the elements and the limits). */
export function parseReceiptText(text: string): ParsedReceipt {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch (e) {
    return { error: `That is not valid JSON: ${e instanceof Error ? e.message : String(e)}` };
  }
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return { error: 'The receipt must be a JSON object like {"content": [...]}.' };
  }
  return { receipt: value as Record<string, unknown> };
}

/** The receipt of the Receipt tab for a job: the object, or an Error in the parser's own words. */
export function receiptForJob(text: string): Record<string, unknown> {
  const parsed = parseReceiptText(text);
  if ("error" in parsed) throw new Error(parsed.error);
  return parsed.receipt;
}

/** What the preview box says while it has no picture: the text is not a receipt yet, the preview
 *  failed (the reason is shown above the box), or the picture is on its way. */
export function previewPlaceholder(valid: boolean, problem: string | null): string {
  if (!valid) return "Fix the JSON to see the receipt.";
  return problem === null ? "Drawing the receipt…" : "No preview.";
}

/** What the history row says when "Show receipt" fails, and whether the button stays: only a job
 *  that has no receipt (404) is final; a busy server (503) or a lost connection can be tried again. */
export function showReceiptFailure(status: number | null, message: string): { note: string; retry: boolean } {
  return status === 404 ? { note: "No receipt to show for this job.", retry: false } : { note: message, retry: true };
}

type ProfileLike = { paper_mm: number; width_dots: number; codepage: string; output: string };

/** The one muted line under the preview. `picked`: a printer is selected, so its settings (not
 *  the defaults) draw the preview; while they are unknown (loading, or a printer that has none)
 *  nothing is claimed about the paper. */
export function describeProfile(profile: ProfileLike | null, canPdf: boolean, picked: boolean): string {
  if (!profile) return picked ? "" : "Preview for 80 mm paper. Pick a printer to use its settings.";
  const chars = Math.floor(profile.width_dots / 12);
  const pdf = canPdf && profile.output === "auto";
  const base = `${profile.paper_mm} mm, ${chars} characters, ${pdf ? "sent as a PDF" : "ESC/POS"}`;
  // a PDF is made from the same dots as the preview, so only ESC/POS has the printer's own font
  return !pdf && profile.codepage === "cp1252" ? `${base}. The printer's own font looks slightly different.` : base;
}

/** Deep equality of two JSON values (object key order does not matter). */
export function sameJson(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a);
  const kb = Object.keys(b);
  if (ka.length !== kb.length) return false;
  return ka.every((k) => k in b && sameJson((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k]));
}

export type Previewer<B> = { request(body: B): void; dispose(): void };

/** Sends preview requests one at a time. While one is in flight, a newer body waits (only the
 *  newest is kept) and goes out when the answer is there; a busy answer is retried silently after
 *  `retryMs`. The last good image stays with the caller while an error is shown. */
export function createPreviewer<B>(opts: {
  send: (body: B) => Promise<string>;
  onImage: (url: string) => void;
  onError: (message: string) => void;
  isBusy: (error: unknown) => boolean;
  retryMs?: number;
}): Previewer<B> {
  const retryMs = opts.retryMs ?? 1000;
  let inFlight = false;
  let waiting: { body: B } | null = null;
  let retry: ReturnType<typeof setTimeout> | null = null;
  let dead = false;

  const next = () => {
    inFlight = false;
    if (dead || waiting === null) return;
    const { body } = waiting;
    waiting = null;
    go(body);
  };

  const go = (body: B) => {
    inFlight = true;
    opts.send(body).then(
      (url) => {
        if (dead) return;
        opts.onImage(url);
        next();
      },
      (e: unknown) => {
        if (dead) return;
        if (opts.isBusy(e)) {
          // the server asked for a moment: try this body again unless a newer one arrived
          waiting ??= { body };
          retry = setTimeout(() => {
            retry = null;
            next();
          }, retryMs);
          return;
        }
        opts.onError(e instanceof Error ? e.message : String(e));
        next();
      },
    );
  };

  return {
    request(body) {
      if (dead) return;
      if (inFlight) {
        waiting = { body };
        return;
      }
      go(body);
    },
    dispose() {
      dead = true;
      if (retry !== null) clearTimeout(retry);
    },
  };
}
