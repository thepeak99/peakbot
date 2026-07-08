// Phase 0 mock data. Shapes mirror the Rust `AppState` (src/ui/app_state.rs)
// so the Phase-1 swap to a live WebSocket reader is a drop-in: the same
// component tree renders real `AppState` frames instead of these constants.
// Nothing here is wired to a socket — this is the "final UI, no functionality"
// preview the plan (webui.md §5, Phase 0) calls for.

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
  timestamp: string; // pre-formatted "HH:MM" for the mock
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

// ── Mock frame ────────────────────────────────────────────────────────

export const welcome: Welcome = {
  provider: "openrouter",
  model: "anthropic/claude-3.7-sonnet",
  maxTokens: 8192,
  builtinTools: 12,
  mcpTools: 7,
  skills: 21,
  searxngEnabled: true,
  costTracking: true,
  compactionEnabled: true,
  compactionThreshold: 0.8,
  cwd: "~/workz/peakhands/peakbot",
};

export const stats: SessionStats = {
  inputTokens: 48213,
  outputTokens: 12904,
  apiCalls: 9,
  costUsd: 0.2841,
  modelAlias: "sonnet",
  model: "anthropic/claude-3.7-sonnet",
  provider: "openrouter",
};

export const context: ContextUsage = {
  currentUsage: 61117,
  windowSize: 200000,
  compactionThreshold: 0.8,
};

export const todos: TodoItem[] = [
  { id: 1, text: "Read AppState and mirror it in the mock", status: "completed" },
  { id: 2, text: "Build the transcript component", status: "completed" },
  { id: 3, text: "Wire the side rail (stats + context)", status: "inProgress" },
  { id: 4, text: "Add bg-process + bash panels", status: "pending" },
  { id: 5, text: "Drop the old placeholder screen", status: "cancelled" },
];

export const bgProcesses: BgProcess[] = [
  { id: 3, command: "cargo watch -x test", label: "test-watch", status: "running" },
  { id: 4, command: "npm run dev", status: "running" },
  { id: 2, command: "make build-linux", status: "exited", exitCode: 0 },
];

export const bashPanel: BashPanel = {
  command: "cargo clippy --all-targets -- -D warnings",
  status: "running",
  pid: 48213,
  elapsed: "00:12",
  tail: [
    "   Compiling peakbot v0.11.1",
    "    Checking rust-embed v8.5.0",
    "    Checking axum v0.8.1",
    "   Building [=======>          ] 214/512",
  ],
};

export const messages: ChatMessage[] = [
  {
    role: "user",
    content: "Add a `peakbot --web` flag that serves the embedded SPA.",
    timestamp: "23:41",
  },
  {
    role: "agent",
    content:
      "On it. I'll wire a `WebUi` that implements the `Ui` trait and slots in " +
      "where `--stdio` does. First let me read how the boot sequence dispatches views.",
    timestamp: "23:41",
  },
  {
    role: "toolCall",
    content: "💭 Read the view-dispatch block in main.rs\n🔧 file_read(path=src/main.rs, start_line=333, end_line=360)",
    timestamp: "23:41",
    toolName: "file_read",
  },
  {
    role: "toolResult",
    content: "📄 27 lines\n    if cli.stdio {\n        let mut ui = StdioUi::new(...)\n... [24 more lines]",
    timestamp: "23:41",
    toolName: "file_read",
  },
  {
    role: "toolCall",
    content:
      "💭 Add the --web branch next to --stdio\n🔧 file_str_replace(path=src/main.rs, old_str=\"if cli.stdio {\", ...)",
    timestamp: "23:42",
    toolName: "file_str_replace",
  },
  {
    role: "toolResult",
    content: "✅ Successfully edited src/main.rs — replaced 1 occurrence",
    timestamp: "23:42",
    toolName: "file_str_replace",
  },
  {
    role: "toolCall",
    content: "💭 Confirm it still compiles\n🔧 bash(command=cargo build)",
    timestamp: "23:42",
    toolName: "bash",
  },
  {
    role: "toolResult",
    content: "✅ Exit 0 | Compiling peakbot v0.11.1 | Finished `dev` profile in 3.34s",
    timestamp: "23:43",
    toolName: "bash",
  },
  {
    role: "agent",
    content:
      "Done. The `--web` branch binds `127.0.0.1:7823`, serves the embedded " +
      "`web/dist/` with an SPA fallback, and shuts down on Ctrl+C. Here's the shape:\n\n" +
      "```rust\nlet mut ui = WebUi::new(addr).with_shutdown(shutdown_rx);\nui.init().await?;\n```\n\n" +
      "Phase 1 adds the WebSocket route and the per-connection session factory.",
    timestamp: "23:43",
  },
  {
    role: "summary",
    content: "Earlier context compacted — 6 messages summarized to fit the window.",
    timestamp: "23:40",
  },
  {
    role: "system",
    content: "🛰 Background · #3 `cargo watch -x test` (12 new lines)",
    timestamp: "23:44",
    fromBackground: true,
  },
];
