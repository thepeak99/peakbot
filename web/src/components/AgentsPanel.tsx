import { useState } from "react";
import type { ViewFilter } from "../types";

// The "Agents" side panel: an enable toggle plus a selectable list of views.
// Selecting an entry sets the single `viewFilter` selector in App, which
// re-scopes the chat (and todo/stats). "Global" and "Orchestrator" are
// pseudo-entries (views, not agents) shown above the real roles. Roles are
// derived live from the transcript (App passes the per-call roster in) and
// grouped: one row per role, its individual delegations expandable beneath.

// A pseudo-view row (Global / Orchestrator). Real agents are rendered from the
// grouped roster instead, which carries its own counts.
interface Entry {
  id: ViewFilter;
  label: string;
  glyph: string;
  hint: string;
}

const VIEW_ENTRIES: Entry[] = [
  {
    id: "global",
    label: "Global",
    glyph: "🌐",
    hint: "All agents, one transcript with badges",
  },
  {
    id: "orchestrator",
    label: "Orchestrator",
    glyph: "🎬",
    hint: "Top-level turns only",
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

  // Group the per-call roster by role: one row per agent, its delegations
  // nested underneath. A role's msg count is the sum of its calls'.
  const byRole = new Map<
    string,
    { role: string; total: number; calls: { key: string; n: number; count: number }[] }
  >();
  for (const r of roster) {
    const g = byRole.get(r.role) ?? { role: r.role, total: 0, calls: [] };
    g.total += r.count;
    g.calls.push({ key: r.key, n: r.n, count: r.count });
    byRole.set(r.role, g);
  }
  const groups = [...byRole.values()];

  // Expanded roles — purely user-controlled. No auto-expansion: a role the
  // user never opened stays closed, so the list can't shuffle under them.
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const toggle = (role: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (!next.delete(role)) next.add(role);
      return next;
    });

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

      {active_ && (
        <p className="mb-3 text-[11px] text-zinc-500">
          Picking a role is a view. Your message goes to the orchestrator, which
          decides what to delegate.
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
            {VIEW_ENTRIES.map((e) => {
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
                    <span className="flex-1 truncate font-medium">{e.label}</span>
                    {isActive && (
                      <span className="text-[10px] text-sky-400">watching</span>
                    )}
                  </button>
                </li>
              );
            })}

            {groups.map((g) => {
              const roleActive = g.role === active;
              const open = expanded.has(g.role);
              // One delegation ⇒ the role row IS that delegation; no caret.
              const splittable = g.calls.length > 1;
              return (
                <li key={g.role}>
                  <div
                    className={`flex items-center gap-1 rounded-md border transition-colors ${
                      roleActive
                        ? "border-sky-700 bg-sky-950/40 text-sky-200"
                        : "border-transparent text-zinc-300 hover:border-zinc-800 hover:bg-zinc-900/70"
                    }`}
                  >
                    {splittable ? (
                      <button
                        onClick={() => toggle(g.role)}
                        aria-expanded={open}
                        title={`${open ? "Hide" : "Show"} ${g.role}'s ${g.calls.length} delegations`}
                        className="cursor-pointer px-1 py-1.5 text-[10px] text-zinc-500 hover:text-zinc-300"
                      >
                        {open ? "▾" : "▸"}
                      </button>
                    ) : (
                      <span className="px-1 py-1.5 text-[10px] text-transparent">▸</span>
                    )}
                    <button
                      onClick={() => onSelect(g.role)}
                      aria-pressed={roleActive}
                      title={`Watch every turn ${g.role} took (${g.calls.length} delegation${g.calls.length === 1 ? "" : "s"})`}
                      className="flex flex-1 cursor-pointer items-center gap-2 py-1.5 pr-2.5 text-left text-xs"
                    >
                      <span className="text-sm leading-none">🧩</span>
                      <span className="flex-1 truncate font-mono">{g.role}</span>
                      {splittable && (
                        <span
                          className="rounded bg-zinc-800/80 px-1.5 py-0.5 text-[10px] tabular-nums text-zinc-400"
                          title={`${g.calls.length} separate delegations`}
                        >
                          {g.calls.length}×
                        </span>
                      )}
                      <span
                        className="rounded bg-zinc-800/80 px-1.5 py-0.5 text-[10px] tabular-nums text-zinc-400"
                        title={`${g.total} transcript messages — not API calls (see the Session tab for those)`}
                      >
                        {g.total} msg
                      </span>
                      {roleActive && (
                        <span className="text-[10px] text-sky-400">watching</span>
                      )}
                    </button>
                  </div>

                  {splittable && open && (
                    <ul className="mt-1 space-y-0.5 border-l border-zinc-800 pl-2 ml-3">
                      {g.calls.map((c) => {
                        const callActive = c.key === active;
                        return (
                          <li key={c.key}>
                            <button
                              onClick={() => onSelect(c.key)}
                              aria-pressed={callActive}
                              title={`Watch only delegation ${c.n} of ${g.role}`}
                              className={`flex w-full cursor-pointer items-center gap-2 rounded border px-2 py-1 text-left text-[11px] transition-colors ${
                                callActive
                                  ? "border-sky-700 bg-sky-950/40 text-sky-200"
                                  : "border-transparent text-zinc-400 hover:border-zinc-800 hover:bg-zinc-900/70"
                              }`}
                            >
                              <span className="flex-1 truncate font-mono">
                                call {c.n}
                              </span>
                              <span className="tabular-nums text-zinc-500">
                                {c.count} msg
                              </span>
                              {callActive && (
                                <span className="text-[10px] text-sky-400">
                                  watching
                                </span>
                              )}
                            </button>
                          </li>
                        );
                      })}
                    </ul>
                  )}
                </li>
              );
            })}
          </ul>
          {groups.length === 0 && (
            <p className="mt-2 rounded-md border border-dashed border-zinc-800 px-2.5 py-3 text-center text-[11px] text-zinc-600">
              No subagents yet. Roles appear here once they take a turn.
            </p>
          )}
        </>
      )}
    </section>
  );
}
