"use client";

import { useEffect, useState } from "react";
import { motion } from "motion/react";
import { toast } from "sonner";
import { Image as ImageIcon, Pencil, Plus, Tags, Trash2, TriangleAlert, Type } from "lucide-react";
import { usePoll } from "@/hooks/use-poll";
import { deleteLabel, listLabels, type LabelTemplate } from "@/lib/api";
import { LABEL_SIZES } from "@/lib/label-sizes";
import { Card, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
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
import { EmptyState } from "@/components/bits";
import { NAV_RESELECT_EVENT, useNavGuard } from "@/components/nav-guard";
import { LabelEditor } from "@/components/labels/label-editor";
import { PaperThumb } from "@/components/labels/label-stage";

// Static export: no dynamic segment, so list and editor are two states of the same route.
type View = { editing: false } | { editing: true; template: LabelTemplate | null };

const mm = (v: number) => String(Math.round(v * 10) / 10);

export default function LabelsPage() {
  const { data: templates, loading, error, refresh } = usePoll(listLabels, 0);
  const [view, setView] = useState<View>({ editing: false });
  const guard = useNavGuard();
  const { request } = guard;
  const editing = view.editing;

  const leave = () => {
    setView({ editing: false });
    refresh();
  };

  // The "Labels" entry of the sidebar, clicked while a template is open, goes back to the list.
  useEffect(() => {
    if (!editing) return;
    const onReselect = () =>
      request(() => {
        setView({ editing: false });
        refresh();
      });
    window.addEventListener(NAV_RESELECT_EVENT, onReselect);
    return () => window.removeEventListener(NAV_RESELECT_EVENT, onReselect);
  }, [editing, request, refresh]);

  if (view.editing) {
    return (
      <LabelEditor
        key={view.template?.id ?? "new"}
        template={view.template}
        onSaved={refresh}
        onBack={leave}
      />
    );
  }

  const create = () => setView({ editing: true, template: null });

  async function remove(t: LabelTemplate) {
    try {
      await deleteLabel(t.id);
      toast.success(`Deleted "${t.name}"`);
      refresh();
    } catch (err) {
      toast.error("Could not delete", { description: err instanceof Error ? err.message : String(err) });
    }
  }

  if (loading && !templates) {
    return (
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
        {[0, 1, 2].map((i) => (
          <Skeleton key={i} className="h-60 w-full" />
        ))}
      </div>
    );
  }

  if (!templates) {
    return (
      <Card>
        <EmptyState
          icon={TriangleAlert}
          title="Could not load the templates"
          hint={error ?? "The server did not answer."}
          action={
            <Button variant="outline" size="sm" className="mt-2" onClick={refresh}>
              Try again
            </Button>
          }
        />
      </Card>
    );
  }

  if (templates.length === 0) {
    return (
      <Card>
        <EmptyState
          icon={Tags}
          title="No label templates yet"
          hint="A template says where the content sits on one kind of label. Set it up once, and every job that names it prints the same way."
          action={
            <Button variant="brand" size="sm" className="mt-2" onClick={create}>
              <Plus /> New template
            </Button>
          }
        />
      </Card>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-sm text-muted-foreground">
          {templates.length} {templates.length === 1 ? "template" : "templates"}
        </p>
        <Button variant="brand" onClick={create}>
          <Plus /> New template
        </Button>
      </div>

      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
        {templates.map((t, i) => (
          <motion.div
            key={t.id}
            initial={{ opacity: 0, y: 10 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ delay: i * 0.04, duration: 0.28 }}
          >
            <TemplateCard
              template={t}
              onEdit={() => setView({ editing: true, template: t })}
              onDelete={() => remove(t)}
            />
          </motion.div>
        ))}
      </div>
    </div>
  );
}

function TemplateCard({
  template: t,
  onEdit,
  onDelete,
}: {
  template: LabelTemplate;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const round = !!LABEL_SIZES.find((s) => s.id === t.size_ref)?.round;
  return (
    <Card className="gap-0 py-4 transition-colors hover:border-brand/50">
      <CardContent className="px-4">
        <button
          type="button"
          onClick={onEdit}
          className="block w-full rounded-md text-left outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
        >
          <span className="flex h-28 items-center justify-center rounded-md bg-muted/40">
            <PaperThumb label={t.label_mm} round={round} box={t.place_mm} max={88} />
          </span>
          <span className="mt-3 flex items-start gap-2">
            <span className="min-w-0 flex-1">
              <span className="block truncate font-medium" title={t.name}>
                {t.name}
              </span>
              <span className="block font-mono text-xs text-muted-foreground">
                {mm(t.label_mm.w)} × {mm(t.label_mm.h)} mm
              </span>
            </span>
            <Badge variant="outline" className="gap-1">
              {t.content === "image" ? <ImageIcon /> : <Type />}
              {t.content === "image" ? "Image" : "Text"}
            </Badge>
          </span>
        </button>

        <div className="mt-3 flex items-center gap-2">
          <Button variant="outline" size="sm" className="flex-1" onClick={onEdit}>
            <Pencil /> Edit
          </Button>
          <AlertDialog>
            <AlertDialogTrigger asChild>
              <Button variant="destructive" size="icon-sm" aria-label={`Delete ${t.name}`}>
                <Trash2 />
              </Button>
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Delete “{t.name}”?</AlertDialogTitle>
                <AlertDialogDescription>
                  Jobs that name this template are refused from then on. This cannot be undone.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Keep it</AlertDialogCancel>
                <AlertDialogAction onClick={onDelete}>Delete</AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        </div>
      </CardContent>
    </Card>
  );
}
