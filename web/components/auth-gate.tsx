"use client";

import { createContext, useCallback, useContext, useEffect, useSyncExternalStore } from "react";
import { AnimatePresence, motion } from "motion/react";
import {
  UNAUTHORIZED_EVENT,
  clearToken,
  getToken,
  logout,
  setToken,
  subscribeToken,
} from "@/lib/api";
import { LoginScreen } from "@/components/login-screen";
import { AppShell } from "@/components/app-shell";
import { Logo } from "@/components/brand";

const AuthContext = createContext<{ signOut: () => void }>({ signOut: () => {} });
export const useAuth = () => useContext(AuthContext);

// Static export: the first paint happens before localStorage is readable, so the
// pre-hydration snapshot is `undefined` and we hold a splash for that one render.
const NOT_HYDRATED = () => undefined;

export function AuthGate({ children }: { children: React.ReactNode }) {
  const token = useSyncExternalStore(subscribeToken, getToken, NOT_HYDRATED);

  // Drop the server-side session too, so signing out on a shared machine really ends it.
  // Fire-and-forget: the local token goes either way.
  const signOut = useCallback(() => {
    void logout();
    clearToken();
  }, []);
  const signIn = useCallback((value: string) => setToken(value), []);

  useEffect(() => {
    window.addEventListener(UNAUTHORIZED_EVENT, signOut);
    return () => window.removeEventListener(UNAUTHORIZED_EVENT, signOut);
  }, [signOut]);

  if (token === undefined) {
    return (
      <div className="flex min-h-svh items-center justify-center">
        <motion.div
          animate={{ opacity: [0.35, 1, 0.35] }}
          transition={{ duration: 1.4, repeat: Infinity }}
        >
          <Logo className="size-10" />
        </motion.div>
      </div>
    );
  }

  return (
    <AuthContext.Provider value={{ signOut }}>
      <AnimatePresence mode="wait" initial={false}>
        {token ? (
          <motion.div
            key="app"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            transition={{ duration: 0.25 }}
          >
            <AppShell>{children}</AppShell>
          </motion.div>
        ) : (
          <motion.div key="login" exit={{ opacity: 0 }} transition={{ duration: 0.15 }}>
            <LoginScreen onSignIn={signIn} />
          </motion.div>
        )}
      </AnimatePresence>
    </AuthContext.Provider>
  );
}
