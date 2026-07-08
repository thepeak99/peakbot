import type { SessionStats } from "../types";

// Top status bar. Model alias, the working spinner (AppState.is_running),
// a compact tokens/cost readout mirroring the TUI status line, and a
// connection indicator.
export function TopBar({
  stats,
  isRunning,
  connected,
}: {
  stats: SessionStats | null;
  isRunning: boolean;
  connected: boolean;
}) {
  return (
    <header className="flex items-center gap-3 border-b border-zinc-800 bg-zinc-950/80 px-4 py-2 backdrop-blur">
      <div className="flex items-center gap-2">
        <span className="text-base">✦</span>
        <span className="font-semibold text-zinc-100">PeakBot</span>
      </div>

      {stats && (
        <button
          disabled
          className="flex items-center gap-1.5 rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300"
          title="Model switcher (Phase 2)"
        >
          <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
          {stats.modelAlias}
          <span className="text-zinc-600">▾</span>
        </button>
      )}

      {isRunning && (
        <span className="flex items-center gap-1.5 text-xs text-amber-400">
          <span className="h-3 w-3 animate-spin rounded-full border-2 border-amber-400 border-t-transparent" />
          working…
        </span>
      )}

      <div className="ml-auto flex items-center gap-4 font-mono text-[11px] tabular-nums text-zinc-500">
        {stats && (
          <>
            <span>{(stats.inputTokens + stats.outputTokens).toLocaleString()} tok</span>
            <span>${stats.costUsd.toFixed(4)}</span>
          </>
        )}
        <span
          className={`flex items-center gap-1.5 rounded px-1.5 py-0.5 ${
            connected ? "text-emerald-400" : "text-zinc-500"
          }`}
        >
          <span
            className={`h-1.5 w-1.5 rounded-full ${
              connected ? "bg-emerald-400" : "bg-zinc-600"
            }`}
          />
          {connected ? "connected" : "offline"}
        </span>
      </div>
    </header>
  );
}
