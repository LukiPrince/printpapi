"use client";

import { useEffect, useState } from "react";
import { motion } from "motion/react";
import { ArrowRight, Eye, EyeOff, Loader2, MailCheck, ShieldCheck } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Barcode, Logo } from "@/components/brand";
import {
  ApiError,
  checkToken,
  confirmPasswordReset,
  getServerInfo,
  login,
  requestPasswordReset,
  signup,
  type ServerInfo,
} from "@/lib/api";

const LABEL =
  "font-mono text-[0.68rem] font-semibold tracking-[0.14em] text-muted-foreground uppercase";

/** account = sign in, token = paste a key, signup = new org, forgot = ask for a reset mail,
 *  reset = spend the token from that mail (the ?reset= query lands here). */
type Mode = "account" | "token" | "signup" | "forgot" | "reset";

const ACTION: Record<Mode, string> = {
  account: "SIGN IN",
  token: "CONNECT",
  signup: "CREATE ACCOUNT",
  forgot: "SEND RESET LINK",
  reset: "SET PASSWORD",
};

export function LoginScreen({ onSignIn }: { onSignIn: (token: string) => void }) {
  // Two ways in: an account (e-mail + password, minting a session) or a raw token pasted in
  // — the bootstrap PRINTAPI_TOKEN and per-client keys have no password to type.
  // A reset mail links to /?reset=<token>: read it once, and open on the new-password form.
  // Safe at first render — AuthGate only mounts this after hydration.
  const [resetToken] = useState(
    () => new URLSearchParams(window.location.search).get("reset") ?? "",
  );
  const [mode, setMode] = useState<Mode>(resetToken ? "reset" : "account");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [orgName, setOrgName] = useState("");
  const [value, setValue] = useState("");
  const [reveal, setReveal] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [shake, setShake] = useState(0);
  const [info, setInfo] = useState<ServerInfo>({ signup: "closed", password_reset: false });

  // What this server actually offers. Both default to off, so a server that does not answer
  // never advertises a door that is not there.
  useEffect(() => {
    void getServerInfo().then(setInfo);
  }, []);

  function go(next: Mode) {
    setMode(next);
    setError("");
    setNotice("");
  }

  function fail(message: string) {
    setError(message);
    setNotice("");
    setShake((n) => n + 1);
  }

  function clearResetQuery() {
    window.history.replaceState({}, "", window.location.pathname);
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
      } else if (mode === "signup") {
        onSignIn((await signup(email.trim(), password, orgName.trim())).token);
      } else if (mode === "forgot") {
        await requestPasswordReset(email.trim());
        // Deliberately the same answer for an address with no account.
        setNotice("If that address has an account, a reset link is on its way.");
      } else if (mode === "reset") {
        await confirmPasswordReset(resetToken, password);
        clearResetQuery();
        setPassword("");
        setMode("account");
        setNotice("Password set. Sign in with it.");
      } else {
        const token = value.trim();
        if (!token) return;
        if (await checkToken(token)) onSignIn(token);
        else fail("That token was rejected.");
      }
    } catch (err) {
      const status = err instanceof ApiError ? err.status : 0;
      if (status === 401) fail("Wrong e-mail or password.");
      else if (status === 429) fail("Too many attempts. Wait a bit.");
      else if (err instanceof ApiError) fail(err.message);
      else fail("Cannot reach the server.");
    } finally {
      setBusy(false);
    }
  }

  const emailField = (
    <>
      <label htmlFor="email" className={LABEL}>
        E-mail
      </label>
      <Input
        id="email"
        autoFocus
        type="email"
        required
        autoComplete="username"
        placeholder="you@yourshop.example"
        value={email}
        onChange={(e) => setEmail(e.target.value)}
        aria-invalid={!!error}
        className="mt-1.5 font-mono"
      />
    </>
  );

  const passwordField = (label: string, autoComplete: string, min?: number) => (
    <>
      <label htmlFor="password" className={`${LABEL} mt-3 block`}>
        {label}
      </label>
      <div className="mt-1.5 flex gap-1.5">
        <Input
          id="password"
          type={reveal ? "text" : "password"}
          required
          minLength={min}
          autoComplete={autoComplete}
          placeholder={min ? "at least 8 characters" : undefined}
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
  );

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

        {mode === "account" && (
          <>
            {emailField}
            {passwordField("Password", "current-password")}
          </>
        )}

        {mode === "signup" && (
          <>
            <label htmlFor="org" className={LABEL}>
              Company
            </label>
            <Input
              id="org"
              autoFocus
              autoComplete="organization"
              placeholder="Your shop"
              value={orgName}
              onChange={(e) => setOrgName(e.target.value)}
              className="mt-1.5 font-mono"
            />
            <label htmlFor="email" className={`${LABEL} mt-3 block`}>
              E-mail
            </label>
            <Input
              id="email"
              type="email"
              required
              autoComplete="username"
              placeholder="you@yourshop.example"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              aria-invalid={!!error}
              className="mt-1.5 font-mono"
            />
            {passwordField("Password", "new-password", 8)}
          </>
        )}

        {mode === "forgot" && (
          <>
            {emailField}
            <p className="mt-3 text-xs text-muted-foreground">
              We mail you a one-shot link. Setting a new password signs out every browser.
            </p>
          </>
        )}

        {mode === "reset" && passwordField("New password", "new-password", 8)}

        {mode === "token" && (
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
          {busy ? "WORKING" : ACTION[mode]}
        </Button>

        <div className="mt-2 flex flex-wrap justify-center gap-x-1">
          {mode === "account" && info.signup === "open" && (
            <Button type="button" variant="ghost" size="xs" className="font-mono" onClick={() => go("signup")}>
              Create an account
            </Button>
          )}
          {mode === "account" && info.password_reset && (
            <Button type="button" variant="ghost" size="xs" className="font-mono" onClick={() => go("forgot")}>
              Forgot password?
            </Button>
          )}
          <Button
            type="button"
            variant="ghost"
            size="xs"
            className="font-mono"
            onClick={() => {
              if (mode === "reset") clearResetQuery();
              go(mode === "account" ? "token" : "account");
            }}
          >
            {mode === "account" ? "Use an API token instead" : "Back to sign in"}
          </Button>
        </div>

        {error ? (
          <motion.p
            initial={{ opacity: 0, height: 0 }}
            animate={{ opacity: 1, height: "auto" }}
            className="mt-3 text-sm text-destructive"
            role="alert"
          >
            {error}
          </motion.p>
        ) : notice ? (
          <p className="mt-3 flex items-start gap-1.5 text-sm text-muted-foreground" role="status">
            <MailCheck className="mt-px size-3.5 shrink-0" />
            {notice}
          </p>
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
