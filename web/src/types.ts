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

// Which agent's view the UI is scoped to (chat/todo/stats). "global" = the
// current all-agents view with badges; "orchestrator" = the top-level lane
// only; any other string = a specific sub-agent role. Phase-1 dummy: a single
// client-side selector, no backend.
export type ViewFilter = "global" | "orchestrator" | string;

export type TodoStatus = "pending" | "inProgress" | "completed" | "cancelled";

export interface TodoItem {
  id: number;
  text: string;
  status: TodoStatus;
}

export interface SessionStats {
  inputTokens: number;
  outputTokens: number;
  apiCalls: number;
  costUsd: number;
  modelAlias: string;
  model: string;
  provider: string;
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
