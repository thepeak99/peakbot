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
}

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
}
