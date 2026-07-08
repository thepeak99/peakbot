import type { SessionStats } from "../mock";

// Top status bar. Model alias + a mock "switch" affordance, the working
// spinner (driven by AppState.is_running in Phase 1), and a compact
// tokens/cost readout mirroring the TUI status line.
export function TopBar({
  stats,
  isRunning,
}: {
  stats: SessionStats;
  isRunning: boolean;
}) {
  return (
    <header className="flex items-center gap-3 border-b border-zinc-800 bg-zinc-950/80 px-4 py-2 backdrop-blur">
      <div className="flex items-center gap-2">
        <span className="text-base">✦</span>
        <span className="font-semibold text-zinc-100">PeakBot</span>
      </div>

      <button
        disabled
        className="flex items-center gap-1.5 rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300"
        title="Model switcher (Phase 2)"
      >
        <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
        {stats.modelAlias}
        <span className="text-zinc-600">▾</span>
      </button>

      {isRunning && (
        <span className="flex items-center gap-1.5 text-xs text-amber-400">
          <span className="h-3 w-3 animate-spin rounded-full border-2 border-amber-400 border-t-transparent" />
          working…
        </span>
      )}

      <div className="ml-auto flex items-center gap-4 font-mono text-[11px] tabular-nums text-zinc-500">
        <span>{(stats.inputTokens + stats.outputTokens).toLocaleString()} tok</span>
        <span>${stats.costUsd.toFixed(4)}</span>
        <span className="rounded bg-zinc-800/80 px-1.5 py-0.5 text-zinc-400">
          static mock · Phase 0
        </span>
      </div>
    </header>
  );
}
