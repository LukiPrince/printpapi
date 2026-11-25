"use client";

import { useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { toast } from "sonner";
import { KeyRound, Loader2, Lock, Plus, ShieldCheck, ShieldOff } from "lucide-react";
import { usePoll } from "@/hooks/use-poll";
import { issueApiKey, listApiKeys, revokeApiKey, setApiKeyScope, type ApiKey } from "@/lib/api";
import { fmtTime } from "@/lib/format";
import {
  KEY_SCOPES,
  canSaveScope,
  initialEditScope,
  scopeHint,
  scopeLabel,
  type KeyScope,
} from "@/lib/key-scopes";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
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

/** The Select of the issue form and of the rights dialog: the same five rights. */
function RightsSelect({
  id,
  value,
  onChange,
  placeholder,
  className,
}: {
  id?: string;
  value: KeyScope | "";
  onChange: (v: KeyScope) => void;
  placeholder?: string;
  className?: string;
}) {
  return (
    <Select value={value} onValueChange={(v) => onChange(v as KeyScope)}>
      <SelectTrigger id={id} className={className}>
        <SelectValue placeholder={placeholder} />
      </SelectTrigger>
      <SelectContent>
        {KEY_SCOPES.map((s) => (
          <SelectItem key={s.value} value={s.value}>
            {s.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

export default function KeysPage() {
  const { data: keys, loading, error, refresh } = usePoll(listApiKeys, 0);
  const [label, setLabel] = useState("");
  const [busy, setBusy] = useState(false);
  const [scope, setScope] = useState<KeyScope>("full");
  const [issued, setIssued] = useState<{ label: string; key: string; scope?: string } | null>(
    null,
  );
  // the one rights dialog of the page: the key it is open for and the choice made in it
  const [editing, setEditing] = useState<ApiKey | null>(null);
  const [editScope, setEditScope] = useState<KeyScope | "">("full");
  const [saving, setSaving] = useState(false);

  if (error === "unauthorized") {
    return (
      <Card>
        <EmptyState
          icon={Lock}
          title="An account is required"
          hint="Issuing and revoking keys needs an account login or the bootstrap PRINTAPI_TOKEN — a per-client key cannot manage keys. Sign out and connect with one of those."
        />
      </Card>
    );
  }

  async function issue(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    try {
      const key = await issueApiKey(label.trim() || "client", scope);
      setIssued({ label: key.label, key: key.key, scope: key.scope });
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

  function openRights(k: ApiKey) {
    setEditScope(initialEditScope(k.scope));
    setEditing(k);
  }

  async function saveRights() {
    if (!editing || editScope === "") return;
    setSaving(true);
    try {
      await setApiKeyScope(editing.id, editScope);
      toast.success(`"${editing.label}" is now ${scopeLabel(editScope === "full" ? undefined : editScope)}`);
      setEditing(null);
      refresh();
    } catch (err) {
      toast.error("Could not change the rights", {
        description: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setSaving(false);
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
            One key per integration or device, so you can cut off exactly one when you need to.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={issue} className="flex flex-wrap items-end gap-2">
            <div className="min-w-48 flex-1">
              <FieldLabel htmlFor="label">Label</FieldLabel>
              <Input
                id="label"
                className="mt-1.5"
                placeholder="e.g. n8n, warehouse-app"
                value={label}
                onChange={(e) => setLabel(e.target.value)}
              />
            </div>
            <div>
              <FieldLabel htmlFor="rights">Rights</FieldLabel>
              <RightsSelect id="rights" value={scope} onChange={setScope} className="mt-1.5 w-40" />
            </div>
            <Button type="submit" variant="brand" disabled={busy}>
              {busy ? <Loader2 className="animate-spin" /> : <Plus />}
              Issue
            </Button>
          </form>
          <p className="mt-2 text-xs text-muted-foreground">{scopeHint(scope)}</p>

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
                    Key for <span className="font-mono">{issued.label}</span> ({scopeLabel(issued.scope)})
                    — shown once, copy it now.
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
                  <TableHead>Rights</TableHead>
                  <TableHead className="hidden sm:table-cell">Created</TableHead>
                  <TableHead className="hidden sm:table-cell">Status</TableHead>
                  <TableHead className="text-right">Actions</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {(keys ?? []).map((k) => (
                  <TableRow key={k.id}>
                    <TableCell className="font-medium whitespace-normal [overflow-wrap:anywhere]">
                      {k.label}
                      {/* the Status column is hidden on a phone: a revoked key still says so */}
                      {!k.active && (
                        <Badge variant="outline" className="ml-2 sm:hidden">
                          revoked
                        </Badge>
                      )}
                    </TableCell>
                    <TableCell className="whitespace-normal">{scopeLabel(k.scope)}</TableCell>
                    <TableCell className="hidden text-muted-foreground sm:table-cell">
                      {fmtTime(k.created_at)}
                    </TableCell>
                    <TableCell className="hidden sm:table-cell">
                      <Badge variant={k.active ? "secondary" : "outline"}>
                        {k.active ? "active" : "revoked"}
                      </Badge>
                    </TableCell>
                    <TableCell className="text-right">
                      {k.active ? (
                        <div className="flex justify-end gap-2">
                        <Button
                          variant="outline"
                          size="sm"
                          aria-label="Rights"
                          title="Rights"
                          onClick={() => openRights(k)}
                        >
                          <ShieldCheck /> <span className="hidden sm:inline">Rights</span>
                        </Button>
                        <AlertDialog>
                          <AlertDialogTrigger asChild>
                            <Button variant="destructive" size="sm" aria-label="Revoke" title="Revoke">
                              <ShieldOff /> <span className="hidden sm:inline">Revoke</span>
                            </Button>
                          </AlertDialogTrigger>
                          <AlertDialogContent>
                            <AlertDialogHeader>
                              <AlertDialogTitle>Revoke “{k.label}”?</AlertDialogTitle>
                              <AlertDialogDescription>
                                Anything using this key stops working immediately, a computer
                                enrolled with it included. This cannot be undone. Issue a new key
                                instead.
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
                        </div>
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

      <Dialog open={editing !== null} onOpenChange={(open) => !open && setEditing(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{editing?.label}</DialogTitle>
            <DialogDescription>
              Takes effect at once. Anything that uses this key and needs a right it no longer has
              stops: computers and Star printers stop printing, shops and scripts stop submitting.
              Refused orders do not appear in the history.
            </DialogDescription>
          </DialogHeader>
          <div>
            <FieldLabel htmlFor="edit-rights">Rights</FieldLabel>
            <RightsSelect
              id="edit-rights"
              value={editScope}
              onChange={setEditScope}
              placeholder={editing ? scopeLabel(editing.scope) : undefined}
              className="mt-1.5 w-48"
            />
            <p className="mt-2 text-xs text-muted-foreground">{scopeHint(editScope)}</p>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setEditing(null)}>
              Cancel
            </Button>
            <Button
              variant="brand"
              disabled={saving || editing === null || !canSaveScope(editing.scope, editScope)}
              onClick={saveRights}
            >
              {saving && <Loader2 className="animate-spin" />}
              Save
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
