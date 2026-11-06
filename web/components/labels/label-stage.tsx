"use client";

import { useEffect, useRef, useState } from "react";
import type { LabelBox } from "@/lib/api";
import { moveSnapped, resizeFrom, type Size } from "@/lib/label-geometry";
import { cn } from "@/lib/utils";

type Handle = "n" | "s" | "e" | "w" | "ne" | "nw" | "se" | "sw";

const MM_PER_INCH = 25.4;
/** Room around the label for the rulers and the handles, in px. */
const PAD = 40;
const MAX_ZOOM = 18;
/** Snap distance in screen px. */
const SNAP_PX = 7;
const STEP_MM = 0.5;
const BIG_STEP_MM = 5;
/** Ruler ticks sit at least this many px apart. */
const TICK_PX = 40;
const TICK_STEPS = [1, 2, 5, 10, 20, 50, 100];

const HANDLES: { id: Handle; pos: string; corner: boolean }[] = [
  { id: "nw", pos: "top-0 left-0 -translate-x-1/2 -translate-y-1/2 cursor-nwse-resize", corner: true },
  { id: "ne", pos: "top-0 right-0 translate-x-1/2 -translate-y-1/2 cursor-nesw-resize", corner: true },
  { id: "sw", pos: "bottom-0 left-0 -translate-x-1/2 translate-y-1/2 cursor-nesw-resize", corner: true },
  { id: "se", pos: "right-0 bottom-0 translate-x-1/2 translate-y-1/2 cursor-nwse-resize", corner: true },
  { id: "n", pos: "top-0 left-1/2 -translate-x-1/2 -translate-y-1/2 cursor-ns-resize", corner: false },
  { id: "s", pos: "bottom-0 left-1/2 -translate-x-1/2 translate-y-1/2 cursor-ns-resize", corner: false },
  { id: "w", pos: "top-1/2 left-0 -translate-x-1/2 -translate-y-1/2 cursor-ew-resize", corner: false },
  { id: "e", pos: "top-1/2 right-0 translate-x-1/2 -translate-y-1/2 cursor-ew-resize", corner: false },
];

/** Paints a canvas by copying it into its own, so the same source can show up twice. */
function CanvasView({ source, className }: { source: HTMLCanvasElement; className?: string }) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const c = ref.current;
    if (!c) return;
    c.width = source.width;
    c.height = source.height;
    c.getContext("2d")!.drawImage(source, 0, 0);
  }, [source]);
  return <canvas ref={ref} className={cn("block size-full", className)} />;
}

/** The label drawn to scale, as small as a card needs it. `box` is the content in mm. */
export function PaperThumb({
  label,
  round = false,
  box,
  max = 56,
  className,
}: {
  label: Size;
  round?: boolean;
  box?: LabelBox;
  max?: number;
  className?: string;
}) {
  // a continuous roll has no length; show it as a tall strip
  const h = label.h || label.w * 2;
  const k = max / Math.max(label.w, h);
  return (
    <span
      className={cn(
        "relative block shrink-0 overflow-hidden bg-white ring-1 ring-foreground/20",
        round ? "rounded-full" : "rounded-[3px]",
        className,
      )}
      style={{ width: Math.max(6, label.w * k), height: Math.max(4, h * k) }}
      aria-hidden
    >
      {box && (
        <span
          className="absolute border border-brand bg-brand/25"
          style={{ left: box.x * k, top: box.y * k, width: box.w * k, height: box.h * k }}
        />
      )}
    </span>
  );
}

type Drag = { mode: Handle | "move"; x: number; y: number; start: LabelBox };

/**
 * The label to scale with the content on it. Drag the content to move it (it snaps to the label's
 * edges and centre), drag a handle to resize, or use the arrow keys (Shift for bigger steps).
 * What hangs over the edge is drawn faint; what prints is drawn solid.
 */
export function LabelStage({
  label,
  round,
  box,
  lock,
  content,
  preview,
  dpi,
  onBox,
}: {
  label: Size;
  round: boolean;
  box: LabelBox;
  lock: boolean;
  content: HTMLCanvasElement;
  /** The black-and-white render of the whole label; replaces the content when set. */
  preview: HTMLCanvasElement | null;
  dpi: number;
  onBox: (b: LabelBox) => void;
}) {
  const holder = useRef<HTMLDivElement>(null);
  const [avail, setAvail] = useState<Size>({ w: 0, h: 0 });
  const [guide, setGuide] = useState<{ x: number | null; y: number | null }>({ x: null, y: null });
  const drag = useRef<Drag | null>(null);

  useEffect(() => {
    const el = holder.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => setAvail({ w: e.contentRect.width, h: e.contentRect.height }));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const zoom = Math.max(
    0.1,
    Math.min((avail.w - PAD * 2) / label.w, (avail.h - PAD * 2) / label.h, MAX_ZOOM),
  );
  const px = (mm: number) => mm * zoom;
  const step = TICK_STEPS.find((s) => s * zoom >= TICK_PX) ?? 100;
  const ticks = (max: number) => Array.from({ length: Math.floor(max / step) + 1 }, (_, i) => i * step);

  function down(e: React.PointerEvent<HTMLDivElement>) {
    if (e.button !== 0) return;
    const mode = ((e.target as HTMLElement).dataset.handle as Handle | undefined) ?? "move";
    drag.current = { mode, x: e.clientX, y: e.clientY, start: box };
    e.currentTarget.setPointerCapture(e.pointerId);
    e.currentTarget.focus({ preventScroll: true });
    e.preventDefault();
  }
  function move(e: React.PointerEvent<HTMLDivElement>) {
    const d = drag.current;
    if (!d) return;
    const dx = (e.clientX - d.x) / zoom;
    const dy = (e.clientY - d.y) / zoom;
    if (d.mode === "move") {
      const m = moveSnapped(d.start, d.start.x + dx, d.start.y + dy, label, SNAP_PX / zoom);
      setGuide({ x: m.guideX, y: m.guideY });
      onBox(m.box);
    } else {
      onBox(resizeFrom(d.mode, d.start, dx, dy, lock));
    }
  }
  function up() {
    drag.current = null;
    setGuide({ x: null, y: null });
  }
  function key(e: React.KeyboardEvent<HTMLDivElement>) {
    const dir = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[e.key];
    if (!dir) return;
    e.preventDefault();
    const s = e.shiftKey ? BIG_STEP_MM : STEP_MM;
    onBox(moveSnapped(box, box.x + dir[0] * s, box.y + dir[1] * s, label, 0).box);
  }

  const place = { left: px(box.x), top: px(box.y), width: px(box.w), height: px(box.h) };

  return (
    <div ref={holder} className="relative h-[min(70svh,34rem)] min-h-72 overflow-hidden bg-muted/40">
      {avail.w > 0 && (
        <div
          className="absolute"
          style={{
            width: px(label.w),
            height: px(label.h),
            left: (avail.w - px(label.w)) / 2,
            top: (avail.h - px(label.h)) / 2,
          }}
        >
          {/* rulers, in mm */}
          <div className="pointer-events-none absolute -top-5 left-0 h-4 w-full font-mono text-[10px] text-muted-foreground select-none">
            {ticks(label.w).map((v) => (
              <span key={v} className="absolute bottom-0 -translate-x-1/2" style={{ left: px(v) }}>
                {v}
              </span>
            ))}
          </div>
          <div className="pointer-events-none absolute top-0 -left-5 h-full w-4 font-mono text-[10px] text-muted-foreground select-none">
            {ticks(label.h).map((v) => (
              <span
                key={v}
                className="absolute right-0 -translate-y-1/2"
                style={{ top: px(v) }}
              >
                {v}
              </span>
            ))}
          </div>

          {/* what hangs over the edge, faint */}
          {!preview && (
            <div className="pointer-events-none absolute opacity-25" style={place}>
              <CanvasView source={content} />
            </div>
          )}

          {/* the label: white in both themes, it is paper */}
          <div
            className={cn(
              "absolute inset-0 overflow-hidden bg-white shadow-sm ring-1 ring-foreground/20",
              round ? "rounded-full" : "rounded-[2px]",
            )}
          >
            {preview ? (
              <CanvasView
                source={preview}
                className={zoom >= dpi / MM_PER_INCH ? "[image-rendering:pixelated]" : undefined}
              />
            ) : (
              <div className="absolute" style={place}>
                <CanvasView source={content} />
              </div>
            )}
            {guide.x !== null && (
              <span
                className="absolute inset-y-0 w-px bg-brand"
                style={{ left: px(guide.x) }}
              />
            )}
            {guide.y !== null && (
              <span
                className="absolute inset-x-0 h-px bg-brand"
                style={{ top: px(guide.y) }}
              />
            )}
          </div>

          {/* the selection */}
          <div
            role="group"
            tabIndex={0}
            aria-label="Label content. Drag to move, arrow keys to nudge by 0.5 mm, Shift for 5 mm."
            className="absolute cursor-move touch-none border-[1.5px] border-brand outline-none focus-visible:ring-3 focus-visible:ring-brand/40"
            style={place}
            onPointerDown={down}
            onPointerMove={move}
            onPointerUp={up}
            onPointerCancel={up}
            onKeyDown={key}
          >
            {HANDLES.filter((h) => h.corner || !lock).map((h) => (
              <span
                key={h.id}
                data-handle={h.id}
                className={cn(
                  "absolute size-2.5 rounded-[2px] border-[1.5px] border-brand bg-card after:absolute after:-inset-2 after:content-['']",
                  h.pos,
                )}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
