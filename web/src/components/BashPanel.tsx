import { useEffect, useRef } from "react";
import type { BashPanel as BashPanelData } from "../types";
import { edition } from "../desktop";
import { PANEL_HEAD, PANEL_SUB, PANEL_TITLE } from "./phoneUi";

// Foreground `bash` output for the drawer "Bash" tab. Mirrors BashPanelState
// (src/ui/app_state.rs) — the running/last `bash` invocation. Green header
// while running, exit-code glyph when finished. The output is a scrollable
// buffer: it auto-follows the tail on new lines, pausing the follow the moment
// the user scrolls up so they can read history undisturbed (auto-follow
// resumes when they scroll back to the bottom). Only the latest `bash` command
// is tracked by the backend — background (`bash_bg`) processes and their
// history live in the "Tasks" tab.
export function BashPanel({ panel, phone = false }: { panel: BashPanelData; phone?: boolean }) {
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

  // The local edition (phone build) has no shell to run bash in — the
  // panel renders nothing (after the hooks: the hook order must stay stable).
  if (edition() === "local") return null;

  if (phone) {
    // Mock .term: header moves out of the box; no semantic line colours (no data).
    return (
      <section>
        <div className={PANEL_HEAD}>
          <h3 className={PANEL_TITLE}>Bash</h3>
          <span className={`${PANEL_SUB} min-w-0 truncate pl-3 font-mono`}>{panel.command}</span>
        </div>
        <pre
          ref={preRef}
          onScroll={onScroll}
          className="max-h-[70vh] overflow-y-auto rounded-[14px] border border-zinc-800 bg-[#050506] p-3 font-mono text-[11.5px] leading-[1.6] text-zinc-300 shadow-[inset_0_1px_0_rgba(255,255,255,0.03)]"
        >
          {body}
        </pre>
      </section>
    );
  }

  return (
    <div className="overflow-hidden rounded-lg border border-zinc-800 bg-black/40">
      <div className="flex items-center gap-2 border-b border-zinc-800/70 px-3 py-1.5 text-xs">
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
        className="max-h-[70vh] overflow-y-auto px-3 py-2 font-mono text-[11px] leading-relaxed text-zinc-400"
      >
        {body}
      </pre>
    </div>
  );
}
