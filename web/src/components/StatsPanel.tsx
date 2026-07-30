import type { ContextUsage, SessionStats } from "../types";
import { viewLabel } from "../adapt";

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

// One figure in an agent card: micro-label above the number.
function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-[10px] uppercase tracking-wide text-zinc-600">{label}</div>
      <div className="font-mono tabular-nums text-zinc-300">{value}</div>
    </div>
  );
}

// Session stats + context-usage meter. Mirrors SessionState / ContextState
// (src/ui/app_state.rs). The meter turns amber past the compaction
// threshold, red when full — same signal the TUI status bar carries.
export function StatsPanel({
  stats,
  context,
  peakbotVersion,
  scopeLabel,
}: {
  stats: SessionStats;
  context: ContextUsage;
  /** PeakBot binary version (e.g. "0.13.3"). Populated from
   * `WelcomeState::peakbot_version` in AppState; empty until the first
   * wire snapshot that includes the field lands. */
  peakbotVersion?: string;
  /** When watching a single lane (a role or the orchestrator), the session
   * rows show that lane's numbers and this label names it. Null = global. */
  scopeLabel?: string | null;
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
        {scopeLabel && (
          <p className="mb-2 rounded border border-sky-900/60 bg-sky-950/30 px-2 py-1 text-[11px] text-sky-300">
            scoped to <span className="font-medium">{scopeLabel}</span>
          </p>
        )}
        <div className="space-y-1 text-xs">
          <Row label="model" value={stats.modelAlias} />
          {peakbotVersion && <Row label="peakbot" value={`v${peakbotVersion}`} />}
          <Row label="in" value={fmtTokens(stats.inputTokens)} />
          <Row label="out" value={fmtTokens(stats.outputTokens)} />
          <Row label="calls" value={String(stats.apiCalls)} />
          <Row label="cost" value={`$${stats.costUsd.toFixed(4)}`} />
        </div>
      </section>

      {stats.lanes.length > 1 && (
        <section>
          <h3 className="mb-2 text-[11px] font-semibold uppercase tracking-wide text-zinc-500">
            Agents · cumulative
          </h3>
          <div className="space-y-2">
            {stats.lanes.map((l) => (
              <div
                key={l.lane}
                className="rounded-md border border-zinc-800 bg-zinc-900/40 px-2.5 py-2"
              >
                <div className="mb-1.5 flex items-baseline justify-between gap-2">
                  <span className="truncate text-xs font-medium text-zinc-300">
                    {viewLabel(l.lane)}
                  </span>
                  <span
                    className="truncate font-mono text-[10px] text-zinc-500"
                    title={l.model || undefined}
                  >
                    {l.model || "—"}
                  </span>
                </div>
                <div className="grid grid-cols-3 gap-2 text-xs">
                  <Stat label="in" value={fmtTokens(l.inputTokens)} />
                  <Stat label="out" value={fmtTokens(l.outputTokens)} />
                  <Stat label="calls" value={String(l.apiCalls)} />
                </div>
              </div>
            ))}
          </div>
        </section>
      )}

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
