import type { BashPanel as BashPanelData } from "../mock";

// Foreground `bash` output panel. Mirrors BashPanelState (src/ui/app_state.rs)
// — the bottom strip that surfaces the running/last `bash` invocation with a
// 5-line tail. Green header while running, exit-code glyph when finished.
export function BashPanel({ panel }: { panel: BashPanelData }) {
  const running = panel.status === "running";
  return (
    <div className="border-t border-zinc-800 bg-black/40">
      <div className="flex items-center gap-2 border-b border-zinc-800/70 px-4 py-1.5 text-xs">
        <span className={running ? "text-emerald-400" : "text-zinc-400"}>
          {running ? "▶" : panel.exitCode === 0 ? "✓" : "✗"}
        </span>
        <span className="font-mono text-zinc-300">bash</span>
        <span className="truncate font-mono text-zinc-500" title={panel.command}>
          {panel.command}
        </span>
        <span className="ml-auto shrink-0 font-mono tabular-nums text-zinc-600">
          {running ? `pid ${panel.pid} · ${panel.elapsed}` : `exit ${panel.exitCode}`}
        </span>
      </div>
      <pre className="max-h-28 overflow-hidden px-4 py-2 font-mono text-[11px] leading-relaxed text-zinc-400">
        {panel.tail.join("\n")}
      </pre>
    </div>
  );
}
