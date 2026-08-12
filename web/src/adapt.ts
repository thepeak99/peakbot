// Adapter — the single boundary that maps the wire `AppState` (state.ts)
// into the camelCase view model (types.ts) the components render. All
// field-name and enum translation lives here; components stay clean.

import type {
  AppState,
  WireBashPanel,
  WireChatMessage,
  WireRole,
  WireTodoStatus,
} from "./state";
import type {
  BashPanel,
  BgProcess,
  ChatMessage,
  ContextUsage,
  FileEdit,
  LaneStat,
  MessageRole,
  SessionStats,
  TodoItem,
  TodoNode,
  TodoStatus,
  ViewFilter,
  Welcome,
} from "./types";

const ROLE_MAP: Record<WireRole, MessageRole> = {
  user: "user",
  agent: "agent",
  system: "system",
  toolcall: "toolCall",
  toolresult: "toolResult",
  summary: "summary",
};

const TODO_STATUS_MAP: Record<WireTodoStatus, TodoStatus> = {
  pending: "pending",
  in_progress: "inProgress",
  completed: "completed",
  cancelled: "cancelled",
};

/** ISO 8601 → local "HH:MM". Falls back to the raw string on parse failure. */
function toClock(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export function adaptMessage(m: WireChatMessage): ChatMessage {
  // ToolCall/ToolResult carry structured fields; the `content` already holds
  // the display string the TUI renders, so we pass it through verbatim.
  return {
    role: ROLE_MAP[m.role] ?? "system",
    content: m.content ?? "",
    timestamp: toClock(m.timestamp),
    toolName: m.tool_name ?? undefined,
    fromBackground: m.source?.kind === "background",
    subAgentRole:
      m.source?.kind === "sub_agent" ? m.source.role : undefined,
  };
}

export function adaptStats(s: AppState): SessionStats {
  const lanes = (s.stats.lanes ?? []).map((l) => ({
    lane: l.lane,
    inputTokens: l.input_tokens,
    outputTokens: l.output_tokens,
    apiCalls: l.api_calls,
    model: l.model ?? "",
    costUsd: l.cost,
  }));
  // The flat wire totals hold only the LAST request's tokens, so they'd
  // contradict the cumulative per-lane rows sitting right below them. Derive
  // the session figure as the sum of the lanes instead — the total is then the
  // sum of its parts by construction. Live context size is a separate concern
  // and has its own meter (ContextUsage). Pre-lane backends fall back to flat.
  const summed = lanes.reduce(
    (a, l) => ({ input: a.input + l.inputTokens, output: a.output + l.outputTokens }),
    { input: 0, output: 0 },
  );
  return {
    inputTokens: lanes.length ? summed.input : s.stats.total_input_tokens,
    outputTokens: lanes.length ? summed.output : s.stats.total_output_tokens,
    apiCalls: s.stats.total_api_calls,
    costUsd: s.stats.total_cost,
    modelAlias: s.stats.model_alias,
    model: s.stats.model,
    provider: s.stats.provider_name,
    lanes,
  };
}

// Scope the stats panel to the active view. "global" keeps the grand totals;
// "orchestrator"/"<role>" narrows the token/call/cost rows to that lane's
// bucket, leaving model/lanes intact. A view with no lane yet (nothing ran on
// it) reads as zeros — honest, not stale. Model identity is a session fact,
// not a per-lane one, so it always shows the session model.
export function scopeStatsToView(
  stats: SessionStats,
  view: ViewFilter,
): SessionStats {
  if (view === "global") return stats;
  // A per-call view ("role#n") rolls up to its role's lane bucket — token
  // counts aren't per-message on the wire, so a single delegation can't be
  // costed apart from the role's aggregate (documented honest degradation).
  const lane = laneOf(view);
  const bucket: LaneStat | undefined = stats.lanes.find((l) => l.lane === lane);
  return {
    ...stats,
    inputTokens: bucket?.inputTokens ?? 0,
    outputTokens: bucket?.outputTokens ?? 0,
    apiCalls: bucket?.apiCalls ?? 0,
    costUsd: bucket?.costUsd ?? 0,
  };
}

export function adaptContext(s: AppState): ContextUsage {
  return {
    currentUsage: s.context.current_usage,
    windowSize: s.context.window_size,
    compactionThreshold: s.context.compaction_threshold,
  };
}

export function adaptBg(s: AppState): BgProcess[] {
  return s.bg.recent_summaries.map((b) => ({
    id: b.id,
    command: b.command,
    label: b.label ?? undefined,
    status: b.status === "running" ? "running" : "exited",
    exitCode: b.exit_code ?? undefined,
  }));
}

// File tools whose `path` arg we surface in the Files tab. Writes (create /
// str_replace / insert) drive the created/modified kinds; file_read only ever
// yields the "read" kind, and any write outranks it.
const FILE_WRITE_TOOLS = new Set([
  "file_create",
  "file_str_replace",
  "file_insert",
]);
const FILE_TOOLS = new Set([...FILE_WRITE_TOOLS, "file_read"]);

/**
 * Derive the list of files the agent touched from a transcript slice (#126).
 * No new backend state — the path lives in each file tool call's `tool_args`
 * JSON. Order is first-touch; `edits` counts write operations (reads don't
 * count as edits). `kind` follows created > modified > read. Callers pass the
 * already-scoped message slice, so this doubles as the per-lane Files view.
 */
export function filesFromMessages(messages: WireChatMessage[]): FileEdit[] {
  const byPath = new Map<string, FileEdit>();
  for (const m of messages) {
    if (m.role !== "toolcall" || !m.tool_name) continue;
    if (!FILE_TOOLS.has(m.tool_name)) continue;
    let path: unknown;
    try {
      path = JSON.parse(m.tool_args ?? "{}").path;
    } catch {
      continue; // malformed args — skip rather than crash the panel
    }
    if (typeof path !== "string" || path.length === 0) continue;

    const isWrite = FILE_WRITE_TOOLS.has(m.tool_name);
    const isCreate = m.tool_name === "file_create";
    const existing = byPath.get(path);
    if (existing) {
      if (isWrite) existing.edits += 1;
      // Precedence: a create is sticky; else a write upgrades read → modified.
      if (isCreate) existing.kind = "created";
      else if (isWrite && existing.kind === "read") existing.kind = "modified";
    } else {
      byPath.set(path, {
        path,
        edits: isWrite ? 1 : 0,
        kind: isCreate ? "created" : isWrite ? "modified" : "read",
      });
    }
  }
  return [...byPath.values()];
}

// Wire status strings the `todo` tool accepts. An update carrying anything
// else would have errored in the real tool (no state change), so the replay
// below treats an unknown status as a no-op — see todosFromMessages.
const TODO_WIRE_STATUSES = new Set<WireTodoStatus>([
  "pending",
  "in_progress",
  "completed",
  "cancelled",
]);

// A replayed todo item plus its `uid` — a monotonic identity that, unlike the
// tool-facing `id`, is NEVER reused. `clear` resets the id counter when the
// list empties (mirroring TodoList), so ids alone cannot tell an old item from
// a later one that reuses its number. todoTree binds delegation parents by uid
// so id reuse can't silently re-parent an old delegation. `uid` never leaves
// this module.
interface ReplayEntry {
  item: TodoItem;
  uid: number;
}

interface ReplayState {
  entries: ReplayEntry[];
  nextId: number;
  nextUid: number;
}

function newReplay(): ReplayState {
  return { entries: [], nextId: 1, nextUid: 1 };
}

/**
 * Apply one transcript message to a todo replay state — the single reducer
 * shared by todosFromMessages and todoTree (two copies would drift from each
 * other and from Rust). Messages that aren't `todo` tool calls, and calls the
 * real tool would have rejected, are no-ops.
 *
 * This reimplements the Rust `TodoList` transition function (src/tools/todo.rs)
 * — it never consults tool *results*, keeping the derivation a pure function of
 * the call args.
 */
function applyTodoCall(st: ReplayState, m: WireChatMessage): void {
  if (m.role !== "toolcall" || m.tool_name !== "todo") return;
  let args: {
    action?: string;
    tasks?: unknown;
    task_id?: unknown;
    status?: unknown;
  };
  try {
    args = JSON.parse(m.tool_args ?? "{}");
  } catch {
    return; // malformed args — skip rather than crash the panel
  }

  switch (args.action) {
    case "add":
      if (Array.isArray(args.tasks)) {
        for (const t of args.tasks) {
          if (typeof t !== "string") continue;
          const lower = t.toLowerCase();
          if (st.entries.some((e) => e.item.text.toLowerCase() === lower))
            continue; // dedupe — does not consume an id
          st.entries.push({
            item: { id: st.nextId++, text: t, status: "pending" },
            uid: st.nextUid++,
          });
        }
      }
      break;
    case "update": {
      if (
        typeof args.status !== "string" ||
        !TODO_WIRE_STATUSES.has(args.status as WireTodoStatus)
      )
        break; // invalid status → tool would error → no-op
      const e = st.entries.find((x) => x.item.id === args.task_id);
      if (e) e.item.status = TODO_STATUS_MAP[args.status as WireTodoStatus];
      break;
    }
    case "remove": {
      const idx = st.entries.findIndex((x) => x.item.id === args.task_id);
      if (idx !== -1) st.entries.splice(idx, 1);
      break;
    }
    case "clear": {
      for (let i = st.entries.length - 1; i >= 0; i--) {
        const s = st.entries[i].item.status;
        if (s === "completed" || s === "cancelled") st.entries.splice(i, 1);
      }
      if (st.entries.length === 0) st.nextId = 1; // mirror TodoList id reset
      break;
    }
    // "list" and anything else are read-only / unknown → no state change.
  }
}

/**
 * Derive a lane's todo list from its transcript slice — the todo counterpart
 * to filesFromMessages (#208-adjacent). Sub-agent lanes drive no backend todo
 * panel, but their `todo` tool calls already TEE into the transcript tagged by
 * role; replaying those calls reconstructs the list without any new state. The
 * orchestrator and agents-off flows fall out as the single-lane case.
 *
 * Callers pass the already-scoped slice. No `lane` tag — that's the global
 * view's job (todoTree).
 */
export function todosFromMessages(messages: WireChatMessage[]): TodoItem[] {
  const st = newReplay();
  for (const m of messages) applyTodoCall(st, m);
  return st.entries.map((e) => e.item);
}

/** Items → childless nodes, order preserved. The scoped-view shape: no tree to
 * surface when the panel is already narrowed to one lane. */
export function flatTree(items: TodoItem[]): TodoNode[] {
  return items.map((item) => ({ item, children: [] }));
}

function isDelegateCall(m: WireChatMessage): boolean {
  return m.role === "toolcall" && m.tool_name === "delegate";
}

/**
 * Global-view todos as a one-level tree: the orchestrator's list, with each
 * delegation's todos nested under the orchestrator item it was handed off from
 * (`delegate`'s `parent_task_id`). Each lane has its own independent backend
 * TodoList (own id space, own clear-resets), so we replay per lane — never on
 * the mixed transcript, which would corrupt ids/dedupe/clear.
 *
 * The parent link is resolved id→uid *at the delegate call* (see ReplayEntry):
 * a `clear`-then-refill that reuses id 2 therefore cannot re-parent an old
 * delegation onto a new, unrelated task.
 *
 * A delegation with no resolvable parent — no `parent_task_id` (old
 * transcript), a non-integer or unknown id, or a parent that was removed before
 * the end of the replay — surfaces as a top-level group after the orchestrator
 * items, i.e. exactly the flat lane rendering this replaced. Nothing is ever
 * dropped. Scoped views use flatTree(todosFromMessages(...)) instead.
 */
export function todoTree(messages: WireChatMessage[]): TodoNode[] {
  const roster = deriveSubAgentRoster(messages);
  // No sub-agent activity at all ⇒ nothing to disambiguate: plain unlabeled
  // todos, so a single-agent conversation renders exactly as it did pre-agents
  // (no "Orchestrator" pill on every item). A `delegate` call with no sub-agent
  // turns (empty or malformed delegation) still counts as activity — the
  // orchestrator lane is real once it has delegated.
  if (roster.length === 0 && !messages.some(isDelegateCall))
    return flatTree(todosFromMessages(messages));

  // One ordered pass: replay the orchestrator lane's todo calls and bind each
  // delegation's parent as of the moment it was called.
  const st = newReplay();
  const parentUidByKey = new Map<string, number>();
  const countByRole = new Map<string, number>();
  for (const m of messages) {
    if (isDelegateCall(m)) {
      let args: { role?: unknown; parent_task_id?: unknown };
      try {
        args = JSON.parse(m.tool_args ?? "{}");
      } catch {
        continue; // malformed delegate args — skip, same as assignDelegationCalls
      }
      const role = args.role;
      if (typeof role !== "string" || role.length === 0) continue;
      // Same counter rule as assignDelegationCalls, so the keys line up with
      // the roster's.
      const n = (countByRole.get(role) ?? 0) + 1;
      countByRole.set(role, n);
      const parentId = args.parent_task_id;
      if (typeof parentId !== "number" || !Number.isInteger(parentId)) continue;
      const parent = st.entries.find((e) => e.item.id === parentId);
      if (parent) parentUidByKey.set(`${role}#${n}`, parent.uid);
      continue;
    }
    if (m.source?.kind === "sub_agent") continue; // other lanes replay separately
    applyTodoCall(st, m);
  }

  const nodes: TodoNode[] = st.entries.map((e) => ({
    item: { ...e.item, lane: "Orchestrator" },
    children: [],
  }));
  const nodeByUid = new Map<number, TodoNode>(
    st.entries.map((e, i) => [e.uid, nodes[i]]),
  );

  const callsPerRole = new Map<string, number>();
  for (const r of roster)
    callsPerRole.set(r.role, (callsPerRole.get(r.role) ?? 0) + 1);
  const unparented: TodoNode[] = [];
  for (const call of roster) {
    const lane = viewLabel(call.key, (callsPerRole.get(call.role) ?? 0) > 1);
    const items = todosFromMessages(
      filterMessagesByView(messages, call.key),
    ).map((it) => ({ ...it, lane }));
    const uid = parentUidByKey.get(call.key);
    const parent = uid === undefined ? undefined : nodeByUid.get(uid);
    if (parent) parent.children.push(...items);
    else for (const it of items) unparented.push({ item: it, children: [] });
  }
  return [...nodes, ...unparented];
}

/** Whole seconds → "MM:SS". */
function fmtDuration(totalSecs: number): string {
  const m = Math.floor(totalSecs / 60);
  const s = totalSecs % 60;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

/** Returns null when the panel is idle (hidden). */
export function adaptBashPanel(p: WireBashPanel): BashPanel | null {
  switch (p.kind) {
    case "idle":
      return null;
    case "running":
      return {
        command: p.command,
        status: "running",
        pid: p.pid,
        elapsed: "", // renderer computes from started_at if desired
        tail: p.tail,
      };
    case "finished":
      return {
        command: p.command,
        status: "finished",
        elapsed: fmtDuration(p.duration_secs),
        exitCode: p.exit_code,
        tail: p.tail,
      };
  }
}

// A per-delegation view key: "<role>#<n>", 1-based per role in call order. This
// is the composite that lets the UI address one delegation of a role that ran
// several times. `role` and `n` are the parsed pieces.
export interface DelegationCall {
  key: string; // "role#n"
  role: string;
  n: number;
}

// Split a "role#n" view key back into its pieces. A plain role (no "#") reads
// as call 1 — so the pre-per-call callers ("orchestrator", a bare role) still
// resolve. Returns null for the reserved views (global/orchestrator).
export function parseCallKey(view: ViewFilter): DelegationCall | null {
  if (view === "global" || view === "orchestrator") return null;
  const hash = view.lastIndexOf("#");
  if (hash === -1) return { key: `${view}#1`, role: view, n: 1 };
  const role = view.slice(0, hash);
  const n = Number(view.slice(hash + 1));
  if (!role || !Number.isInteger(n) || n < 1) return null;
  return { key: view, role, n };
}

// The lane a view rolls up into for coarse-grained lookups (stats). A per-call
// key "role#n" collapses to its role bucket — per-message tokens aren't on the
// wire, so a single delegation can't be costed apart from its role's aggregate
// (honest degradation, same as an old convo with no lane data).
export function laneOf(view: ViewFilter): ViewFilter {
  const call = parseCallKey(view);
  return call ? call.role : view;
}

/**
 * Assign each sub-agent message to a specific delegation call. Delegations are
 * sequential and non-nested (the `delegate` tool runs one sub-agent to
 * completion before returning), so a *contiguous run* of one role's messages is
 * exactly one delegation — fully derivable from transcript position, with no
 * backend and no reliance on the orchestrator's `delegate` ToolCall surviving in
 * the transcript (long conversations can lose it: its ToolResult lands thousands
 * of messages later, so pair-sanitising drops it).
 *
 * A role's counter bumps whenever its messages resume after any other lane
 * spoke. Caveat: a `[bg output]` turn arriving mid-delegation splits one
 * delegation in two — rare, and it errs toward more granularity, not less.
 */
export function assignDelegationCalls(
  messages: WireChatMessage[],
): Map<number, string> {
  const keyByIndex = new Map<number, string>();
  const countByRole = new Map<string, number>();
  let prevRole: string | null = null;

  for (let i = 0; i < messages.length; i++) {
    const role =
      messages[i].source?.kind === "sub_agent"
        ? (messages[i].source as { role: string }).role
        : null;
    if (!role) {
      prevRole = null; // another lane spoke — the next run is a new delegation
      continue;
    }
    if (role !== prevRole) {
      countByRole.set(role, (countByRole.get(role) ?? 0) + 1);
      prevRole = role;
    }
    keyByIndex.set(i, `${role}#${countByRole.get(role)}`);
  }
  return keyByIndex;
}

// Chat scoping. Given the raw wire messages and a ViewFilter, return only the
// messages that belong to that view. Uses the `source` already on every message
// — no backend. "global" = everything; "orchestrator" = the orchestrator lane
// (anything not a sub-agent turn); a bare role = all of that role's turns; a
// per-call key "role#n" = just that one delegation.
export function filterMessagesByView(
  messages: WireChatMessage[],
  filter: ViewFilter,
): WireChatMessage[] {
  if (filter === "global") return messages;
  if (filter === "orchestrator")
    return messages.filter((m) => m.source?.kind !== "sub_agent");
  const call = parseCallKey(filter);
  if (!call) return [];
  // A per-call key isolates one delegation; a bare role keeps its old
  // all-turns-of-that-role behaviour.
  if (filter.includes("#")) {
    const keyByIndex = assignDelegationCalls(messages);
    return messages.filter((_, i) => keyByIndex.get(i) === filter);
  }
  return messages.filter(
    (m) => m.source?.kind === "sub_agent" && m.source.role === call.role,
  );
}

// Transcript message count per lane, keyed exactly as `stats.lanes[].lane`
// ("orchestrator" or a role). The single source for both the Session cards and
// the Agents panel's role totals, so the two can't drift apart.
export function messagesByLane(
  messages: WireChatMessage[],
): Record<string, number> {
  const out: Record<string, number> = {};
  for (const m of messages) {
    // `role` is optional on the wire; a sub_agent source without one can't be
    // attributed, so it counts as orchestrator rather than a phantom lane.
    const lane =
      m.source?.kind === "sub_agent" ? (m.source.role ?? "orchestrator") : "orchestrator";
    out[lane] = (out[lane] ?? 0) + 1;
  }
  return out;
}

// Roster. Return one entry per delegation call, in call order, each with its
// composite key and turn count. A role delegated to N times yields N entries
// (role#1 … role#N). Zero backend — derived from the transcript via
// assignDelegationCalls. AgentsPanel renders the "#n" suffix only when a role
// has more than one call.
export function deriveSubAgentRoster(
  messages: WireChatMessage[],
): { key: string; role: string; n: number; count: number }[] {
  const keyByIndex = assignDelegationCalls(messages);
  const order: string[] = []; // first-appearance order of keys
  const countByKey = new Map<string, number>();
  for (let i = 0; i < messages.length; i++) {
    const key = keyByIndex.get(i);
    if (!key) continue;
    if (!countByKey.has(key)) order.push(key);
    countByKey.set(key, (countByKey.get(key) ?? 0) + 1);
  }
  return order.map((key) => {
    const { role, n } = parseCallKey(key)!;
    return { key, role, n, count: countByKey.get(key) ?? 0 };
  });
}

// Human-facing label for a view. "orchestrator" and a single-call role read as
// their bare name; a multi-call delegation reads "role · call N". Shared by the
// Agents panel, the watch banner, and the per-todo lane pill.
export function viewLabel(view: ViewFilter, multiCall = false): string {
  if (view === "global") return "Global";
  if (view === "orchestrator") return "Orchestrator";
  const call = parseCallKey(view);
  if (!call) return view;
  return multiCall ? `${call.role} · call ${call.n}` : call.role;
}

export function adaptWelcome(s: AppState): Welcome | null {
  const w = s.welcome;
  if (!w) return null;
  return {
    provider: w.provider_name,
    model: w.model,
    maxTokens: w.max_tokens,
    builtinTools: w.builtin_tools_count,
    mcpTools: w.mcp_tools_count,
    skills: w.skills_count,
    searxngEnabled: w.searxng_enabled,
    costTracking: w.cost_tracking_enabled,
    compactionEnabled: w.compaction_enabled,
    compactionThreshold: w.compaction_threshold,
    cwd: w.cwd,
    peakbotVersion: w.peakbot_version ?? "",
  };
}
