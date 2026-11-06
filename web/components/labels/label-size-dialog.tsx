"use client";

import { useMemo, useState } from "react";
import { ExternalLink } from "lucide-react";
import { LABEL_SIZES, SIZE_GROUPS, searchSizes, sizeLabel, type LabelSize } from "@/lib/label-sizes";
import { Button } from "@/components/ui/button";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { PaperThumb } from "@/components/labels/label-stage";

/** Pick a stock size from the catalogue. Applies on "Use this size", so the notes can be read first. */
export function LabelSizeDialog({
  open,
  onOpenChange,
  current,
  onPick,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** `size_ref` of the template, if it has one. */
  current: string | undefined;
  onPick: (size: LabelSize) => void;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="gap-0 p-0 sm:max-w-lg" showCloseButton={false}>
        <DialogHeader className="sr-only">
          <DialogTitle>Label size</DialogTitle>
          <DialogDescription>Search the stock sizes and pick the one on your roll.</DialogDescription>
        </DialogHeader>
        {/* The content unmounts on close, so the search and the pick start fresh each time. */}
        <SizePicker
          current={current}
          onPick={(s) => {
            onPick(s);
            onOpenChange(false);
          }}
        />
      </DialogContent>
    </Dialog>
  );
}

function SizePicker({
  current,
  onPick,
}: {
  current: string | undefined;
  onPick: (size: LabelSize) => void;
}) {
  const [query, setQuery] = useState("");
  const [picked, setPicked] = useState(current ?? "");
  const found = useMemo(() => searchSizes(query), [query]);
  const size = LABEL_SIZES.find((s) => s.id === picked);

  return (
    <>
      <Command shouldFilter={false} className="rounded-none! bg-transparent">
        <CommandInput
          value={query}
          onValueChange={setQuery}
          placeholder="Search a size, e.g. 4x8, DK-11209 or 57 x 32"
        />
        <CommandList className="max-h-72">
          <CommandEmpty>Nothing found. Type the size by hand instead.</CommandEmpty>
          {SIZE_GROUPS.map((group) => {
            const items = found.filter((s) => s.group === group);
            if (items.length === 0) return null;
            return (
              <CommandGroup key={group} heading={group}>
                {items.map((s) => (
                  <CommandItem
                    key={s.id}
                    value={s.id}
                    data-checked={s.id === picked}
                    onSelect={() => setPicked(s.id)}
                  >
                    <span className="flex w-7 shrink-0 justify-center">
                      <PaperThumb label={{ w: s.w, h: s.h }} round={s.round} max={22} />
                    </span>
                    <span className="min-w-0 flex-1 truncate">
                      {s.name}
                      {s.group !== "Standard" && (
                        <span className="ml-1.5 font-mono text-xs text-muted-foreground">{s.code}</span>
                      )}
                    </span>
                    <span className="shrink-0 font-mono text-xs text-muted-foreground">
                      {sizeLabel(s)}
                    </span>
                  </CommandItem>
                ))}
              </CommandGroup>
            );
          })}
        </CommandList>
      </Command>

      <div className="min-h-24 space-y-1 border-t px-4 py-3 text-sm">
        {size ? (
          <>
            <p className="font-medium">
              {size.name} <span className="font-mono text-xs text-muted-foreground">{sizeLabel(size)}</span>
            </p>
            {size.h === 0 && (
              <p className="text-muted-foreground">
                Continuous roll: the length is free. It starts at 60 mm, change it in the editor.
              </p>
            )}
            {size.note && <p className="text-muted-foreground">{size.note}</p>}
            {size.source ? (
              <a
                href={size.source.url}
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex items-center gap-1 text-muted-foreground underline underline-offset-3 hover:text-foreground"
              >
                Source: {size.source.label} <ExternalLink className="size-3" />
              </a>
            ) : (
              <p className="text-muted-foreground">Standard size, converted from the nominal size.</p>
            )}
          </>
        ) : (
          <p className="text-muted-foreground">Pick a size to see where it comes from.</p>
        )}
        <p className="pt-1 text-xs text-muted-foreground">
          Brand names only say which stock a size belongs to. printpapi is not affiliated with any of them.
        </p>
      </div>

      <div className="flex justify-end gap-2 rounded-b-xl border-t bg-muted/50 p-3">
        <DialogClose asChild>
          <Button variant="outline">Cancel</Button>
        </DialogClose>
        <Button variant="brand" disabled={!size} onClick={() => size && onPick(size)}>
          Use this size
        </Button>
      </div>
    </>
  );
}
