// The phone's panels bottom sheet (mobile-redesign design §2.5): a detent
// Sheet hosting App's existing `visibleTabs` — one open-panel state
// (`drawerTab`), two surfaces. The floating "working… ■ Stop" pill rides the
// sheet's top edge via the `above` slot. `PanelsButton` (icon + combined
// badge) is shared by ChatHeader and TopBar.

import { useEffect, useRef, type JSX } from "react";
import { Sheet } from "./Sheet";
import type { DrawerTab } from "./TabbedDrawer";
import { IB, IBI_HI, PILL_OFF, PILL_ON } from "./phoneUi";

export interface PanelsButtonProps {
  /** Running background tasks + open todos, summed over visible tabs. */
  count: number;
  onOpen(): void;
}

export function PanelsButton({ count, onOpen }: PanelsButtonProps): JSX.Element {
  return (
    <button
      type="button"
      data-testid="panels-button"
      aria-label="Open panels"
      onClick={onOpen}
      className={IB}
    >
      <span className={IBI_HI}>
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.9"
          strokeLinecap="round"
          aria-hidden
          className="h-5 w-5"
        >
          <path d="M4 8h8M18 8h2M4 16h2M12 16h8" />
          <circle cx="15" cy="8" r="2.5" />
          <circle cx="9" cy="16" r="2.5" />
        </svg>
      </span>
      {count > 0 && (
        <span
          data-testid="panels-badge"
          className="absolute right-0 top-px h-[18px] min-w-[18px] rounded-[9px] bg-sky-600 px-[5px] text-center font-mono text-[11px] font-semibold leading-[18px] text-white shadow-[0_0_0_2px_var(--color-zinc-950)]"
        >
          {count}
        </span>
      )}
    </button>
  );
}

export interface PanelsSheetProps {
  /** App's visibleTabs (≥ 1; App never mounts it with 0). */
  tabs: DrawerTab[];
  /** drawerTab; if not in tabs for one render → empty body (the relocation
   * effect settles it on the next commit). */
  active: string;
  onActiveChange(id: string): void;
  /** Stable (App's useCallback) — the Sheet's back layer requires it. */
  onClose(): void;
  /** isRunning → the floating working pill. */
  running: boolean;
  /** send({type:"stop"}) — same as the composer Stop. */
  onStop(): void;
}

export function PanelsSheet({
  tabs,
  active,
  onActiveChange,
  onClose,
  running,
  onStop,
}: PanelsSheetProps): JSX.Element {
  const activeTab = tabs.find((t) => t.id === active);
  const stripRef = useRef<HTMLDivElement>(null);
  // Keep the selected pill in view when the strip overflows (the mock centres it).
  useEffect(() => {
    stripRef.current
      ?.querySelector<HTMLElement>('[aria-selected="true"]')
      ?.scrollIntoView?.({ inline: "center", block: "nearest", behavior: "smooth" });
  }, [active]);
  return (
    <Sheet
      title="Panels"
      onClose={onClose}
      detents
      above={
        running ? (
          <div
            data-testid="panels-working"
            className="flex h-11 items-center gap-1 rounded-full border border-zinc-700 bg-zinc-900/90 px-1.5 pl-4 shadow-xl backdrop-blur"
          >
            <span className="flex items-center gap-2 text-sm font-medium text-amber-400">
              <span className="h-2 w-2 animate-spin rounded-full border-2 border-amber-400 border-t-transparent" />
              working…
            </span>
            <span aria-hidden className="mx-1.5 h-5 w-px bg-zinc-700" />
            <button
              type="button"
              data-testid="panels-stop"
              aria-label="Stop"
              onClick={onStop}
              className="flex h-9 cursor-pointer items-center rounded-full bg-red-500/15 px-3.5 text-sm font-semibold text-red-300"
            >
              ■ Stop
            </button>
          </div>
        ) : null
      }
    >
      <div className="relative flex-none">
        <div
          ref={stripRef}
          role="tablist"
          aria-label="Panels"
          className="flex gap-2 overflow-x-auto px-4 pb-3 pt-1.5"
          style={{ scrollbarWidth: "none" }}
        >
          {tabs.map((t) => {
            const selected = active === t.id;
            return (
              <button
                key={t.id}
                type="button"
                role="tab"
                data-testid="panels-pill"
                data-tab={t.id}
                aria-selected={selected}
                onClick={() => onActiveChange(t.id)}
                className={`flex h-10 flex-none cursor-pointer items-center rounded-[20px] border px-[15px] text-sm transition-colors ${
                  selected ? PILL_ON : PILL_OFF
                }`}
              >
                {t.label}
                {t.badge != null && t.badge > 0 && (
                  <span
                    data-testid="panels-pill-badge"
                    className="ml-[7px] h-4 min-w-4 rounded-lg bg-sky-600 px-1 text-center font-mono text-[10px] font-semibold leading-4 text-white"
                  >
                    {t.badge}
                  </span>
                )}
              </button>
            );
          })}
        </div>
        {/* Right-edge fade: the strip scrolls sideways (mock .pills::after). */}
        <span aria-hidden className="pointer-events-none absolute inset-y-0 right-0 w-9 bg-gradient-to-r from-transparent to-zinc-900" />
      </div>
      <div role="tabpanel" className="min-h-0 px-4 pb-2 pt-1">
        {activeTab?.content}
      </div>
    </Sheet>
  );
}
