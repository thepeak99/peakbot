// T32 (D11 chat, spec §9.1 "Agents chip popover"): read-only roster of the
// bound agent — main + subagents (the same roster the Agents drawer panel
// renders, at a glance). The MAIN row shows the ACTIVE model alias (after a
// /model switch it can differ from the agent's configured model); a
// subagent row shows its own model, or `inherits <agent model>` when it just
// rides the agent's model. No sending from here — the roster is display-only.
//
// Esc closes the popover and preventDefaults (Esc order modal → popover →
// Settings, matching SelectionChip). Mousedown outside closes.

import { useEffect, useRef, useState } from "react";
import type { AgentsChipProps } from "../settings/contract";

export function AgentsChip({
  profile,
  agent,
  activeModel,
  dropUp = false,
}: AgentsChipProps) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.preventDefault();
        setOpen(false);
      }
    }
    function onDown(e: MouseEvent) {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    }
    window.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onDown);
    return () => {
      window.removeEventListener("keydown", onKey);
      document.removeEventListener("mousedown", onDown);
    };
  }, [open]);

  // Nothing bound (old server / before first frame) → no chip.
  if (agent === null) return null;

  const count = 1 + agent.subagents.length;

  return (
    <div className="relative" ref={rootRef}>
      <button
        data-testid="agents-chip"
        type="button"
        onClick={() => setOpen((o) => !o)}
        title="Agent roster"
        className="flex items-center gap-1.5 rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300 transition-colors hover:bg-zinc-800"
      >
        Agents · {count}
        <span className="text-zinc-600">▾</span>
      </button>

      {open && (
        <div
          data-testid="agents-popover"
          className={`absolute left-0 z-20 w-72 overflow-hidden rounded-md border border-zinc-800 bg-zinc-900 py-1 shadow-xl ${
            dropUp ? "bottom-full mb-1" : "top-full mt-1"
          }`}
        >
          <div
            data-testid="agents-roster-title"
            className="border-b border-zinc-800 px-3 py-1.5 text-[10px] uppercase tracking-wide text-zinc-500"
          >
            Roster · {profile ?? "?"} › {agent.name}
          </div>
          <div
            data-testid="agents-roster-main"
            className="flex items-baseline gap-2 px-3 py-1.5 text-xs text-zinc-200"
          >
            <span className="font-medium">{agent.name}</span>
            <span className="rounded bg-zinc-800 px-1 text-[10px] text-zinc-400">main</span>
            <span className="text-zinc-500">{activeModel || agent.model}</span>
          </div>
          {agent.subagents.map((s) => (
            <div
              key={s.name}
              data-testid={`agents-roster-sub-${s.name}`}
              className="flex items-baseline gap-2 px-3 py-1 text-xs text-zinc-400"
            >
              <span>
                ↳ {s.name}
              </span>
              <span className="text-zinc-600">{s.inherited ? `inherits ${agent.model}` : s.model}</span>
            </div>
          ))}
          {agent.subagents.length === 0 && (
            <div data-testid="agents-roster-empty" className="px-3 py-1.5 text-xs text-zinc-600">
              Single agent — no delegate tool.
            </div>
          )}
        </div>
      )}
    </div>
  );
}
