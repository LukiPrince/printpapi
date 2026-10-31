// The session token store, apart from api.ts so node --test can load it. localStorage is the
// truth (that is how another tab signing out reaches this one); memory only holds a token whose
// storage write failed (blocked site data, private modes).
export const TOKEN_KEY = "printpapi_token";

const tokenListeners = new Set<() => void>();
let memoryToken: string | null = null;

export function getToken(): string {
  if (typeof window === "undefined") return "";
  try {
    return memoryToken ?? window.localStorage.getItem(TOKEN_KEY) ?? "";
  } catch {
    return memoryToken ?? "";
  }
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
  try {
    window.localStorage.setItem(TOKEN_KEY, token);
    memoryToken = null;
  } catch {
    memoryToken = token;
  }
  tokenListeners.forEach((cb) => cb());
}

export function clearToken() {
  memoryToken = null;
  try {
    window.localStorage.removeItem(TOKEN_KEY);
  } catch {
    // blocked storage: memory is already cleared
  }
  tokenListeners.forEach((cb) => cb());
}
