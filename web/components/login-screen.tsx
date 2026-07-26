"use client";

import { useState } from "react";
import { motion } from "motion/react";
import { ArrowRight, Eye, EyeOff, Loader2, ShieldCheck } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Barcode, Logo } from "@/components/brand";
import { ApiError, checkToken, login } from "@/lib/api";

const LABEL =
  "font-mono text-[0.68rem] font-semibold tracking-[0.14em] text-muted-foreground uppercase";

export function LoginScreen({ onSignIn }: { onSignIn: (token: string) => void }) {
  // Two ways in: an account (e-mail + password, minting a session) or a raw token pasted in
  // — the bootstrap PRINTAPI_TOKEN and per-client keys have no password to type.
  const [mode, setMode] = useState<"account" | "token">("account");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [value, setValue] = useState("");
  const [reveal, setReveal] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [shake, setShake] = useState(0);

  function fail(message: string) {
    setError(message);
    setShake((n) => n + 1);
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      if (mode === "account") {
        if (!email.trim() || !password) return;
        onSignIn((await login(email.trim(), password)).token);
      } else {
        const token = value.trim();
        if (!token) return;
        if (await checkToken(token)) onSignIn(token);
        else fail("That token was rejected.");
      }
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) fail("Wrong e-mail or password.");
      else if (err instanceof ApiError && err.status === 429) fail("Too many attempts. Wait a bit.");
      else fail("Cannot reach the server.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="relative flex min-h-svh items-center justify-center overflow-hidden px-4">
      {/* feed rollers: two slow amber washes, purely decorative */}
      <motion.div
        aria-hidden
        className="pointer-events-none absolute -top-40 -left-32 size-[28rem] rounded-full bg-brand/10 blur-3xl"
        animate={{ y: [0, 24, 0] }}
        transition={{ duration: 14, repeat: Infinity, ease: "easeInOut" }}
      />
      <motion.div
        aria-hidden
        className="pointer-events-none absolute -right-32 -bottom-40 size-[26rem] rounded-full bg-brand/10 blur-3xl"
        animate={{ y: [0, -20, 0] }}
        transition={{ duration: 16, repeat: Infinity, ease: "easeInOut" }}
      />

      <motion.form
        onSubmit={submit}
        key={shake}
        initial={{ opacity: 0, y: -18 }}
        animate={{ opacity: 1, y: 0, x: shake ? [0, -8, 8, -5, 0] : 0 }}
        transition={{ duration: 0.45, ease: [0.2, 0.8, 0.3, 1] }}
        className="relative z-10 w-full max-w-sm border-[1.5px] border-foreground bg-card p-6 shadow-[6px_6px_0_color-mix(in_oklch,var(--foreground),transparent_86%)]"
      >
        <div className="flex items-center gap-3">
          <Logo className="size-11" />
          <div>
            <div className="font-mono text-[0.65rem] font-semibold tracking-[0.22em] text-muted-foreground uppercase">
              Print bridge
            </div>
            <h1 className="font-mono text-2xl font-bold tracking-tight">printpapi</h1>
          </div>
        </div>

        <Barcode className="my-4 h-4 text-foreground" />

        {mode === "account" ? (
          <>
            <label htmlFor="email" className={LABEL}>
              E-mail
            </label>
            <Input
              id="email"
              autoFocus
              type="email"
              autoComplete="username"
              placeholder="you@yourshop.example"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              aria-invalid={!!error}
              className="mt-1.5 font-mono"
            />
            <label htmlFor="password" className={`${LABEL} mt-3 block`}>
              Password
            </label>
            <div className="mt-1.5 flex gap-1.5">
              <Input
                id="password"
                type={reveal ? "text" : "password"}
                autoComplete="current-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                aria-invalid={!!error}
                className="font-mono"
              />
              <Button
                type="button"
                variant="outline"
                size="icon"
                aria-label={reveal ? "Hide password" : "Show password"}
                onClick={() => setReveal((r) => !r)}
              >
                {reveal ? <EyeOff /> : <Eye />}
              </Button>
            </div>
          </>
        ) : (
          <>
            <label htmlFor="token" className={LABEL}>
              API token
            </label>
            <div className="mt-1.5 flex gap-1.5">
              <Input
                id="token"
                autoFocus
                type={reveal ? "text" : "password"}
                autoComplete="off"
                placeholder="PRINTAPI_TOKEN or a client key"
                value={value}
                onChange={(e) => setValue(e.target.value)}
                aria-invalid={!!error}
                className="font-mono"
              />
              <Button
                type="button"
                variant="outline"
                size="icon"
                aria-label={reveal ? "Hide token" : "Show token"}
                onClick={() => setReveal((r) => !r)}
              >
                {reveal ? <EyeOff /> : <Eye />}
              </Button>
            </div>
          </>
        )}

        <Button type="submit" variant="brand" size="lg" className="mt-4 w-full font-mono" disabled={busy}>
          {busy ? <Loader2 className="animate-spin" /> : <ArrowRight />}
          {busy ? "CONNECTING" : mode === "account" ? "SIGN IN" : "CONNECT"}
        </Button>

        <Button
          type="button"
          variant="ghost"
          size="xs"
          className="mt-2 w-full font-mono"
          onClick={() => {
            setMode((m) => (m === "account" ? "token" : "account"));
            setError("");
          }}
        >
          {mode === "account" ? "Use an API token instead" : "Sign in with an account"}
        </Button>

        {error ? (
          <motion.p
            initial={{ opacity: 0, height: 0 }}
            animate={{ opacity: 1, height: "auto" }}
            className="mt-3 text-sm text-destructive"
            role="alert"
          >
            {error}
          </motion.p>
        ) : (
          <p className="mt-3 flex items-start gap-1.5 text-xs text-muted-foreground">
            <ShieldCheck className="mt-px size-3.5 shrink-0" />
            Stored in this browser only. The page itself carries no secrets — every request is
            signed with your token.
          </p>
        )}
      </motion.form>
    </div>
  );
}
