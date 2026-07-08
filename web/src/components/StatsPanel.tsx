import type { ContextUsage, SessionStats } from "../types";

function fmtTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between">
      <span className="text-zinc-500">{label}</span>
      <span className="font-mono tabular-nums text-zinc-300">{value}</span>
    </div>
  );
}

// Session stats + context-usage meter. Mirrors SessionState / ContextState
// (src/ui/app_state.rs). The meter turns amber past the compaction
// threshold, red when full — same signal the TUI status bar carries.
export function StatsPanel({
  stats,
  context,
}: {
  stats: SessionStats;
  context: ContextUsage;
}) {
  const pct = context.windowSize
    ? (context.currentUsage / context.windowSize) * 100
    : 0;
  const thresholdPct = context.compactionThreshold * 100;
  const barColor =
    pct >= 95 ? "bg-red-500" : pct >= thresholdPct ? "bg-amber-500" : "bg-emerald-500";

  return (
    <div className="space-y-4">
      <section>
        <h3 className="mb-2 text-[11px] font-semibold uppercase tracking-wide text-zinc-500">
          Session
        </h3>
        <div className="space-y-1 text-xs">
          <Row label="model" value={stats.modelAlias} />
          <Row label="in" value={fmtTokens(stats.inputTokens)} />
          <Row label="out" value={fmtTokens(stats.outputTokens)} />
          <Row label="calls" value={String(stats.apiCalls)} />
          <Row label="cost" value={`$${stats.costUsd.toFixed(4)}`} />
        </div>
      </section>

      <section>
        <h3 className="mb-2 text-[11px] font-semibold uppercase tracking-wide text-zinc-500">
          Context
        </h3>
        <div className="mb-1.5 flex items-baseline justify-between text-xs">
          <span className="font-mono tabular-nums text-zinc-400">
            {fmtTokens(context.currentUsage)} / {fmtTokens(context.windowSize)}
          </span>
          <span className="font-mono tabular-nums text-zinc-400">{pct.toFixed(0)}%</span>
        </div>
        <div className="relative h-2 overflow-hidden rounded-full bg-zinc-800">
          <div
            className={`h-full rounded-full ${barColor}`}
            style={{ width: `${Math.min(pct, 100)}%` }}
          />
          <div
            className="absolute inset-y-0 w-px bg-zinc-500"
            style={{ left: `${thresholdPct}%` }}
            title={`compaction at ${thresholdPct.toFixed(0)}%`}
          />
        </div>
      </section>
    </div>
  );
}
