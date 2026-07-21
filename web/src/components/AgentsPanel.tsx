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
  pipelineAvailable,
  subagentsEnabled,
  locked,
  onToggle,
  active,
  onSelect,
  roster,
}: {
  /** Whether a multi-agent pipeline is *configured* for this session
   * (`pipeline.enabled` + roles, boot-only). Decides whether the opt-in is
   * offered at all. */
  pipelineAvailable: boolean;
  /** Whether the user opted THIS conversation into sub-agents. Default off. */
  subagentsEnabled: boolean;
  /** True once the conversation has a real turn — the opt-in is then frozen
   * (the agent's tool list is fixed for the life of the conversation). */
  locked: boolean;
  /** Toggle the opt-in. No-op while `locked` (the checkbox is disabled). */
  onToggle: (enabled: boolean) => void;
  active: ViewFilter;
  onSelect: (filter: ViewFilter) => void;
  /** One entry per delegation call, in call order, with its composite key and
   * turn count (see deriveSubAgentRoster). */
  roster: { key: string; role: string; n: number; count: number }[];
}) {
  // Sub-agent views are shown only when the conversation actually opted in.
  const active_ = pipelineAvailable && subagentsEnabled;
  // The checkbox is interactive only when a pipeline is configured AND the
  // conversation hasn't started yet.
  const canToggle = pipelineAvailable && !locked;

  // Suffix a call with "#n" only when its role ran more than once — a
  // single-call role reads as its bare name (no noisy "#1").
  const callsPerRole = new Map<string, number>();
  for (const r of roster)
    callsPerRole.set(r.role, (callsPerRole.get(r.role) ?? 0) + 1);

  const roleEntries: Entry[] = roster.map((r) => ({
    id: r.key,
    label:
      (callsPerRole.get(r.role) ?? 0) > 1 ? `${r.role} #${r.n}` : r.role,
    glyph: "🧩",
    hint: "Sub-agent delegation",
    count: r.count,
  }));

  const rows = active_ ? [...VIEW_ENTRIES, ...roleEntries] : [];

  return (
    <section>
      <div className="mb-3 flex items-baseline justify-between">
        <h3 className="text-[11px] font-semibold uppercase tracking-wide text-zinc-500">
          Agents
        </h3>
      </div>

      <p
        className={`mb-3 rounded border px-2 py-1 text-[11px] ${
          pipelineAvailable
            ? "border-emerald-900/60 bg-emerald-950/30 text-emerald-300"
            : "border-zinc-800 bg-zinc-900/50 text-zinc-500"
        }`}
      >
        {pipelineAvailable ? (
          <>Pipeline configured — you can opt this conversation into sub-agents.</>
        ) : (
          <>
            Pipeline not configured. Add a{" "}
            <code className="text-zinc-400">pipeline:</code> block to config.yaml
            and restart to enable sub-agents.
          </>
        )}
      </p>

      <label
        className={`mb-1 flex items-center gap-2 text-xs ${
          canToggle
            ? "cursor-pointer text-zinc-300"
            : "text-zinc-500"
        }`}
        title={
          !pipelineAvailable
            ? "No pipeline configured (pipeline.enabled in config.yaml)."
            : locked
              ? "Locked — the conversation has already started."
              : "Enable sub-agents for this conversation (before the first message)."
        }
      >
        <input
          type="checkbox"
          checked={subagentsEnabled}
          disabled={!canToggle}
          onChange={(e) => onToggle(e.target.checked)}
          className={`h-3.5 w-3.5 accent-sky-500 ${
            canToggle ? "cursor-pointer" : "cursor-not-allowed"
          }`}
        />
        Enable subagents
      </label>

      {pipelineAvailable && locked && (
        <p className="mb-3 text-[11px] text-zinc-600">
          Locked for this conversation — start a new one to change it.
        </p>
      )}

      {!active_ ? (
        <p className="mt-2 text-xs text-zinc-600">
          {pipelineAvailable
            ? "Sub-agents are off for this conversation."
            : "No sub-agents in this session."}
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
