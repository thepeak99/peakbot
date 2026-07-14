//! PeakBot library - Core functionality for connecting to MCP servers and managing tools.

/// PeakBot binary version, baked in at compile time. The single source of truth
/// for "what is this?" — the TUI welcome banner, the system prompt the model
/// sees, and the `WelcomeState` payload sent over the WebSocket wire all read
/// from this one constant. Adding a duplicate `env!("CARGO_PKG_VERSION")`
/// literal anywhere else in the crate is a regression: any of the three sites
/// could drift from the binary the user actually runs.
pub const PEAKBOT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod bg_processes;
mod config;
mod context_manager;
mod conversation;
mod conversation_manager;
mod conversation_title;
mod hooks;
mod http;
mod mcp_auth;
mod memory_compaction;
#[cfg(feature = "mock")]
pub mod mock;
mod pipeline;
mod providers;
pub mod pty_runner;
pub mod session;
mod skills;
pub mod state;
pub mod storage;
pub mod test_runner;
mod tool_use_validator;
mod tools;
pub mod ui;
pub mod utils;
pub mod vector;
pub mod vision;

pub use config::{
    AgentDefinition, AnthropicConfig, BashConfig, Config, ContextConfig, ConversationConfig,
    LoadedConfig, McpServerConfig, McpTransportType, ModelEntry, ModelRegistry, OllamaConfig,
    OpenRouterConfig, PipelineConfig, ProviderConfig, ProviderEntry, ProviderType, RegistryError,
    ResolvedModel, RetryConfig, SearXngConfig, VectorDbConfig, get_config_file_path,
};
use context_manager::ContextManager;
pub use context_manager::{CompactionResult, auto_detect_context_size};
pub use conversation::{
    Conversation, ConversationMetadata, ConversationSummary, Message as ConversationMessage,
};
pub use conversation_manager::{ConversationManager, ConversationManagerConfig};
pub use hooks::{
    AgentEvent, ModelPricing, SessionHook, SessionStats, TokenUsage, fetch_model_pricing,
};
pub use pipeline::{DelegateTool, SubAgentRegistry};
#[cfg(feature = "mock")]
pub use providers::create_mock_agent;
pub use providers::{
    CompactionModel, DynAgent, ProviderInfo, create_compaction_model, create_provider,
};

use rig_core::completion::{Message, PromptError};
use rig_core::tool::ToolDyn;
use rig_core::tool::rmcp::McpTool;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{TokioChildProcess, streamable_http_client::StreamableHttpClientTransport};
pub use session::{Session, SessionDeps, create_session};
pub use skills::{SkillRegistry, load_default_skills};
pub use state::StateManager;
pub use storage::{ConversationStorage, FileStorage};
pub use test_runner::{CompactionInfo, TestRunner};
pub use tools::{
    BashTool, FetchPageTool, FetchUrlTool, FileCreateTool, FileInsertTool, FileReadTool,
    FileStrReplaceTool, ListDirectoryTool, PowerShellTool, SearchTool, ShellKind, ThinkTool,
    TodoArgs, TodoItem, TodoStatus, TodoTool, print_no_shell_warning,
};
pub use ui::{Ui, UiAction};

use anyhow::{Context, Result, anyhow};
use rmcp::service::{RoleClient, RunningService, ServiceExt};
use std::process::Stdio;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::debug;

/// Message types for internal queue between event loop and agent loop.
///
/// **Single-writer invariant** (see `make-flow-great-again.md`):
/// `UserMessage` carries the buffered text + attachments forward through
/// the channel. The `add_user_message{,_with_attachments}` write into
/// `StateManager` happens in the **agent loop** at dequeue time — never in
/// the event loop. This is what guarantees user-typed text only ever
/// lands between agent turns, never inside one (specifically, never
/// between an in-flight `ToolCall` and its `ToolResult`).
enum QueueMessage {
    UserMessage {
        text: String,
        attachments: Vec<crate::vision::ImageAttachment>,
    },
    Command(String),
    StopMarker, // Signals that stop was requested
    /// Switch the active model and restart on a fresh conversation.
    /// Carries the validated alias (validation happens in the View
    /// before the action is sent). The agent loop dequeues this between
    /// turns and runs `rebuild_agent_for_alias`.
    SwitchModel(String),
    /// Change the working directory and restart on a fresh conversation.
    /// Carries the validated, canonicalised path (validation happens in
    /// the View before the action is sent). Dequeued between turns and
    /// handled by `handle_change_cwd`. Same rebuild seam as SwitchModel,
    /// different axis. See ticket #124.
    ChangeCwd(String),
    /// One or more `bash_bg` processes have output ready. Payload is
    /// empty — the agent loop drains every bg buffer in one pass.
    /// Multiple notifications coalesce naturally (the drain returns
    /// `None` if buffers were already cleared). Pushed by per-process
    /// reader threads (debounced inside the reader) and consumed
    /// between turns. See `bash-background.md` § "Wiring into the agent loop".
    BackgroundOutputReady,
}

/// How should a submitted input buffer be routed by the event loop?
///
/// The REPL emits every Enter-submission as `UiAction::SendMessage(msg)` —
/// the popup path and the plain-typed path go through the same action.
/// `classify_submission` is the single chokepoint that decides whether the
/// text is a slash command (dispatched internally) or a user turn (sent to
/// the LLM).
///
/// Regression guard: before this existed, every slash command (`/new`,
/// `/help`, …) was appended as a user message and billed as an LLM turn.
/// See `allehailmenu.md`.
#[derive(Debug)]
enum SubmitKind {
    /// `/stop` — interrupts the running agent instead of queueing.
    StopCommand,
    /// Any other `/xxx` — routed to `process_command_internal`.
    Command(String),
    /// Plain chat content — sent to the LLM.
    UserMessage(String),
    /// User turn with one or more `[img:…]` attachments parsed from the
    /// buffer. Capability-checked against `ProviderInfo::supports_vision`
    /// before dispatch.
    MultimodalMessage {
        text: String,
        attachments: Vec<crate::vision::ImageAttachment>,
    },
    /// `[img:…]` parsed but failed (missing file, too large, invalid token…).
    /// Surfaces as a system error; does not reach the LLM.
    InvalidAttachment(crate::vision::AttachmentError),
}

fn classify_submission(msg: &str) -> SubmitKind {
    let trimmed = msg.trim();
    if trimmed == "/stop" {
        return SubmitKind::StopCommand;
    }
    // Preserve whitespace on commands — leading whitespace in "/ quit"
    // should not be stripped before the command handler sees it.
    if msg.starts_with('/') {
        return SubmitKind::Command(msg.to_string());
    }
    // Try inline image parsing FIRST — before deciding it's plain text.
    // A buffer without `[img:` returns `(buf, [])` immediately (cheap).
    match crate::vision::parse_attachments_inline(msg) {
        Ok((text, attachments)) if !attachments.is_empty() => SubmitKind::MultimodalMessage {
            text: text.trim().to_string(),
            attachments,
        },
        Ok(_) => SubmitKind::UserMessage(msg.to_string()),
        Err(e) => SubmitKind::InvalidAttachment(e),
    }
}

/// Completion result sent from agent loop back to event loop
#[derive(Clone)]
enum CompletionResult {
    Success,
    Stopped,
    Error,
    CommandDone,
}

const SYSTEM_PROMPT: &str = include_str!("system_prompt.txt");

/// Build the system prompt dynamically with environment information.
///
/// `shell_kind` is the shell detected at startup; its presence appends a
/// `**Shell**:` line to the environment block so the model uses the right
/// syntax and the right shell tool name. On a PowerShell host this also
/// overrides the bash-centric guidance baked into the static prompt.
pub fn build_system_prompt(
    skills: &SkillRegistry,
    shell_kind: Option<&ShellKind>,
    cwd: &std::path::Path,
) -> String {
    let mut prompt = SYSTEM_PROMPT.to_string();

    let cwd_display = cwd.to_string_lossy().to_string();

    let current_time = chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S %Z")
        .to_string();

    // Load agents.md from the session cwd (case-insensitive filename match).
    let agents_md_content = std::fs::read_dir(cwd)
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(|e| e.ok())
                .find(|e| {
                    e.path()
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|name| name.to_lowercase() == "agents.md")
                        .unwrap_or(false)
                })
        })
        .and_then(|entry| std::fs::read_to_string(entry.path()).ok())
        .map(|content| format!("\n# Agents.md Content\n\n--------------------------------------------------------\n{}\n", content.trim()))
        .unwrap_or_default();

    let skills_section = skills.to_system_prompt_section();

    let os = std::env::consts::OS;

    let binary_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unknown".to_string());

    let env_info = format!(
        "\n# Environment Information\n\n- **Current Working Directory**: {}\n- **Current Time**: {}\n- **PeakBot Version**: {}\n- **Operating System**: {}\n- **PeakBot Binary Path**: {}\n{}",
        cwd_display,
        current_time,
        PEAKBOT_VERSION,
        os,
        binary_path,
        shell_line(shell_kind),
    );

    prompt.push_str(&skills_section);
    prompt.push_str(&env_info);
    prompt.push_str(&agents_md_content);

    debug!("System prompt:\n {}", prompt);

    prompt
}

/// The `**Shell**:` env-block line, derived from the detected shell.
///
/// On PowerShell this is deliberately emphatic: it names the `powershell`
/// tool and instructs PowerShell syntax, overriding the bash examples in
/// the static prompt (the recency/specificity of this trailing line wins).
/// Empty when no shell was detected (Windows with nothing installed).
fn shell_line(shell_kind: Option<&ShellKind>) -> String {
    match shell_kind {
        Some(ShellKind::PowerShell { path }) => format!(
            "- **Shell**: powershell ({path}) — your shell tool is named `powershell`. \
            Write PowerShell syntax (e.g. `Select-String`, `Get-ChildItem`, `Get-Content`), \
            NOT bash/sed/grep. The bash examples elsewhere in this prompt do not apply here.\n"
        ),
        Some(ShellKind::Bash { path }) => {
            format!("- **Shell**: bash ({path}) — use POSIX/bash syntax.\n")
        }
        None => String::new(),
    }
}

/// Convert stored conversation messages to rig Messages for LLM chat history
pub fn convert_conversation_to_rig_messages(conv: &Conversation) -> Vec<Message> {
    use crate::conversation::Message as StoredMessage;
    use rig_core::completion::message::{
        AssistantContent, ToolCall, ToolFunction, ToolResult, ToolResultContent, UserContent,
    };
    use rig_core::one_or_many::OneOrMany;

    let mut messages = Vec::new();

    for msg in &conv.messages {
        match msg {
            StoredMessage::User { content, .. } => {
                messages.push(Message::user(content.clone()));
            }
            StoredMessage::Assistant { content, .. } => {
                messages.push(Message::assistant(content.clone()));
            }
            StoredMessage::ToolCall {
                tool_name,
                arguments,
                call_id,
                ..
            } => {
                let args = serde_json::from_str(arguments)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                let id = call_id.clone().unwrap_or_else(|| tool_name.clone());
                messages.push(Message::Assistant {
                    id: None,
                    content: OneOrMany::one(AssistantContent::ToolCall(ToolCall::new(
                        id,
                        ToolFunction::new(tool_name.clone(), args),
                    ))),
                });
            }
            StoredMessage::ToolResult {
                tool_name,
                result,
                call_id,
                ..
            } => {
                let id = call_id.clone().unwrap_or_else(|| tool_name.clone());
                messages.push(Message::User {
                    content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                        id,
                        call_id: None,
                        // Image-JSON results (`view_image`) reconstruct as Image;
                        // plain results stay text. See get_agent_history.
                        content: ToolResultContent::from_tool_output(result.clone()),
                    })),
                });
            }
            StoredMessage::Summary { content, .. } => {
                messages.push(Message::user(format!("[Conversation summary] {content}")));
            }
        }
    }

    messages
}

/// Bundle of construction deps the agent loop needs to rebuild
/// `DynAgent` on a `/model` switch. All fields are model-agnostic and
/// stay alive for the entire process lifetime — only the agent itself
/// is replaced. `mcp_handles` is kept around explicitly so MCP
/// subprocesses survive a switch (rebuilt tool list, same processes).
///
/// `None` for any field means "don't try to switch models" (the
/// `/model` command will be inert because no registry is attached, or
/// agent-loop will surface an error if the user somehow gets a
/// `SwitchModel` action through). Production boot in `main.rs` always
/// populates this.
pub struct RebuildContext {
    pub registry: Arc<crate::config::ModelRegistry>,
    pub system_prompt: String,
    pub mcp_handles: Arc<Vec<McpServerHandle>>,
    pub searxng_config: Option<crate::config::SearXngConfig>,
    pub max_turns: usize,
    pub todo_tool: Option<crate::tools::TodoTool>,
    pub bash_config: crate::config::BashConfig,
    pub pipeline_registry: Option<Arc<crate::pipeline::SubAgentRegistry>>,
    /// Detected shell kind — OS-level, so it persists across `/model` switches.
    pub shell_kind: Option<crate::tools::ShellKind>,
    /// Skill registry — needed to rebuild the system prompt when `/cd`
    /// changes the working directory (the prompt embeds both the cwd and
    /// the skills section). Persists across switches like `shell_kind`.
    pub skills: crate::skills::SkillRegistry,
    /// Shared vector store (doc_index / doc_search), if configured. Cheap to
    /// clone (Arc-backed); persists across `/model` switches like the MCP
    /// handles, since the DB handle is provider-independent.
    pub vector_store: Option<crate::vector::VectorStore>,
}

/// Shared cell holding the *currently active* SessionHook. Replaced
/// in-place by the agent loop on `/model` rebuild; the event loop's
/// `/stop` path reads through the cell so it always cancels the
/// active prompt's hook, never a stale predecessor. Reads clone the
/// inner Arc immediately so the guard never spans an await point.
pub type SharedSessionHook = Arc<std::sync::RwLock<Arc<SessionHook>>>;

/// Shared cell holding the *currently active* `ProviderInfo`. Same
/// shape and reasoning as [`SharedSessionHook`] — needed for the
/// vision-capability gate in the multimodal arm of `event_loop`.
pub type SharedProviderInfo = Arc<std::sync::RwLock<Arc<ProviderInfo>>>;

/// AgentRunner — the Controller in MVC.
///
/// Receives input (UiAction) from Views, calls the agent, writes results to
/// StateManager (Model). Never reads stdin or prints directly.
pub struct AgentRunner {
    agent: Arc<DynAgent>,
    config: Config,
    provider_info: ProviderInfo,
    #[allow(unused)]
    skills: SkillRegistry,
    state_manager: Option<Arc<StateManager>>,
    // Shared session hook for interrupt/queue state
    session_hook: Arc<SessionHook>,
    // Retained for streaming output handler (view concern, set up by main.rs)
    event_receiver: Option<mpsc::UnboundedReceiver<AgentEvent>>,
    /// Optional rebuild deps for `/model` switching. Set by
    /// [`AgentRunner::with_rebuild_context`] from `main.rs` after
    /// construction. When `None`, `/model` is a no-op.
    rebuild_ctx: Option<RebuildContext>,
    /// Tool-free compaction model shared with ContextManager.
    /// Cloned at construction so memory compaction can use it
    /// without reaching through StateManager.
    compaction_model: Option<Arc<CompactionModel>>,
}

impl AgentRunner {
    /// `context_size` drives ContextManager compaction thresholds and must
    /// come from the resolved model (config `context_size:` or auto-detect).
    #[allow(clippy::too_many_arguments)] // context_size is the new mandatory parameter; callers supply resolved value
    pub fn new(
        agent: DynAgent,
        config: Config,
        provider_info: ProviderInfo,
        skills: SkillRegistry,
        event_receiver: Option<mpsc::UnboundedReceiver<AgentEvent>>,
        state_manager: Option<Arc<StateManager>>,
        session_hook: Arc<SessionHook>,
        context_size: usize,
    ) -> anyhow::Result<Self> {
        let agent = Arc::new(agent);

        // Create a tool-free compaction model from the same provider.
        // When compaction is enabled, a construction failure is fatal —
        // refuse to boot rather than silently degrade into the death
        // spiral.  See unfuck-compact.md Layer 1.
        let compaction_model = if config.context.enabled {
            let model = crate::providers::create_compaction_model(
                &config.provider,
                config.context.compaction_model.as_deref(),
            )
            .with_context(|| {
                "Failed to construct compaction model for the active provider. \
                 The active provider config is what's read for compaction (same \
                 credentials as the main model). To boot without compaction, set \
                 `context.enabled: false` in config.yaml."
            })?;
            Some(Arc::new(model))
        } else {
            crate::providers::create_compaction_model(
                &config.provider,
                config.context.compaction_model.as_deref(),
            )
            .ok()
            .map(Arc::new)
        };

        // Initialize ContextManager inside StateManager (StateManager owns it)
        if let Some(ref sm) = state_manager {
            let cm = ContextManager::new(
                config.context.clone(),
                context_size,
                compaction_model.clone(),
            );
            sm.init_context_manager(cm);
            // The title model is the same as the compaction model — reuse the Arc
            if let Some(model) = compaction_model.clone() {
                sm.init_title_model(model);
            }
        }

        Ok(Self {
            agent,
            config,
            provider_info,
            skills,
            state_manager,
            session_hook,
            event_receiver,
            rebuild_ctx: None,
            compaction_model,
        })
    }

    /// Attach a [`RebuildContext`] so `/model` switches can rebuild
    /// the agent in-place between turns. Builder-style — returns
    /// `self` for chaining off `AgentRunner::new(…)`.
    pub fn with_rebuild_context(mut self, ctx: RebuildContext) -> Self {
        self.rebuild_ctx = Some(ctx);
        self
    }

    /// Force context compaction
    pub async fn force_compact(&mut self) {
        if let Some(ref sm) = self.state_manager {
            match sm.force_compact().await {
                Some(result) => {
                    sm.add_system_message(format!(
                        "Context compacted: {} → {} messages, {} discarded",
                        result.original_count, result.compacted_count, result.num_discarded
                    ));
                }
                None => {
                    sm.add_system_message("Nothing to compact.".to_string());
                }
            }
        }
    }

    /// Message types for internal queue between event loop and agent loop
    pub const _QUEUE_PLACEHOLDER: () = ();

    /// The controller loop — spawns two loops: event loop (receives from View) and
    /// agent loop (processes messages). This allows /stop to interrupt the agent.
    pub async fn run_loop(&mut self, action_receiver: mpsc::UnboundedReceiver<UiAction>) {
        // Channel between event loop and agent loop
        let (msg_tx, msg_rx) = tokio::sync::mpsc::channel::<QueueMessage>(32);

        // Background-process notification bridge: per-process reader
        // threads push `()` pings here (debounced inside the reader);
        // a small forwarder task translates each ping into a
        // `QueueMessage::BackgroundOutputReady` and ships it through
        // the same agent-loop queue user messages flow on. The
        // `Sender<()>` is handed to `StateManager` so the `bash_bg`
        // tool's `start` verb can hand a clone to each reader. See
        // `bash-background.md` § "Wiring into the agent loop".
        let (bg_notify_tx, mut bg_notify_rx) = mpsc::unbounded_channel::<()>();
        let bg_sm = self.state_manager.clone();
        if let Some(sm) = bg_sm.as_ref() {
            sm.attach_bg_notify(bg_notify_tx);
        }
        let bg_bridge_handle = {
            let msg_tx = msg_tx.clone();
            tokio::spawn(async move {
                loop {
                    // A process may be coalescing output behind its
                    // cooldown; arm a wakeup at the soonest window expiry
                    // so a buffer that goes quiet still flushes on time.
                    let deadline = bg_sm.as_ref().and_then(|sm| sm.next_bg_poke_deadline());
                    let flush = async {
                        match deadline {
                            Some(d) => tokio::time::sleep_until(d.into()).await,
                            // No pending cooldown → park this arm forever;
                            // the next reader ping re-evaluates the deadline.
                            None => std::future::pending::<()>().await,
                        }
                    };
                    tokio::select! {
                        biased;
                        ping = bg_notify_rx.recv() => {
                            if ping.is_none() {
                                break; // sender dropped → loop torn down
                            }
                            let _ = msg_tx.send(QueueMessage::BackgroundOutputReady).await;
                        }
                        _ = flush => {
                            let _ = msg_tx.send(QueueMessage::BackgroundOutputReady).await;
                        }
                    }
                }
            })
        };

        // Drain flag — set by event loop on /stop, consumed by agent loop to
        // discard any queued UserMessage/Command between the dropped turn and
        // the matching StopMarker. See `make-flow-great-again.md`: /stop ==
        // stop, queued follow-ups are discarded along with the in-flight turn.
        let drain_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));

        // Completion notifications back to event loop
        let (completion_tx, _completion_rx) =
            tokio::sync::broadcast::channel::<CompletionResult>(8);

        // Extract fields we need to pass to spawned loops
        let state_manager = self.state_manager.clone();
        let config_model = self.config.model().to_string();
        let agent = self.agent.clone();
        let state_manager_for_agent = self.state_manager.clone();
        let config_for_agent = self.config.clone();
        let event_receiver = self.event_receiver.take();
        let compaction_model = self.compaction_model.clone();
        // Shared cells for state that swaps on `/model` rebuild. The
        // event loop reads through these so `/stop` always cancels the
        // active prompt and the multimodal-vision gate always reflects
        // the active model. See [`SharedSessionHook`] / [`SharedProviderInfo`].
        let session_hook_cell: SharedSessionHook =
            Arc::new(std::sync::RwLock::new(self.session_hook.clone()));
        let provider_info_cell: SharedProviderInfo =
            Arc::new(std::sync::RwLock::new(Arc::new(self.provider_info.clone())));
        // The rebuild context is consumed once into the agent loop. If
        // it's `None`, `/model` switches are inert (the loop emits a
        // system-message error if a `SwitchModel` action somehow
        // arrives).
        let rebuild_ctx = self.rebuild_ctx.take();

        // Spawn the two loops.
        //
        // Note: the per-event processor task used to live here (it
        // consumed `event_receiver` and forwarded events to the
        // `StateManager`). It now runs *inside* the agent loop so it
        // can be torn down and respawned with the fresh receiver
        // produced by a `/model` rebuild. See `agent_loop`.
        let event_handle = tokio::spawn({
            let msg_tx = msg_tx.clone();
            let completion_tx = completion_tx.clone();
            let drain_requested = drain_requested.clone();
            let session_hook_cell = session_hook_cell.clone();
            let provider_info_cell = provider_info_cell.clone();

            async move {
                Self::event_loop(
                    action_receiver,
                    msg_tx,
                    completion_tx,
                    state_manager,
                    session_hook_cell,
                    config_model,
                    provider_info_cell,
                    drain_requested,
                )
                .await;
            }
        });

        let agent_handle = tokio::spawn({
            let msg_rx = tokio::sync::Mutex::new(msg_rx);
            let completion_tx = completion_tx.clone();
            let drain_requested = drain_requested.clone();
            let session_hook_cell = session_hook_cell.clone();
            let provider_info_cell = provider_info_cell.clone();

            async move {
                Self::agent_loop(
                    msg_rx,
                    completion_tx,
                    state_manager_for_agent,
                    agent,
                    config_for_agent,
                    drain_requested,
                    event_receiver,
                    session_hook_cell,
                    provider_info_cell,
                    rebuild_ctx,
                    compaction_model,
                )
                .await;
            }
        });

        // Wait for event loop to exit (View closed)
        event_handle.await.ok();
        agent_handle.abort();
        // Drop the bg notify sender so the bridge wakes and exits.
        if let Some(sm) = self.state_manager.as_ref() {
            sm.detach_bg_notify();
            // Kill any still-running bg processes before tearing down.
            sm.clear_bg();
        }
        bg_bridge_handle.abort();
    }

    /// Event loop - receives UiActions from View, queues messages for agent loop.
    ///
    /// **Single-writer invariant** (see `make-flow-great-again.md`): this loop
    /// **never** calls `add_user_message{,_with_attachments}`. User-typed text
    /// only enters `state.chat.messages` from the agent loop, at dequeue time,
    /// immediately before its turn fires. That structurally prevents user-text
    /// from wedging between an in-flight `ToolCall` and its `ToolResult`.
    ///
    /// Typing during a busy turn is therefore a queued **follow-up**, not an
    /// interrupt. The explicit interrupt is `/stop` (or `Esc` →
    /// `UiAction::RequestStop`), which sets the drain flag, sends a
    /// `StopMarker`, and the agent loop discards every queued message until
    /// the marker is seen. /stop means stop.
    #[allow(clippy::too_many_arguments)] // event_loop coordinates many handles; refactoring loses clarity
    async fn event_loop(
        mut action_receiver: mpsc::UnboundedReceiver<UiAction>,
        msg_tx: tokio::sync::mpsc::Sender<QueueMessage>,
        _completion_tx: tokio::sync::broadcast::Sender<CompletionResult>,
        state_manager: Option<Arc<StateManager>>,
        session_hook_cell: SharedSessionHook,
        config_model: String,
        provider_info_cell: SharedProviderInfo,
        drain_requested: Arc<std::sync::atomic::AtomicBool>,
    ) {
        use std::sync::atomic::Ordering;

        // Helper: trigger a full /stop. Sets the drain flag, signals the
        // running agent to cancel, sends StopMarker, and zeroes the queued
        // counter so the status bar updates immediately. Reads the
        // *currently active* SessionHook through the shared cell so it
        // always cancels the live prompt — not a stale predecessor from
        // before a `/model` switch.
        let request_stop_and_drain =
            |state_manager: &Option<Arc<StateManager>>,
             session_hook_cell: &SharedSessionHook,
             msg_tx: &tokio::sync::mpsc::Sender<QueueMessage>,
             drain_requested: &Arc<std::sync::atomic::AtomicBool>| {
                let sm = state_manager.clone();
                // Clone the inner Arc out of the read guard before any
                // await — we never want the guard to span an await point.
                let session_hook = session_hook_cell.read().unwrap().clone();
                let msg_tx = msg_tx.clone();
                let drain_requested = drain_requested.clone();
                async move {
                    if !sm.as_ref().is_some_and(|sm| sm.is_running()) {
                        return;
                    }
                    drain_requested.store(true, Ordering::Release);
                    session_hook.request_stop();
                    if let Some(ref sm) = sm {
                        sm.set_pending_input_count(0);
                        sm.set_status(Some("Stop requested...".to_string()));
                    }
                    msg_tx.send(QueueMessage::StopMarker).await.ok();
                }
            };
        // Ensure a boot conversation exists. Idempotent: `create_session`
        // already minted (fresh) or loaded (resume) one synchronously before
        // spawning this loop, so this only fires for callers that bypass the
        // factory (test harnesses). The active wire identity (provider_name +
        // model) was stamped on the StateManager before run_loop; we read it
        // back so saved conversations carry the right re-activation key.
        // `cwd` is the SM-owned session cwd (set in `create_session` before
        // run_loop is spawned; for the test-harness bypass path the SM
        // seeds it from `current_dir()` at construction).
        if let Some(ref sm) = state_manager {
            sm.ensure_boot_conversation(&sm.session_cwd(), &config_model);
        }

        while let Some(action) = action_receiver.recv().await {
            match action {
                UiAction::SendMessage(msg) => {
                    // Route by shape of the text. Slash commands must NOT
                    // be sent to the LLM, and must NOT be appended as user
                    // messages — their handlers emit their own system
                    // output. See `classify_submission` docs.
                    match classify_submission(&msg) {
                        SubmitKind::StopCommand => {
                            request_stop_and_drain(
                                &state_manager,
                                &session_hook_cell,
                                &msg_tx,
                                &drain_requested,
                            )
                            .await;
                        }
                        SubmitKind::Command(cmd) => {
                            // Dispatched by agent_loop via process_command_internal.
                            msg_tx.send(QueueMessage::Command(cmd)).await.ok();
                        }
                        SubmitKind::UserMessage(text) => {
                            // Single-writer invariant: do NOT call
                            // add_user_message here. The agent loop appends
                            // user input to chat at dequeue time, between
                            // turns, where it can never wedge between a
                            // ToolCall and its ToolResult.
                            if let Some(ref sm) = state_manager {
                                sm.increment_pending_input();
                            }
                            msg_tx
                                .send(QueueMessage::UserMessage {
                                    text,
                                    attachments: Vec::new(),
                                })
                                .await
                                .ok();
                        }
                        SubmitKind::MultimodalMessage { text, attachments } => {
                            // Capability guardrail — fail loud rather than drop images silently.
                            // Read the active provider_info through the
                            // shared cell so a `/model` switch to a
                            // vision-capable model takes effect
                            // immediately without process restart.
                            let pi = provider_info_cell.read().unwrap().clone();
                            if !pi.supports_vision {
                                if let Some(ref sm) = state_manager {
                                    sm.add_system_message(format!(
                                        "❌ Model `{}` does not support vision. Switch to a \
                                         vision-capable model in config.yaml (e.g. \
                                         `anthropic/claude-3.5-sonnet`, `gpt-4o`, \
                                         `google/gemini-2.0-flash-001`).",
                                        pi.model
                                    ));
                                }
                                continue;
                            }
                            // Single-writer invariant: attachments travel
                            // through the channel; the agent loop is the
                            // sole writer of `add_user_message_with_attachments`.
                            if let Some(ref sm) = state_manager {
                                sm.increment_pending_input();
                            }
                            msg_tx
                                .send(QueueMessage::UserMessage { text, attachments })
                                .await
                                .ok();
                        }
                        SubmitKind::InvalidAttachment(e) => {
                            if let Some(ref sm) = state_manager {
                                sm.add_system_message(format!("❌ {e}"));
                            }
                            // Do not enqueue — the model is never called.
                        }
                    }
                }

                UiAction::RequestStop => {
                    // Esc key — same shape as /stop. Stop means stop, queue is dropped.
                    request_stop_and_drain(
                        &state_manager,
                        &session_hook_cell,
                        &msg_tx,
                        &drain_requested,
                    )
                    .await;
                }

                UiAction::SwitchModel(alias) => {
                    // Forward the validated alias to the agent loop. The
                    // agent loop is the single owner of the agent handle,
                    // so the rebuild has to happen there. Counts as a
                    // pending input so the status bar shows activity until
                    // the new agent is up. (`decrement_pending_input` is
                    // called after the rebuild completes.)
                    if let Some(ref sm) = state_manager {
                        sm.increment_pending_input();
                    }
                    msg_tx.send(QueueMessage::SwitchModel(alias)).await.ok();
                }

                UiAction::ChangeCwd(path) => {
                    // Same shape as SwitchModel — forward the validated
                    // path to the agent loop (sole owner of the agent
                    // handle) and count it as pending input so the status
                    // bar shows activity until the rebuild completes.
                    if let Some(ref sm) = state_manager {
                        sm.increment_pending_input();
                    }
                    msg_tx.send(QueueMessage::ChangeCwd(path)).await.ok();
                }
            }
        }
    }

    /// Agent loop - processes messages from event loop, sends completions back.
    ///
    /// **Single-writer for user input.** This loop is the *only* place that
    /// calls `add_user_message{,_with_attachments}` — see
    /// `make-flow-great-again.md`. The write happens at dequeue time, before
    /// the turn is built and sent to the model, so the message lands strictly
    /// between agent turns. This makes it structurally impossible for
    /// user-typed text to wedge between an in-flight `ToolCall` and its
    /// `ToolResult`.
    ///
    /// **Drain on /stop.** When `drain_requested` is set, all queued
    /// `UserMessage`/`Command` items are discarded until the matching
    /// `StopMarker` is consumed; the flag is then cleared. /stop = stop,
    /// queued follow-ups are dropped along with the in-flight turn.
    #[allow(clippy::too_many_arguments)] // agent_loop owns rebuild + event-processor lifecycle
    async fn agent_loop(
        msg_rx: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<QueueMessage>>,
        completion_tx: tokio::sync::broadcast::Sender<CompletionResult>,
        state_manager: Option<Arc<StateManager>>,
        initial_agent: Arc<DynAgent>,
        mut config: Config,
        drain_requested: Arc<std::sync::atomic::AtomicBool>,
        initial_event_receiver: Option<mpsc::UnboundedReceiver<AgentEvent>>,
        session_hook_cell: SharedSessionHook,
        provider_info_cell: SharedProviderInfo,
        mut rebuild_ctx: Option<RebuildContext>,
        compaction_model: Option<Arc<CompactionModel>>,
    ) {
        use std::sync::atomic::Ordering;

        // The agent reference is `mut` here — `/model` rebuilds it
        // in-place between turns. `provider_info_cell` and
        // `session_hook_cell` are mirrored into the live event_loop.
        let mut agent = initial_agent;

        // Memory compaction is deferred to the first user message so
        // startup stays instant and the spinner provides visual feedback.
        let mut memory_compaction_done = false;

        // The event-processor task pulls AgentEvents off the
        // receiver and forwards them into StateManager. It used to
        // live in `run_loop` but now lives here so we can tear it
        // down and respawn it whenever a `/model` rebuild produces a
        // fresh receiver. The handle is `Some` while a processor is
        // active.
        let mut event_processor: Option<tokio::task::JoinHandle<()>> =
            initial_event_receiver.map(|rx| {
                let sm = state_manager.clone();
                tokio::spawn(async move {
                    let mut rx = rx;
                    while let Some(event) = rx.recv().await {
                        Self::process_event_for_ui(&sm, event);
                    }
                })
            });

        loop {
            // Wait for a message
            let msg = msg_rx.lock().await.recv().await;

            // Drain mode: discard everything that isn't the StopMarker. The
            // event loop has already zeroed the pending counter and signalled
            // the running prompt to cancel; our job is just to throw out the
            // queue contents until the marker arrives.
            if drain_requested.load(Ordering::Acquire) {
                match msg {
                    Some(QueueMessage::StopMarker) => {
                        drain_requested.store(false, Ordering::Release);
                        if let Some(ref sm) = state_manager {
                            sm.set_status(None);
                            sm.add_system_message("Agent stopped by user".to_string());
                        }
                        completion_tx.send(CompletionResult::Stopped).ok();
                        continue;
                    }
                    Some(QueueMessage::UserMessage { .. })
                    | Some(QueueMessage::Command(_))
                    | Some(QueueMessage::SwitchModel(_))
                    | Some(QueueMessage::ChangeCwd(_))
                    | Some(QueueMessage::BackgroundOutputReady) => {
                        // Discarded — pending counter was already zeroed by
                        // the event loop's drain trigger. (SwitchModel is
                        // included for safety; in practice the View only
                        // emits it between turns when nothing is running.)
                        //
                        // `BackgroundOutputReady` is discarded too: /stop ==
                        // stop, *including* auto-injection. Buffers
                        // themselves keep filling inside the registry (the
                        // reader threads don't know about /stop); when a
                        // new ping arrives after drain mode clears, the
                        // accumulated lines flush in a single synthetic
                        // turn. Per Q3: /stop suppresses synthetic turns,
                        // unlimited-tier processes are functionally paused
                        // (still running, just not flushed).
                        continue;
                    }
                    None => break,
                }
            }

            match msg {
                Some(QueueMessage::UserMessage { text, attachments }) => {
                    // Single-writer point: append to chat *now*, between turns.
                    if let Some(ref sm) = state_manager {
                        if attachments.is_empty() {
                            sm.add_user_message(text.clone());
                        } else {
                            sm.add_user_message_with_attachments(text.clone(), attachments);
                        }
                        sm.decrement_pending_input();
                        // A real human turn clears all bg cooldowns, so any
                        // buffered background output flushes on the next drain
                        // alongside the user's message.
                        sm.reset_bg_cooldowns();
                        sm.set_running(true);
                    }

                    // Lazy memory compaction: run once on the first user
                    // message so startup stays instant and the spinner gives
                    // visual feedback while we work.
                    if !memory_compaction_done && config.memory.enabled {
                        memory_compaction_done = true;
                        if let (Some(sm), Some(model)) =
                            (state_manager.as_ref(), compaction_model.as_ref())
                        {
                            let path = std::path::Path::new("memory.md");
                            if let Some(content) = crate::memory_compaction::read_if_oversized(
                                path,
                                config.memory.threshold_bytes,
                            ) {
                                sm.set_status(Some("Compacting memory.md...".to_string()));
                                let size_before = content.len();
                                match crate::memory_compaction::compact_memory(&content, model)
                                    .await
                                {
                                    Ok(compacted) => {
                                        if let Err(e) = std::fs::write(path, compacted) {
                                            tracing::warn!(
                                                "Failed to write compacted memory.md: {}",
                                                e
                                            );
                                        } else {
                                            sm.add_system_message(format!(
                                                "memory.md compacted (was {} bytes)",
                                                size_before
                                            ));
                                            tracing::info!(
                                                "memory.md compacted: {} -> {} bytes",
                                                size_before,
                                                std::fs::metadata(path)
                                                    .map(|m| m.len() as usize)
                                                    .unwrap_or(0)
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("Memory compaction failed: {}", e);
                                    }
                                }
                                sm.set_status(None);
                            }
                        }
                    }

                    // Build the current-turn `Message` — attachments (if any)
                    // are read from state, so both text and vision turns use
                    // the same dispatch path.
                    let current_turn = state_manager
                        .as_ref()
                        .and_then(|sm| sm.build_current_turn_message())
                        .unwrap_or_else(|| {
                            // Fallback: no state manager (test-only paths) —
                            // pass the String through as a text-only Message.
                            rig_core::completion::message::Message::from(text.as_str())
                        });

                    let result = Self::process_message_internal(
                        current_turn,
                        &state_manager,
                        &agent,
                        &config,
                    )
                    .await;

                    // Mark as done — snapshot run_started_at BEFORE set_running(false) clears
                    // it, then emit a "worked for MM:SS" system message (reuses the spinner
                    // formatter so the post-run figure matches the live indicator).
                    if let Some(ref sm) = state_manager {
                        let started_at = sm.get_state().run_started_at;
                        sm.set_running(false);
                        if let Some(t) = started_at {
                            sm.add_system_message(format!(
                                "worked for {}",
                                crate::ui::repl::spinner::fmt_elapsed(t)
                            ));
                        }
                    }

                    // Send completion notification
                    completion_tx.send(result).ok();

                    // Post-turn bg drain seam: if any background process
                    // produced output during this turn (or while we were
                    // parked), drain it into a synthetic user turn and
                    // immediately run another iteration. See
                    // `bash-background.md` § "Wiring into the agent loop".
                    Self::run_bg_synthetic_turn_if_any(
                        &state_manager,
                        &agent,
                        &config,
                        &completion_tx,
                    )
                    .await;
                }

                Some(QueueMessage::BackgroundOutputReady) => {
                    // Wake-up triggered by a reader thread. Same drain
                    // path as the post-turn seam — extracted into a
                    // helper so the contract is identical.
                    Self::run_bg_synthetic_turn_if_any(
                        &state_manager,
                        &agent,
                        &config,
                        &completion_tx,
                    )
                    .await;
                }

                Some(QueueMessage::Command(cmd)) => {
                    if let Some(ref sm) = state_manager {
                        sm.set_running(true);
                    }
                    Self::process_command_internal(&cmd, &state_manager, &config).await;

                    // `/load <arg>` may have just swapped the active
                    // conversation to one saved against a different
                    // wire identity. Rebuild the agent if so — same
                    // path `/model <alias>` uses. Falls through
                    // (no-op) when the wire id matches the running
                    // agent or `/load` failed validation.
                    let lcmd = cmd.trim().to_ascii_lowercase();
                    if lcmd.starts_with("/load ") {
                        Self::maybe_rebuild_after_load(
                            &mut agent,
                            &mut config,
                            &state_manager,
                            &session_hook_cell,
                            &provider_info_cell,
                            &mut event_processor,
                            rebuild_ctx.as_mut(),
                        )
                        .await;
                    }
                    if let Some(ref sm) = state_manager {
                        sm.set_running(false);
                    }
                    completion_tx.send(CompletionResult::CommandDone).ok();
                }

                Some(QueueMessage::StopMarker) => {
                    // StopMarker outside drain mode — defensive: shouldn't
                    // happen because the event loop always sets drain_requested
                    // before sending. Treat as a benign acknowledgement.
                    if let Some(ref sm) = state_manager {
                        sm.set_status(None);
                        sm.add_system_message("Agent stopped by user".to_string());
                    }
                    completion_tx.send(CompletionResult::Stopped).ok();
                }

                Some(QueueMessage::SwitchModel(alias)) => {
                    // Per the locked plan in `multi-model.md`: `/model`
                    // is `/new` + boot the agent against a different
                    // ProviderConfig. The View has already validated
                    // the alias before sending the `UiAction`, so we
                    // trust it here — but we still re-resolve in case
                    // the config has been hot-reloaded under us, and
                    // emit a clean error if so.
                    if let Some(ref sm) = state_manager {
                        sm.set_running(true);
                    }
                    let outcome = Self::handle_switch_model(
                        &alias,
                        &mut agent,
                        &mut config,
                        &state_manager,
                        &session_hook_cell,
                        &provider_info_cell,
                        &mut event_processor,
                        rebuild_ctx.as_ref(),
                    )
                    .await;
                    if let Some(ref sm) = state_manager {
                        sm.decrement_pending_input();
                        sm.set_running(false);
                    }
                    match outcome {
                        Ok(()) => {
                            completion_tx.send(CompletionResult::CommandDone).ok();
                        }
                        Err(msg) => {
                            if let Some(ref sm) = state_manager {
                                sm.add_system_message(format!("❌ /model: {msg}"));
                            }
                            completion_tx.send(CompletionResult::CommandDone).ok();
                        }
                    }
                }

                Some(QueueMessage::ChangeCwd(path)) => {
                    // Same rebuild seam as SwitchModel, different axis:
                    // chdir + rebuild the system prompt + rebuild the
                    // agent on the *current* model, then reset the
                    // conversation like /new. The View already validated
                    // the path; we re-validate defensively.
                    if let Some(ref sm) = state_manager {
                        sm.set_running(true);
                    }
                    let outcome = Self::handle_change_cwd(
                        &path,
                        &mut agent,
                        &mut config,
                        &state_manager,
                        &session_hook_cell,
                        &provider_info_cell,
                        &mut event_processor,
                        rebuild_ctx.as_mut(),
                    )
                    .await;
                    if let Some(ref sm) = state_manager {
                        sm.decrement_pending_input();
                        sm.set_running(false);
                    }
                    if let Err(msg) = outcome
                        && let Some(ref sm) = state_manager
                    {
                        sm.add_system_message(format!("❌ /cd: {msg}"));
                    }
                    completion_tx.send(CompletionResult::CommandDone).ok();
                }

                None => {
                    // Channel closed, exit
                    if let Some(handle) = event_processor.take() {
                        handle.abort();
                    }
                    break;
                }
            }
        }
    }

    /// Rebuild the live `DynAgent` for a new model alias. Runs the
    /// same conversation-reset semantics as `/new`, then constructs a
    /// fresh `DynAgent` against the resolved provider config and
    /// publishes the new ProviderInfo + SessionHook through their
    /// shared cells. Restarts the event-processor task to consume
    /// from the new event channel.
    ///
    /// On error (unknown alias, build failure), returns a string
    /// describing the failure for the caller to surface as a system
    /// message; the previous agent is left intact.
    #[allow(clippy::too_many_arguments)]
    async fn handle_switch_model(
        alias: &str,
        agent_slot: &mut Arc<DynAgent>,
        config: &mut Config,
        state_manager: &Option<Arc<StateManager>>,
        session_hook_cell: &SharedSessionHook,
        provider_info_cell: &SharedProviderInfo,
        event_processor: &mut Option<tokio::task::JoinHandle<()>>,
        rebuild_ctx: Option<&RebuildContext>,
    ) -> Result<(), String> {
        let Some(ctx) = rebuild_ctx else {
            return Err(
                "model registry not configured (legacy single-provider boot — restart with a \
                 `providers:` block to enable model switching)"
                    .to_string(),
            );
        };
        let Some(resolved) = ctx.registry.resolve(alias) else {
            let available = ctx.registry.aliases_sorted().join(", ");
            return Err(format!("unknown alias `{alias}`. Available: {available}"));
        };
        let resolved = resolved.clone();

        let sm_for_provider = state_manager
            .clone()
            .ok_or_else(|| "state manager required for /model".to_string())?;

        // Rebuild the agent + publish new provider info — shared with
        // `/load` (which re-activates a saved wire identity).
        Self::rebuild_agent_for_resolved(
            &resolved,
            agent_slot,
            config,
            &sm_for_provider,
            session_hook_cell,
            provider_info_cell,
            event_processor,
            state_manager,
            ctx,
        )
        .await?;

        // Reset conversation-scoped state — same path as /new. Stamps
        // the new wire identity (provider_name + model) on the
        // metadata so /load on the freshly-reset convo (after the next
        // prompt) restores the right model. The alias is *also* set on
        // AppState for status-bar display, but is NOT persisted.
        sm_for_provider.reset_conversation_state();
        let convo_name = format!(
            "Conversation {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M")
        );
        sm_for_provider.create_conversation(
            convo_name,
            resolved.provider_name.clone(),
            resolved.model_name.clone(),
            sm_for_provider.session_cwd().to_string_lossy().into_owned(),
        );
        sm_for_provider.set_model(resolved.model_name.clone());
        sm_for_provider.set_provider_name(resolved.provider_name.clone());
        sm_for_provider.set_model_alias(resolved.alias.clone());

        // Announce the swap with a short banner so the user sees it
        // happened. Format mirrors the listing format from
        // process_command_internal::"/model".
        sm_for_provider.add_system_message(format!(
            "🔁 New conversation on {} ({} · {})",
            resolved.alias, resolved.provider_name, resolved.model_name
        ));

        Ok(())
    }

    /// Change the working directory and rebuild on a fresh conversation.
    /// The cwd sibling of [`Self::handle_switch_model`] — same reset +
    /// rebuild seam, different axis.
    ///
    /// Sequence (order matters):
    /// 1. `clear_bg()` first, so no `bash_bg` reader thread straddles the
    ///    session-cwd flip with a half-old, half-new view of the tree.
    /// 2. `state_manager.set_session_cwd(target)` — the load-bearing act.
    ///    Everything the prompt derives from the cwd and the
    ///    path-aware tools' `session_cwd` flip here, **without** mutating
    ///    the process-global cwd. Two web sessions in different trees
    ///    stay correct because nothing they share is mutated.
    /// 3. Rebuild the system prompt and store it back into
    ///    `ctx.system_prompt`, so a later `/model` switch keeps the new
    ///    cwd (the two axes are independent — ticket #124).
    /// 4. Rebuild the agent on the *current* model, then reset the
    ///    conversation like `/new`. Tools snapshot the new
    ///    `session_cwd` at agent-build time, so the rebuild order is
    ///    `set_session_cwd` first, rebuild second.
    ///
    /// The View pre-validated `path`; we re-validate defensively (config
    /// or the filesystem may have changed under us).
    #[allow(clippy::too_many_arguments)]
    async fn handle_change_cwd(
        path: &str,
        agent_slot: &mut Arc<DynAgent>,
        config: &mut Config,
        state_manager: &Option<Arc<StateManager>>,
        session_hook_cell: &SharedSessionHook,
        provider_info_cell: &SharedProviderInfo,
        event_processor: &mut Option<tokio::task::JoinHandle<()>>,
        rebuild_ctx: Option<&mut RebuildContext>,
    ) -> Result<(), String> {
        let Some(ctx) = rebuild_ctx else {
            return Err(
                "model registry not configured (legacy single-provider boot — restart with a \
                 `providers:` block to enable /cd)"
                    .to_string(),
            );
        };
        // Re-validate: the path must still be an existing directory.
        let target = std::path::Path::new(path);
        if !target.is_dir() {
            return Err(format!("not a directory: {path}"));
        }
        // Resolve the *current* model — /cd keeps the model, only the cwd
        // changes. Prefer the active alias; fall back to the wire id.
        let sm = state_manager
            .clone()
            .ok_or_else(|| "state manager required for /cd".to_string())?;
        let resolved = ctx
            .registry
            .resolve(&sm.get_model_alias())
            .or_else(|| {
                ctx.registry
                    .find_by_wire_id(&sm.get_provider_name(), &sm.get_model())
            })
            .ok_or_else(|| "current model not found in registry".to_string())?
            .clone();

        // Kill bg processes BEFORE the session-cwd flip so no reader
        // thread straddles it — they were rooted in the old tree anyway
        // (ticket #124).
        sm.clear_bg();

        // The load-bearing act. Per-session only — no `set_current_dir`,
        // so two web sessions in different trees stay race-free. Must run
        // BEFORE the agent rebuild below, because `add_builtin_tools`
        // snapshots `sm.session_cwd()` at build time.
        sm.set_session_cwd(target.to_path_buf());

        // Rebuild the prompt against the new cwd + refreshed agents.md and
        // persist it into ctx so subsequent /model switches keep the cwd.
        ctx.system_prompt = build_system_prompt(&ctx.skills, ctx.shell_kind.as_ref(), target);

        // Refresh the welcome banner's cwd so the status bar / web banner
        // reflect the new directory (welcome was stamped once at boot).
        sm.update_welcome_cwd(target.to_path_buf());

        Self::rebuild_agent_for_resolved(
            &resolved,
            agent_slot,
            config,
            &sm,
            session_hook_cell,
            provider_info_cell,
            event_processor,
            state_manager,
            ctx,
        )
        .await?;

        // Reset conversation-scoped state — same path as /new and /model.
        // bg was already killed above (before the chdir); the unified
        // reset's clear_bg is a no-op on the now-empty registry.
        // `create_conversation` persists the new cwd into the metadata
        // so /load re-activates this tree.
        sm.reset_conversation_state();
        let convo_name = format!(
            "Conversation {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M")
        );
        sm.create_conversation(
            convo_name,
            resolved.provider_name.clone(),
            resolved.model_name.clone(),
            target.to_string_lossy().into_owned(),
        );
        sm.set_model(resolved.model_name.clone());
        sm.set_provider_name(resolved.provider_name.clone());
        sm.set_model_alias(resolved.alias.clone());

        sm.add_system_message(format!("📁 New conversation in {path}"));

        Ok(())
    }

    /// Rebuild the live `DynAgent` against a `ResolvedModel` — the
    /// **shared seam** between `/model <alias>` and `/load`. Builds
    /// the agent, publishes the new `ProviderInfo` + `SessionHook`
    /// through their shared cells, swaps the active config provider,
    /// re-inits the context manager, and restarts the event-processor
    /// task on the new receiver.
    ///
    /// Does NOT reset the conversation (chat/stats/todos) and does NOT
    /// emit a system banner — those are caller-specific:
    /// - `/model` calls `create_conversation(...)` afterwards (fresh
    ///   chat) and emits `🔁 New conversation on ...`.
    /// - `/load` calls `load_conversation(...)` afterwards (restore
    ///   saved chat) and emits `Loaded conversation: '...'`.
    ///
    /// *(simplicity is the key — switching models is one operation
    /// regardless of trigger)*
    #[allow(clippy::too_many_arguments)]
    async fn rebuild_agent_for_resolved(
        resolved: &ResolvedModel,
        agent_slot: &mut Arc<DynAgent>,
        config: &mut Config,
        sm_for_provider: &Arc<StateManager>,
        session_hook_cell: &SharedSessionHook,
        provider_info_cell: &SharedProviderInfo,
        event_processor: &mut Option<tokio::task::JoinHandle<()>>,
        state_manager: &Option<Arc<StateManager>>,
        ctx: &RebuildContext,
    ) -> Result<(), String> {
        // Validate compaction model *before* mutating any state. When
        // compaction is enabled, a construction failure is fatal for the
        // switch — bail early with an honest error rather than silently
        // degrading.  See unfuck-compact.md Layer 1.
        let context_size = resolved.context_size;
        let compaction_model = if config.context.enabled {
            let model = crate::providers::create_compaction_model(
                &resolved.provider_config,
                config.context.compaction_model.as_deref(),
            )
            .map_err(|e| {
                format!(
                    "compaction model unavailable for `{}`: {e} — \
                     set context.enabled=false to switch without compaction",
                    resolved.alias
                )
            })?;
            Some(Arc::new(model))
        } else {
            crate::providers::create_compaction_model(
                &resolved.provider_config,
                config.context.compaction_model.as_deref(),
            )
            .ok()
            .map(Arc::new)
        };

        // Rebuild MCP tool list from the long-lived handles. McpTool
        // implements Clone (rig 0.33), so we get a fresh Vec without
        // restarting any subprocess. See agents.md / multi-model.md.
        let mcp_tools: Option<Vec<Box<dyn rig_core::tool::ToolDyn>>> = if ctx.mcp_handles.is_empty()
        {
            None
        } else {
            let mut all = Vec::new();
            for h in ctx.mcp_handles.iter() {
                for t in h.tools().iter().cloned() {
                    all.push(Box::new(t) as Box<dyn rig_core::tool::ToolDyn>);
                }
            }
            Some(all)
        };

        let (new_agent, new_info, new_receiver, new_hook) = crate::providers::create_provider(
            &resolved.provider_config,
            mcp_tools,
            &ctx.system_prompt,
            ctx.searxng_config.as_ref(),
            ctx.max_turns,
            ctx.todo_tool.clone(),
            &ctx.bash_config,
            ctx.pipeline_registry.as_deref(),
            sm_for_provider.clone(),
            ctx.shell_kind.as_ref(),
            ctx.vector_store.as_ref(),
        )
        .map_err(|e| format!("failed to build agent for `{}`: {e}", resolved.alias))?;

        // Publish through the shared cells *first* so the event_loop's
        // /stop and vision-gate paths immediately see the new state.
        {
            let mut hook_guard = session_hook_cell.write().unwrap();
            *hook_guard = new_hook;
        }
        {
            let mut pi_guard = provider_info_cell.write().unwrap();
            *pi_guard = Arc::new(new_info.clone());
        }
        *agent_slot = Arc::new(new_agent);

        // Update Config so subsequent reads of provider/model see the
        // new values. We swap the active provider; everything else
        // (mcp_servers, searxng, context, retry, …) stays put.
        config.provider = resolved.provider_config.clone();

        // Keep the welcome banner's model-scoped fields in sync so live
        // Views (web banner) show the model just switched to, not the
        // boot model. TUI's banner is a static string — unaffected.
        sm_for_provider.update_welcome_for_model(
            resolved.provider_name.clone(),
            resolved.model_name.clone(),
            config.max_tokens() as usize,
        );

        // Re-init the context manager with the pre-validated compaction
        // model so compaction thresholds match the new context window.
        let cm = ContextManager::new(
            config.context.clone(),
            context_size,
            compaction_model.clone(),
        );
        sm_for_provider.init_context_manager(cm);
        // The title model is the same as the compaction model — reuse the Arc
        if let Some(model) = compaction_model {
            sm_for_provider.init_title_model(model);
        }

        // Restart the event-processor task on the new receiver. The
        // receiver is `Option` because not every provider supports
        // event hooks (Ollama returns `None`); skip the spawn in that
        // case and we'll just have no event-driven UI updates.
        if let Some(handle) = event_processor.take() {
            handle.abort();
        }
        if let Some(rx) = new_receiver {
            let sm_for_processor = state_manager.clone();
            *event_processor = Some(tokio::spawn(async move {
                let mut rx = rx;
                while let Some(event) = rx.recv().await {
                    Self::process_event_for_ui(&sm_for_processor, event);
                }
            }));
        }

        Ok(())
    }

    /// After `/load` has potentially swapped the active conversation
    /// to one saved against a different wire identity, rebuild the
    /// agent so the next prompt actually goes to the saved model.
    /// No-op when:
    /// - the running agent is already on the loaded conversation's
    ///   wire id (alias may differ — that's fine),
    /// - the registry isn't available,
    /// - the loaded conversation's wire id isn't in the registry.
    ///
    /// The error path doesn't emit a system message: `/load` itself
    /// has already either rejected unavailability or accepted the load,
    /// and a no-op rebuild is the silent normal case.
    #[allow(clippy::too_many_arguments)]
    async fn maybe_rebuild_after_load(
        agent_slot: &mut Arc<DynAgent>,
        config: &mut Config,
        state_manager: &Option<Arc<StateManager>>,
        session_hook_cell: &SharedSessionHook,
        provider_info_cell: &SharedProviderInfo,
        event_processor: &mut Option<tokio::task::JoinHandle<()>>,
        rebuild_ctx: Option<&mut RebuildContext>,
    ) {
        let Some(sm) = state_manager else { return };
        let Some(ctx) = rebuild_ctx else { return };

        let Some(conv) = sm.get_current_conversation() else {
            return;
        };
        let saved_provider = conv.provider_name.clone();
        let saved_model = conv.model.clone();
        let saved_cwd = conv.cwd.clone();

        let wire_id_changed =
            sm.get_provider_name() != saved_provider || sm.get_model() != saved_model;

        // Restore the saved cwd best-effort: only if it's non-empty (pre-cwd
        // files default to ""), still exists, and actually differs from the
        // current session cwd. The comparison is against `sm.session_cwd()`
        // (the per-session source of truth), not the process-global cwd —
        // every change is per-session. A gone path is warned about, not
        // fatal — the load already succeeded.
        let current_session_cwd = sm.session_cwd();
        let mut cwd_changed = false;
        if !saved_cwd.is_empty() {
            let target = std::path::PathBuf::from(&saved_cwd);
            if target.is_dir() {
                if target != current_session_cwd {
                    // Kill bg processes rooted in the previous tree, then
                    // refresh the prompt for the restored cwd/agents.md.
                    // Per-session only — no `set_current_dir`, so the
                    // process-global cwd is untouched and concurrent web
                    // sessions in other trees stay race-free.
                    sm.clear_bg();
                    sm.set_session_cwd(target.clone());
                    ctx.system_prompt =
                        build_system_prompt(&ctx.skills, ctx.shell_kind.as_ref(), &target);
                    sm.update_welcome_cwd(target);
                    cwd_changed = true;
                }
                // else: already on the right tree — no work, no rebuild.
            } else {
                sm.add_system_message(format!(
                    "⚠ /load: saved working directory no longer exists: {saved_cwd}"
                ));
            }
        }
        // An empty `saved_cwd` (from a pre-cwd conversation file) keeps
        // the SM's current session_cwd — it is the authoritative value
        // for that conversation.

        // Nothing to rebuild if neither axis moved.
        if !wire_id_changed && !cwd_changed {
            return;
        }
        let Some(resolved) = ctx.registry.find_by_wire_id(&saved_provider, &saved_model) else {
            // /load already emitted the unavailability error — and yet
            // somehow the slot got swapped. Defensive: do nothing.
            return;
        };
        let resolved = resolved.clone();
        let sm_for_provider = sm.clone();
        if let Err(e) = Self::rebuild_agent_for_resolved(
            &resolved,
            agent_slot,
            config,
            &sm_for_provider,
            session_hook_cell,
            provider_info_cell,
            event_processor,
            state_manager,
            ctx,
        )
        .await
        {
            sm.add_system_message(format!("❌ /load: failed to rebuild agent: {e}"));
            return;
        }
        // Stamp display state to match the freshly-rebuilt agent.
        sm.set_provider_name(resolved.provider_name);
        sm.set_model(resolved.model_name);
        sm.set_model_alias(resolved.alias);
    }
    /// Process an AgentEvent and update StateManager accordingly.
    ///
    /// This is the Controller's responsibility — it decides how domain events
    /// affect the UI state. The Model (StateManager) is passive and only holds data.
    ///
    /// Note: This now persists ALL messages (including tool calls/results) to
    /// StateManager, which serves as the single source of truth for persistence.
    fn process_event_for_ui(state_manager: &Option<Arc<StateManager>>, event: AgentEvent) {
        let sm = match state_manager {
            Some(sm) => sm,
            None => return,
        };

        match event {
            AgentEvent::CompletionResponse { usage, .. } => {
                // Update stats in StateManager (single source of truth)
                sm.add_request(usage.input_tokens, usage.output_tokens, usage.cost);
            }
            AgentEvent::ToolCall {
                tool_name,
                arguments,
                call_id,
                ..
            } => {
                // Indicator phase → show the tool name in the working banner
                // (see `workin-baby.md` §6). Cleared on the matching result.
                sm.set_status(Some(tool_name.clone()));
                // Add to chat AND persist (StateManager handles persistence)
                sm.add_tool_call(tool_name, arguments, call_id);
            }
            AgentEvent::ToolResult {
                tool_name,
                arguments,
                result,
                call_id,
                ..
            } => {
                // Back to "thinking" — the model is about to reason again.
                sm.set_status(None);
                // Add to chat AND persist (StateManager handles persistence)
                sm.add_tool_result(tool_name, arguments, result, call_id);
            }
            AgentEvent::CompletionRequest { .. }
            | AgentEvent::SessionStart { .. }
            | AgentEvent::SessionEnd { .. } => {
                // No UI update needed for these events
            }
        }
    }

    /// Internal process_message — takes the already-built current-turn `Message`
    /// (text or multimodal) and history from `StateManager`, runs the agent,
    /// writes the response back to state. Retries on transient errors.
    ///
    /// **In-loop compaction (`mid-compaction.md`).** When the wired
    /// `SessionHook` detects that the imminent wire payload would breach
    /// the context-window threshold, it terminates rig's agentic loop with
    /// reason `"compact"`. We catch that here, run `force_compact().await`
    /// synchronously, rebuild the `current_turn` from the (now compacted)
    /// `StateManager`, and re-enter `prompt_with_history`. The loop guard
    /// against infinite terminate-restart cycles is in
    /// `apply_compaction` — see that fn's doc and `SessionStats::clear_last_input_tokens`.
    async fn process_message_internal(
        mut current_turn: rig_core::completion::message::Message,
        state_manager: &Option<Arc<StateManager>>,
        agent: &Arc<DynAgent>,
        config: &Config,
    ) -> CompletionResult {
        let mut retry_count = 0;
        // One-shot override for the post-compaction iteration. The compact
        // arm fills this with the resumption-shape history returned by
        // `build_resumption_for_compaction()`; the loop top consumes it
        // via `.take()` on the very next iteration so the resumption
        // payload reaches the wire intact. Without this override, the
        // top-of-loop `get_agent_history()` re-derives history from
        // StateManager and DUPLICATES the resumption message (which is
        // also `current_turn`), breaking Anthropic / OpenAI conversation
        // invariants. See the data-layer pin
        // `production_resumption_payload_must_not_duplicate_toolresult`.
        let mut history_override: Option<Vec<rig_core::completion::message::Message>> = None;

        loop {
            // Compaction is handled at the wire boundary by SessionHook
            // (gate in `on_completion_call`). The handler below catches
            // the resulting Terminate("compact") and rebuilds state.

            // Call the agent with history from StateManager (single source
            // of truth), unless the previous iteration set a one-shot
            // override (the post-compaction case — see `history_override`
            // above).
            let mut history = derive_history_for_iteration(&mut history_override, state_manager);
            let result = agent
                .as_ref()
                .prompt_with_history(current_turn.clone(), &mut history)
                .await;

            match result {
                Ok(response) => {
                    // Add assistant response to chat AND persist (StateManager handles persistence)
                    if let Some(sm) = state_manager.as_ref() {
                        sm.add_assistant_message(response.clone());
                        sm.set_final_broadcast(true);
                        // Fire-and-forget: generate title after first reply
                        sm.maybe_generate_title();
                    }

                    return CompletionResult::Success;
                }

                Err(PromptError::PromptCancelled { reason, .. }) if reason == "stop" => {
                    return CompletionResult::Stopped;
                }

                Err(PromptError::PromptCancelled { reason, .. }) if reason == "compact" => {
                    // The hook decided the imminent wire payload is over budget.
                    // Run compaction synchronously, rebuild the current turn from
                    // the post-compaction StateManager, and re-enter the loop.
                    let Some(sm) = state_manager.as_ref() else {
                        // No StateManager — we can't compact. This shouldn't
                        // happen in production (the hook's gate only fires
                        // when one is wired), but bail loudly if it does.
                        return CompletionResult::Error;
                    };

                    sm.set_status(Some("Compacting context...".to_string()));
                    let compacted = sm.force_compact().await;
                    sm.set_status(None);

                    let Some(result) = compacted else {
                        // Compaction failed — abort the turn with an honest
                        // error. No retry, no clear_last_input_tokens dance.
                        // The next user turn gets a fresh attempt.
                        // See unfuck-compact.md Layer 2.
                        sm.add_system_message(
                            "❌ Compaction failed — the conversation could not be summarised. \
                             Aborting this turn. Try a new conversation, or check your \
                             compaction model configuration."
                                .to_string(),
                        );
                        sm.set_final_broadcast(true);
                        return CompletionResult::Error;
                    };

                    sm.add_system_message(format!(
                        "Context compacted: {} → {} messages, {} compacted",
                        result.original_count, result.compacted_count, result.num_discarded
                    ));

                    // Build the resumption prompt and history from the post-compaction
                    // StateManager. Unlike the initial dispatch path (which always
                    // uses the last User as the prompt), mid-action resumption
                    // must continue from whatever message the hook terminated
                    // with — which may be a ToolResult or Agent response, not
                    // necessarily a User. `build_resumption_for_compaction` handles
                    // this correctly by finding the actual last non-compacted
                    // message as the prompt and everything before it as history.
                    //
                    // If it returns None (empty state or fresh turn), fall back
                    // to the normal initial-dispatch path for safety.
                    if let Some((p, h)) = sm.build_resumption_for_compaction() {
                        current_turn = p;
                        // Stash the resumption-shape history in the loop-local
                        // one-shot override. The next iteration's top-of-loop
                        // will consume it via `.take()` instead of calling
                        // `get_agent_history()`. This is the load-bearing
                        // line — without it, the trailing non-User message
                        // (typically a ToolResult after a tool round-trip)
                        // would be duplicated on the wire because
                        // `get_agent_history()` only strips trailing User.
                        history_override = Some(h);
                    } else if let Some(turn) = sm.build_current_turn_message() {
                        current_turn = turn;
                    }

                    // Loop. The next prompt_with_history call sees the
                    // compacted history and the correct resumption prompt.
                    // The hook's needs_compaction() now
                    // reads false (apply_compaction cleared last_input_tokens,
                    // OR our explicit clear above did), so it does NOT
                    // re-terminate.
                    continue;
                }

                Err(PromptError::PromptCancelled { .. }) => {
                    // Any other cancellation reason — return error instead of
                    // silently looping (which previously caused an infinite loop).
                    return CompletionResult::Error;
                }

                Err(_e) => {
                    if retry_count == config.retry().max_retries {
                        if let Some(sm) = state_manager {
                            sm.set_status(None);
                            sm.add_system_message("❌ Max number of retries exceeded".to_string());
                        }
                        return CompletionResult::Error;
                    }
                    if let Some(sm) = state_manager {
                        sm.set_status(Some(format!("Retrying (attempt {})...", retry_count + 1)));
                    }
                    retry_count += 1;
                }
            }
        }
    }

    /// Resolve a user-typed conversation reference into a UUID.
    ///
    /// Accepts two forms:
    /// - **1-based ordinal index** (e.g. `1`, `2`, `42`) — looked up in the
    ///   current `list_conversations()` sorted newest-first. This is the
    ///   primary, human-friendly path. Indices are deliberately stateless:
    ///   re-derived on every call so there is no cached mapping to drift out
    ///   of sync with storage.
    /// - **Full UUID** — fallback for scripts, log copy-paste, and the rare
    ///   case where index addressing isn't enough.
    ///
    /// The two formats are syntactically disjoint (digits vs. hyphenated
    /// 36-char string), so the parse-int-first / parse-uuid-second strategy
    /// has no ambiguity. See `conversazione.md` § "The shared resolver" for
    /// the full design rationale.
    fn resolve_conversation_id(arg: &str, sm: &StateManager) -> Result<uuid::Uuid, String> {
        // Try integer first — common case, cheaper to parse, and fails fast
        // (no allocation) when the user typed a UUID.
        if let Ok(n) = arg.parse::<usize>() {
            if n == 0 {
                return Err(
                    "Indices are 1-based. Use /conversations to see available items.".to_string(),
                );
            }
            let mut list = sm
                .list_conversations()
                .ok_or_else(|| "Conversation storage is not configured.".to_string())?;
            list.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
            return list.get(n - 1).map(|c| c.id).ok_or_else(|| {
                format!(
                    "Index {} not found. /conversations shows {} saved conversation(s).",
                    n,
                    list.len()
                )
            });
        }
        // Fallback: full UUID.
        uuid::Uuid::parse_str(arg).map_err(|_| {
            "Invalid argument. Pass an index from /conversations or a full UUID.".to_string()
        })
    }

    /// Drain any pending background-process output and, if non-empty,
    /// inject it as a synthetic user turn and run an agent turn over
    /// it. No-op when buffers are empty OR the capped-tier circuit
    /// breaker is suppressing.
    ///
    /// Called by the agent loop in two places (the "two seams" of
    /// `bash-background.md`):
    /// 1. After a `UserMessage` turn completes (post-turn drain).
    /// 2. On `QueueMessage::BackgroundOutputReady` while the loop is
    ///    parked (idle wake-up).
    ///
    /// Both paths share the same drain + dispatch shape, so the helper
    /// guarantees they behave identically — the only difference is who
    /// triggers them.
    async fn run_bg_synthetic_turn_if_any(
        state_manager: &Option<Arc<StateManager>>,
        agent: &Arc<DynAgent>,
        config: &Config,
        completion_tx: &tokio::sync::broadcast::Sender<CompletionResult>,
    ) {
        let Some(sm) = state_manager.as_ref() else {
            return;
        };
        let Some(synthetic) = sm.drain_bg_output_into_synthetic_turn() else {
            return;
        };
        // Persist the synthetic message with the bg discriminator so
        // /load restores it with the right styling and proc-id
        // provenance.
        sm.add_user_message_from_background(synthetic.text.clone(), synthetic.proc_ids.clone());
        sm.set_running(true);
        let current_turn = sm.build_current_turn_message().unwrap_or_else(|| {
            rig_core::completion::message::Message::from(synthetic.text.as_str())
        });
        let result =
            Self::process_message_internal(current_turn, state_manager, agent, config).await;
        sm.set_running(false);
        // No "worked for …" system row here — bg-driven turns are
        // ambient by design; the user already saw the bg block.
        completion_tx.send(result).ok();
    }

    /// Internal process_command
    async fn process_command_internal(
        cmd: &str,
        state_manager: &Option<Arc<StateManager>>,
        config: &Config,
    ) {
        let cmd_lower = cmd.to_lowercase();

        match cmd_lower.as_str() {
            // /reset was previously in the no-op arm below alongside /stats,
            // /context, etc. with the comment "UI pulls this data". The fossil
            // assumption was that the UI would notice the slash command and
            // handle it itself — but nothing in the REPL does, so the command
            // was silently inert. Per the derived-view-invalidation rule in
            // memory.md (2026-04-24 /new bug), when a reset command is advertised
            // in the popup as "Reset session statistics" (see
            // `ui_trait::builtin_commands`), the handler must actually reset
            // something visible to the user. /reset zeros the counters and keeps
            // the conversation; /new is the one that clears chat history too.
            "/reset" => {
                if let Some(sm) = state_manager {
                    sm.reset_stats();
                    sm.add_system_message("Session statistics reset.".to_string());
                } else {
                    tracing::warn!("State manager not available for /reset command");
                }
            }
            "/stats" | "/context" => {
                // /stats and /context are correctly silent: the data they
                // expose is rendered ambiently in the status bar (see
                // `ui::repl::repl_impl`). Nothing to do here.
            }
            "/compact" => {
                // /compact is an ACTION, not a data-display command.
                // It was mistakenly grouped with /stats and /context above.
                if let Some(sm) = state_manager {
                    // Same status the auto/in-loop path emits, so the working
                    // spinner reads "Compacting context..." instead of falling
                    // back to the default "thinking" label.
                    sm.set_status(Some("Compacting context...".to_string()));
                    let outcome = sm.force_compact().await;
                    sm.set_status(None);
                    match outcome {
                        Some(result) => {
                            sm.add_system_message(format!(
                                "Context compacted: {} → {} messages, {} discarded",
                                result.original_count, result.compacted_count, result.num_discarded
                            ));
                        }
                        None => {
                            sm.add_system_message("Nothing to compact.".to_string());
                        }
                    }
                } else {
                    tracing::warn!("State manager not available for /compact command");
                }
            }
            "/conversations" => {
                // List saved conversations as a system message, addressed by
                // 1-based ordinal index. UUIDs are an implementation detail
                // and deliberately hidden from the user — the index is the
                // human-facing handle that pairs with /load <n>, /delete <n>,
                // and /export <n>. UUIDs are still accepted as a fallback in
                // those commands via `resolve_conversation_id`. See
                // `conversazione.md` for the full design.
                if let Some(sm) = state_manager {
                    match sm.list_conversations() {
                        None => sm.add_system_message(
                            "Conversation storage is not configured.".to_string(),
                        ),
                        Some(list) if list.is_empty() => {
                            sm.add_system_message("No saved conversations.".to_string())
                        }
                        Some(mut list) => {
                            list.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
                            let current_id = sm.get_current_conversation_id();
                            // Build the registry once for the whole listing
                            // so each row's availability tag is a cheap
                            // hash lookup. If no providers list is
                            // declared, registry is empty and *every*
                            // saved alias is treated as unknown — which
                            // is correct: legacy single-provider boot
                            // can't reactivate any specific saved model.
                            let registry = config.build_model_registry().ok();
                            let mut msg = format!("Saved conversations ({}):\n", list.len());
                            for (idx, c) in list.iter().enumerate() {
                                let n = idx + 1;
                                let marker = if Some(c.id) == current_id {
                                    "▶ "
                                } else {
                                    "   "
                                };
                                // Availability check resolves on the
                                // wire identity `(provider_name, model)`
                                // — the stable persistence key.
                                // Aliases are NOT consulted (they're
                                // mutable user handles).
                                let resolved = registry
                                    .as_ref()
                                    .and_then(|r| r.find_by_wire_id(&c.provider_name, &c.model));
                                let model_tag = match (resolved, &registry) {
                                    (Some(r), _) => {
                                        // Show the current alias as a
                                        // hint next to the wire id —
                                        // helps the user spot which
                                        // entry in their config will
                                        // re-activate.
                                        format!("{} · {}/{}", r.alias, c.provider_name, c.model)
                                    }
                                    (None, Some(_)) => {
                                        let display_id = if c.provider_name.is_empty() {
                                            // Pre-v5 file with no
                                            // provider_name on disk.
                                            format!("(pre-v5) {}", c.model)
                                        } else {
                                            format!("{}/{}", c.provider_name, c.model)
                                        };
                                        format!("{display_id} ⚠ unavailable")
                                    }
                                    (None, None) => {
                                        // No registry attached at all
                                        // (legacy single-provider
                                        // boot). Show the wire id as
                                        // the only honest handle.
                                        if c.provider_name.is_empty() {
                                            c.model.clone()
                                        } else {
                                            format!("{}/{}", c.provider_name, c.model)
                                        }
                                    }
                                };
                                msg.push_str(&format!(
                                    "{}{:>3}  {}  {} msgs  {}  {}\n",
                                    marker,
                                    n,
                                    c.name,
                                    c.message_count,
                                    c.updated_at
                                        .with_timezone(&chrono::Local)
                                        .format("%Y-%m-%d %H:%M"),
                                    model_tag,
                                ));
                            }
                            msg.push_str(
                                "\nUse /load <n> to resume, /delete <n> to remove. UUIDs also accepted.",
                            );
                            sm.add_system_message(msg);
                        }
                    }
                } else {
                    tracing::warn!("State manager not available for /conversations command");
                }
            }
            "/exit" => {
                // No-confirmation quit. Bypasses the Ctrl+C confirmation
                // dialog on purpose — /exit is an explicit request.
                //
                // Cannot flip `ReplUi::running` directly from here (we're
                // in the agent loop), so we set `AppState.exit_requested`
                // via StateManager. The view observes it each tick and
                // breaks its run loop. See `request_exit` docs.
                //
                // No system banner: the terminal is about to be cleared
                // by `ReplUi::shutdown` and any flash of text would be
                // pointless noise. If you *want* a visible "Goodbye",
                // add an `add_system_message` here — but honestly, the
                // cleanest exit is a silent one.
                if let Some(sm) = state_manager {
                    sm.request_exit();
                } else {
                    tracing::warn!("State manager not available for /exit command");
                }
            }
            "/bg" => {
                // Human-facing listing of background processes — mirrors
                // what `bash_bg list` returns to the model. See
                // `bash-background.md` open Q7.
                if let Some(sm) = state_manager {
                    let rows = sm.list_bg();
                    if rows.is_empty() {
                        sm.add_system_message("No background processes.".to_string());
                    } else {
                        let mut msg = format!("Background processes ({}):\n", rows.len());
                        for r in rows {
                            let cooldown = if r.cooldown.is_zero() {
                                "real-time".to_string()
                            } else {
                                format!("{}s cooldown", r.cooldown.as_secs())
                            };
                            let status = match r.status {
                                crate::bg_processes::BgStatus::Running { .. } => {
                                    "running".to_string()
                                }
                                crate::bg_processes::BgStatus::Exited { code, .. } => {
                                    format!("exited({code})")
                                }
                            };
                            let label = r
                                .label
                                .as_ref()
                                .map(|l| format!(" [{l}]"))
                                .unwrap_or_default();
                            msg.push_str(&format!(
                                "  #{} pid={} 🛰 {} · {} · {}/{} lines · {}{}\n",
                                r.id,
                                r.pid,
                                cooldown,
                                status,
                                r.buffer_len,
                                r.capture_cap,
                                r.command,
                                label,
                            ));
                        }
                        sm.add_system_message(msg);
                    }
                } else {
                    tracing::warn!("State manager not available for /bg command");
                }
            }
            "/help" => {
                // Derive the help text from `builtin_commands()` so the popup
                // menu, the dispatcher, and /help stay in lockstep.
                // See `allehailmenu.md` §9.
                if let Some(sm) = state_manager {
                    let mut msg = String::from("Available commands:\n");
                    for cmd in crate::ui::ui_trait::builtin_commands() {
                        let args = if cmd.takes_args { " <args>" } else { "" };
                        msg.push_str(&format!("  /{}{} — {}\n", cmd.name, args, cmd.description));
                    }
                    sm.add_system_message(msg);
                } else {
                    tracing::warn!("State manager not available for /help command");
                }
            }
            "/new" => {
                // Create new conversation via StateManager (single source of truth).
                //
                // `create_conversation` alone only swaps the current-conversation
                // slot; the derived views (chat, stats, todos, bg, bash panel)
                // survive. `reset_conversation_state` clears them all so /new
                // doesn't lie about starting fresh.
                if let Some(sm) = state_manager {
                    sm.reset_conversation_state();
                    let name = format!(
                        "Conversation {}",
                        chrono::Local::now().format("%Y-%m-%d %H:%M")
                    );
                    // Read the active wire identity from AppState — set
                    // at boot and refreshed on every `/model` switch, so
                    // it always matches the running agent. Falls back to
                    // the config wire id with empty provider for legacy
                    // single-provider boots that never stamped state.
                    let provider_name = sm.get_provider_name();
                    let model = {
                        let m = sm.get_model();
                        if m.is_empty() {
                            config.model().to_string()
                        } else {
                            m
                        }
                    };
                    sm.create_conversation(
                        name,
                        provider_name,
                        model,
                        sm.session_cwd().to_string_lossy().into_owned(),
                    );
                    sm.add_system_message("Started a new conversation.".to_string());
                } else {
                    tracing::warn!("State manager not available for /new command");
                }
            }
            "/save" => {
                // Explicit save via StateManager
                if let Some(sm) = state_manager {
                    sm.save_conversation();
                    if let Some(conv) = sm.get_current_conversation() {
                        sm.add_system_message(format!("Conversation saved: {}", conv.name));
                    }
                } else {
                    tracing::warn!("State manager not available for /save command");
                }
            }
            _ if cmd_lower.starts_with("/load ") => {
                if let Some(arg) = cmd.strip_prefix("/load ") {
                    let Some(sm) = state_manager else {
                        tracing::warn!("State manager not available for /load command");
                        return;
                    };
                    let id = match Self::resolve_conversation_id(arg.trim(), sm) {
                        Ok(id) => id,
                        Err(msg) => {
                            sm.add_system_message(format!("❌ {}", msg));
                            return;
                        }
                    };

                    // Pre-flight wire-id check: peek at the saved
                    // file's `(provider_name, model)` BEFORE any
                    // teardown so the current conversation survives a
                    // failed load. The wire id is the **stable**
                    // re-activation key — aliases are mutable user
                    // handles in `config.yaml` and are NEVER consulted
                    // here. *(persisted artifacts must carry every
                    // field needed to be re-activated)*
                    let (saved_provider, saved_model) = match sm.peek_conversation_wire_id(id) {
                        Ok(t) => t,
                        Err(e) => {
                            sm.add_system_message(format!("❌ Failed to read conversation: {e}"));
                            return;
                        }
                    };
                    let registry = config.build_model_registry().ok();
                    let resolved = registry
                        .as_ref()
                        .and_then(|r| r.find_by_wire_id(&saved_provider, &saved_model))
                        .cloned();
                    // We only consult `resolved` as an availability
                    // check here — the actual stamping of display
                    // state happens inside
                    // `agent_loop::maybe_rebuild_after_load` after a
                    // successful agent rebuild. See the NOTE in the
                    // success branch below.
                    let Some(_resolved) = resolved else {
                        let display_id = if saved_provider.is_empty() {
                            saved_model.clone()
                        } else {
                            format!("{saved_provider}/{saved_model}")
                        };
                        sm.add_system_message(format!("❌ Model '{display_id}' not available."));
                        return;
                    };

                    match sm.load_conversation(id) {
                        Ok(()) => {
                            // NOTE: do NOT pre-stamp display state
                            // (`provider_name` / `model` / `model_alias`)
                            // here. `agent_loop::maybe_rebuild_after_load`
                            // runs immediately after this returns and
                            // uses the still-boot-identity vs the
                            // saved identity as its rebuild guard:
                            //   if sm.get_provider_name() == saved_provider
                            //   && sm.get_model() == saved_model { return; }
                            // Pre-stamping here makes that guard
                            // vacuously true on every load → the
                            // rebuild silently no-ops → the agent
                            // keeps its boot wire id and boot context
                            // window → the user gets the wrong model
                            // and the status bar lies. The display
                            // state is correctly stamped *after* a
                            // successful rebuild, inside
                            // `maybe_rebuild_after_load`. Regression
                            // pin: `tests::load_does_not_prestamp_display_identity`.
                            // See `bugs-opus.md` §"Issue 3+4+5".
                            if let Some(conv) = sm.get_current_conversation() {
                                sm.add_system_message(format!(
                                    "Loaded conversation: '{}'",
                                    conv.name
                                ));
                            }
                        }
                        Err(e) => {
                            sm.add_system_message(format!("❌ Failed to load conversation: {e}"));
                        }
                    }
                }
            }
            _ if cmd_lower.starts_with("/delete ") => {
                if let Some(arg) = cmd.strip_prefix("/delete ") {
                    let Some(sm) = state_manager else {
                        tracing::warn!("State manager not available for /delete command");
                        return;
                    };
                    let id = match Self::resolve_conversation_id(arg.trim(), sm) {
                        Ok(id) => id,
                        Err(msg) => {
                            sm.add_system_message(format!("❌ {}", msg));
                            return;
                        }
                    };
                    match sm.delete_conversation(id) {
                        Ok(_) => {
                            sm.add_system_message("Conversation deleted.".to_string());
                        }
                        Err(e) => {
                            sm.add_system_message(format!("❌ Failed to delete: {}", e));
                        }
                    }
                }
            }
            _ if cmd_lower.starts_with("/export ") => {
                if let Some(args) = cmd.strip_prefix("/export ") {
                    let parts: Vec<&str> = args.splitn(2, ' ').collect();
                    let Some(sm) = state_manager else {
                        tracing::warn!("State manager not available for /export command");
                        return;
                    };
                    if parts.len() != 2 {
                        sm.add_system_message(
                            "Usage: /export <n|uuid> <json|markdown>".to_string(),
                        );
                        return;
                    }
                    let id = match Self::resolve_conversation_id(parts[0].trim(), sm) {
                        Ok(id) => id,
                        Err(msg) => {
                            sm.add_system_message(format!("❌ {}", msg));
                            return;
                        }
                    };
                    let format = parts[1].to_lowercase();
                    match sm.export_conversation(id, &format) {
                        Ok(output) => {
                            sm.add_system_message(format!("Export:\n{}", output));
                        }
                        Err(e) => {
                            sm.add_system_message(format!("❌ Export failed: {}", e));
                        }
                    }
                }
            }
            _ if cmd_lower.starts_with("/rename ") => {
                if let Some(name) = cmd.strip_prefix("/rename ") {
                    if let Some(sm) = state_manager {
                        match sm.rename_conversation(name.to_string()) {
                            Ok(_) => {
                                sm.add_system_message(format!("Conversation renamed to: {}", name));
                            }
                            Err(e) => {
                                sm.add_system_message(format!("❌ Failed to rename: {}", e));
                            }
                        }
                    } else {
                        tracing::warn!("State manager not available for /rename command");
                    }
                }
            }
            // `/model` — list available models (with the active one
            // marked). Switching is intercepted in the View *before*
            // this point: see `ReplUi::try_intercept_model_command`.
            // This handler runs for the bare `/model` (no arg) case,
            // which includes `/model` followed only by trailing
            // whitespace — the slash-command popup completes `/model`
            // to `/model ` (trailing space, since the command is
            // declared with `takes_args = true`), so a user who hits
            // Enter without typing an alias must still land on the
            // listing path. See `tests::model_with_trailing_whitespace_lists_available_models`.
            _ if cmd_lower.trim_end() == "/model" => {
                let Some(sm) = state_manager else {
                    tracing::warn!("State manager not available for /model command");
                    return;
                };
                let registry = match config.build_model_registry() {
                    Ok(reg) => reg,
                    Err(e) => {
                        sm.add_system_message(format!(
                            "❌ /model: model registry not configured: {e}"
                        ));
                        return;
                    }
                };
                let aliases = registry.aliases_sorted();
                if aliases.is_empty() {
                    sm.add_system_message(
                        "No models declared. Add a `providers:` block to config.yaml. See `multi-model.md`."
                            .to_string(),
                    );
                    return;
                }
                let current = sm.get_model_alias();
                let mut msg = String::from("Available models:\n");
                for alias in &aliases {
                    let arrow = if &current == alias { "→ " } else { "  " };
                    if let Some(resolved) = registry.resolve(alias) {
                        msg.push_str(&format!(
                            "{arrow}{alias}  ({} · {})\n",
                            resolved.provider_name, resolved.model_name
                        ));
                    }
                }
                msg.push_str("\nUse /model <alias> to switch (starts a new conversation).");
                sm.add_system_message(msg);
            }
            _ if cmd_lower.starts_with("/model ") => {
                // The View should have intercepted this before it
                // reached us. If we get here, the View has no
                // registry attached (legacy single-provider boot or
                // test harness) — emit a helpful diagnostic instead
                // of silent inertness.
                if let Some(sm) = state_manager {
                    sm.add_system_message(
                        "❌ /model: not available in this build. Configure a `providers:` \
                         block in config.yaml and restart. See `multi-model.md`."
                            .to_string(),
                    );
                }
            }
            _ if cmd_lower.trim_end() == "/cd" => {
                // Bare /cd — print the session's working directory for
                // orientation. Pure read; works in every View. Use the
                // per-session cwd, not the process-global — the two can
                // differ in web sessions after `/cd`.
                if let Some(sm) = state_manager {
                    sm.add_system_message(sm.session_cwd().to_string_lossy().into_owned());
                }
            }
            _ if cmd_lower.starts_with("/cd ") => {
                // The View should have intercepted `/cd <path>` (validate
                // + confirm + emit ChangeCwd). Reaching here means no
                // registry is attached (legacy single-provider boot or a
                // non-REPL View) — emit a helpful diagnostic rather than a
                // silent no-op.
                if let Some(sm) = state_manager {
                    sm.add_system_message(
                        "❌ /cd: not available in this build. Configure a `providers:` \
                         block in config.yaml and restart."
                            .to_string(),
                    );
                }
            }
            _ => {
                // Unknown command — for now, let the agent handle it
                // The agent will respond via StateManager
            }
        }
    }
}

/// Derive the chat history that the next `prompt_with_history` call will
/// receive, honouring a one-shot override.
///
/// Two callers in the loop:
///
/// 1. **Normal turn**: `override_` is `None` → returns `get_agent_history()`
///    from `StateManager` (the source of truth, single trailing-User strip).
/// 2. **Post-compaction iteration**: `override_` was filled by the compact
///    arm with the resumption-shape history from
///    `StateManager::build_resumption_for_compaction()`. We `.take()` it so
///    the next iteration falls back to the normal path.
///
/// **Why this exists as a separate function:** the production bug fixed
/// in the 2026-05-09 cleanup pass was that the compact arm wrote the
/// resumption history to a loop-body-local `history` variable that was
/// shadowed and re-derived on the next iteration via
/// `get_agent_history()`. The override was effectively a no-op and the
/// resumption message ended up duplicated on the wire (once as the
/// prompt, once as the trailing entry of history). Pulling the
/// derivation out into a named function makes the override-vs-derivation
/// contract explicit and unit-testable; without that, the only way to
/// catch the regression would be a full mock-driven runtime test of the
/// agent loop. See the regression pins
/// `derive_history_for_iteration_*` in this module.
fn derive_history_for_iteration(
    override_: &mut Option<Vec<rig_core::completion::message::Message>>,
    state_manager: &Option<Arc<StateManager>>,
) -> Vec<rig_core::completion::message::Message> {
    match override_.take() {
        Some(h) => h,
        None => state_manager
            .as_ref()
            .map(|sm| sm.get_agent_history())
            .unwrap_or_default(),
    }
}

/// Handle for a connected MCP server.
///
/// Holds both the tools and the service connection. The service connection
/// must be kept alive for as long as the tools are used, and should be
/// properly closed on drop to avoid the "RunningService dropped without
/// explicit close()" warning.
pub struct McpServerHandle {
    #[allow(unused)]
    name: String,
    tools: Vec<McpTool>,
    /// The running service connection. Must be closed on drop for clean shutdown.
    /// This uses a wrapper enum since stdio and HTTP transports return different
    /// service types internally.
    service: McpService,
}

/// Wrapper enum for MCP service connections.
///
/// Both stdio and HTTP transports use `RunningService<RoleClient, ()>` when acting
/// as a client, but the inner service type differs (child process vs HTTP client).
/// This enum allows us to store the correct type while providing a uniform interface.
enum McpService {
    Stdio(Option<RunningService<RoleClient, ()>>),
    Http(Option<RunningService<RoleClient, ()>>),
}

impl Drop for McpServerHandle {
    fn drop(&mut self) {
        // Take the service out so we don't double-close
        let service = match &mut self.service {
            McpService::Stdio(s) => s.take(),
            McpService::Http(s) => s.take(),
        };

        if let Some(mut s) = service {
            // Spawn a task to close the service properly
            // This avoids blocking in Drop while ensuring clean shutdown
            tokio::spawn(async move {
                match s.close().await {
                    Ok(reason) => {
                        tracing::debug!("MCP service closed: {:?}", reason);
                    }
                    Err(e) => {
                        tracing::warn!("MCP service close error: {:?}", e);
                    }
                }
            });
        }
    }
}

impl McpServerHandle {
    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// Take ownership of the tools, consuming this handle.
    ///
    /// Note: The MCP service connection will be closed when this handle is dropped.
    /// For explicit control over service shutdown, use `close_and_take_tools()` instead.
    pub fn into_tools(self) -> Vec<Box<dyn ToolDyn>> {
        // Use ManuallyDrop to prevent our Drop impl from running
        // since we're consuming the handle intentionally
        let this = std::mem::ManuallyDrop::new(self);

        // Access fields through the ManuallyDrop'd reference
        this.tools
            .iter()
            .cloned()
            .map(|tool| Box::new(tool) as Box<dyn ToolDyn>)
            .collect()
    }

    /// Close the MCP service connection and get the tools.
    ///
    /// This properly closes the service before extracting tools.
    pub async fn close_and_take_tools(mut self) -> Vec<Box<dyn ToolDyn>> {
        // Close the service first (extract from Option via take on &mut self)
        // We need to do this carefully since we can't move out of self.service
        // Take the service out by matching on &mut self
        let service = match &mut self.service {
            McpService::Stdio(s) => s.take(),
            McpService::Http(s) => s.take(),
        };

        if let Some(mut s) = service {
            s.close().await.ok();
        }

        self.into_tools()
    }
}

pub async fn connect_mcp_server(config: &McpServerConfig) -> Result<McpServerHandle> {
    // Validate configuration
    if let Err(e) = config.validate() {
        return Err(anyhow::anyhow!("Invalid MCP server config: {}", e));
    }

    match config.transport_type() {
        McpTransportType::Stdio => connect_mcp_stdio(config).await,
        McpTransportType::Sse => {
            // rmcp 0.16 dropped the dedicated SSE client transport. The MCP
            // spec (2025-03-26) replaced SSE with Streamable HTTP, which is
            // wire-compatible with most existing "sse" servers. We route
            // there and warn loudly so users aren't surprised.
            tracing::warn!(
                "MCP server '{}': transport_type 'sse' is deprecated; \
                 routing through streamable-http. Set 'type: streamable-http' \
                 in your config to silence this warning.",
                config.name
            );
            connect_mcp_http(config).await
        }
        McpTransportType::StreamableHttp => connect_mcp_http(config).await,
    }
}

/// Connect to an MCP server using stdio transport (spawns a local process)
async fn connect_mcp_stdio(config: &McpServerConfig) -> Result<McpServerHandle> {
    let command = config
        .command
        .as_ref()
        .ok_or_else(|| anyhow!("stdio transport requires 'command'"))?;

    let mut cmd = tokio::process::Command::new(command);
    if let Some(args) = &config.args {
        cmd.args(args);
    }
    if let Some(env) = &config.env {
        for (key, value) in env {
            cmd.env(key, value);
        }
    }

    let (transport, _stderr) = TokioChildProcess::builder(cmd)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to create child process: {}", e))?;

    let service = ()
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to MCP server: {}", e))?;

    let server_info = service
        .peer_info()
        .ok_or_else(|| anyhow!("Can't get MCP info"))?;
    tracing::info!(
        "Connected to MCP server '{}': ({}:{})",
        config.name,
        server_info.server_info.name,
        server_info.server_info.version
    );

    let tools = service
        .list_all_tools()
        .await?
        .into_iter()
        .map(|tool| McpTool::from_mcp_server(tool, service.peer().clone()))
        .collect::<Vec<_>>();

    tracing::info!("MCP server '{}' has {} tools", config.name, tools.len());

    // Store the service in our wrapper enum, keeping a clone for the tools
    Ok(McpServerHandle {
        name: config.name.clone(),
        tools,
        service: McpService::Stdio(Some(service)),
    })
}

/// Connect to an MCP server using HTTP transport (remote server)
async fn connect_mcp_http(config: &McpServerConfig) -> Result<McpServerHandle> {
    let url = config
        .url
        .as_ref()
        .ok_or_else(|| anyhow!("HTTP transport requires 'url'"))?;

    tracing::info!("Connecting to MCP server '{}' at {}", config.name, url);

    // Surface deprecation warnings (e.g. legacy `auth_token`) at connect
    // time. `validate()` already rejected the "both set" footgun, so this
    // is purely informational.
    for warning in config.deprecation_warnings() {
        tracing::warn!("{warning}");
    }

    // Build the streamable-http config with optional bearer token + custom headers.
    let mut transport_config = StreamableHttpClientTransportConfig::with_uri(url.clone());
    // Lazily holds the OAuth-aware HTTP client when the server uses oauth.
    // `None` means the default reqwest client baked into `from_config` will be used.
    let mut auth_client: Option<crate::mcp_auth::AuthorizedClient> = None;

    match config.auth_resolved() {
        Some(crate::config::ResolvedAuth::Bearer { token }) => {
            transport_config = transport_config.auth_header(token);
        }
        Some(crate::config::ResolvedAuth::Oauth {
            client_id,
            client_secret,
            scopes,
        }) => {
            // Slice 2 + Slice 3a: full OAuth 2.1 + PKCE flow.
            // - `client_id` absent → DCR (Linear shape)
            // - `client_id` present → static credentials (Google shape)
            // `authorize` returns an `AuthClient<reqwest::Client>` that
            // implements `StreamableHttpClient`, so we feed it into the
            // transport via `with_client` below.
            let params = crate::mcp_auth::OauthParams {
                client_id,
                client_secret,
                scopes,
            };
            auth_client = Some(
                crate::mcp_auth::authorize(&config.name, url, params)
                    .await
                    .map_err(|e| anyhow!("MCP server '{}': {e}", config.name))?,
            );
        }
        None => {}
    }

    if let Some(headers) = config.headers.as_ref() {
        let mut parsed: std::collections::HashMap<::http::HeaderName, ::http::HeaderValue> =
            std::collections::HashMap::with_capacity(headers.len());
        for (k, v) in headers {
            match (
                ::http::HeaderName::try_from(k.as_str()),
                ::http::HeaderValue::try_from(v.as_str()),
            ) {
                (Ok(name), Ok(value)) => {
                    parsed.insert(name, value);
                }
                _ => {
                    tracing::warn!(
                        "MCP server '{}': skipping invalid header '{}: {}'",
                        config.name,
                        k,
                        v
                    );
                }
            }
        }
        if !parsed.is_empty() {
            transport_config = transport_config.custom_headers(parsed);
        }
    }

    // `StreamableHttpClientTransport<C>` is generic over the inner HTTP
    // client, so the two branches produce different concrete types and
    // can't be unified by a `let transport = if ...`. Drive `.serve`
    // from inside each arm — the resulting `service` (`RunningService<…>`)
    // has the same type regardless because the transport type-parameter
    // is erased by the worker boundary.
    let service = if let Some(client) = auth_client {
        // OAuth-aware client: signs every request and silently refreshes
        // access tokens.
        ().serve(StreamableHttpClientTransport::with_client(
            client,
            transport_config,
        ))
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to MCP server: {}", e))?
    } else {
        ().serve(StreamableHttpClientTransport::with_client(
            crate::http::client(),
            transport_config,
        ))
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to MCP server: {}", e))?
    };

    let server_info = service
        .peer_info()
        .ok_or_else(|| anyhow!("Can't get MCP info"))?;
    tracing::info!(
        "Connected to MCP server '{}': ({}:{})",
        config.name,
        server_info.server_info.name,
        server_info.server_info.version
    );

    let tools = service
        .list_all_tools()
        .await?
        .into_iter()
        .map(|tool| McpTool::from_mcp_server(tool, service.peer().clone()))
        .collect::<Vec<_>>();

    tracing::info!("MCP server '{}' has {} tools", config.name, tools.len());

    // Store the service in our wrapper enum, keeping a clone for the tools
    Ok(McpServerHandle {
        name: config.name.clone(),
        tools,
        service: McpService::Http(Some(service)),
    })
}

pub async fn load_mcp_servers(config: &Config) -> Result<Vec<McpServerHandle>> {
    let mut handles = Vec::new();

    let servers = match &config.mcp_servers {
        Some(servers) => servers,
        None => {
            tracing::info!("No MCP servers configured");
            return Ok(Vec::new());
        }
    };

    for server_config in servers {
        if !server_config.enabled {
            continue;
        }

        tracing::info!("Connecting to MCP server: {}", server_config.name);
        match connect_mcp_server(server_config).await {
            Ok(handle) => {
                handles.push(handle);
            }
            Err(e) => {
                tracing::error!(
                    "Failed to connect to MCP server '{}': {}",
                    server_config.name,
                    e
                );
            }
        }
    }

    Ok(handles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // --- /help handler tests -------------------------------------------------

    #[tokio::test]
    async fn help_command_emits_system_message_listing_all_builtin_commands() {
        let sm = StateManager::new_arc();
        let config = Config::default();

        AgentRunner::process_command_internal("/help", &Some(sm.clone()), &config).await;

        let state = sm.get_state();
        let system_msgs: Vec<_> = state
            .chat
            .messages
            .iter()
            .filter(|m| matches!(m.role, crate::ui::MessageRole::System))
            .collect();
        assert_eq!(
            system_msgs.len(),
            1,
            "/help should produce exactly one system message"
        );
        let body = &system_msgs[0].content;

        // Header
        assert!(body.starts_with("Available commands:"));

        // Every command in the list must appear in the help text
        for cmd in crate::ui::ui_trait::builtin_commands() {
            let needle = format!("/{}", cmd.name);
            assert!(
                body.contains(&needle),
                "/help output missing command: {}",
                needle
            );
            assert!(
                body.contains(&cmd.description),
                "/help output missing description for {}: {}",
                needle,
                cmd.description
            );
        }
    }

    // --- system-prompt shell awareness (#82) ---------------------------------

    #[test]
    fn system_prompt_powershell_instructs_powershell_syntax() {
        let skills = SkillRegistry::new();
        let ps = ShellKind::PowerShell {
            path: "pwsh.exe".to_string(),
        };
        let prompt = build_system_prompt(&skills, Some(&ps), &std::env::current_dir().unwrap());
        assert!(
            prompt.contains("**Shell**: powershell"),
            "PowerShell env line missing from prompt"
        );
        assert!(
            prompt.contains("`powershell`"),
            "prompt must name the `powershell` tool on a PowerShell host"
        );
        assert!(
            prompt.contains("PowerShell syntax"),
            "prompt must instruct PowerShell syntax on a PowerShell host"
        );
    }

    #[test]
    fn system_prompt_bash_does_not_instruct_powershell() {
        let skills = SkillRegistry::new();
        let bash = ShellKind::Bash {
            path: "/bin/sh".to_string(),
        };
        let prompt = build_system_prompt(&skills, Some(&bash), &std::env::current_dir().unwrap());
        assert!(
            prompt.contains("**Shell**: bash"),
            "bash env line missing from prompt"
        );
        assert!(
            !prompt.contains("PowerShell syntax"),
            "bash host must not carry PowerShell guidance"
        );
    }

    #[test]
    fn system_prompt_stamps_passed_cwd_and_reads_its_agents_md() {
        // A temp dir that is NOT the process cwd, carrying its own agents.md.
        let dir = std::env::temp_dir().join(format!(
            "peakbot-prompt-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agents.md"), "SENTINEL-AGENTS-CONTENT").unwrap();

        let skills = SkillRegistry::new();
        let prompt = build_system_prompt(&skills, None, &dir);

        assert!(
            prompt.contains(&dir.to_string_lossy().to_string()),
            "env block must stamp the passed cwd, not the process cwd"
        );
        assert!(
            prompt.contains("SENTINEL-AGENTS-CONTENT"),
            "agents.md must be read from the passed cwd"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn help_command_marks_arg_taking_commands_with_args_placeholder() {
        let sm = StateManager::new_arc();
        let config = Config::default();

        AgentRunner::process_command_internal("/help", &Some(sm.clone()), &config).await;

        let state = sm.get_state();
        let body = &state
            .chat
            .messages
            .iter()
            .find(|m| matches!(m.role, crate::ui::MessageRole::System))
            .expect("system message")
            .content;

        // Arg-taking commands should have " <args>" after their name
        assert!(body.contains("/load <args>"));
        assert!(body.contains("/delete <args>"));
        assert!(body.contains("/export <args>"));
        assert!(body.contains("/rename <args>"));

        // No-arg commands must NOT have the placeholder
        assert!(body.contains("/stats —") || body.contains("/stats"));
        assert!(!body.contains("/stats <args>"));
        assert!(!body.contains("/help <args>"));
    }

    // --- /conversations + index-resolver tests -------------------------------
    //
    // The shared `resolve_conversation_id` helper underpins /load, /delete and
    // /export. The /conversations command itself is the index-publishing
    // surface. See `conversazione.md` for the full design.

    /// Minimal helper: build an Arc<StateManager> backed by a fresh
    /// `InMemoryStorage`. Inline `pub use` would be cleaner — but importing
    /// the storage type once at function level keeps the test module's
    /// surface narrow.
    fn sm_with_storage() -> Arc<StateManager> {
        use crate::storage::InMemoryStorage;
        StateManager::new_arc_with_storage(Arc::new(InMemoryStorage::new()))
    }

    /// Build a StateManager pre-seeded with conversations at explicit
    /// `updated_at` offsets (in hours, relative to now). Returns the
    /// `(name, id)` pairs in **insertion order** (NOT display order — display
    /// order is newest-first, derived by the dispatcher at call time).
    ///
    /// Implementation note: we cannot use `StateManager::save_conversation`
    /// here because it stamps `updated_at = now` on every call, defeating the
    /// purpose. Instead we save directly through the storage trait, which is
    /// the source of truth, and then wrap it in the StateManager. No
    /// test-only accessor on StateManager required.
    fn sm_with_seeded(specs: &[(&str, i64)]) -> (Arc<StateManager>, Vec<(String, uuid::Uuid)>) {
        use crate::conversation::Conversation;
        use crate::storage::{ConversationStorage, InMemoryStorage};
        use chrono::Utc;

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        let mut out = Vec::new();
        // Seed with the wire identity that matches the legacy
        // synthesised registry: provider_name = "openrouter" (the
        // ProviderType::OpenRouter display value used by the synth
        // path) and model = the Config default. This way the post-v5
        // `/load` wire-id check accepts these fixtures.
        let provider_name = "openrouter".to_string();
        let model_wire_id = Config::default().model().to_string();
        for (name, hours_ago) in specs {
            let mut conv = Conversation::new(
                (*name).to_string(),
                provider_name.clone(),
                model_wire_id.clone(),
                String::new(),
            );
            conv.updated_at = Utc::now() - chrono::Duration::hours(*hours_ago);
            storage.save(&conv).expect("seed save");
            out.push((name.to_string(), conv.id));
        }
        let sm = StateManager::new_arc_with_storage(storage);
        (sm, out)
    }

    fn last_system_msg(sm: &StateManager) -> String {
        sm.get_state()
            .chat
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, crate::ui::MessageRole::System))
            .expect("at least one system message")
            .content
            .clone()
    }

    #[tokio::test]
    async fn conversations_empty_list_emits_explicit_message() {
        let sm = sm_with_storage();
        let config = Config::default();
        AgentRunner::process_command_internal("/conversations", &Some(sm.clone()), &config).await;
        assert_eq!(last_system_msg(&sm), "No saved conversations.");
    }

    #[tokio::test]
    async fn conversations_no_storage_emits_distinct_message() {
        let sm = StateManager::new_arc(); // no storage
        let config = Config::default();
        AgentRunner::process_command_internal("/conversations", &Some(sm.clone()), &config).await;
        assert_eq!(
            last_system_msg(&sm),
            "Conversation storage is not configured."
        );
    }

    #[tokio::test]
    async fn conversations_lists_every_saved_name_with_index() {
        let (sm, seeded) = sm_with_seeded(&[("Alpha", 1), ("Beta", 2), ("Gamma", 3)]);
        let config = Config::default();
        AgentRunner::process_command_internal("/conversations", &Some(sm.clone()), &config).await;
        let body = last_system_msg(&sm);
        for (name, _) in &seeded {
            assert!(
                body.contains(name),
                "missing name {} in body:\n{}",
                name,
                body
            );
        }
        // Indices 1, 2, 3 should appear right-aligned in the row prefix.
        assert!(body.contains("  1  "), "missing index 1 row:\n{}", body);
        assert!(body.contains("  2  "), "missing index 2 row:\n{}", body);
        assert!(body.contains("  3  "), "missing index 3 row:\n{}", body);
    }

    #[tokio::test]
    async fn conversations_sorted_newest_first() {
        // Seeded in jumbled order: 3h ago, 1h ago, 2h ago.
        let (sm, _) = sm_with_seeded(&[("Older", 3), ("Newest", 1), ("Middle", 2)]);
        let config = Config::default();
        AgentRunner::process_command_internal("/conversations", &Some(sm.clone()), &config).await;
        let body = last_system_msg(&sm);
        let pos_newest = body.find("Newest").expect("Newest present");
        let pos_middle = body.find("Middle").expect("Middle present");
        let pos_older = body.find("Older").expect("Older present");
        assert!(
            pos_newest < pos_middle && pos_middle < pos_older,
            "wrong order in body:\n{}",
            body
        );
    }

    #[tokio::test]
    async fn conversations_marks_current() {
        let (sm, seeded) = sm_with_seeded(&[("Alpha", 1), ("Beta", 2)]);
        let config = Config::default();
        // Set Beta as current.
        sm.load_conversation(seeded[1].1).expect("load Beta");
        AgentRunner::process_command_internal("/conversations", &Some(sm.clone()), &config).await;
        let body = last_system_msg(&sm);
        // ▶ marker should appear exactly once.
        assert_eq!(
            body.matches("▶ ").count(),
            1,
            "expected exactly one ▶ marker, body:\n{}",
            body
        );
        // It should prefix Beta's row (verify by line containment).
        let beta_line = body
            .lines()
            .find(|l| l.contains("Beta"))
            .expect("Beta line");
        assert!(
            beta_line.contains("▶ "),
            "▶ should mark Beta row:\n{}",
            beta_line
        );
    }

    #[tokio::test]
    async fn conversations_hides_uuids() {
        let (sm, _) = sm_with_seeded(&[("Alpha", 1), ("Beta", 2)]);
        let config = Config::default();
        AgentRunner::process_command_internal("/conversations", &Some(sm.clone()), &config).await;
        let body = last_system_msg(&sm);
        // A UUID is 8-4-4-4-12 hex chars with hyphens. Robust check: no
        // hyphenated 36-char tokens anywhere in the rendered body.
        for token in body.split_whitespace() {
            assert!(
                !(token.len() == 36 && token.matches('-').count() == 4),
                "found UUID-shaped token in /conversations output: {}\nFull body:\n{}",
                token,
                body
            );
        }
    }

    // --- /load + resolver tests ---------------------------------------------

    #[tokio::test]
    async fn load_by_index_resolves_to_correct_uuid() {
        let (sm, seeded) = sm_with_seeded(&[("Alpha", 1), ("Beta", 2), ("Gamma", 3)]);
        let config = Config::default();
        // Newest-first: Alpha=1h ago, Beta=2h, Gamma=3h. So index 2 → Beta.
        AgentRunner::process_command_internal("/load 2", &Some(sm.clone()), &config).await;
        let beta_id = seeded[1].1;
        assert_eq!(
            sm.get_current_conversation_id(),
            Some(beta_id),
            "/load 2 should load Beta"
        );
    }

    #[tokio::test]
    async fn load_by_uuid_still_works() {
        let (sm, seeded) = sm_with_seeded(&[("Alpha", 1), ("Beta", 2)]);
        let config = Config::default();
        let alpha_id = seeded[0].1;
        let cmd = format!("/load {}", alpha_id);
        AgentRunner::process_command_internal(&cmd, &Some(sm.clone()), &config).await;
        assert_eq!(sm.get_current_conversation_id(), Some(alpha_id));
    }

    #[tokio::test]
    async fn load_index_zero_emits_helpful_error() {
        let (sm, _) = sm_with_seeded(&[("Alpha", 1)]);
        let config = Config::default();
        AgentRunner::process_command_internal("/load 0", &Some(sm.clone()), &config).await;
        let body = last_system_msg(&sm);
        assert!(
            body.contains("1-based"),
            "expected '1-based' guidance, got:\n{}",
            body
        );
        assert!(
            body.contains("/conversations"),
            "expected pointer to /conversations, got:\n{}",
            body
        );
    }

    #[tokio::test]
    async fn load_index_out_of_range_emits_count() {
        let (sm, _) = sm_with_seeded(&[("Alpha", 1), ("Beta", 2)]);
        let config = Config::default();
        AgentRunner::process_command_internal("/load 99", &Some(sm.clone()), &config).await;
        let body = last_system_msg(&sm);
        assert!(
            body.contains("99"),
            "error should cite the bad index:\n{}",
            body
        );
        assert!(body.contains("2"), "error should cite the count:\n{}", body);
    }

    #[tokio::test]
    async fn load_invalid_arg_emits_uniform_error() {
        let (sm, _) = sm_with_seeded(&[("Alpha", 1)]);
        let config = Config::default();
        AgentRunner::process_command_internal(
            "/load not-a-uuid-or-int",
            &Some(sm.clone()),
            &config,
        )
        .await;
        let body = last_system_msg(&sm);
        assert!(
            body.contains("Invalid argument"),
            "expected resolver's invalid-argument message, got:\n{}",
            body
        );
    }

    /// `/load` of a saved conversation whose wire identity
    /// `(provider_name, model)` is no longer in the registry must
    /// reject the load with the canonical
    /// `Model 'provider/model' not available.` error AND leave the
    /// previously active conversation untouched. v5 contract:
    /// aliases are NOT consulted; only the wire identity is the
    /// stable re-activation key. *(persisted artifacts must carry
    /// every field needed to be re-activated)*
    #[tokio::test]
    async fn load_with_unavailable_wire_id_returns_clean_error_and_does_not_teardown() {
        use crate::conversation::Conversation;
        use crate::storage::{ConversationStorage, InMemoryStorage};

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        // Seed one convo whose wire id won't be in the legacy synth
        // registry (provider name "ghost" doesn't match the synth's
        // "openrouter").
        let mut conv = Conversation::new(
            "Stranger".to_string(),
            "ghost-provider".to_string(),
            "ghost-model".to_string(),
            String::new(),
        );
        conv.updated_at = chrono::Utc::now() - chrono::Duration::hours(1);
        storage.save(&conv).expect("seed save");
        let saved_id = conv.id;
        let sm = StateManager::new_arc_with_storage(storage);

        // Pre-state: no current conversation loaded.
        assert!(sm.get_current_conversation_id().is_none());

        let config = Config::default();
        let cmd = format!("/load {saved_id}");
        AgentRunner::process_command_internal(&cmd, &Some(sm.clone()), &config).await;

        let body = last_system_msg(&sm);
        assert!(
            body.contains("Model 'ghost-provider/ghost-model' not available."),
            "expected canonical unavailable-model error, got:\n{body}"
        );
        // Critical: the failed load must NOT have torn down whatever
        // was previously current. Since pre-state was None, current
        // must still be None.
        assert!(
            sm.get_current_conversation_id().is_none(),
            "failed /load must leave current conversation untouched"
        );
    }

    /// Pre-v5 conversations on disk default `provider_name` to the
    /// empty string. The wire-id lookup misses cleanly and `/load`
    /// rejects with the canonical error (the user is expected to
    /// retire them or re-save under v5).
    #[tokio::test]
    async fn load_pre_v5_file_treated_as_unavailable() {
        use crate::storage::{ConversationStorage, InMemoryStorage};

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        // Hand-craft a pre-v5 JSON (no `provider_name` field). Round-
        // trip via serde_json so deserialization populates the
        // `#[serde(default)]` empty string for `provider_name`.
        let id = uuid::Uuid::new_v4();
        let json = format!(
            r#"{{
                "id": "{id}",
                "name": "Old",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z",
                "messages": [],
                "model": "anthropic/claude-3.7-sonnet",
                "metadata": {{"message_count": 0}}
            }}"#
        );
        let conv: crate::conversation::Conversation =
            serde_json::from_str(&json).expect("parse pre-v5 fixture");
        assert_eq!(conv.provider_name, "");
        storage.save(&conv).expect("seed save");
        let sm = StateManager::new_arc_with_storage(storage);

        let config = Config::default();
        let cmd = format!("/load {id}");
        AgentRunner::process_command_internal(&cmd, &Some(sm.clone()), &config).await;

        let body = last_system_msg(&sm);
        assert!(
            body.contains("not available"),
            "expected unavailable-model error for pre-v5 file, got:\n{body}"
        );
    }

    /// **Regression pin (issue #3+4+5 from `bugs-opus.md`).** `/load`
    /// must NOT pre-stamp the StateManager's display identity
    /// (`provider_name` / `model` / `model_alias`) with the loaded
    /// conversation's saved identity. Stamping is the responsibility
    /// of `agent_loop::maybe_rebuild_after_load`, which runs *after*
    /// `/load` and uses the still-boot-identity as the rebuild guard
    /// (`if sm.get_provider_name() == saved_provider && sm.get_model()
    /// == saved_model { return; }`). When `/load` pre-stamps, that
    /// guard becomes vacuously true on every load, the rebuild is
    /// silently skipped, and the agent keeps its boot wire id and
    /// boot context window — so the user gets the wrong model and
    /// the status bar lies. See `bugs-opus.md` §"Issue 3+4+5".
    #[tokio::test]
    async fn load_does_not_prestamp_display_identity() {
        use crate::conversation::Conversation;
        use crate::storage::{ConversationStorage, InMemoryStorage};

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        // Seed a conv on the legacy synth identity that will resolve
        // (`openrouter` + Config default model) so the wire-id check
        // accepts it and `/load` reaches the success branch.
        let model = Config::default().model().to_string();
        let mut conv = Conversation::new(
            "Saved".to_string(),
            "openrouter".to_string(),
            model,
            String::new(),
        );
        conv.updated_at = chrono::Utc::now() - chrono::Duration::hours(1);
        storage.save(&conv).expect("seed save");
        let saved_id = conv.id;
        let sm = StateManager::new_arc_with_storage(storage);

        // Establish a *different* boot identity on the StateManager.
        // The rebuild guard in `maybe_rebuild_after_load` compares
        // these against the saved identity — they must remain distinct
        // through `/load` so the rebuild fires.
        sm.set_provider_name("boot-prov".to_string());
        sm.set_model("boot-model".to_string());
        sm.set_model_alias("boot-alias".to_string());

        let config = Config::default();
        let cmd = format!("/load {saved_id}");
        AgentRunner::process_command_internal(&cmd, &Some(sm.clone()), &config).await;

        // The conversation swap itself must succeed.
        assert_eq!(
            sm.get_current_conversation_id(),
            Some(saved_id),
            "/load should swap the active conversation"
        );
        // …but the display identity must still be the boot identity,
        // so `maybe_rebuild_after_load`'s guard sees a mismatch and
        // actually rebuilds the agent. Pre-stamping here is the bug.
        assert_eq!(
            sm.get_provider_name(),
            "boot-prov",
            "/load must not pre-stamp provider_name — that's maybe_rebuild_after_load's job"
        );
        assert_eq!(
            sm.get_model(),
            "boot-model",
            "/load must not pre-stamp model — that's maybe_rebuild_after_load's job"
        );
        assert_eq!(
            sm.get_model_alias(),
            "boot-alias",
            "/load must not pre-stamp model_alias — that's maybe_rebuild_after_load's job"
        );
    }

    /// `/load` of a conversation whose wire id resolves under a
    /// renamed alias must succeed: the alias change in `config.yaml`
    /// is invisible to persistence. v5 contract.
    #[tokio::test]
    async fn load_succeeds_when_alias_is_renamed_but_wire_id_is_stable() {
        use crate::conversation::Conversation;
        use crate::storage::{ConversationStorage, InMemoryStorage};

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        let model = Config::default().model().to_string();
        // Save under the legacy synth identity (provider "openrouter",
        // default model). The alias was "default" at save time —
        // but we don't even care, we never wrote it.
        let mut conv = Conversation::new(
            "Saved".to_string(),
            "openrouter".to_string(),
            model,
            String::new(),
        );
        conv.updated_at = chrono::Utc::now() - chrono::Duration::hours(1);
        storage.save(&conv).expect("seed save");
        let saved_id = conv.id;
        let sm = StateManager::new_arc_with_storage(storage);

        // Use a fresh Config — the alias may have been rebuilt under
        // any name in config.yaml; doesn't matter, the wire-id
        // lookup is alias-blind.
        let config = Config::default();
        let cmd = format!("/load {saved_id}");
        AgentRunner::process_command_internal(&cmd, &Some(sm.clone()), &config).await;

        assert_eq!(
            sm.get_current_conversation_id(),
            Some(saved_id),
            "/load should resolve by wire id regardless of alias rename"
        );
    }

    // --- /delete + /export by index -----------------------------------------

    #[tokio::test]
    async fn delete_by_index_works() {
        let (sm, _) = sm_with_seeded(&[("Alpha", 1), ("Beta", 2), ("Gamma", 3)]);
        let config = Config::default();
        // /delete 1 → newest (Alpha). After: 2 conversations remain.
        AgentRunner::process_command_internal("/delete 1", &Some(sm.clone()), &config).await;
        let remaining = sm.list_conversations().expect("storage").len();
        assert_eq!(remaining, 2, "/delete 1 should remove one of three");
    }

    #[tokio::test]
    async fn export_by_index_works() {
        let (sm, _) = sm_with_seeded(&[("OnlyOne", 1)]);
        let config = Config::default();
        AgentRunner::process_command_internal("/export 1 markdown", &Some(sm.clone()), &config)
            .await;
        let body = last_system_msg(&sm);
        assert!(
            body.starts_with("Export:"),
            "export header missing:\n{}",
            body
        );
        assert!(
            body.contains("OnlyOne"),
            "export should include the conversation name:\n{}",
            body
        );
    }

    // --- /new handler tests --------------------------------------------------
    //
    // Regression guard for the "/new doesn't actually start a new conversation"
    // bug: `sm.create_conversation(...)` only swaps the current-conversation
    // slot; without also clearing `chat.messages` and resetting stats, the
    // agent's next turn still sees the old history (since `get_agent_history()`
    // derives from `chat.messages`) and the token/cost counters keep climbing.

    #[tokio::test]
    async fn new_command_clears_chat_messages() {
        let sm = StateManager::new_arc();
        let config = Config::default();

        // Seed a conversation with some user/assistant turns
        sm.add_user_message("hello".to_string());
        sm.add_assistant_message("hi there".to_string());
        assert_eq!(sm.get_state().chat.messages.len(), 2);

        AgentRunner::process_command_internal("/new", &Some(sm.clone()), &config).await;

        // After /new, the only remaining message should be the "Started a new
        // conversation." system banner — the prior user/assistant turns must
        // be gone so the next prompt doesn't carry them into the agent.
        let state = sm.get_state();
        let non_system: Vec<_> = state
            .chat
            .messages
            .iter()
            .filter(|m| !matches!(m.role, crate::ui::MessageRole::System))
            .collect();
        assert!(
            non_system.is_empty(),
            "/new must clear user/assistant/tool messages; got {:?}",
            non_system.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn new_command_resets_session_stats() {
        let sm = StateManager::new_arc();
        let config = Config::default();

        // Seed the stats as if a request had happened
        {
            let stats_arc = sm.stats_arc();
            let mut stats = stats_arc.lock().unwrap();
            stats.total_input_tokens = 1234;
            stats.total_output_tokens = 567;
            stats.total_api_calls = 3;
            stats.total_cost = 0.42;
        }

        AgentRunner::process_command_internal("/new", &Some(sm.clone()), &config).await;

        let stats_arc = sm.stats_arc();
        let stats = stats_arc.lock().unwrap();
        assert_eq!(stats.total_input_tokens, 0, "/new must zero input tokens");
        assert_eq!(stats.total_output_tokens, 0, "/new must zero output tokens");
        assert_eq!(stats.total_api_calls, 0, "/new must zero api calls");
        assert_eq!(stats.total_cost, 0.0, "/new must zero cost");
    }

    #[tokio::test]
    async fn new_command_clears_todo_list() {
        // /new starts a fresh conversation, and the todo list is conceptually
        // scoped to "the work the user is doing right now". Carrying todos
        // from the previous conversation into a new one is the same class of
        // bug as carrying chat history: stale state leaking across a
        // user-initiated reset. See the docs above the /new handler.
        let sm = StateManager::new_arc();
        let config = Config::default();

        // Seed the todo list with two tasks
        sm.add_todo("write the docs".to_string());
        sm.add_todo("ship the bug fix".to_string());
        assert_eq!(sm.get_todo_list().list().len(), 2);

        AgentRunner::process_command_internal("/new", &Some(sm.clone()), &config).await;

        let todos = sm.get_todo_list();
        assert!(
            todos.list().is_empty(),
            "/new must clear the todo list; got {:?}",
            todos.list().iter().map(|t| &t.task).collect::<Vec<_>>()
        );
        // Also verify the UI-facing view was synced (otherwise the panel
        // would keep displaying the dead tasks until the next state update).
        assert!(
            sm.get_state().todo.items.is_empty(),
            "/new must sync the cleared todo list to the UI state"
        );
    }

    #[tokio::test]
    async fn new_command_swaps_in_a_fresh_conversation() {
        let sm = StateManager::new_arc();
        let config = Config::default();

        sm.create_conversation(
            "old one".to_string(),
            "test-prov".to_string(),
            "test-model".to_string(),
            String::new(),
        );
        let old_id = sm
            .get_current_conversation_id()
            .expect("seeded conversation");

        AgentRunner::process_command_internal("/new", &Some(sm.clone()), &config).await;

        let new_id = sm
            .get_current_conversation_id()
            .expect("/new must create a current conversation");
        assert_ne!(new_id, old_id, "/new must produce a fresh conversation id");
    }

    // --- /model listing handler tests --------------------------------------
    //
    // Regression guard for "bare `/model` falls through the View interceptor
    // and into the controller's `/model ` (with-args) arm because the slash-
    // command popup completes `/model` to `/model ` (trailing space, since
    // `takes_args = true`). The user submits `/model ` and gets the
    // "not available in this build" diagnostic instead of the listing.
    // See memory.md episodic entry 2026-05-07.

    fn config_with_two_aliases() -> Config {
        use crate::config::{ModelEntry, ProviderEntry, ProviderType};
        Config {
            providers: vec![ProviderEntry {
                name: "openrouter".into(),
                kind: ProviderType::OpenRouter,
                api_key: Some("sk-test".into()),
                base_url: None,
                models: vec![
                    ModelEntry {
                        name: "anthropic/claude-3.7-sonnet".into(),
                        alias: Some("sonnet".into()),
                        max_tokens: None,
                        temperature: None,
                        extra_params: None,
                        prompt_caching: None,
                        vision: None,
                        context_size: None,
                    },
                    ModelEntry {
                        name: "openai/gpt-4o".into(),
                        alias: Some("gpt4o".into()),
                        max_tokens: None,
                        temperature: None,
                        extra_params: None,
                        prompt_caching: None,
                        vision: None,
                        context_size: None,
                    },
                ],
            }],
            default_model: Some("sonnet".into()),
            ..Config::default()
        }
    }

    fn last_system_message(sm: &Arc<StateManager>) -> String {
        sm.get_state()
            .chat
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, crate::ui::app_state::MessageRole::System))
            .map(|m| m.content.clone())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn bare_model_command_lists_available_models() {
        let sm = StateManager::new_arc();
        let config = config_with_two_aliases();

        AgentRunner::process_command_internal("/model", &Some(sm.clone()), &config).await;

        let msg = last_system_message(&sm);
        assert!(
            msg.contains("Available models:"),
            "bare /model must show the listing, got: {msg}"
        );
        assert!(msg.contains("sonnet"), "listing must include 'sonnet'");
        assert!(msg.contains("gpt4o"), "listing must include 'gpt4o'");
    }

    #[tokio::test]
    async fn model_with_trailing_whitespace_lists_available_models() {
        // Repro: slash-command popup completion fills the input with
        // "/model " (trailing space) because `takes_args = true`. If the
        // user hits Enter without typing an alias, the submission is
        // "/model " — which used to fall into the with-args arm and emit
        // the misleading "not available in this build" diagnostic. The
        // controller must treat this as the bare-listing case.
        let sm = StateManager::new_arc();
        let config = config_with_two_aliases();

        AgentRunner::process_command_internal("/model ", &Some(sm.clone()), &config).await;

        let msg = last_system_message(&sm);
        assert!(
            msg.contains("Available models:"),
            "/model with trailing space must show the listing, got: {msg}"
        );
        assert!(
            !msg.contains("not available in this build"),
            "must NOT emit the legacy-build diagnostic for trailing-whitespace bare /model, \
             got: {msg}"
        );
    }

    // --- /reset handler tests -----------------------------------------------
    //
    // Regression guard for the "/reset is silently inert" bug: /reset used to
    // sit in the fossil no-op match arm with a comment saying "UI pulls this
    // data" — but nothing in the REPL handled it, so the counters stayed stuck.
    // See memory.md derived-view-invalidation rule.

    #[tokio::test]
    async fn reset_command_zeros_session_stats() {
        let sm = StateManager::new_arc();
        let config = Config::default();

        // Seed the stats as if some requests had happened
        {
            let stats_arc = sm.stats_arc();
            let mut stats = stats_arc.lock().unwrap();
            stats.total_input_tokens = 5000;
            stats.total_output_tokens = 2000;
            stats.total_api_calls = 7;
            stats.total_cost = 1.23;
        }

        AgentRunner::process_command_internal("/reset", &Some(sm.clone()), &config).await;

        let stats_arc = sm.stats_arc();
        let stats = stats_arc.lock().unwrap();
        assert_eq!(stats.total_input_tokens, 0, "/reset must zero input tokens");
        assert_eq!(
            stats.total_output_tokens, 0,
            "/reset must zero output tokens"
        );
        assert_eq!(stats.total_api_calls, 0, "/reset must zero api calls");
        assert_eq!(stats.total_cost, 0.0, "/reset must zero cost");
    }

    #[tokio::test]
    async fn reset_command_preserves_chat_history() {
        // /reset resets *stats only* — the conversation itself must survive.
        // /new is the one that clears history. Keeping them orthogonal is the
        // whole reason they're two commands.
        let sm = StateManager::new_arc();
        let config = Config::default();

        sm.add_user_message("keep me".to_string());
        sm.add_assistant_message("and me".to_string());
        let before = sm.get_state().chat.messages.len();

        AgentRunner::process_command_internal("/reset", &Some(sm.clone()), &config).await;

        let after_msgs = sm.get_state().chat.messages;
        // The two seeded messages must still be there (plus one new system
        // banner confirming the reset).
        let user_and_assistant: Vec<_> = after_msgs
            .iter()
            .filter(|m| !matches!(m.role, crate::ui::MessageRole::System))
            .collect();
        assert_eq!(
            user_and_assistant.len(),
            before,
            "/reset must NOT clear user/assistant messages; /new is the one that does that"
        );
    }

    #[tokio::test]
    async fn reset_command_emits_system_banner() {
        let sm = StateManager::new_arc();
        let config = Config::default();

        AgentRunner::process_command_internal("/reset", &Some(sm.clone()), &config).await;

        let state = sm.get_state();
        let system_msgs: Vec<_> = state
            .chat
            .messages
            .iter()
            .filter(|m| matches!(m.role, crate::ui::MessageRole::System))
            .collect();
        assert_eq!(
            system_msgs.len(),
            1,
            "/reset should emit exactly one system banner"
        );
        assert!(
            system_msgs[0].content.to_lowercase().contains("reset"),
            "banner should mention the reset; got {:?}",
            system_msgs[0].content
        );
    }

    // --- /exit handler tests ------------------------------------------------
    //
    // /exit must signal the view to quit WITHOUT the Ctrl+C confirmation
    // dialog. Since the dispatcher runs in the agent loop and can't
    // touch ReplUi directly, it sets an AppState flag the view polls.
    // The view-side wiring (ReplUi reads the flag and sets running=false)
    // is covered by repl_impl tests; here we pin the dispatcher contract.

    #[tokio::test]
    async fn exit_command_sets_exit_requested_flag() {
        let sm = StateManager::new_arc();
        let config = Config::default();

        assert!(
            !sm.exit_requested(),
            "precondition: no exit request in a fresh state"
        );

        AgentRunner::process_command_internal("/exit", &Some(sm.clone()), &config).await;

        assert!(
            sm.exit_requested(),
            "/exit must flip the exit_requested flag"
        );
    }

    #[tokio::test]
    async fn exit_command_does_not_clear_chat_or_stats() {
        // /exit leaves the world as-is — no "Goodbye" banner, no stats
        // reset, no chat clear. The REPL is about to tear down; extra
        // writes would just flash before the screen is cleared.
        let sm = StateManager::new_arc();
        let config = Config::default();

        sm.add_user_message("keep me".to_string());
        sm.add_assistant_message("and me".to_string());
        {
            let stats_arc = sm.stats_arc();
            let mut stats = stats_arc.lock().unwrap();
            stats.total_input_tokens = 42;
            stats.total_cost = 0.01;
        }
        let msgs_before = sm.get_state().chat.messages.len();

        AgentRunner::process_command_internal("/exit", &Some(sm.clone()), &config).await;

        assert_eq!(
            sm.get_state().chat.messages.len(),
            msgs_before,
            "/exit must not touch chat messages"
        );
        let stats_arc = sm.stats_arc();
        let stats = stats_arc.lock().unwrap();
        assert_eq!(stats.total_input_tokens, 42, "/exit must not reset stats");
        assert_eq!(stats.total_cost, 0.01, "/exit must not reset cost");
    }

    // --- Submission routing tests -------------------------------------------
    //
    // Regression guard for the "/new goes straight to the model" bug
    // (see `allehailmenu.md`). The event loop classifies every
    // `UiAction::SendMessage(msg)` through `classify_submission` and routes
    // accordingly. If any arm regresses, slash commands silently become
    // expensive LLM turns again.

    #[test]
    fn classify_plain_text_is_user_message() {
        assert!(matches!(
            classify_submission("hello world"),
            SubmitKind::UserMessage(s) if s == "hello world"
        ));
    }

    #[test]
    fn classify_slash_new_is_command() {
        // THE bug: used to classify as UserMessage, got sent to the LLM.
        assert!(matches!(
            classify_submission("/new"),
            SubmitKind::Command(s) if s == "/new"
        ));
    }

    #[test]
    fn classify_slash_help_is_command() {
        assert!(matches!(
            classify_submission("/help"),
            SubmitKind::Command(s) if s == "/help"
        ));
    }

    #[test]
    fn classify_slash_with_args_is_command() {
        // Arg-taking commands (popup closes on space, user finishes typing).
        assert!(matches!(
            classify_submission("/load 123e4567-e89b-12d3-a456-426614174000"),
            SubmitKind::Command(_)
        ));
    }

    #[test]
    fn classify_slash_stop_is_stop_command() {
        // /stop is special: it interrupts the running agent rather than
        // queueing another command behind the current run.
        assert!(matches!(
            classify_submission("/stop"),
            SubmitKind::StopCommand
        ));
    }

    #[test]
    fn classify_trims_whitespace_before_deciding() {
        // Trailing newline from the input buffer must not demote a command
        // back to UserMessage.
        assert!(matches!(
            classify_submission("/new\n"),
            SubmitKind::Command(_)
        ));
        assert!(matches!(
            classify_submission("  /stop  "),
            SubmitKind::StopCommand
        ));
    }

    #[test]
    fn classify_mid_sentence_slash_stays_user_message() {
        // A slash that isn't at the start (after trim) is chat content.
        assert!(matches!(
            classify_submission("TODO: /foo or /bar?"),
            SubmitKind::UserMessage(_)
        ));
    }

    #[test]
    fn classify_empty_is_user_message() {
        // The Enter handler already drops empty buffers; defensive default.
        assert!(matches!(
            classify_submission(""),
            SubmitKind::UserMessage(_)
        ));
        assert!(matches!(
            classify_submission("   "),
            SubmitKind::UserMessage(_)
        ));
    }

    #[test]
    fn classify_inline_image_path_is_multimodal() {
        use std::io::Write;
        // Create a real tempfile the classifier can resolve.
        let path = std::env::temp_dir().join(format!(
            "peakbot-classify-{}-{}.png",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(b"x").expect("write");
        let input = format!("describe [img:{}]", path.display());
        match classify_submission(&input) {
            SubmitKind::MultimodalMessage { text, attachments } => {
                assert_eq!(text, "describe");
                assert_eq!(attachments.len(), 1);
            }
            other => panic!("expected MultimodalMessage, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn classify_inline_image_missing_is_invalid_attachment() {
        let input = "look at [img:/does/not/exist-7f8a.png]";
        assert!(matches!(
            classify_submission(input),
            SubmitKind::InvalidAttachment(_)
        ));
    }

    // --- MCP tests (pre-existing) -------------------------------------------

    #[tokio::test]
    async fn test_connect_mcp_server_invalid_command() {
        let mut env = HashMap::new();
        env.insert("TEST_VAR".to_string(), "test_value".to_string());

        let config = McpServerConfig {
            name: "test_invalid".to_string(),
            transport_type: McpTransportType::Stdio,
            command: Some("nonexistent_command_xyz123".to_string()),
            args: None,
            env: Some(env),
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };

        let result = connect_mcp_server(&config).await;
        assert!(result.is_err(), "Expected error for invalid command");
    }

    #[tokio::test]
    #[ignore = "needs network + uvx (Python); run locally with --ignored"]
    async fn test_connect_mcp_server_hello() {
        let config = McpServerConfig {
            name: "hello-mcp-server".to_string(),
            transport_type: McpTransportType::Stdio,
            command: Some("uvx".to_string()),
            args: Some(vec![
                "--from".to_string(),
                "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
                "hello-mcp-server".to_string(),
            ]),
            env: None,
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };

        let result = connect_mcp_server(&config).await;
        let handle = result.expect("Failed to connect to hello-mcp-server");
        let tools = handle.tools();
        assert!(!tools.is_empty(), "Expected at least one tool");

        println!("Connected to hello-mcp-server with {} tools", tools.len());
    }

    #[tokio::test]
    #[ignore = "needs network + uvx (Python); run locally with --ignored"]
    async fn test_connect_mcp_server_with_env() {
        let mut env = HashMap::new();
        env.insert("TEST_ENV_VAR".to_string(), "test_value".to_string());

        let config = McpServerConfig {
            name: "hello-mcp-server-with-env".to_string(),
            transport_type: McpTransportType::Stdio,
            command: Some("uvx".to_string()),
            args: Some(vec![
                "--from".to_string(),
                "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
                "hello-mcp-server".to_string(),
            ]),
            env: Some(env),
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };

        let result = connect_mcp_server(&config).await;
        let handle = result.expect("Failed to connect to hello-mcp-server with env vars");
        let tools = handle.tools();

        assert!(
            !tools.is_empty(),
            "Expected at least one tool with custom env"
        );
    }

    #[tokio::test]
    #[ignore = "needs network + uvx (Python); run locally with --ignored"]
    async fn test_connect_mcp_server_call_tool() {
        let config = McpServerConfig {
            name: "hello-mcp-server".to_string(),
            transport_type: McpTransportType::Stdio,
            command: Some("uvx".to_string()),
            args: Some(vec![
                "--from".to_string(),
                "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
                "hello-mcp-server".to_string(),
            ]),
            env: None,
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };

        let handle = connect_mcp_server(&config)
            .await
            .expect("Failed to connect to hello-mcp-server");

        let tools = handle.tools();
        assert!(!tools.is_empty(), "Expected at least one tool");

        let first_tool = &tools[0];
        println!("Calling tool: {}", first_tool.name());

        let result = first_tool
            .call("{}".to_string())
            .await
            .expect("Failed to call tool");

        println!("Tool call result: {:?}", result);

        assert!(
            !result.is_empty(),
            "Expected non-empty result from tool call"
        );
    }

    #[test]
    fn test_mcp_transport_type_deserialization() {
        // Test stdio (default)
        let yaml = r#"
name: test-stdio
command: npx
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.transport_type, McpTransportType::Stdio);
        assert_eq!(config.command.as_ref().unwrap(), "npx");
        assert!(config.url.is_none());

        // Test explicit stdio
        let yaml = r#"
name: test-explicit-stdio
type: stdio
command: npx
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.transport_type, McpTransportType::Stdio);

        // Test SSE
        let yaml = r#"
name: test-sse
type: sse
url: https://example.com/mcp
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.transport_type, McpTransportType::Sse);
        assert!(config.command.is_none());
        assert_eq!(config.url.as_ref().unwrap(), "https://example.com/mcp");

        // Test streamable-http (hyphens are removed in lowercase serde)
        let yaml = r#"
name: test-http
type: streamablehttp
url: https://example.com/mcp
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.transport_type, McpTransportType::StreamableHttp);
        assert!(config.command.is_none());
        assert_eq!(config.url.as_ref().unwrap(), "https://example.com/mcp");
    }

    #[test]
    fn test_mcp_http_auth_deserialization() {
        // Defaults: no auth_token, no headers
        let yaml = r#"
name: plain-http
type: streamablehttp
url: https://example.com/mcp
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.auth_token.is_none());
        assert!(config.headers.is_none());

        // Bearer token
        let yaml = r#"
name: with-token
type: streamablehttp
url: https://example.com/mcp
auth_token: "sk-abc123"
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.auth_token.as_deref(), Some("sk-abc123"));

        // Custom headers
        let yaml = r#"
name: with-headers
type: streamablehttp
url: https://example.com/mcp
headers:
  X-Api-Key: my-key
  X-Tenant: acme
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        let headers = config.headers.expect("headers should deserialize");
        assert_eq!(headers.get("X-Api-Key").map(String::as_str), Some("my-key"));
        assert_eq!(headers.get("X-Tenant").map(String::as_str), Some("acme"));
    }

    #[test]
    fn test_mcp_config_validation() {
        // Valid stdio config
        let config = McpServerConfig {
            name: "test".to_string(),
            transport_type: McpTransportType::Stdio,
            command: Some("npx".to_string()),
            args: None,
            env: None,
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };
        assert!(config.validate().is_ok());

        // Invalid stdio (missing command)
        let config = McpServerConfig {
            name: "test".to_string(),
            transport_type: McpTransportType::Stdio,
            command: None,
            args: None,
            env: None,
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };
        assert!(config.validate().is_err());

        // Valid HTTP config
        let config = McpServerConfig {
            name: "test".to_string(),
            transport_type: McpTransportType::Sse,
            command: None,
            args: None,
            env: None,
            url: Some("https://example.com/mcp".to_string()),
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };
        assert!(config.validate().is_ok());

        // Invalid HTTP (missing url)
        let config = McpServerConfig {
            name: "test".to_string(),
            transport_type: McpTransportType::Sse,
            command: None,
            args: None,
            env: None,
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };
        assert!(config.validate().is_err());
    }

    // ─── Slice 2: OAuth wiring (no in-process pin) ──────────────────────────
    //
    // Slice 1 of the MCP OAuth work parsed the `auth: { type: oauth }`
    // shape but didn't wire the flow — that pin
    // (`oauth_variant_returns_not_yet_implemented`) lived here. Slice 2
    // (this branch) wires the real `mcp_auth::authorize` path. The
    // replacement contract is the pre-merge live Linear smoke test
    // recorded in `autho.md`; there is no in-process pin because the
    // happy path requires DCR + browser + token exchange against a real
    // OAuth server.

    // ─── Mid-action compaction: production wire-payload contract ────────────
    //
    // These tests pin the exact data shape that
    // `AgentRunner::process_message_internal` constructs after the
    // SessionHook fires `terminate("compact")`. The bug we're guarding
    // against is subtle: `build_resumption_for_compaction` returns the
    // correct `(prompt, history)` tuple, but if the loop discards the
    // returned history and re-derives it via `get_agent_history()` on
    // the next iteration, the resumption message ends up duplicated on
    // the wire (once as the prompt, once at the tail of history). That
    // breaks Anthropic / OpenAI conversation invariants and the model
    // either re-runs the tool, produces garbage, or refuses.
    //
    // Pinned at the data layer because the runtime race is hard to
    // reproduce deterministically (see procedural rule (c) in
    // `memory.md`: "Pin races at the data layer, not the runtime layer").
    //
    // The fix is a one-shot `history_override: Option<Vec<Message>>`
    // outside the loop body, consumed via `.take()` at the top of the
    // next iteration to override the default `get_agent_history()` call.

    /// Helper: extract every visible text fragment from a rig
    /// `Message` for substring assertions. Walks User text, User
    /// ToolResult text, and Assistant text + tool-call arguments.
    fn message_texts(msg: &rig_core::completion::message::Message) -> Vec<String> {
        use rig_core::completion::message::{
            AssistantContent, Message as RigMessage, ToolResultContent, UserContent,
        };
        let mut out = Vec::new();
        match msg {
            RigMessage::User { content } => {
                for c in std::iter::once(content.first_ref()).chain(content.rest().iter()) {
                    match c {
                        UserContent::Text(t) => out.push(t.text.clone()),
                        UserContent::ToolResult(tr) => {
                            for rc in std::iter::once(tr.content.first_ref())
                                .chain(tr.content.rest().iter())
                            {
                                if let ToolResultContent::Text(t) = rc {
                                    out.push(t.text.clone());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            RigMessage::Assistant { content, .. } => {
                for c in std::iter::once(content.first_ref()).chain(content.rest().iter()) {
                    match c {
                        AssistantContent::Text(t) => out.push(t.text.clone()),
                        AssistantContent::ToolCall(tc) => {
                            // Include tool-call arguments so substring
                            // checks can verify ToolCall preservation.
                            out.push(tc.function.arguments.to_string());
                        }
                        _ => {}
                    }
                }
            }
            RigMessage::System { .. } => {}
        }
        out
    }

    /// Count occurrences of `needle` across a slice of rig Messages.
    fn count_occurrences(msgs: &[rig_core::completion::message::Message], needle: &str) -> usize {
        msgs.iter()
            .flat_map(message_texts)
            .filter(|t| t.contains(needle))
            .count()
    }

    /// **The bug.** After a tool round-trip the chat ends in a
    /// `ToolResult`. When mid-action compaction fires, the production
    /// loop must use the resumption tuple's history (returned alongside
    /// the prompt by `build_resumption_for_compaction`) — NOT a fresh
    /// `get_agent_history()` call, which still includes the trailing
    /// ToolResult and would duplicate it on the wire.
    ///
    /// This test exercises [`derive_history_for_iteration`] — the same
    /// function the production loop calls — with the post-compact-arm
    /// state: `history_override = Some(resumption_history)` and a
    /// `current_turn = ToolResult`. Asserts the resumption marker
    /// appears exactly once across `(prompt + derived_history)`.
    ///
    /// **Without `derive_history_for_iteration` honouring the override**
    /// (i.e. the regression introduced in `a1d705c`), this test fails:
    /// the trailing ToolResult ends up duplicated.
    ///
    /// Pinned by [`memory.md` §"context fukup #2 (2026-05-09)"].
    #[test]
    fn derive_history_for_iteration_does_not_duplicate_toolresult_on_resume() {
        let sm = Arc::new(StateManager::new());
        // Simulate a tool round-trip: User → Agent → ToolCall → ToolResult.
        // Compaction fires after the ToolResult lands.
        sm.add_user_message("list files".to_string());
        sm.add_assistant_message("I'll run ls for you".to_string());
        sm.add_tool_call(
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            Some("call_1".to_string()),
        );
        sm.add_tool_result(
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            "UNIQUE_TOOLRESULT_MARKER_12345".to_string(),
            Some("call_1".to_string()),
        );

        // The compact arm sets current_turn = prompt and stashes
        // resumption_history into the one-shot override.
        let (prompt, resumption_history) = sm
            .build_resumption_for_compaction()
            .expect("non-empty conversation must produce a resumption");
        let mut history_override = Some(resumption_history);

        // The loop top calls derive_history_for_iteration to get the
        // history that goes on the wire. With the override Some, this
        // MUST return the resumption history verbatim — NOT re-derive
        // from StateManager (which would re-include the trailing
        // ToolResult and duplicate it).
        let sm_opt: Option<Arc<StateManager>> = Some(sm.clone());
        let derived_history = derive_history_for_iteration(&mut history_override, &sm_opt);

        // Assertion: the resumption marker must appear exactly ONCE
        // across (prompt + derived_history).
        let prompt_count = count_occurrences(
            std::slice::from_ref(&prompt),
            "UNIQUE_TOOLRESULT_MARKER_12345",
        );
        let history_count = count_occurrences(&derived_history, "UNIQUE_TOOLRESULT_MARKER_12345");
        let total = prompt_count + history_count;

        assert_eq!(
            total,
            1,
            "Resumption ToolResult must appear exactly once across (prompt + history); \
             got {prompt_count} in prompt + {history_count} in history = {total}. \
             prompt={prompt:?}, history_len={}",
            derived_history.len()
        );

        // The override must have been consumed (one-shot semantics).
        // The next iteration would fall through to get_agent_history().
        assert!(
            history_override.is_none(),
            "history_override must be cleared after .take() in derive_history_for_iteration"
        );
    }

    /// No-regression: when there is no override, `derive_history_for_iteration`
    /// must fall through to `StateManager::get_agent_history()`.
    #[test]
    fn derive_history_for_iteration_falls_through_to_state_manager_when_no_override() {
        let sm = Arc::new(StateManager::new());
        sm.add_user_message("hello".to_string());
        sm.add_assistant_message("hi there".to_string());

        let mut history_override: Option<Vec<rig_core::completion::message::Message>> = None;
        let sm_opt: Option<Arc<StateManager>> = Some(sm.clone());
        let derived = derive_history_for_iteration(&mut history_override, &sm_opt);

        // Should match get_agent_history() exactly.
        let expected = sm.get_agent_history();
        assert_eq!(
            derived.len(),
            expected.len(),
            "without override, must return get_agent_history() verbatim"
        );
    }

    /// No-regression: empty `(None, None)` returns an empty Vec.
    #[test]
    fn derive_history_for_iteration_handles_no_state_manager() {
        let mut history_override: Option<Vec<rig_core::completion::message::Message>> = None;
        let sm_opt: Option<Arc<StateManager>> = None;
        let derived = derive_history_for_iteration(&mut history_override, &sm_opt);
        assert!(derived.is_empty());
    }

    /// **The bug.** This is the *original* data-shape pin, kept as a
    /// no-regression for the underlying mismatch between
    /// `build_resumption_for_compaction` (returns the full resumption
    /// shape) and `get_agent_history` (only strips trailing User).
    /// If anyone "fixes" `get_agent_history` to also strip trailing
    /// ToolResult — masking the lib.rs bug instead of fixing the
    /// wiring — this test stays meaningful: it documents the
    /// asymmetry and the per-method contracts. The actual fix lives
    /// at the wiring layer and is pinned by
    /// `derive_history_for_iteration_does_not_duplicate_toolresult_on_resume`.
    #[test]
    fn naive_get_agent_history_after_resumption_demonstrates_the_bug() {
        let sm = StateManager::new();
        sm.add_user_message("list files".to_string());
        sm.add_assistant_message("I'll run ls for you".to_string());
        sm.add_tool_call(
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            Some("call_1".to_string()),
        );
        sm.add_tool_result(
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            "UNIQUE_TOOLRESULT_MARKER_12345".to_string(),
            Some("call_1".to_string()),
        );

        let (prompt, _) = sm
            .build_resumption_for_compaction()
            .expect("non-empty conversation must produce a resumption");
        // This is the BROKEN pattern (re-derive history from state).
        // get_agent_history() doesn't strip trailing non-User messages,
        // so the ToolResult stays in history AND is the prompt.
        let history_naive = sm.get_agent_history();

        let prompt_count = count_occurrences(
            std::slice::from_ref(&prompt),
            "UNIQUE_TOOLRESULT_MARKER_12345",
        );
        let history_count = count_occurrences(&history_naive, "UNIQUE_TOOLRESULT_MARKER_12345");

        // This documents the bug: the broken pattern produces a
        // duplicate. The fix (use the resumption_history from the
        // tuple, not re-derive) is pinned by the test above.
        assert_eq!(prompt_count, 1, "prompt has the marker once");
        assert_eq!(
            history_count, 1,
            "naive get_agent_history() ALSO has the marker (the source of the duplication)"
        );
    }
}
