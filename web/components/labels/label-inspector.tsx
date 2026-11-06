"use client";

import { useState } from "react";
import { ChevronsUpDown, Image as ImageIcon, Type } from "lucide-react";
import {
  setCrop,
  setLabelDim,
  setPlace,
  type Draft,
} from "@/lib/label-draft";
import type { LabelBox, LabelSpec } from "@/lib/api";
import { LABEL_SIZES, sizeLabel } from "@/lib/label-sizes";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Dropzone } from "@/components/dropzone";
import { FieldLabel } from "@/components/bits";
import { PaperThumb } from "@/components/labels/label-stage";
import { cn } from "@/lib/utils";

const DPIS = [203, 300];

const shown = (v: number, decimals: number) => String(Math.round(v * 10 ** decimals) / 10 ** decimals);

/**
 * A number field that lets you type. The text you are typing stays as typed while the field has
 * focus; a value that parses is passed on right away, and the field snaps to the stored value
 * when you leave it.
 */
function NumField({
  id,
  label,
  value,
  onChange,
  decimals = 1,
  className,
}: {
  id: string;
  label: string;
  value: number;
  onChange: (v: number) => void;
  decimals?: number;
  className?: string;
}) {
  const [typing, setTyping] = useState<string | null>(null);
  return (
    <div className={className}>
      <FieldLabel htmlFor={id}>{label}</FieldLabel>
      <Input
        id={id}
        inputMode="decimal"
        autoComplete="off"
        className="mt-1.5 font-mono"
        value={typing ?? shown(value, decimals)}
        onChange={(e) => {
          setTyping(e.target.value);
          const v = Number(e.target.value.replace(",", "."));
          if (e.target.value.trim() !== "" && Number.isFinite(v)) onChange(v);
        }}
        onBlur={() => setTyping(null)}
      />
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="space-y-3 p-4">
      <h3 className="text-sm font-medium">{title}</h3>
      {children}
    </section>
  );
}

/** Everything about the template that is a number or a choice. The stage handles the dragging. */
export function LabelInspector({
  draft,
  onChange,
  lock,
  onLock,
  sampleText,
  onSampleText,
  file,
  onFile,
  onContent,
  onOpenSizes,
}: {
  draft: Draft;
  onChange: (d: Draft) => void;
  lock: boolean;
  onLock: (v: boolean) => void;
  sampleText: string;
  onSampleText: (v: string) => void;
  file: File | null;
  onFile: (f: File | null) => void;
  onContent: (c: Draft["content"]) => void;
  onOpenSizes: () => void;
}) {
  const cat = LABEL_SIZES.find((s) => s.id === draft.sizeRef);
  const dpis = DPIS.includes(draft.dpi) ? DPIS : [...DPIS, draft.dpi].sort((a, b) => a - b);
  const place = (k: keyof LabelBox) => (v: number) => onChange(setPlace(draft, k, v, lock));
  const crop = (k: keyof LabelSpec["crop_percent"]) => (v: number) => onChange(setCrop(draft, k, v));
  const textField = (k: "wrap_chars" | "lines_per_label") => (v: number) =>
    onChange({ ...draft, text: { ...draft.text, [k]: Math.min(200, Math.max(0, Math.round(v))) } });

  return (
    <>
      <Section title="Label size">
        <Button variant="outline" className="h-auto w-full justify-start gap-3 py-2" onClick={onOpenSizes}>
          <PaperThumb
            label={draft.label}
            round={!!cat?.round}
            max={28}
            className="mx-0.5"
          />
          <span className="min-w-0 flex-1 text-left">
            <span className="block truncate">
              {cat ? `${cat.group === "Standard" ? "" : `${cat.group} `}${cat.name}` : "Custom size"}
            </span>
            <span className="block font-mono text-xs font-normal text-muted-foreground">
              {cat && cat.h !== 0 ? sizeLabel(cat) : "Type the size below"}
            </span>
          </span>
          <ChevronsUpDown className="text-muted-foreground" />
        </Button>
        <div className="grid grid-cols-2 gap-3">
          <NumField
            id="label-w"
            label="Width mm"
            value={draft.label.w}
            onChange={(v) => onChange(setLabelDim(draft, "w", v))}
          />
          <NumField
            id="label-h"
            label="Height mm"
            value={draft.label.h}
            onChange={(v) => onChange(setLabelDim(draft, "h", v))}
          />
        </div>
      </Section>

      <Separator />

      <Section title="Content">
        <Tabs value={draft.content} onValueChange={(v) => onContent(v as Draft["content"])}>
          <TabsList className="w-full">
            <TabsTrigger value="image">
              <ImageIcon /> Image
            </TabsTrigger>
            <TabsTrigger value="text">
              <Type /> Text
            </TabsTrigger>
          </TabsList>

          <TabsContent value="image" className="mt-3 space-y-2">
            <Dropzone
              file={file}
              onFile={onFile}
              accept="image/gif,image/png,image/jpeg"
              hint="A GIF, PNG or JPG to try the template on. It stays in your browser."
            />
            {!file && (
              <p className="text-xs text-muted-foreground">Showing the built-in sample label.</p>
            )}
          </TabsContent>

          <TabsContent value="text" className="mt-3 space-y-3">
            <div>
              <FieldLabel htmlFor="sample-text">Sample text</FieldLabel>
              <Input
                id="sample-text"
                autoComplete="off"
                className="mt-1.5 font-mono"
                value={sampleText}
                onChange={(e) => onSampleText(e.target.value)}
              />
            </div>
            <div className="grid grid-cols-2 gap-3">
              <NumField
                id="wrap"
                label="Wrap at"
                decimals={0}
                value={draft.text.wrap_chars}
                onChange={textField("wrap_chars")}
              />
              <NumField
                id="lines"
                label="Lines per label"
                decimals={0}
                value={draft.text.lines_per_label}
                onChange={textField("lines_per_label")}
              />
            </div>
            <p className="text-xs text-muted-foreground">
              0 means no wrapping, and all lines on one label.
            </p>
          </TabsContent>
        </Tabs>
      </Section>

      <Separator />

      <Section title="Position">
        <div className="grid grid-cols-2 gap-3">
          <NumField id="pos-x" label="X mm" value={draft.place.x} onChange={place("x")} />
          <NumField id="pos-y" label="Y mm" value={draft.place.y} onChange={place("y")} />
          <NumField id="pos-w" label="W mm" value={draft.place.w} onChange={place("w")} />
          <NumField id="pos-h" label="H mm" value={draft.place.h} onChange={place("h")} />
        </div>
        <label className="flex items-center gap-2.5 text-sm">
          <Switch checked={lock} onCheckedChange={onLock} />
          Lock aspect ratio
        </label>
      </Section>

      {draft.content === "image" && (
        <>
          <Separator />
          <Section title="Crop">
            <div className="grid grid-cols-4 gap-2">
              {(["t", "r", "b", "l"] as const).map((k) => (
                <NumField
                  key={k}
                  id={`crop-${k}`}
                  label={`${k.toUpperCase()} %`}
                  value={draft.crop[k]}
                  onChange={crop(k)}
                  className="min-w-0"
                />
              ))}
            </div>
            <p className="text-xs text-muted-foreground">
              Cut from the image after it is turned: top, right, bottom, left.
            </p>
          </Section>
        </>
      )}

      <Separator />

      <Section title="Printer resolution">
        <Select value={String(draft.dpi)} onValueChange={(v) => onChange({ ...draft, dpi: Number(v) })}>
          <SelectTrigger className={cn("w-full")}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {dpis.map((d) => (
              <SelectItem key={d} value={String(d)}>
                {d} dpi
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </Section>
    </>
  );
}
