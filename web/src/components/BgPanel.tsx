import type { BgProcess } from "../mock";

// Background-process list. Mirrors BgState / BgSummary (src/ui/app_state.rs)
// — the `🛰 N bg` counter's expanded view.
export function BgPanel({ processes }: { processes: BgProcess[] }) {
  const running = processes.filter((p) => p.status === "running").length;
  return (
    <section>
      <div className="mb-2 flex items-baseline justify-between">
        <h3 className="text-[11px] font-semibold uppercase tracking-wide text-zinc-500">
          Background
        </h3>
        <span className="font-mono text-[11px] tabular-nums text-cyan-400">
          🛰 {running}
        </span>
      </div>
      <ul className="space-y-1.5 text-xs">
        {processes.map((p) => (
          <li key={p.id} className="flex items-start gap-2">
            <span
              className={
                p.status === "running"
                  ? "mt-1 h-1.5 w-1.5 shrink-0 animate-pulse rounded-full bg-cyan-400"
                  : "mt-1 h-1.5 w-1.5 shrink-0 rounded-full bg-zinc-600"
              }
            />
            <div className="min-w-0">
              <div className="truncate font-mono text-zinc-300" title={p.command}>
                <span className="text-zinc-600">#{p.id}</span> {p.command}
              </div>
              <div className="text-[10px] text-zinc-600">
                {p.label && <span className="text-zinc-500">{p.label} · </span>}
                {p.status === "exited"
                  ? `exited (code ${p.exitCode ?? 0})`
                  : "running"}
              </div>
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}
