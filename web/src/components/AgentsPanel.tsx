import type { ViewFilter } from "../types";

// The "Agents" side panel: an enable toggle plus a selectable list of views.
// Selecting an entry sets the single `viewFilter` selector in App, which
// re-scopes the chat (and, later, todo/stats). "Global" and "Orchestrator" are
// pseudo-entries (views, not agents) shown above the real roles and styled
// distinctly. The role roster is derived live from the transcript (App passes
// it in) — each role's turn count rides along as a badge.

interface Entry {
  id: ViewFilter;
  label: string;
  glyph: string;
  hint: string;
  /** turn count shown as a badge (sub-agent roles only). */
  count?: number;
  /** A view (Global/Orchestrator) rather than a concrete sub-agent role. */
  pseudo?: boolean;
}

const VIEW_ENTRIES: Entry[] = [
  {
    id: "global",
    label: "Global",
    glyph: "🌐",
    hint: "All agents, one transcript with badges",
    pseudo: true,
  },
  {
    id: "orchestrator",
    label: "Orchestrator",
    glyph: "🎬",
    hint: "Top-level turns only",
    pseudo: true,
  },
];

export function AgentsPanel({
  pipelineEnabled,
  active,
  onSelect,
  roster,
}: {
  /** Whether the backend pipeline is enabled for this session
   * (`pipeline.enabled`, boot-only). This is the single source of truth: a
   * conversation either delegates or it doesn't, and it can't be toggled from
   * the UI, so the "Enable subagents" checkbox reflects this state and is
   * grayed out — never a lie about a capability it can't switch. */
  pipelineEnabled: boolean;
  active: ViewFilter;
  onSelect: (filter: ViewFilter) => void;
  /** Sub-agent roles derived live from the transcript, with turn counts. */
  roster: { role: string; count: number }[];
}) {
  const roleEntries: Entry[] = roster.map((r) => ({
    id: r.role,
    label: r.role,
    glyph: "🧩",
    hint: "Sub-agent",
    count: r.count,
  }));

  const rows = pipelineEnabled ? [...VIEW_ENTRIES, ...roleEntries] : [];

  return (
    <section>
      <div className="mb-3 flex items-baseline justify-between">
        <h3 className="text-[11px] font-semibold uppercase tracking-wide text-zinc-500">
          Agents
        </h3>
      </div>

      <p
        className={`mb-3 rounded border px-2 py-1 text-[11px] ${
          pipelineEnabled
            ? "border-emerald-900/60 bg-emerald-950/30 text-emerald-300"
            : "border-zinc-800 bg-zinc-900/50 text-zinc-500"
        }`}
      >
        {pipelineEnabled ? (
          <>Pipeline active — sub-agents available this session.</>
        ) : (
          <>
            Pipeline not configured. Add a{" "}
            <code className="text-zinc-400">pipeline:</code> block to config.yaml
            and restart to enable sub-agents.
          </>
        )}
      </p>

      <label
        className="mb-3 flex items-center gap-2 text-xs text-zinc-500"
        title="Set by config.yaml (pipeline.enabled, boot-only) — can't be toggled from the UI."
      >
        <input
          type="checkbox"
          checked={pipelineEnabled}
          disabled
          readOnly
          className="h-3.5 w-3.5 cursor-not-allowed accent-sky-500"
        />
        Enable subagents
      </label>

      {!pipelineEnabled ? (
        <p className="text-xs text-zinc-600">
          No sub-agents in this session.
        </p>
      ) : (
        <>
          <ul className="space-y-1">
            {rows.map((e) => {
              const isActive = e.id === active;
              return (
                <li key={e.id}>
                  <button
                    onClick={() => onSelect(e.id)}
                    aria-pressed={isActive}
                    title={e.hint}
                    className={`flex w-full cursor-pointer items-center gap-2 rounded-md border px-2.5 py-1.5 text-left text-xs transition-colors ${
                      isActive
                        ? "border-sky-700 bg-sky-950/40 text-sky-200"
                        : "border-transparent text-zinc-300 hover:border-zinc-800 hover:bg-zinc-900/70"
                    }`}
                  >
                    <span className="text-sm leading-none">{e.glyph}</span>
                    <span
                      className={`flex-1 truncate ${
                        e.pseudo ? "font-medium" : "font-mono"
                      }`}
                    >
                      {e.label}
                    </span>
                    {e.count !== undefined && (
                      <span className="rounded bg-zinc-800/80 px-1.5 py-0.5 text-[10px] tabular-nums text-zinc-400">
                        {e.count}
                      </span>
                    )}
                    {isActive && (
                      <span className="text-[10px] text-sky-400">watching</span>
                    )}
                  </button>
                </li>
              );
            })}
          </ul>
          {roleEntries.length === 0 && (
            <p className="mt-2 rounded-md border border-dashed border-zinc-800 px-2.5 py-3 text-center text-[11px] text-zinc-600">
              No subagents yet. Roles appear here once they take a turn.
            </p>
          )}
        </>
      )}
    </section>
  );
}
