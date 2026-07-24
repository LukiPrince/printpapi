// Thin client for the printpapi HTTP API. The token lives in localStorage and is
// sent as a bearer header on every call — the static shell itself carries no secrets.

export const TOKEN_KEY = "printpapi_token";
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

export type Metrics = {
  jobs: Record<JobState, number>;
  agents_online: number;
  agents_total: number;
  printers_total: number;
};

export type NewJob = {
  printer_id: number;
  type: "pdf_base64" | "raw_base64" | "pdf_uri" | "raw_uri";
  content?: string;
  url?: string;
  title?: string | null;
  copies?: number;
  callback_url?: string | null;
};

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

const tokenListeners = new Set<() => void>();

export function getToken(): string {
  if (typeof window === "undefined") return "";
  return window.localStorage.getItem(TOKEN_KEY) ?? "";
}

/** Subscribe to token changes — from this tab, or from another one signing out. */
export function subscribeToken(onChange: () => void) {
  tokenListeners.add(onChange);
  window.addEventListener("storage", onChange);
  return () => {
    tokenListeners.delete(onChange);
    window.removeEventListener("storage", onChange);
  };
}

export function setToken(token: string) {
  window.localStorage.setItem(TOKEN_KEY, token);
  tokenListeners.forEach((cb) => cb());
}

export function clearToken() {
  window.localStorage.removeItem(TOKEN_KEY);
  tokenListeners.forEach((cb) => cb());
}

async function request(path: string, init: RequestInit = {}, token = getToken()): Promise<Response> {
  const res = await fetch(path, {
    ...init,
    headers: { ...init.headers, Authorization: `Bearer ${token}` },
  });
  if (res.status === 401) {
    // /apikeys is admin-only, so a 401 there is "wrong token for this page", not
    // "session dead". Only a rejected token on a client endpoint signs you out.
    if (!path.startsWith("/apikeys")) {
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

export const listPrinters = () =>
  getJSON<{ printers: Printer[] }>("/printers").then((r) => r.printers ?? []);

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
