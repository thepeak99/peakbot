// Bottom-sheet primitive for the phone UI (mobile-redesign design §2.5).
// Portaled to document.body — the ChatHeader/TopBar `backdrop-blur` makes a
// stacking context that would trap a `fixed` child. Owns the whole dismiss
// contract: Done, backdrop, Esc, OS Back (one `useBackDismiss` layer) and,
// for detent sheets, drag-dismiss. Mounted = open: the parent renders it
// conditionally, and `onClose` must stay stable while mounted.

import {
  useEffect,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { useBackDismiss } from "../useBackDismiss";
import { detentHeight, dragHeight, settleDrag } from "../sheetDrag";

export interface SheetProps {
  /** Visible title AND the dialog's accessible name. */
  title: string;
  /** Done / backdrop / Esc / OS Back / drag-dismiss. MUST be stable while
   * mounted (useBackDismiss releases and re-pushes on a new identity). */
  onClose(): void;
  /** Panels only: medium/large detents + draggable grabber/header. Absent:
   * content height (max 92dvh), no grabber, not draggable. */
  detents?: boolean;
  /** Rides the sheet's top edge, outside the clipped panel (the Panels
   * working pill). */
  above?: ReactNode;
  children: ReactNode;
}

/** One in-flight drag: the pointer and height at `pointerdown` plus the
 * clock start, so the release velocity is measurable. */
interface DragState {
  startY: number;
  startH: number;
  t0: number;
}

export function Sheet({ title, onClose, detents = false, above, children }: SheetProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const drag = useRef<DragState | null>(null);
  const [detent, setDetent] = useState<"medium" | "large">("medium");
  const [dragging, setDragging] = useState(false);
  const [vh, setVh] = useState(() => window.innerHeight);

  // Focusing the dialog blurs the composer and drops the soft keyboard.
  useEffect(() => {
    dialogRef.current?.focus();
  }, []);

  // One document listener covers every Esc for this sheet.
  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Detent heights are fractions of the viewport, so they must track resize
  // while a detent sheet is mounted (one listener).
  useEffect(() => {
    if (!detents) return;
    const onResize = (): void => setVh(window.innerHeight);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [detents]);

  useBackDismiss(true, onClose);

  // The detent owns the dialog height; while dragging the pointer owns it,
  // so this skips the drag and the settle re-applies the settled detent.
  useEffect(() => {
    if (!detents || dragging) return;
    dialogRef.current?.style.setProperty("height", `${detentHeight(detent, vh)}px`);
  }, [detents, detent, vh, dragging]);

  const startDrag = (e: ReactPointerEvent): void => {
    if (!detents) return;
    // A press on the Done button is a tap, not a drag.
    if ((e.target as HTMLElement).closest("button")) return;
    const el = dialogRef.current;
    if (!el) return;
    const h = parseInt(el.style.height, 10);
    drag.current = {
      startY: e.clientY,
      startH: Number.isNaN(h) ? detentHeight(detent, vh) : h,
      t0: performance.now(),
    };
    // jsdom has no pointer capture; real browsers retarget the moves to the
    // grabber so they keep bubbling to the dialog's handlers below.
    e.currentTarget.setPointerCapture?.(e.pointerId);
    setDragging(true);
  };

  const onMove = (e: ReactPointerEvent): void => {
    const d = drag.current;
    if (!d) return;
    dialogRef.current?.style.setProperty("height", `${dragHeight(d.startH, e.clientY - d.startY, vh)}px`);
  };

  const settle = (e: ReactPointerEvent): void => {
    const d = drag.current;
    if (!d) return;
    drag.current = null;
    setDragging(false);
    const dy = e.clientY - d.startY;
    const h = dragHeight(d.startH, dy, vh);
    const result = settleDrag({ h, dy, dtMs: Math.max(1, performance.now() - d.t0), vh });
    if (result === "close") onClose();
    else setDetent(result);
  };

  return createPortal(
    <div className="fixed inset-0 z-50">
      <div
        data-testid="sheet-backdrop"
        className="absolute inset-0 bg-black/55 light:bg-black/40"
        onClick={onClose}
      />
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        data-testid="sheet"
        tabIndex={-1}
        data-detent={detents ? detent : undefined}
        data-dragging={dragging ? "" : undefined}
        className={`absolute inset-x-0 bottom-0 ${
          detents && !dragging ? "transition-[height] duration-150" : ""
        }`}
        onPointerMove={onMove}
        onPointerUp={settle}
        onPointerCancel={settle}
      >
        {above && <div className="absolute inset-x-0 bottom-full mb-3.5 flex justify-center px-4">{above}</div>}
        <div
          className={`${detents ? "h-full" : "max-h-[92dvh]"} flex flex-col overflow-hidden rounded-t-sheet border-t border-zinc-700 bg-zinc-900 pb-[env(safe-area-inset-bottom)] shadow-[0_-12px_40px_rgba(0,0,0,0.5)] motion-safe:animate-sheet-in`}
        >
          {/* The mock's .grab is on every sheet; only detent sheets drag from it. */}
          <div
            data-testid="sheet-grabber"
            onPointerDown={detents ? startDrag : undefined}
            className={`flex flex-none justify-center pt-2 ${detents ? "touch-none" : ""}`}
          >
            <div className="h-[5px] w-[38px] rounded-[3px] bg-zinc-700" />
          </div>
          <div
            data-testid="sheet-header"
            onPointerDown={startDrag}
            className="flex flex-none touch-none items-center justify-between gap-2 px-[18px] pb-1.5 pt-2.5"
          >
            <h2 className="text-[17px] font-bold tracking-[-0.01em] text-zinc-100">{title}</h2>
            <button
              type="button"
              data-testid="sheet-done"
              onClick={onClose}
              className="flex h-11 cursor-pointer items-center px-2 text-base font-semibold text-sky-400"
            >
              Done
            </button>
          </div>
          <div className="flex-1 overflow-y-auto overscroll-contain">{children}</div>
        </div>
      </div>
    </div>,
    document.body,
  );
}
