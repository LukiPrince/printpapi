"use client";

import { useCallback, useState } from "react";
import { toast } from "sonner";
import { CreditCard, Gauge, Loader2, Lock, Save, Webhook } from "lucide-react";
import { usePoll } from "@/hooks/use-poll";
import { getMe, getOrg, listOrgs, listPlans, updateOrg, type Org } from "@/lib/api";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Progress } from "@/components/ui/progress";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { EmptyState, FieldLabel } from "@/components/bits";

const message = (err: unknown) => (err instanceof Error ? err.message : String(err));

export default function SettingsPage() {
  const { data: me, error: meError } = usePoll(useCallback(() => getMe(), []), 0);
  const isRoot = me?.kind === "root";
  // Root belongs to no org, so it picks one; a session only ever edits its own.
  const { data: orgs } = usePoll(useCallback(() => listOrgs(), []), 0, isRoot);
  const [orgId, setOrgId] = useState<number | null>(null);
  const chosen = orgId ?? me?.org_id ?? orgs?.[0]?.id ?? null;

  const { data: org, refresh } = usePoll(
    useCallback(() => (chosen === null ? Promise.resolve(null) : getOrg(chosen)), [chosen]),
    0,
  );
  // Empty on a server without a plan catalogue — then the whole billing card stays hidden.
  const { data: catalogue } = usePoll(useCallback(() => listPlans(), []), 0);

  const [eventUrl, setEventUrl] = useState("");
  const [secret, setSecret] = useState("");
  const [quota, setQuota] = useState("");
  const [busy, setBusy] = useState("");
  const [seeded, setSeeded] = useState<number | null>(null);

  // Seed the forms from whichever org is loaded, once per org — during render, not in an effect,
  // so switching orgs never paints one org's settings under another's name. Keying on the id
  // alone also means a background refresh cannot overwrite what someone is typing.
  if (org && seeded !== org.id) {
    setSeeded(org.id);
    setEventUrl(org.event_url ?? "");
    setSecret("");
    setQuota(org.job_quota === null ? "" : String(org.job_quota));
  }

  if (meError === "unauthorized" || me?.kind === "key") {
    return (
      <Card>
        <EmptyState
          icon={Lock}
          title="An account is required"
          hint="Org settings need an account login or the bootstrap PRINTAPI_TOKEN — a per-client key cannot manage the org."
        />
      </Card>
    );
  }

  async function save(what: string, patch: Parameters<typeof updateOrg>[1]) {
    if (chosen === null) return;
    setBusy(what);
    try {
      await updateOrg(chosen, patch);
      toast.success(`${what} saved`);
      if (patch.shopify_secret !== undefined) setSecret("");   // write-only: nothing to show back
      refresh();
    } catch (err) {
      toast.error(`Could not save ${what.toLowerCase()}`, { description: message(err) });
    } finally {
      setBusy("");
    }
  }

  const used = org?.jobs_this_month ?? 0;
  const cap = org?.job_quota ?? null;

  return (
    <div className="max-w-3xl space-y-4">
      {isRoot && (
        <Card>
          <CardHeader>
            <CardTitle>Org</CardTitle>
            <CardDescription>
              The bootstrap token spans every org, so pick the one to look at.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Select
              value={chosen === null ? undefined : String(chosen)}
              onValueChange={(v) => setOrgId(Number(v))}
            >
              <SelectTrigger className="w-72">
                <SelectValue placeholder="Pick an org" />
              </SelectTrigger>
              <SelectContent>
                {(orgs ?? []).map((o: Org) => (
                  <SelectItem key={o.id} value={String(o.id)}>
                    {o.name} <span className="text-muted-foreground">#{o.id}</span>
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </CardContent>
        </Card>
      )}

      {(catalogue?.plans.length ?? 0) > 0 && (
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <CreditCard className="size-4" /> Plan
            </CardTitle>
            <CardDescription>
              What this org may print per month. Checkout happens at the payment provider — this
              server never sees a card, it only hears back which plan you are on.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-2">
            {catalogue!.plans.map((plan) => {
              const current = org?.plan === plan.id;
              return (
                <div
                  key={plan.id}
                  className="flex flex-wrap items-center gap-3 rounded-lg border p-3"
                >
                  <div className="flex-1">
                    <div className="flex items-center gap-2 font-medium">
                      {plan.name}
                      {current && <Badge>current</Badge>}
                    </div>
                    <div className="text-muted-foreground text-sm">
                      {plan.jobs === null ? "unlimited jobs" : `${plan.jobs} jobs / month`}
                      {plan.price ? ` — ${plan.price}` : ""}
                    </div>
                  </div>
                  {isRoot ? (
                    <Button
                      variant="outline"
                      disabled={current || busy === "Plan"}
                      onClick={() => save("Plan", { plan: plan.id })}
                    >
                      {current ? "In use" : "Move here"}
                    </Button>
                  ) : (
                    plan.checkout_url &&
                    !current && (
                      <Button asChild variant="outline">
                        <a href={plan.checkout_url} target="_blank" rel="noreferrer">
                          Choose
                        </a>
                      </Button>
                    )
                  )}
                </div>
              );
            })}
          </CardContent>
        </Card>
      )}

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Gauge className="size-4" /> This month
          </CardTitle>
          <CardDescription>
            Jobs submitted since the first of the month. The quota is set by whoever runs this
            server; a spent quota answers 402 instead of printing.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          {!org ? (
            <Skeleton className="h-10 w-full" />
          ) : (
            <>
              <div className="flex items-baseline gap-2 font-mono">
                <span className="text-3xl font-bold">{used}</span>
                <span className="text-muted-foreground">
                  {cap === null ? "jobs — no quota" : `of ${cap} jobs`}
                </span>
                {cap !== null && used >= cap && <Badge variant="destructive">spent</Badge>}
              </div>
              {cap !== null && cap > 0 && (
                <Progress value={Math.min(100, (used / cap) * 100)} />
              )}
            </>
          )}
          {isRoot && (
            <form
              className="flex flex-wrap items-end gap-2 pt-1"
              onSubmit={(e) => {
                e.preventDefault();
                const trimmed = quota.trim();
                save("Quota", { job_quota: trimmed === "" ? null : Number(trimmed) });
              }}
            >
              <div className="min-w-40">
                <FieldLabel htmlFor="quota">Monthly job quota</FieldLabel>
                <Input
                  id="quota"
                  type="number"
                  min={0}
                  className="mt-1.5 font-mono"
                  placeholder="empty = unlimited"
                  value={quota}
                  onChange={(e) => setQuota(e.target.value)}
                />
              </div>
              <Button type="submit" disabled={busy === "Quota"}>
                {busy === "Quota" ? <Loader2 className="animate-spin" /> : <Save />}
                Save
              </Button>
            </form>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Webhook className="size-4" /> Agent events
          </CardTitle>
          <CardDescription>
            Where this org&apos;s <code>computer_online</code> / <code>computer_offline</code>{" "}
            events are POSTed when an agent appears or drops off. Empty turns them off. Payloads
            are unsigned — treat the URL itself as the secret.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form
            className="flex flex-wrap items-end gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              save("Event URL", { event_url: eventUrl.trim() || null });
            }}
          >
            <div className="min-w-72 flex-1">
              <FieldLabel htmlFor="event_url">Event URL</FieldLabel>
              <Input
                id="event_url"
                type="url"
                className="mt-1.5 font-mono"
                placeholder="https://hooks.yourshop.example/printpapi"
                value={eventUrl}
                onChange={(e) => setEventUrl(e.target.value)}
              />
            </div>
            <Button type="submit" disabled={busy === "Event URL"}>
              {busy === "Event URL" ? <Loader2 className="animate-spin" /> : <Save />}
              Save
            </Button>
          </form>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Shopify webhook secret</CardTitle>
          <CardDescription>
            Shopify signs its order webhooks with this. Without it the order endpoint refuses to
            print. It is stored write-only — it can be replaced or cleared, never read back.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form
            className="flex flex-wrap items-end gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              save("Shopify secret", { shopify_secret: secret.trim() || null });
            }}
          >
            <div className="min-w-72 flex-1">
              <FieldLabel htmlFor="secret">
                Secret {org?.shopify_secret_set ? "(configured)" : "(not set)"}
              </FieldLabel>
              <Input
                id="secret"
                type="password"
                autoComplete="off"
                className="mt-1.5 font-mono"
                placeholder={org?.shopify_secret_set ? "•••••••• — type to replace" : "from Shopify"}
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
              />
            </div>
            <Button type="submit" disabled={busy === "Shopify secret"}>
              {busy === "Shopify secret" ? <Loader2 className="animate-spin" /> : <Save />}
              {secret.trim() ? "Save" : "Clear"}
            </Button>
          </form>
        </CardContent>
      </Card>
    </div>
  );
}
