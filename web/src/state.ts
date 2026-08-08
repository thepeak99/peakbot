// Wire types — an exact mirror of the serialized Rust `AppState`
// (src/ui/app_state.rs) and the `src/ui/wire.rs` protocol envelopes.
// Field names match serde output verbatim (snake_case, lowercase enums).
// The adapter (`adapt.ts`) maps these into the camelCase view types the
// components consume.

export type WireRole =
  | "user"
  | "agent"
  | "system"
  | "toolcall"
  | "toolresult"
  | "summary";

export interface WireMessageSource {
  kind: "human" | "background" | "sub_agent";
  proc_ids?: number[];
  role?: string; // present when kind === "sub_agent"
}

export interface WireChatMessage {
  role: WireRole;
  content: string;
  timestamp: string; // ISO 8601
  tool_name?: string;
  tool_args?: string;
  tool_result?: string;
  call_id?: string;
  compacted?: boolean;
  source?: WireMessageSource;
}

export interface WireChat {
  messages: WireChatMessage[];
  auto_scroll: boolean;
  scroll_offset: number;
}

export type WireTodoStatus =
  | "pending"
  | "in_progress"
  | "completed"
  | "cancelled";

export interface WireTodoItem {
  id: number;
  text: string;
  status: WireTodoStatus;
}

export interface WireTodo {
  visible: boolean;
  items: WireTodoItem[];
}

export interface WireLaneStat {
  lane: string;
  input_tokens: number;
  output_tokens: number;
  api_calls: number;
  /** Model alias behind this lane. Optional so old snapshots parse. */
  model?: string;
  cost: number;
}

export interface WireStats {
  total_input_tokens: number;
  total_output_tokens: number;
  total_api_calls: number;
  total_cost: number;
  /** Per-lane breakdown (orchestrator + sub-agent roles). Optional so old
   * wire snapshots parse cleanly; empty until the first lane-attributed
   * request. */
  lanes?: WireLaneStat[];
  model: string;
  provider_name: string;
  model_alias: string;
}

export interface WireContext {
  current_usage: number;
  window_size: number;
  compaction_enabled: boolean;
  compaction_threshold: number;
}

export interface WireWelcome {
  provider_name: string;
  model: string;
  max_tokens: number;
  builtin_tools_count: number;
  mcp_tools_count: number;
  skills_count: number;
  searxng_enabled: boolean;
  searxng_url?: string | null;
  cost_tracking_enabled: boolean;
  compaction_enabled: boolean;
  compaction_threshold: number;
  compaction_keep_recent: number;
  conversation_persistence_enabled: boolean;
  cwd: string;
  /** Optional so old wire snapshots (pre-v0.14) parse cleanly. */
  peakbot_version?: string;
}

export interface WireBgSummary {
  id: number;
  command: string;
  label?: string | null;
  status: string;
  exit_code?: number | null;
}

export interface WireBg {
  running_count: number;
  recent_summaries: WireBgSummary[];
}

// BashPanelState is an internally-tagged enum (`kind`).
export type WireBashPanel =
  | { kind: "idle" }
  | { kind: "running"; command: string; pid: number; started_at: string; tail: string[] }
  | { kind: "finished"; command: string; exit_code: number; duration_secs: number; tail: string[] };

export interface WireConversationMeta {
  id?: string;
  name?: string;
}

export interface AppState {
  chat: WireChat;
  todo: WireTodo;
  stats: WireStats;
  context: WireContext;
  conversation: WireConversationMeta | null;
  is_running: boolean;
  is_loading: boolean;
  /** Messages typed during a busy turn, queued but not yet dequeued by the
   * agent loop. Drives the "⏳ N queued" hint (issue #123 — counter only;
   * per-message deletion needs a queue refactor still to come). */
  pending_input_count?: number;
  welcome: WireWelcome | null;
  status_message?: string | null;
  exit_requested: boolean;
  bg: WireBg;
  bash_panel: WireBashPanel;
  /** The pipelines configured at boot, in declaration order (the UI's order).
   * Empty / absent means no `pipelines:` block — selection isn't offered.
   * Optional so old wire snapshots parse. */
  pipelines?: WirePipelineInfo[];
  /** The pipeline THIS conversation is bound to, or null for single-agent
   * mode. Mutable only before the first turn; everything downstream
   * (orchestrator model, delegate roster, `/model` lock) derives from it. */
  selected_pipeline?: string | null;
}

/** One configured pipeline — the wire projection of `PipelineInfo`
 * (src/pipeline/set.rs), carried on `AppState.pipelines`. */
export interface WirePipelineInfo {
  name: string;
  /** Model alias the orchestrator is pinned to (config, not live state). */
  orchestrator_model: string;
  /** `[role, model alias]` pairs, sorted by role. TUPLES on the wire: serde
   * serialises the Rust `Vec<(String, String)>` as arrays of two strings. */
  members: [string, string][];
}

// ── Protocol envelopes (src/ui/wire.rs) ───────────────────────────────

export interface ModelInfo {
  alias: string;
  provider_name: string;
  model_name: string;
  context_size: number;
}

export interface ConversationSummary {
  id: string;
  name: string;
  updated_at: string;
  message_count: number;
  model: string;
  active: boolean;
}

// One entry in a `dir_listing` reply (src/ui/wire.rs DirEntryWire).
export interface DirEntry {
  name: string;
  is_dir: boolean;
}

// A `dir_listing` reply payload (the frame minus its `type` tag).
export interface DirListing {
  path: string;
  parent: string | null;
  entries: DirEntry[];
  error: string | null;
}

// Served by `GET /commands` (src/ui/ui_trait.rs::SlashCommand). The single
// source of truth for the composer's slash palette; fetched once on load.
export interface SlashCommand {
  name: string;
  description: string;
  takes_args: boolean;
}

export type OutboundMessage =
  | { type: "ready" }
  | { type: "attached"; convo: string }
  | { type: "models_available"; active: string; models: ModelInfo[] }
  | { type: "state"; state: AppState }
  | { type: "conversations_list"; items: ConversationSummary[] }
  | {
      type: "dir_listing";
      path: string;
      parent: string | null;
      entries: DirEntry[];
      error: string | null;
    }
  | { type: "error"; message: string };

export type InboundMessage =
  | { type: "attach"; convo: string | null }
  | { type: "send_message"; text: string }
  | { type: "stop" }
  | { type: "switch_model"; alias: string }
  | { type: "switch_cwd"; path: string }
  /** Bind this conversation to a named pipeline; `null` clears the binding
   * (single-agent mode). The backend enforces the pre-first-turn lock. */
  | { type: "select_pipeline"; name: string | null }
  | { type: "list_dir"; path: string }
  | { type: "request_conversations" }
  | { type: "kill_session"; convo: string }
  | { type: "shutdown" };
