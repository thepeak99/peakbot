import { useState } from "react";
import type { ViewFilter } from "../types";
import type { WirePipelineInfo } from "../state";

// The "Agents" side panel, three clearly-separated sections:
//   1. Pipeline selector — which configured team this conversation runs on
//      (radio: None + one row per pipeline). One selection drives everything
//      downstream; it locks once the conversation has a turn.
//   2. Roster — the *configured* cast of the selected team (orchestrator +
//      members with their model aliases), straight from config.
//   3. Watch list — what has actually RUN, derived live from the transcript.
//      Selecting an entry sets the single `viewFilter` in App, which re-scopes
//      the chat (and todo/stats). "Global" and "Orchestrator" are
//      pseudo-entries (views, not agents); roles are grouped, one row each,
//      with their individual delegations expandable beneath.
// Sections 2 and 3 are never merged: the roster says who is on the team, the
// watch list says who has spoken, and their counts legitimately differ.

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

const SECTION_HEADING =
  "mb-1.5 text-[10px] font-semibold uppercase tracking-wide text-zinc-500";

export function AgentsPanel({
  pipelines,
  selected,
  locked,
  onSelectPipeline,
  active,
  onSelect,
  roster,
  laneCalls,
  laneMessages,
}: {
  /** The pipelines configured at boot, in declaration order. Empty means no
   * `pipelines:` block — there is nothing to select. */
  pipelines: WirePipelineInfo[];
  /** The pipeline this conversation is bound to, or null for single agent. */
  selected: string | null;
  /** True once the conversation has a real turn — the selection is then frozen
   * (the agent's tool list is fixed for the life of the conversation). */
  locked: boolean;
  /** Bind the conversation to a pipeline; `null` clears it. No-op while
   * `locked` (the radios are disabled). */
  onSelectPipeline: (name: string | null) => void;
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
  const configured = pipelines.length > 0;
  const canSelect = configured && !locked;
  // The selected name resolved against the catalogue. Null while nothing is
  // selected — or, briefly, if a saved name no longer exists in config (the
  // backend clears such selections on resume and says so in the transcript).
  const selectedInfo = pipelines.find((p) => p.name === selected) ?? null;
  // Sub-agent views only mean something once a pipeline is bound.
  const active_ = selected !== null;
  // One producer for the "why can't I click this" text, used as the honest
  // title on every row.
  const disabledReason = !configured
    ? "No pipelines configured — add a `pipelines:` block to config.yaml and restart."
    : locked
      ? "Locked — this conversation has already started. Start a new one to pick a different pipeline."
      : null;
  const rowClass = (isSelected: boolean) =>
    `flex items-center gap-2 rounded-md border px-2.5 py-1.5 text-xs transition-colors ${
      isSelected
        ? "border-sky-700 bg-sky-950/40 text-sky-200"
        : "border-transparent text-zinc-300"
    } ${canSelect ? "cursor-pointer hover:border-zinc-800 hover:bg-zinc-900/70" : "cursor-not-allowed text-zinc-500"}`;

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

      {/* ── 1. Pipeline selector ─────────────────────────────────── */}
      <h4 className={SECTION_HEADING}>Pipeline</h4>
      <ul className="mb-2 space-y-1">
        <li>
          <label
            className={rowClass(selected === null)}
            title={
              disabledReason ??
              "Run this conversation as a single agent — no delegation."
            }
          >
            <input
              type="radio"
              name="pipeline"
              checked={selected === null}
              disabled={!canSelect}
              onChange={() => onSelectPipeline(null)}
              className="h-3.5 w-3.5 accent-sky-500"
            />
            <span className="flex-1">None (single agent)</span>
          </label>
        </li>
        {pipelines.map((p) => (
          <li key={p.name}>
            <label
              className={rowClass(selected === p.name)}
              title={
                disabledReason ??
                `Run this conversation on the '${p.name}' team.`
              }
            >
              <input
                type="radio"
                name="pipeline"
                checked={selected === p.name}
                disabled={!canSelect}
                onChange={() => onSelectPipeline(p.name)}
                className="h-3.5 w-3.5 accent-sky-500"
              />
              <span className="flex-1 truncate font-medium">{p.name}</span>
              <span className="shrink-0 text-[10px] text-zinc-500">
                🎬 {p.orchestrator_model} · {p.members.length} sub-agent
                {p.members.length === 1 ? "" : "s"}
              </span>
            </label>
          </li>
        ))}
      </ul>

      {!configured ? (
        <p className="mb-3 rounded border border-zinc-800 bg-zinc-900/50 px-2 py-1 text-[11px] text-zinc-500">
          No pipelines configured. Add a{" "}
          <code className="text-zinc-400">pipelines:</code> block to config.yaml
          and restart to enable delegation.
        </p>
      ) : (
        locked && (
          <p className="mb-3 text-[11px] text-zinc-600">
            Locked for this conversation — start a new one to change pipeline.
          </p>
        )
      )}

      {/* ── 2. Roster: the configured team ───────────────────────── */}
      {selectedInfo && (
        <div className="mb-3">
          <h4 className={SECTION_HEADING}>Roster — {selectedInfo.name}</h4>
          <ul className="space-y-0.5 text-[11px] text-zinc-400">
            <li className="flex items-center gap-2">
              <span>🎬</span>
              <span className="flex-1">orchestrator</span>
              <span className="font-mono text-zinc-500">
                {selectedInfo.orchestrator_model}
              </span>
            </li>
            {selectedInfo.members.map(([role, alias]) => (
              <li key={role} className="flex items-center gap-2">
                <span>🧩</span>
                <span className="flex-1 truncate font-mono">{role}</span>
                <span className="font-mono text-zinc-500">{alias}</span>
              </li>
            ))}
          </ul>
          <p className="mt-1 text-[10px] text-zinc-600">
            Configured team — who <em>can</em> run. What has run is below.
          </p>
        </div>
      )}

      {/* ── 3. Watch list: derived from the transcript ───────────── */}
      <h4 className={SECTION_HEADING}>Watching</h4>

      {active_ && (
        <p className="mb-3 text-[11px] text-zinc-500">
          Picking a role is a view. Your message goes to the orchestrator, which
          decides what to delegate.
        </p>
      )}

      {!active_ ? (
        <p className="mt-2 text-xs text-zinc-600">
          {configured
            ? "No pipeline selected — this conversation runs a single agent."
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
        </>
      )}
    </section>
  );
}
