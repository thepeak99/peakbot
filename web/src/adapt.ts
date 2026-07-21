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
    content: m.content,
    timestamp: toClock(m.timestamp),
    toolName: m.tool_name ?? undefined,
    fromBackground: m.source?.kind === "background",
    subAgentRole:
      m.source?.kind === "sub_agent" ? m.source.role : undefined,
  };
}

export function adaptStats(s: AppState): SessionStats {
  return {
    inputTokens: s.stats.total_input_tokens,
    outputTokens: s.stats.total_output_tokens,
    apiCalls: s.stats.total_api_calls,
    costUsd: s.stats.total_cost,
    modelAlias: s.stats.model_alias,
    model: s.stats.model,
    provider: s.stats.provider_name,
    lanes: (s.stats.lanes ?? []).map((l) => ({
      lane: l.lane,
      inputTokens: l.input_tokens,
      outputTokens: l.output_tokens,
      apiCalls: l.api_calls,
      costUsd: l.cost,
    })),
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

/**
 * Derive a lane's todo list from its transcript slice — the todo counterpart
 * to filesFromMessages (#208-adjacent). Sub-agent lanes drive no backend todo
 * panel, but their `todo` tool calls already TEE into the transcript tagged by
 * role; replaying those calls reconstructs the list without any new state. The
 * orchestrator and agents-off flows fall out as the single-lane case.
 *
 * This reimplements the Rust `TodoList` transition function (src/tools/todo.rs)
 * — it never consults tool *results*. A call the real tool would have rejected
 * (unknown id, invalid status) no-ops here for free, keeping the derivation a
 * pure function of the call args. Callers pass the already-scoped slice.
 */
export function todosFromMessages(messages: WireChatMessage[]): TodoItem[] {
  const items: TodoItem[] = [];
  let nextId = 1;

  const add = (task: string) => {
    const lower = task.toLowerCase();
    if (items.some((t) => t.text.toLowerCase() === lower)) return; // dedupe
    items.push({ id: nextId++, text: task, status: "pending" });
  };

  for (const m of messages) {
    if (m.role !== "toolcall" || m.tool_name !== "todo") continue;
    let args: {
      action?: string;
      tasks?: unknown;
      task_id?: unknown;
      status?: unknown;
    };
    try {
      args = JSON.parse(m.tool_args ?? "{}");
    } catch {
      continue; // malformed args — skip rather than crash the panel
    }

    switch (args.action) {
      case "add":
        if (Array.isArray(args.tasks)) {
          for (const t of args.tasks) if (typeof t === "string") add(t);
        }
        break;
      case "update": {
        if (
          typeof args.status !== "string" ||
          !TODO_WIRE_STATUSES.has(args.status as WireTodoStatus)
        )
          break; // invalid status → tool would error → no-op
        const it = items.find((t) => t.id === args.task_id);
        if (it) it.status = TODO_STATUS_MAP[args.status as WireTodoStatus];
        break;
      }
      case "remove": {
        const idx = items.findIndex((t) => t.id === args.task_id);
        if (idx !== -1) items.splice(idx, 1);
        break;
      }
      case "clear": {
        for (let i = items.length - 1; i >= 0; i--) {
          const s = items[i].status;
          if (s === "completed" || s === "cancelled") items.splice(i, 1);
        }
        if (items.length === 0) nextId = 1; // mirror TodoList id reset
        break;
      }
      // "list" and anything else are read-only / unknown → no state change.
    }
  }

  return items;
}

/**
 * Global-view todos: every lane's list, each item tagged with its lane so the
 * panel can label it. Each lane has its own independent backend TodoList (own
 * id space, own clear-resets), so we replay per lane — never on the mixed
 * transcript, which would corrupt ids/dedupe/clear. Orchestrator first, then
 * each delegation in call order. The `lane` tag is a display-ready label
 * ("Orchestrator", "role", or "role · call N") — todos aren't click targets, so
 * the pill only needs to read well. Scoped views keep using todosFromMessages
 * (no lane tag) — this is only for "global".
 */
export function todosByLane(messages: WireChatMessage[]): TodoItem[] {
  const roster = deriveSubAgentRoster(messages);
  // No sub-agent activity ⇒ nothing to disambiguate. Return plain, unlabeled
  // todos so a single-agent conversation renders exactly as it did pre-agents
  // (no "Orchestrator" pill on every item).
  if (roster.length === 0) return todosFromMessages(messages);

  const out: TodoItem[] = [];
  const tag = (lane: string, items: TodoItem[]) => {
    for (const it of items) out.push({ ...it, lane });
  };
  tag("Orchestrator", todosFromMessages(filterMessagesByView(messages, "orchestrator")));
  const callsPerRole = new Map<string, number>();
  for (const r of roster)
    callsPerRole.set(r.role, (callsPerRole.get(r.role) ?? 0) + 1);
  for (const call of roster) {
    const multi = (callsPerRole.get(call.role) ?? 0) > 1;
    tag(
      viewLabel(call.key, multi),
      todosFromMessages(filterMessagesByView(messages, call.key)),
    );
  }
  return out;
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
 * completion before returning), so per-call identity is fully derivable from
 * transcript position — no backend, works on already-saved convos.
 *
 * A `delegate` ToolCall on the orchestrator lane opens a new call for the role
 * named in its args, bumping that role's 1-based counter. Every following
 * sub-agent message of that role belongs to the open call. The result maps a
 * message's index to its "role#n" key; orchestrator-lane messages are absent.
 */
export function assignDelegationCalls(
  messages: WireChatMessage[],
): Map<number, string> {
  const keyByIndex = new Map<number, string>();
  const countByRole = new Map<string, number>();
  const openCall = new Map<string, number>(); // role → current call number

  for (let i = 0; i < messages.length; i++) {
    const m = messages[i];
    if (m.role === "toolcall" && m.tool_name === "delegate") {
      let role: unknown;
      try {
        role = JSON.parse(m.tool_args ?? "{}").role;
      } catch {
        continue; // malformed delegate args — leave following turns untagged
      }
      if (typeof role !== "string" || role.length === 0) continue;
      const n = (countByRole.get(role) ?? 0) + 1;
      countByRole.set(role, n);
      openCall.set(role, n);
      continue;
    }
    if (m.source?.kind !== "sub_agent") continue;
    const role = m.source.role;
    if (!role) continue;
    // Fall back to call 1 if a sub-agent turn appears with no preceding
    // delegate ToolCall (defensive — shouldn't happen in a well-formed convo).
    const n = openCall.get(role) ?? 1;
    if (!openCall.has(role)) openCall.set(role, n);
    keyByIndex.set(i, `${role}#${n}`);
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
