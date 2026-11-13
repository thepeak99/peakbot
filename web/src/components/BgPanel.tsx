import type { BgProcess } from "../types";
import { PANEL_HEAD, PANEL_SUB, PANEL_TITLE, ROW } from "./phoneUi";

// Background-process list. Mirrors BgState / BgSummary (src/ui/app_state.rs)
// — the `🛰 N bg` counter's expanded view.
export function BgPanel({ processes, phone = false }: { processes: BgProcess[]; phone?: boolean }) {
  const running = processes.filter((p) => p.status === "running").length;
  if (phone) {
    // No Stop button: the wire has no per-task stop frame (parked).
    return (
      <section>
        <div className={PANEL_HEAD}>
          <h3 className={PANEL_TITLE}>Tasks</h3>
          <span className={PANEL_SUB}>{running} running</span>
        </div>
        <ul>
          {processes.map((p) => (
            <li key={p.id} className={ROW}>
              <span
                className={`h-[9px] w-[9px] shrink-0 rounded-full ${
                  p.status === "running"
                    ? "animate-pulse bg-amber-500 shadow-[0_0_0_4px_rgba(245,158,11,0.18)]"
                    : "bg-zinc-700"
                }`}
              />
              <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                <b className="truncate text-[14.5px] font-semibold text-zinc-100">{p.command}</b>
                <small className="text-[11.5px] text-zinc-500">
                  {p.label ? `${p.label} · ` : ""}
                  {p.status === "running" ? "running" : `exited (code ${p.exitCode ?? 0})`}
                </small>
              </div>
            </li>
          ))}
        </ul>
      </section>
    );
  }
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
