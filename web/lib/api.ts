// Thin client for the printpapi HTTP API. The token lives in localStorage and is
// sent as a bearer header on every call — the static shell itself carries no secrets.

import { getToken } from "./token";
import { parseTerms, type Terms } from "./terms";
import type { SupportAccess } from "./support-access";

export { TOKEN_KEY, clearToken, getToken, setToken, subscribeToken } from "./token";
export const UNAUTHORIZED_EVENT = "printpapi:unauthorized";

export type JobState = "queued" | "claimed" | "done" | "failed" | "cancelled";

export type Printer = {
  id: number;
  name: string;
  agent_id: number;
  agent_name: string;
  can_pdf: boolean;
  online: boolean;
};

/** An agent machine. `printers` is a count here — the devices page pairs it with /printers. */
export type Computer = {
  id: number;
  name: string;
  online: boolean;
  last_seen_at: number | null;
  created_at: number;
  printers: number;
};

export type Job = {
  id: number;
  printer_id: number;
  printer_name: string;
  agent_name: string;
  title: string | null;
  state: JobState;
  type: string;
  mode: string;
  error: string | null;
  created_at: number;
  finished_at: number | null;
};

export type ApiKey = {
  id: number;
  label: string;
  active: number;
  created_at: number;
};

/** Who the stored credential belongs to. `session` is an account login, `key` a machine key. */
export type Me = {
  kind: "root" | "session" | "key";
  org_id: number | null;
  email?: string;
  user_id?: number;
};

export type OrgUser = {
  id: number;
  org_id: number;
  email: string;
  created_at: number;
};

/** One org's own settings. The Shopify secret is only ever reported as a flag. */
export type Org = {
  id: number;
  name: string;
  event_url: string | null;
  shopify_secret_set: boolean;
  job_quota: number | null;
  plan: string | null;
  jobs_this_month: number;
  created_at: number;
};

/** A billing plan from the operator's catalogue. `jobs` null = unlimited; the checkout URL
 *  already carries this org's id, so the provider's webhook can name it back. */
export type Plan = {
  id: string;
  name: string;
  jobs: number | null;
  price: string | number | null;
  checkout_url: string | null;
};

/** What this server offers before anyone is signed in — read from /health. */
export type { Terms };
export type ServerInfo = {
  signup: "open" | "closed";
  password_reset: boolean;
  /** Present only when signup requires accepting terms. */
  terms?: Terms | null;
};

export type Metrics = {
  jobs: Record<JobState, number>;
  agents_online: number;
  agents_total: number;
  printers_total: number;
};

export type NewJob = {
  printer_id: number;
  type:
    | "pdf_base64"
    | "raw_base64"
    | "pdf_uri"
    | "raw_uri"
    | "label_uri"
    | "label_base64"
    | "label_text";
  content?: string;
  url?: string;
  /** label_*: a label template by name or id. */
  template?: string | number;
  /** label_text: the text to print. */
  text?: string;
  title?: string | null;
  copies?: number;
  callback_url?: string | null;
};

/** A box on the label in millimetres, measured from its top-left corner. */
export type LabelBox = { x: number; y: number; w: number; h: number };

/** A label template as the server stores it: where the content sits on one kind of label. */
export type LabelSpec = {
  name: string;
  content: "image" | "text";
  label_mm: { w: number; h: number };
  dpi: number;
  /** Clockwise, applied before the crop. */
  rotate: 0 | 90 | 180 | 270;
  crop_percent: { t: number; r: number; b: number; l: number };
  place_mm: LabelBox;
  text?: { wrap_chars: number; lines_per_label: number };
  zpl_darkness?: number;
  /** Which catalogue size the editor started from. The server only passes it through. */
  size_ref?: string;
};

export type LabelTemplate = LabelSpec & { id: number; created_at: number; updated_at: number };

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

async function request(path: string, init: RequestInit = {}, token = getToken()): Promise<Response> {
  const res = await fetch(path, {
    ...init,
    headers: { ...init.headers, Authorization: `Bearer ${token}` },
  });
  if (res.status === 401) {
    // /apikeys, /users, /orgs and /support-access need an account (or the bootstrap token), so a
    // 401 there is "this credential cannot manage the org", not "session dead". Only a rejected
    // token on a client endpoint signs you out.
    if (
      !path.startsWith("/apikeys") &&
      !path.startsWith("/users") &&
      !path.startsWith("/orgs") &&
      !path.startsWith("/support-access")
    ) {
      window.dispatchEvent(new CustomEvent(UNAUTHORIZED_EVENT));
    }
    throw new ApiError(401, "unauthorized");
  }
  if (!res.ok) {
    let detail = `request failed (${res.status})`;
    try {
      const body = await res.json();
      if (body?.error) detail = String(body.error);
    } catch {
      /* non-JSON error body — keep the status line */
    }
    throw new ApiError(res.status, detail);
  }
  return res;
}

async function getJSON<T>(path: string): Promise<T> {
  return (await request(path)).json() as Promise<T>;
}

const jsonBody = (body: unknown): RequestInit => ({
  method: "POST",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify(body),
});

export async function checkToken(token: string): Promise<boolean> {
  const res = await fetch("/printers", { headers: { Authorization: `Bearer ${token}` } });
  // Only a real 2xx counts. Anything else (a 404 from something that isn't this API, a 5xx)
  // must not be mistaken for a good token — otherwise you get "signed in" to nothing.
  return res.ok;
}

/** Exchange e-mail + password for a session token. Sent without a bearer header — it *is* the
 *  call that mints one. A wrong password throws ApiError(401). */
export async function login(email: string, password: string) {
  const res = await fetch("/login", jsonBody({ email, password }));
  const body = await res.json().catch(() => ({}));
  if (!res.ok) {
    throw new ApiError(res.status, body?.error ?? `login failed (${res.status})`);
  }
  return body as { token: string; expires_at: number; org_id: number; user_id: number };
}

/** Unauthenticated calls: the sign-in screen makes them before any token exists. `error` from the
 *  body is preserved so the server's own wording ("signup is disabled…") reaches the user. */
async function open_(path: string, body: unknown) {
  const res = await fetch(path, jsonBody(body));
  const parsed = await res.json().catch(() => ({}));
  if (!res.ok) throw new ApiError(res.status, parsed?.error ?? `request failed (${res.status})`);
  return parsed;
}

/** What this server allows before sign-in. Never throws — a server too old to report these
 *  simply offers neither. */
export async function getServerInfo(): Promise<ServerInfo> {
  let body: Partial<ServerInfo> = {};
  try {
    const res = await fetch("/health");
    if (res.ok) body = (await res.json()) as Partial<ServerInfo>;
  } catch {
    /* unreachable or too old to answer — advertise neither door */
  }
  return {
    signup: body.signup === "open" ? "open" : "closed",
    password_reset: !!body.password_reset,
    terms: parseTerms(body.terms),
  };
}

/** Self-serve: a new org, its first account, and a session token in one call. */
export const signup = (email: string, password: string, orgName: string, acceptTerms?: string) =>
  open_("/signup", {
    email,
    password,
    org_name: orgName,
    ...(acceptTerms ? { accept_terms: acceptTerms } : {}),
  }) as Promise<{
    token: string;
    org_id: number;
    user_id: number;
    email: string;
  }>;

/** Always resolves for a well-formed address, whether or not it has an account. */
export const requestPasswordReset = (email: string) => open_("/password/reset", { email });

export const confirmPasswordReset = (token: string, password: string) =>
  open_("/password/reset/confirm", { token, password });

export const deleteUser = (id: number) => request(`/users/${id}`, { method: "DELETE" });

export const getOrg = (id: number) => getJSON<Org>(`/orgs/${id}`);

/** Operator support access to an org: the grants so far. Root names the org; a session's own is implied. */
export const getSupportAccess = (orgId?: number) =>
  getJSON<SupportAccess>(orgId === undefined ? "/support-access" : `/support-access?org=${orgId}`);

/** Root only — every org on the server, for the settings page's org picker. */
export const listOrgs = () => getJSON<{ orgs: Org[] }>("/orgs").then((r) => r.orgs ?? []);

export const updateOrg = (
  id: number,
  patch: {
    event_url?: string | null;
    shopify_secret?: string | null;
    job_quota?: number | null;
    plan?: string;
  },
) => request(`/orgs/${id}`, { ...jsonBody(patch), method: "PUT" });

/** The plan catalogue plus the caller's current plan. Empty on a server without billing. */
export const listPlans = () => getJSON<{ plans: Plan[]; current: string | null }>("/plans");

/** Best-effort server-side session teardown. Raw fetch: a 401 here must not trigger the
 *  global unauthorized handler — signing out is exactly what the caller is already doing. */
export async function logout() {
  const token = getToken();
  if (!token.startsWith("sess_")) return;
  await fetch("/logout", { method: "POST", headers: { Authorization: `Bearer ${token}` } }).catch(
    () => {},
  );
}

export const getMe = () => getJSON<Me>("/me");

export const listUsers = () => getJSON<{ users: OrgUser[] }>("/users").then((r) => r.users ?? []);

export const createUser = (email: string, password: string) =>
  request("/users", jsonBody({ email, password })).then(
    (r) => r.json() as Promise<{ id: number; email: string; org_id: number }>,
  );

export const changePassword = (current: string, next: string) =>
  request("/me/password", { ...jsonBody({ current, new: next }), method: "PUT" });

export const listPrinters = () =>
  getJSON<{ printers: Printer[] }>("/printers").then((r) => r.printers ?? []);

export const listComputers = () =>
  getJSON<{ computers: Computer[] }>("/computers").then((r) => r.computers ?? []);

export const listJobs = () => getJSON<{ jobs: Job[] }>("/jobs").then((r) => r.jobs ?? []);

export const listApiKeys = () =>
  getJSON<{ keys: ApiKey[] }>("/apikeys").then((r) => r.keys ?? []);

export const createJob = (job: NewJob) =>
  request("/jobs", jsonBody(job)).then((r) => r.json() as Promise<{ job_id: number }>);

export const cancelJob = (id: number) => request(`/jobs/${id}`, { method: "DELETE" });

export const issueApiKey = (label: string) =>
  request("/apikeys", jsonBody({ label })).then(
    (r) => r.json() as Promise<{ id: number; label: string; key: string }>,
  );

export const revokeApiKey = (id: number) => request(`/apikeys/${id}`, { method: "DELETE" });

export const listLabels = () =>
  getJSON<{ templates: LabelTemplate[] }>("/labels").then((r) => r.templates ?? []);

export const createLabel = (spec: LabelSpec) =>
  request("/labels", jsonBody(spec)).then((r) => r.json() as Promise<LabelTemplate>);

export const updateLabel = (id: number, spec: LabelSpec) =>
  request(`/labels/${id}`, { ...jsonBody(spec), method: "PUT" }).then(
    (r) => r.json() as Promise<LabelTemplate>,
  );

export const deleteLabel = (id: number) =>
  request(`/labels/${id}`, { method: "DELETE" }).then(() => undefined);

const EMPTY_JOBS: Record<JobState, number> = {
  queued: 0,
  claimed: 0,
  done: 0,
  failed: 0,
  cancelled: 0,
};

/** Parse the Prometheus text exposition from GET /metrics into a plain object. */
export function parseMetrics(text: string): Metrics {
  const out: Metrics = {
    jobs: { ...EMPTY_JOBS },
    agents_online: 0,
    agents_total: 0,
    printers_total: 0,
  };
  for (const line of text.split("\n")) {
    if (!line || line.startsWith("#")) continue;
    const [name, raw] = line.split(/\s+/);
    const value = Number(raw);
    if (!Number.isFinite(value)) continue;
    const job = /^printpapi_jobs\{state="(\w+)"\}$/.exec(name);
    if (job) {
      out.jobs[job[1] as JobState] = value;
    } else if (name === "printpapi_agents_online") out.agents_online = value;
    else if (name === "printpapi_agents_total") out.agents_total = value;
    else if (name === "printpapi_printers_total") out.printers_total = value;
  }
  return out;
}

export const getMetrics = () => request("/metrics").then((r) => r.text()).then(parseMetrics);

/** Read a File as bare base64 (no data: prefix) for the *_base64 job types. */
export function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result).split(",", 2)[1]);
    reader.onerror = () => reject(new Error("could not read file"));
    reader.readAsDataURL(file);
  });
}
