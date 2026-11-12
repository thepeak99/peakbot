// The phone's panels bottom sheet (mobile-redesign design §2.5): a detent
// Sheet hosting App's existing `visibleTabs` — one open-panel state
// (`drawerTab`), two surfaces. The floating "working… ■ Stop" pill rides the
// sheet's top edge via the `above` slot. `PanelsButton` (icon + combined
// badge) is shared by ChatHeader and TopBar.

import type { JSX } from "react";
import { Sheet } from "./Sheet";
import type { DrawerTab } from "./TabbedDrawer";

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
      className="relative flex h-9 w-9 shrink-0 cursor-pointer items-center justify-center rounded text-zinc-400 transition-colors hover:text-zinc-200"
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        aria-hidden
        className="h-5 w-5"
      >
        <path d="M4 8h8M18 8h2M4 16h2M12 16h8" />
        <circle cx="15" cy="8" r="2.5" />
        <circle cx="9" cy="16" r="2.5" />
      </svg>
      {count > 0 && (
        <span
          data-testid="panels-badge"
          className="absolute -right-1 -top-1 min-w-4 rounded-full bg-sky-600 px-1 text-center font-mono text-[10px] leading-4 text-white"
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
      <div
        role="tablist"
        aria-label="Panels"
        className="flex gap-2 overflow-x-auto px-4 pb-3 pt-1"
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
              className={`flex h-10 flex-none cursor-pointer items-center rounded-full border px-4 text-sm transition-colors ${
                selected
                  ? "border-zinc-100 bg-zinc-100 font-semibold text-zinc-900"
                  : "border-zinc-700 bg-zinc-800/60 font-medium text-zinc-400 hover:text-zinc-200"
              }`}
            >
              {t.label}
              {t.badge != null && t.badge > 0 && (
                <span
                  data-testid="panels-pill-badge"
                  className={`ml-2 rounded-full px-1.5 font-mono text-[11px] leading-4 ${
                    selected ? "bg-zinc-900/20 text-zinc-900" : "bg-sky-600 text-white"
                  }`}
                >
                  {t.badge}
                </span>
              )}
            </button>
          );
        })}
      </div>
      <div role="tabpanel" className="min-h-0 px-4 pb-4">
        {activeTab?.content}
      </div>
    </Sheet>
  );
}
