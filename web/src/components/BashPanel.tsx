import { useEffect, useRef } from "react";
import type { BashPanel as BashPanelData } from "../types";

// Foreground `bash` output panel. Mirrors BashPanelState (src/ui/app_state.rs)
// — the bottom strip that surfaces the running/last `bash` invocation. Green
// header while running, exit-code glyph when finished. The output is a
// scrollable buffer: it auto-follows the tail on new lines, but pausing the
// follow the moment the user scrolls up so they can read history undisturbed
// (auto-follow resumes when they scroll back to the bottom).
export function BashPanel({ panel }: { panel: BashPanelData }) {
  const running = panel.status === "running";
  const preRef = useRef<HTMLPreElement>(null);
  // Follow the tail only while the user is already pinned to the bottom.
  const followRef = useRef(true);
  const body = panel.tail.join("\n");

  const onScroll = () => {
    const el = preRef.current;
    if (!el) return;
    // 24px slack so a near-bottom position still counts as "following".
    followRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
  };

  useEffect(() => {
    const el = preRef.current;
    if (el && followRef.current) el.scrollTop = el.scrollHeight;
  }, [body]);

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
      <pre
        ref={preRef}
        onScroll={onScroll}
        className="max-h-48 overflow-y-auto px-4 py-2 font-mono text-[11px] leading-relaxed text-zinc-400"
      >
        {body}
      </pre>
    </div>
  );
}
