"use client";

import { useCallback, useDeferredValue, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import {
  ArrowLeft,
  Barcode,
  CircleCheck,
  Crosshair,
  Eye,
  Loader2,
  Maximize2,
  Minimize2,
  Printer,
  RotateCcw,
  RotateCw,
  Ruler,
  Save,
  Scaling,
  TriangleAlert,
  Type,
} from "lucide-react";
import {
  createLabel,
  updateLabel,
  type LabelBox,
  type LabelTemplate,
} from "@/lib/api";
import {
  applySize,
  draftFromTemplate,
  forPicture,
  forSample,
  isRound,
  jobExample,
  labelDots,
  maxMm,
  nameProblem,
  newDraft,
  preparedSize,
  previewLines,
  specKey,
  stashDraft,
  switchContent,
  textBox,
  toSpec,
  turn,
  unstashDraft,
  type BoxMemory,
  type Draft,
} from "@/lib/label-draft";
import { barWidthDots, fit, nativeBox, overflowMm, textClipped, textFonts } from "@/lib/label-geometry";
import {
  canvasFromFile,
  printPreview,
  textCanvas,
  turnedAndCropped,
} from "@/lib/label-render";
import { SAMPLE_BAR_PX, SAMPLE_DPI, drawSampleLabel } from "@/lib/label-sample";
import { useClientValue } from "@/hooks/use-client-value";
import { useNavGuard } from "@/components/nav-guard";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { Switch } from "@/components/ui/switch";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { CopyButton } from "@/components/copy-button";
import { LabelStage } from "@/components/labels/label-stage";
import { LabelInspector } from "@/components/labels/label-inspector";
import { LabelSizeDialog } from "@/components/labels/label-size-dialog";
import { TestPrintDialog } from "@/components/labels/test-print-dialog";

const MM_PER_INCH = 25.4;
/** label.rs MAX_DOTS. */
const MAX_DOTS = 8000;
/** A bar narrower than this many dots may not scan. */
const MIN_BAR_DOTS = 2;
const DEFAULT_TEXT = "A-104872";

/** Session storage, or null where the browser blocks it. */
function sessionStore(): Storage | null {
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
}

const message = (err: unknown) => (err instanceof Error ? err.message : String(err));
const one = (v: number) => String(Math.round(v * 10) / 10);

function ToolButton({
  label,
  icon: Icon,
  onClick,
  disabled,
}: {
  label: string;
  icon: React.ComponentType<{ className?: string }>;
  onClick: () => void;
  disabled?: boolean;
}) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button variant="ghost" size="icon-sm" aria-label={label} disabled={disabled} onClick={onClick}>
          <Icon />
        </Button>
      </TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  );
}

/** A state as colour + icon + word, never colour alone. */
function Status({
  tone,
  icon: Icon,
  word,
  children,
}: {
  tone: "ok" | "warn" | "bad" | "info";
  icon: React.ComponentType<{ className?: string }>;
  word: string;
  children?: React.ReactNode;
}) {
  const color = { ok: "var(--state-done)", warn: "var(--state-queued)", bad: "var(--state-failed)", info: "" }[tone];
  return (
    <span
      className="inline-flex items-center gap-1.5 text-xs text-muted-foreground"
      style={color ? { color } : undefined}
    >
      <Icon className="size-3.5 shrink-0" />
      <span className="font-semibold">{word}</span>
      {children && <span>{children}</span>}
    </span>
  );
}

/**
 * Edits one template: the stage in the middle, the numbers on the right. Nothing is stored until
 * Save. `template` is null for a new one.
 */
export function LabelEditor({
  template,
  onBack,
  onSaved,
}: {
  template: LabelTemplate | null;
  onBack: () => void;
  onSaved: () => void;
}) {
  const [first] = useState(() => (template ? draftFromTemplate(template) : newDraft()));
  // Work that a dead session or a reload left behind in session storage comes back.
  const [stashed] = useState(() => unstashDraft(sessionStore(), template?.id ?? null));
  const [draft, setDraft] = useState<Draft>(stashed ?? first);
  const [saved, setSaved] = useState<LabelTemplate | null>(template);
  // The draft as last saved (or as first opened, for a new one): the yardstick for "changed".
  const [baseKey, setBaseKey] = useState(() => specKey(first));
  const [saving, setSaving] = useState(false);
  const [lock, setLock] = useState((stashed ?? first).content === "image");
  // The box each content kind had last, so a stray click on the other tab loses nothing.
  const boxes = useRef<BoxMemory>({});
  const [sampleText, setSampleText] = useState(DEFAULT_TEXT);
  const [printOn, setPrintOn] = useState(false);
  const [file, setFile] = useState<File | null>(null);
  const [sample] = useState(drawSampleLabel);
  const [source, setSource] = useState<HTMLCanvasElement>(sample);
  const [sizesOpen, setSizesOpen] = useState(false);
  const [testOpen, setTestOpen] = useState(false);
  const [leaveOpen, setLeaveOpen] = useState(false);
  const nameInput = useRef<HTMLInputElement>(null);
  const origin = useClientValue(() => window.location.origin, "https://your-server");

  const changed = specKey(draft) !== baseKey;
  const guard = useNavGuard();
  const { setDirty } = guard;
  const savedId = saved?.id ?? null;

  const dropStash = useCallback(() => {
    stashDraft(sessionStore(), savedId, null);
    stashDraft(sessionStore(), null, null);
  }, [savedId]);

  // Unsaved work: ask before the sidebar, the palette, Sign out, a reload or closing the tab
  // throws it away, and keep a copy for the one thing that cannot ask, a session that ended.
  useEffect(() => {
    setDirty(changed, dropStash);
    return () => setDirty(false);
  }, [changed, dropStash, setDirty]);
  useEffect(() => {
    if (changed) stashDraft(sessionStore(), savedId, draft);
    else dropStash();
  }, [changed, draft, savedId, dropStash]);
  useEffect(() => {
    if (stashed) toast.info("Restored your unsaved changes");
  }, [stashed]);
  const needsSave = !saved || changed;
  const round = isRound(draft);
  const dotsOfLabel = labelDots(draft);

  const prepared = useMemo(
    () => turnedAndCropped(source, draft.rotate, draft.crop),
    [source, draft.rotate, draft.crop],
  );
  const lines = useMemo(() => previewLines(sampleText, draft.text), [sampleText, draft.text]);
  const boxW = draft.place.w;
  const boxH = draft.place.h;
  const content = useMemo(
    () =>
      draft.content === "text"
        ? textCanvas(lines, { w: boxW, h: boxH }, draft.dpi)
        : prepared,
    [draft.content, draft.dpi, lines, boxW, boxH, prepared],
  );
  // The black-and-white render is the expensive part; let dragging stay smooth and catch up.
  const lazyPlace = useDeferredValue(draft.place);
  const preview = useMemo(
    () => (printOn ? printPreview(content, lazyPlace, draft.label, draft.dpi) : null),
    [printOn, content, lazyPlace, draft.label, draft.dpi],
  );

  const imagePx = { w: source.width, h: source.height };
  const shape = () => preparedSize(imagePx, draft.rotate, draft.crop);
  const put = (place: LabelBox) => setDraft((d) => ({ ...d, place }));

  const toolFit = () =>
    put(
      draft.content === "text"
        ? textBox(draft.label, round)
        : fit(shape(), draft.label, "contain", round),
    );
  const toolFill = () =>
    put(
      draft.content === "text"
        ? { x: 0, y: 0, w: draft.label.w, h: draft.label.h }
        : fit(shape(), draft.label, "cover"),
    );
  const toolCenter = () =>
    put({
      ...draft.place,
      x: (draft.label.w - draft.place.w) / 2,
      y: (draft.label.h - draft.place.h) / 2,
    });
  const toolActual = () => put(nativeBox(shape(), draft.dpi, draft.label));

  function changeContent(kind: Draft["content"]) {
    const next = switchContent(draft, kind, imagePx, boxes.current);
    boxes.current = next.memory;
    setDraft(next.draft);
    setLock(kind === "image");
  }

  async function pickFile(f: File | null) {
    setFile(f);
    if (!f) {
      setSource(sample);
      // A new template goes back to the sample, set up like a carrier label.
      if (!saved) setDraft((d) => forSample(d));
      return;
    }
    try {
      const c = await canvasFromFile(f);
      setSource(c);
      // A new template takes the picture as it is: upright, whole, fitted into the label (the
      // turn and crop of the built-in sample are its own, not the picture's). A saved one keeps
      // its placement: the picture is only a stand-in to look at.
      if (!saved) setDraft((d) => forPicture(d, { w: c.width, h: c.height }));
    } catch {
      setFile(null);
      toast.error("Could not read that picture", { description: "Pick a GIF, PNG or JPG." });
    }
  }

  async function save(): Promise<LabelTemplate | null> {
    const problem = nameProblem(draft.name);
    if (problem) {
      toast.error(problem);
      nameInput.current?.focus();
      return null;
    }
    setSaving(true);
    try {
      const spec = toSpec(draft);
      const t = saved ? await updateLabel(saved.id, spec) : await createLabel(spec);
      setSaved(t);
      setBaseKey(specKey(draft));
      dropStash();
      toast.success(`Saved "${t.name}"`);
      onSaved();
      return t;
    } catch (err) {
      toast.error("Could not save the template", { description: message(err) });
      return null;
    } finally {
      setSaving(false);
    }
  }

  const back = () => (changed ? setLeaveOpen(true) : onBack());

  // status
  const over = overflowMm(draft.place, draft.label, round);
  const tooBig = dotsOfLabel.w > MAX_DOTS || dotsOfLabel.h > MAX_DOTS;
  const boxDotsW = (draft.place.w / MM_PER_INCH) * draft.dpi;
  const boxDotsH = (draft.place.h / MM_PER_INCH) * draft.dpi;
  const scale = boxDotsW / prepared.width;
  const bar = barWidthDots(
    SAMPLE_BAR_PX,
    draft.place.w / ((prepared.width / SAMPLE_DPI) * MM_PER_INCH),
    draft.dpi,
    SAMPLE_DPI,
  );
  const boxDots = { w: boxDotsW, h: boxDotsH };
  const fonts = textFonts(lines, boxDots);
  const fontMm = (dotsOfFont: number) => (dotsOfFont / draft.dpi) * MM_PER_INCH;
  // The text is shrunk to fit the box, but not below 10 dots: past that it runs out of the box.
  const textTight = draft.content === "text" && textClipped(lines, boxDots);

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="ghost" size="icon" aria-label="Back to all templates" onClick={back}>
          <ArrowLeft />
        </Button>
        <Input
          ref={nameInput}
          aria-label="Template name"
          placeholder="Template name, e.g. shipping-4x6"
          autoComplete="off"
          maxLength={64}
          className="min-w-0 flex-1 basis-48 font-mono sm:max-w-sm sm:flex-none sm:basis-80"
          value={draft.name}
          onChange={(e) => setDraft({ ...draft, name: e.target.value })}
        />
        <div className="ml-auto flex items-center gap-2">
          {needsSave && saved && (
            <span className="hidden text-xs text-muted-foreground sm:inline">Unsaved changes</span>
          )}
          <Button variant="outline" onClick={() => setTestOpen(true)}>
            <Printer /> Test print
          </Button>
          <Button variant="brand" disabled={saving} onClick={save}>
            {saving ? <Loader2 className="animate-spin" /> : <Save />}
            Save
          </Button>
        </div>
      </div>

      <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,1fr)_320px]">
        <Card className="gap-0 py-0 lg:sticky lg:top-6">
          <CardContent className="flex flex-wrap items-center gap-1 border-b px-3 py-2">
            <ToolButton label="Fit" icon={Minimize2} onClick={toolFit} />
            <ToolButton label="Fill" icon={Maximize2} onClick={toolFill} />
            <ToolButton label="Center" icon={Crosshair} onClick={toolCenter} />
            <ToolButton
              label="Actual size"
              icon={Ruler}
              onClick={toolActual}
              disabled={draft.content === "text"}
            />
            <Separator orientation="vertical" className="mx-1 h-5 self-center" />
            <ToolButton
              label="Rotate left"
              icon={RotateCcw}
              onClick={() => setDraft(turn(draft, false))}
              disabled={draft.content === "text"}
            />
            <ToolButton
              label="Rotate right"
              icon={RotateCw}
              onClick={() => setDraft(turn(draft, true))}
              disabled={draft.content === "text"}
            />
            <label className="ml-auto flex items-center gap-2 text-sm">
              <Eye className="size-4 text-muted-foreground" />
              Print preview
              <Switch checked={printOn} onCheckedChange={setPrintOn} />
            </label>
          </CardContent>

          <LabelStage
            label={draft.label}
            round={round}
            box={draft.place}
            lock={lock}
            content={content}
            preview={preview}
            dpi={draft.dpi}
            onBox={put}
          />

          <CardContent className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t px-3 py-2">
            <span className="font-mono text-xs text-muted-foreground">
              {one(draft.label.w)} × {one(draft.label.h)} mm · {dotsOfLabel.w} × {dotsOfLabel.h} dots
            </span>
            <span className="ml-auto flex flex-wrap items-center gap-x-4 gap-y-1">
              {tooBig && (
                <Status tone="bad" icon={TriangleAlert} word="Too large">
                  the server takes at most {MAX_DOTS} dots per side ({one(maxMm(draft.dpi))} mm)
                </Status>
              )}
              {over > 0.05 && (
                <Status tone="warn" icon={TriangleAlert} word="Clipped">
                  up to {one(over)} mm over the edge
                </Status>
              )}
              {textTight && (
                <Status tone="warn" icon={TriangleAlert} word="Clipped">
                  the text does not fit the box at the smallest font (10 dots)
                </Status>
              )}
              {over <= 0.05 && !textTight && <Status tone="ok" icon={CircleCheck} word="Fits" />}
              {draft.content === "image" && (
                <Status tone="info" icon={Scaling} word="Scale">
                  {Math.round(scale * 100)} %
                </Status>
              )}
              {draft.content === "text" && (
                <Status tone="info" icon={Type} word="Font">
                  ZPL {one(fontMm(fonts.zpl))} mm · PDF {one(fontMm(fonts.pdf))} mm tall
                </Status>
              )}
              {draft.content === "image" && !file && (
                <Status
                  tone={bar < MIN_BAR_DOTS ? "warn" : "ok"}
                  icon={bar < MIN_BAR_DOTS ? TriangleAlert : Barcode}
                  word="Barcode"
                >
                  {bar < MIN_BAR_DOTS
                    ? `thinnest bar ${one(bar)} dots, check that it scans`
                    : `thinnest bar ${one(bar)} dots`}
                </Status>
              )}
            </span>
          </CardContent>
        </Card>

        <Card className="gap-0 py-0">
          <LabelInspector
            draft={draft}
            onChange={setDraft}
            lock={lock}
            onLock={setLock}
            sampleText={sampleText}
            onSampleText={setSampleText}
            file={file}
            onFile={pickFile}
            onContent={changeContent}
            onOpenSizes={() => setSizesOpen(true)}
          />
        </Card>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Print with this template</CardTitle>
          <CardDescription>
            Send a job that names the template. The server places the content the way the stage
            shows it. Text keeps the size and line breaks shown here, but the printer draws it in
            its own font.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="flex items-start gap-2">
            <pre className="min-w-0 flex-1 rounded-md bg-muted px-3 py-2 font-mono text-xs break-all whitespace-pre-wrap">
              {jobExample(draft, origin, sampleText)}
            </pre>
            <CopyButton value={jobExample(draft, origin, sampleText)} />
          </div>
        </CardContent>
      </Card>

      <LabelSizeDialog
        open={sizesOpen}
        onOpenChange={setSizesOpen}
        current={draft.sizeRef}
        onPick={(s) => setDraft(applySize(draft, s))}
      />

      <TestPrintDialog
        open={testOpen}
        onOpenChange={setTestOpen}
        content={draft.content}
        sampleText={sampleText}
        image={source}
        needsSave={needsSave}
        ensureSaved={async () => (needsSave ? save() : saved)}
      />

      <AlertDialog open={leaveOpen} onOpenChange={setLeaveOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Discard your changes?</AlertDialogTitle>
            <AlertDialogDescription>
              This template has changes that are not saved. Going back throws them away.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Keep editing</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                dropStash();
                onBack();
              }}
            >
              Discard
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
