import { useState } from "react";
import type { CatalogAgent, ViewFilter } from "../types";

// The "Agents" side panel, two clearly-separated sections:
//   1. Roster — the *configured* cast of the bound agent (main agent +
//      sub-agents with their model aliases), straight from the catalog
//      (App resolves the bound (profile, agent) via `selection` and passes
//      the agent here).
//   2. Watch list — what has actually RUN, derived live from the transcript.
//      Selecting an entry sets the single `viewFilter` in App, which re-scopes
//      the chat (and todo/stats). "Global" and "Main agent" are
//      pseudo-entries (views, not agents); roles are grouped, one row each,
//      with their individual delegations expandable beneath.
// The two sections are never merged: the roster says who is on the team, the
// watch list says who has spoken, and their counts legitimately differ.
// (The (profile, agent) selection UI arrives in a later task — for now the
// panel is display-only about config; the wire's `select` frame is sent by
// whatever component owns the selection.)

// A pseudo-view row (Global / Main agent). Real agents are rendered from the
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
    label: "Main agent",
    glyph: "🎬",
    hint: "Top-level turns only",
  },
];

const SECTION_HEADING =
  "mb-1.5 text-[10px] font-semibold uppercase tracking-wide text-zinc-500";

export function AgentsPanel({
  boundAgent,
  active,
  onSelect,
  roster,
  laneCalls,
  laneMessages,
}: {
  /** The agent this conversation is bound to (resolved from the catalog by
   * `selection`), or null when nothing is bound / the wire predates the
   * catalog. Drives the roster section. */
  boundAgent: CatalogAgent | null;
  active: ViewFilter;
  onSelect: (filter: ViewFilter) => void;
  /** One entry per delegation call, in call order, with its composite key and
   * turn count (see deriveSubAgentRoster). */
  roster: { key: string; role: string; n: number; count: number }[];
  /** Lane → API-call count, from the same source as the Session tab. Shown
   * beside the message count so the two numbers can't be read as one. */
  laneCalls: Record<string, number>;
  /** Lane → transcript message count (messagesByLane), shared with the
   * Session cards so both panels report the same figure. */
  laneMessages: Record<string, number>;
}) {
  // Group the per-call roster by role: one row per agent, its delegations
  // nested underneath. The role total comes from `laneMessages` (shared with
  // the Session cards); per-call counts stay roster-derived.
  const byRole = new Map<
    string,
    { role: string; total: number; calls: { key: string; n: number; count: number }[] }
  >();
  for (const r of roster) {
    const g = byRole.get(r.role) ?? {
      role: r.role,
      total: laneMessages[r.role] ?? 0,
      calls: [],
    };
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

      {/* ── 1. Roster: the configured team of the bound agent ───────── */}
      {boundAgent && (
        <div className="mb-3">
          <h4 className={SECTION_HEADING}>Roster</h4>
          <ul className="space-y-0.5 text-[11px] text-zinc-400">
            <li className="flex items-center gap-2">
              <span>🎬</span>
              <span className="flex-1">Main agent</span>
              <span className="font-mono text-zinc-500">{boundAgent.model}</span>
            </li>
            {boundAgent.subagents.map((sa) => (
              <li key={sa.name} className="flex items-center gap-2">
                <span>🧩</span>
                <span className="flex-1 truncate font-mono">{sa.name}</span>
                <span
                  className="font-mono text-zinc-500"
                  title={sa.inherited ? "Inherited from the main agent" : undefined}
                >
                  {sa.model}
                </span>
              </li>
            ))}
          </ul>
          <p className="mt-1 text-[10px] text-zinc-600">
            Configured agents — who <em>can</em> run. What has run is below.
          </p>
        </div>
      )}

      {/* ── 2. Watch list: derived from the transcript ─────────────── */}
      <h4 className={SECTION_HEADING}>Watching</h4>

      <p className="mb-3 text-[11px] text-zinc-500">
        Picking a role is a view. Your message goes to the main agent, which
        decides what to delegate.
      </p>

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
                    title={`${g.total} transcript messages${
                      laneCalls[g.role] === undefined
                        ? ""
                        : ` from ${laneCalls[g.role]} API calls`
                    } — a call usually produces several messages`}
                  >
                    {g.total} msg
                    {laneCalls[g.role] === undefined
                      ? ""
                      : ` · ${laneCalls[g.role]} calls`}
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
    </section>
  );
}
