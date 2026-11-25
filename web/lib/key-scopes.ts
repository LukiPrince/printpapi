// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The rights a key can carry (the server's auth::Scope) and what the sign-in does with them.
// Pure on purpose: node --test runs it without the app.

export type KeyScope = "full" | "agent" | "cloudprnt" | "print" | "read";

export const KEY_SCOPES: { value: KeyScope; label: string; hint: string }[] = [
  {
    value: "full",
    label: "Everything",
    hint: "Print, read, label templates and agents. Like every key so far.",
  },
  {
    value: "agent",
    label: "Agent only",
    hint: "For one computer with printers. Cannot submit jobs.",
  },
  {
    value: "cloudprnt",
    label: "CloudPRNT only",
    hint: "For Star printers that poll by themselves. Reaches all of them in your org.",
  },
  {
    value: "print",
    label: "Print only",
    hint: "Submits jobs and sees only its own. For a shop, n8n or a script.",
  },
  {
    value: "read",
    label: "Read only",
    hint: "Sees jobs, printers and metrics. Prints nothing.",
  },
];

/** What a key's rights are called. No scope (a full key, or an older server) is Everything; a word
 *  a later server invented is shown as it is. */
export function scopeLabel(scope?: string): string {
  if (scope === undefined) return "Everything";
  return KEY_SCOPES.find((s) => s.value === scope)?.label ?? scope;
}

export function scopeHint(scope?: string): string {
  return KEY_SCOPES.find((s) => s.value === (scope ?? "full"))?.hint ?? "";
}

/** What the rights dialog starts on: the key's own right. A word this server does not know (a newer
 *  server stored it, and this one denies it everything) starts on nothing, so widening it is a
 *  choice and not a default. */
export function initialEditScope(scope?: string): KeyScope | "" {
  if (scope === undefined) return "full";
  return KEY_SCOPES.some((s) => s.value === scope) ? (scope as KeyScope) : "";
}

/** Save is for a choice that differs from what is stored; nothing chosen never saves. */
export function canSaveScope(stored: string | undefined, chosen: KeyScope | ""): boolean {
  return chosen !== "" && chosen !== (stored ?? "full");
}

export type SignInCheck = "ok" | "limited" | "rejected";

/** Reads the answer of GET /me. Only parsed JSON with a known kind signs in (a 200 page that is
 *  not this API must not); a key whose rights are not "everything" or "read" cannot open the
 *  dashboard, which reads and writes. */
export function signInCheck(me: unknown): SignInCheck {
  if (typeof me !== "object" || me === null || Array.isArray(me)) return "rejected";
  const { kind, scope } = me as { kind?: unknown; scope?: unknown };
  if (kind !== "root" && kind !== "session" && kind !== "key") return "rejected";
  if (kind === "key" && scope !== undefined && scope !== "full" && scope !== "read") {
    return "limited";
  }
  return "ok";
}
