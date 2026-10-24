// The save bar + restart banner (spec §9.6).
//
// Save bar (only when the scope is dirty): "Unsaved changes" (no count),
// exactly ONE chip — green "● applies on next /new" or amber "● restart
// required" (iff validate.restart_required is non-empty), buttons
// Discard · View diff · Save (primary). Validate is debounced 400 ms
// (trailing) after each draft edit. The Save button element must stay
// stable across re-renders — it is a fixed child of the bar (no keys, no
// conditional wrapper), so React reconciliation keeps the same DOM node.
//
// Restart banner: bottom banner after a save with restart_required.
// "Restart now" ONLY in the desktop app (the caller navigates to
// "peakbot-desktop:restart"); a browser shows "Restart PeakBot to apply."

import { useEffect, useRef } from "react";
import type { Scope } from "./types";

const VALIDATE_DEBOUNCE_MS = 400;

export interface SaveBarProps {
  scope: Scope;
  dirty: boolean;
  /** Increments on every draft edit; re-arms the 400 ms validate debounce. */
  draftVersion: number;
  /** Server-computed from the last validate; the client keeps no boot-only list. */
  restartRequired: string[];
  saving: boolean;
  /** 409 from the last PUT. */
  stale: boolean;
  staleMessage: string | null;
  /** Debounced (400 ms, trailing) by SaveBar while dirty; also on
      Save / View diff / Raw blur (the shell passes those through here). */
  onValidate(): void;
  onSave(): void;
  onDiscard(): void;
  onReload(): void; // stale banner Reload
  onViewDiff(): void;
}

export function SaveBar({
  scope,
  dirty,
  draftVersion,
  restartRequired,
  saving,
  stale,
  staleMessage,
  onValidate,
  onSave,
  onDiscard,
  onReload,
  onViewDiff,
}: SaveBarProps) {
  // Keep the latest callback without re-arming the debounce on re-render.
  const onValidateRef = useRef(onValidate);
  onValidateRef.current = onValidate;

  // Trailing 400 ms debounce, re-armed on every draft edit (draftVersion)
  // and skipped entirely for a clean scope.
  useEffect(() => {
    if (!dirty) return;
    const t = setTimeout(() => onValidateRef.current(), VALIDATE_DEBOUNCE_MS);
    return () => clearTimeout(t);
  }, [dirty, draftVersion]);

  if (!dirty) return null;

  return (
    <div data-testid="savebar" className="border-t border-zinc-800 bg-zinc-950">
      {stale && staleMessage && (
        <div
          data-testid="savebar-stale"
          className="flex items-center justify-between gap-3 border-b border-red-900/60 bg-red-950/50 px-4 py-1.5 text-xs text-red-300"
        >
          <span>{staleMessage}</span>
          <button
            data-testid="savebar-stale-reload"
            onClick={onReload}
            className="shrink-0 cursor-pointer rounded border border-red-800 px-2 py-0.5 text-red-200 hover:bg-red-900/40"
          >
            Reload
          </button>
        </div>
      )}
      <div className="flex flex-wrap items-center gap-3 px-4 py-2">
        <span data-testid="savebar-unsaved" className="text-xs text-zinc-300">
          ⚠ Unsaved changes
          {scope === "repo" && (
            <span className="ml-2 rounded border border-zinc-700 px-1.5 py-0.5 text-[10px] text-zinc-400">
              📁 repo
            </span>
          )}
        </span>

        {restartRequired.length > 0 ? (
          <span
            data-testid="savebar-chip-restart"
            className="rounded-full border border-amber-700/60 bg-amber-950/40 px-2 py-0.5 text-[11px] text-amber-400"
          >
            ● restart required
          </span>
        ) : (
          <span
            data-testid="savebar-chip-nextnew"
            className="rounded-full border border-emerald-700/60 bg-emerald-950/40 px-2 py-0.5 text-[11px] text-emerald-400"
          >
            ● applies on next /new
          </span>
        )}

        <div className="ml-auto flex items-center gap-2">
          <button
            data-testid="savebar-discard"
            onClick={onDiscard}
            className="cursor-pointer rounded border border-zinc-700 px-3 py-1 text-xs text-zinc-300 hover:bg-zinc-800"
          >
            Discard
          </button>
          <button
            data-testid="savebar-diff"
            onClick={onViewDiff}
            className="cursor-pointer rounded border border-zinc-700 px-3 py-1 text-xs text-zinc-300 hover:bg-zinc-800"
          >
            View diff
          </button>
          <button
            data-testid="savebar-save"
            onClick={onSave}
            disabled={saving}
            className="cursor-pointer rounded bg-emerald-700 px-4 py-1 text-xs font-medium text-white hover:bg-emerald-600 disabled:cursor-default disabled:opacity-60"
          >
            {saving ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}

export interface RestartBannerProps {
  keys: string[]; // the restart_required entries
  desktop: boolean; // isDesktop()
  onLater(): void;
  onRestartNow(): void; // caller navigates to "peakbot-desktop:restart"
}

export function RestartBanner({ keys, desktop, onLater, onRestartNow }: RestartBannerProps) {
  return (
    <div
      data-testid="restart-banner"
      className="flex flex-wrap items-center gap-3 border-t border-amber-700/60 bg-amber-950/60 px-4 py-2 text-xs text-amber-300"
    >
      <span>
        ⚠ Some changes are restart-required ({keys.join(", ")}) — they take effect after a restart.
      </span>
      {desktop ? (
        <button
          data-testid="restart-banner-restart"
          onClick={onRestartNow}
          className="ml-auto cursor-pointer rounded bg-amber-600 px-3 py-1 font-medium text-black hover:bg-amber-500"
        >
          Restart now
        </button>
      ) : (
        <span className="ml-auto text-amber-400/80">Restart PeakBot to apply.</span>
      )}
      <button
        data-testid="restart-banner-later"
        onClick={onLater}
        className="cursor-pointer rounded border border-amber-700 px-3 py-1 text-amber-200 hover:bg-amber-900/40"
      >
        Later
      </button>
    </div>
  );
}
