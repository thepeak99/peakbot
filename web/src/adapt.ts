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
  MessageRole,
  SessionStats,
  TodoItem,
  TodoStatus,
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
  };
}

export function adaptContext(s: AppState): ContextUsage {
  return {
    currentUsage: s.context.current_usage,
    windowSize: s.context.window_size,
    compactionThreshold: s.context.compaction_threshold,
  };
}

export function adaptTodos(s: AppState): TodoItem[] {
  return s.todo.items.map((t) => ({
    id: t.id,
    text: t.text,
    status: TODO_STATUS_MAP[t.status] ?? "pending",
  }));
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
 * Derive the list of files the agent touched this session from the transcript
 * (#126). No new backend state — the path lives in each file tool call's
 * `tool_args` JSON. Order is first-touch; `edits` counts write operations
 * (reads don't count as edits). `kind` follows created > modified > read.
 */
export function adaptFiles(s: AppState): FileEdit[] {
  const byPath = new Map<string, FileEdit>();
  for (const m of s.chat.messages) {
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
