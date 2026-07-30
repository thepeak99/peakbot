// View model — the camelCase shapes the components render. These are the
// display contract (kept from the Phase-0 mock). `adapt.ts` maps the wire
// `AppState` (state.ts) into these; components never see raw wire types.

export type MessageRole =
  | "user"
  | "agent"
  | "system"
  | "toolCall"
  | "toolResult"
  | "summary";

export interface ChatMessage {
  role: MessageRole;
  content: string;
  timestamp: string; // formatted "HH:MM"
  toolName?: string;
  /** background-process origin badge (mirrors MessageSource::Background) */
  fromBackground?: boolean;
  /** sub-agent role, when this turn came from a delegated sub-agent
   * (mirrors MessageSource::SubAgent) */
  subAgentRole?: string;
}

// Which agent's view the UI is scoped to (chat/todo/stats). "global" = all
// agents, one transcript, todos labeled by lane; "orchestrator" = the top-level
// lane only; "role#n" = one specific delegation of a sub-agent role (per-call).
export type ViewFilter = "global" | "orchestrator" | string;

export type TodoStatus = "pending" | "inProgress" | "completed" | "cancelled";

export interface TodoItem {
  id: number;
  text: string;
  status: TodoStatus;
  /** Display label for the lane this todo belongs to ("Orchestrator", a role,
   * or "role · call N"). Set only in the global view (todosByLane) so the panel
   * can label it; absent when the panel is already scoped to a single lane. */
  lane?: string;
}

export interface LaneStat {
  lane: string;
  inputTokens: number;
  outputTokens: number;
  apiCalls: number;
  /** Model alias behind this lane; empty when the backend didn't name one. */
  model: string;
  costUsd: number;
}

export interface SessionStats {
  inputTokens: number;
  outputTokens: number;
  apiCalls: number;
  costUsd: number;
  modelAlias: string;
  model: string;
  provider: string;
  /** Per-lane breakdown (orchestrator + sub-agent roles), orchestrator first.
   * Empty until a lane-attributed request lands. */
  lanes: LaneStat[];
}

export interface ContextUsage {
  currentUsage: number;
  windowSize: number;
  compactionThreshold: number; // 0..1
}

export interface BgProcess {
  id: number;
  command: string;
  label?: string;
  status: "running" | "exited";
  exitCode?: number;
}

// A file the agent touched this session, derived from file-edit tool calls
// in the transcript (#126). `edits` counts how many times it was written.
export interface FileEdit {
  path: string;
  edits: number;
  /** Latest-action kind, with precedence created > modified > read. A file
   * that was ever created stays "created"; a read-then-edited file is
   * "modified"; a file only ever read is "read". */
  kind: "created" | "modified" | "read";
}

export interface BashPanel {
  command: string;
  status: "running" | "finished";
  pid?: number;
  elapsed: string;
  exitCode?: number;
  tail: string[];
}

export interface Welcome {
  provider: string;
  model: string;
  maxTokens: number;
  builtinTools: number;
  mcpTools: number;
  skills: number;
  searxngEnabled: boolean;
  costTracking: boolean;
  compactionEnabled: boolean;
  compactionThreshold: number;
  cwd: string;
  /** PeakBot binary version (e.g. "0.13.3"). Empty when the backend
   * hasn't populated it yet (pre-v0.14 wire snapshot). */
  peakbotVersion: string;
}
