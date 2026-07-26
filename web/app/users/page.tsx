"use client";

import { useCallback, useState } from "react";
import { toast } from "sonner";
import { KeyRound, Loader2, Lock, UserPlus, Users } from "lucide-react";
import { usePoll } from "@/hooks/use-poll";
import { changePassword, createUser, getMe, listUsers } from "@/lib/api";
import { fmtTime } from "@/lib/format";
import { useAuth } from "@/components/auth-gate";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { EmptyState, FieldLabel } from "@/components/bits";

const message = (err: unknown) => (err instanceof Error ? err.message : String(err));

export default function UsersPage() {
  const { signOut } = useAuth();
  const { data: users, loading, error, refresh } = usePoll(listUsers, 0);
  const { data: me } = usePoll(useCallback(() => getMe(), []), 0);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [changing, setChanging] = useState(false);

  if (error === "unauthorized") {
    return (
      <Card>
        <EmptyState
          icon={Lock}
          title="An account is required"
          hint="Adding people needs an account login or the bootstrap PRINTAPI_TOKEN — a per-client key cannot manage the org. Sign out and connect with one of those."
        />
      </Card>
    );
  }

  async function add(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    try {
      const user = await createUser(email.trim(), password);
      toast.success(`Added ${user.email}`);
      setEmail("");
      setPassword("");
      refresh();
    } catch (err) {
      toast.error("Could not add that person", { description: message(err) });
    } finally {
      setBusy(false);
    }
  }

  async function change(e: React.FormEvent) {
    e.preventDefault();
    setChanging(true);
    try {
      await changePassword(current, next);
      toast.success("Password changed — sign in again");
      setCurrent("");
      setNext("");
      signOut();               // the new password invalidated this session server-side
    } catch (err) {
      toast.error("Could not change the password", { description: message(err) });
    } finally {
      setChanging(false);
    }
  }

  return (
    <div className="max-w-3xl space-y-4">
      <Card>
        <CardHeader>
          <CardTitle>Add someone</CardTitle>
          <CardDescription>
            They sign in with this e-mail and password, see this org only, and can manage its keys
            and people.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={add} className="flex flex-wrap items-end gap-2">
            <div className="min-w-56 flex-1">
              <FieldLabel htmlFor="email">E-mail</FieldLabel>
              <Input
                id="email"
                type="email"
                required
                autoComplete="off"
                className="mt-1.5"
                placeholder="packer@yourshop.example"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
              />
            </div>
            <div className="min-w-56 flex-1">
              <FieldLabel htmlFor="password">Password</FieldLabel>
              <Input
                id="password"
                type="password"
                required
                minLength={8}
                autoComplete="new-password"
                className="mt-1.5"
                placeholder="at least 8 characters"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </div>
            <Button type="submit" variant="brand" disabled={busy}>
              {busy ? <Loader2 className="animate-spin" /> : <UserPlus />}
              Add
            </Button>
          </form>
        </CardContent>
      </Card>

      <Card className="gap-0 py-0">
        <CardContent className="px-0">
          {loading && !users ? (
            <div className="space-y-2 p-4">
              {[0, 1].map((i) => (
                <Skeleton key={i} className="h-10 w-full" />
              ))}
            </div>
          ) : (users ?? []).length === 0 ? (
            <EmptyState
              icon={Users}
              title="Nobody here yet"
              hint="Add the people who should reach this dashboard without the bootstrap token."
            />
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>E-mail</TableHead>
                  {me?.kind === "root" && <TableHead>Org</TableHead>}
                  <TableHead className="text-right">Added</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {(users ?? []).map((u) => (
                  <TableRow key={u.id}>
                    <TableCell className="font-medium">
                      {u.email}
                      {me?.user_id === u.id && (
                        <Badge variant="secondary" className="ml-2">
                          you
                        </Badge>
                      )}
                    </TableCell>
                    {me?.kind === "root" && (
                      <TableCell className="font-mono text-muted-foreground">{u.org_id}</TableCell>
                    )}
                    <TableCell className="text-right text-muted-foreground">
                      {fmtTime(u.created_at)}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>

      {me?.kind === "session" && (
        <Card>
          <CardHeader>
            <CardTitle>Your password</CardTitle>
            <CardDescription>
              Changing it signs out every browser, including this one.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <form onSubmit={change} className="flex flex-wrap items-end gap-2">
              <div className="min-w-56 flex-1">
                <FieldLabel htmlFor="current">Current password</FieldLabel>
                <Input
                  id="current"
                  type="password"
                  required
                  autoComplete="current-password"
                  className="mt-1.5"
                  value={current}
                  onChange={(e) => setCurrent(e.target.value)}
                />
              </div>
              <div className="min-w-56 flex-1">
                <FieldLabel htmlFor="new">New password</FieldLabel>
                <Input
                  id="new"
                  type="password"
                  required
                  minLength={8}
                  autoComplete="new-password"
                  className="mt-1.5"
                  placeholder="at least 8 characters"
                  value={next}
                  onChange={(e) => setNext(e.target.value)}
                />
              </div>
              <Button type="submit" disabled={changing}>
                {changing ? <Loader2 className="animate-spin" /> : <KeyRound />}
                Change
              </Button>
            </form>
          </CardContent>
        </Card>
      )}
    </div>
  );
}
