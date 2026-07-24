"use client";

import { useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { toast } from "sonner";
import { KeyRound, Loader2, Lock, Plus, ShieldOff } from "lucide-react";
import { usePoll } from "@/hooks/use-poll";
import { issueApiKey, listApiKeys, revokeApiKey } from "@/lib/api";
import { fmtTime } from "@/lib/format";
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
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { CopyButton } from "@/components/copy-button";
import { EmptyState, FieldLabel } from "@/components/bits";

export default function KeysPage() {
  const { data: keys, loading, error, refresh } = usePoll(listApiKeys, 0);
  const [label, setLabel] = useState("");
  const [busy, setBusy] = useState(false);
  const [issued, setIssued] = useState<{ label: string; key: string } | null>(null);

  if (error === "unauthorized") {
    return (
      <Card>
        <EmptyState
          icon={Lock}
          title="Admin token required"
          hint="Issuing and revoking keys needs the bootstrap PRINTAPI_TOKEN, not a per-client key. Sign out and connect with it."
        />
      </Card>
    );
  }

  async function issue(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    try {
      const key = await issueApiKey(label.trim() || "client");
      setIssued({ label: key.label, key: key.key });
      setLabel("");
      refresh();
    } catch (err) {
      toast.error("Could not issue the key", {
        description: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setBusy(false);
    }
  }

  async function revoke(id: number, name: string) {
    try {
      await revokeApiKey(id);
      toast.success(`Revoked "${name}"`);
      refresh();
    } catch (err) {
      toast.error("Could not revoke", {
        description: err instanceof Error ? err.message : String(err),
      });
    }
  }

  return (
    <div className="max-w-3xl space-y-4">
      <Card>
        <CardHeader>
          <CardTitle>Issue a key</CardTitle>
          <CardDescription>
            One key per integration, so you can cut off exactly one when you need to.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={issue} className="flex items-end gap-2">
            <div className="flex-1">
              <FieldLabel htmlFor="label">Label</FieldLabel>
              <Input
                id="label"
                className="mt-1.5"
                placeholder="e.g. n8n, warehouse-app"
                value={label}
                onChange={(e) => setLabel(e.target.value)}
              />
            </div>
            <Button type="submit" variant="brand" disabled={busy}>
              {busy ? <Loader2 className="animate-spin" /> : <Plus />}
              Issue
            </Button>
          </form>

          <AnimatePresence>
            {issued && (
              <motion.div
                initial={{ opacity: 0, height: 0 }}
                animate={{ opacity: 1, height: "auto" }}
                exit={{ opacity: 0, height: 0 }}
                className="overflow-hidden"
              >
                <div className="mt-4 rounded-lg border-[1.5px] border-dashed border-brand bg-brand/8 p-3">
                  <p className="text-sm font-medium">
                    Key for <span className="font-mono">{issued.label}</span> — shown once, copy it
                    now.
                  </p>
                  <div className="mt-2 flex items-center gap-2">
                    <code className="min-w-0 flex-1 rounded bg-background/70 px-2 py-1.5 font-mono text-xs break-all">
                      {issued.key}
                    </code>
                    <CopyButton value={issued.key} />
                  </div>
                  <Button
                    variant="ghost"
                    size="xs"
                    className="mt-2"
                    onClick={() => setIssued(null)}
                  >
                    Dismiss
                  </Button>
                </div>
              </motion.div>
            )}
          </AnimatePresence>
        </CardContent>
      </Card>

      <Card className="gap-0 py-0">
        <CardContent className="px-0">
          {loading && !keys ? (
            <div className="space-y-2 p-4">
              {[0, 1].map((i) => (
                <Skeleton key={i} className="h-10 w-full" />
              ))}
            </div>
          ) : (keys ?? []).length === 0 ? (
            <EmptyState
              icon={KeyRound}
              title="No client keys yet"
              hint="Until you issue one, only the bootstrap PRINTAPI_TOKEN can submit jobs."
            />
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Label</TableHead>
                  <TableHead>Created</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead className="text-right">Actions</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {(keys ?? []).map((k) => (
                  <TableRow key={k.id}>
                    <TableCell className="font-medium">{k.label}</TableCell>
                    <TableCell className="text-muted-foreground">{fmtTime(k.created_at)}</TableCell>
                    <TableCell>
                      <Badge variant={k.active ? "secondary" : "outline"}>
                        {k.active ? "active" : "revoked"}
                      </Badge>
                    </TableCell>
                    <TableCell className="text-right">
                      {k.active ? (
                        <AlertDialog>
                          <AlertDialogTrigger asChild>
                            <Button variant="destructive" size="sm">
                              <ShieldOff /> Revoke
                            </Button>
                          </AlertDialogTrigger>
                          <AlertDialogContent>
                            <AlertDialogHeader>
                              <AlertDialogTitle>Revoke “{k.label}”?</AlertDialogTitle>
                              <AlertDialogDescription>
                                Anything using this key stops being able to submit jobs
                                immediately. This cannot be undone — issue a new key instead.
                              </AlertDialogDescription>
                            </AlertDialogHeader>
                            <AlertDialogFooter>
                              <AlertDialogCancel>Keep it</AlertDialogCancel>
                              <AlertDialogAction onClick={() => revoke(k.id, k.label)}>
                                Revoke
                              </AlertDialogAction>
                            </AlertDialogFooter>
                          </AlertDialogContent>
                        </AlertDialog>
                      ) : (
                        <span className="text-xs text-muted-foreground">—</span>
                      )}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
