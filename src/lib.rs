//! PeakBot library - Core functionality for connecting to MCP servers and managing tools.

mod config;
mod context_manager;
mod conversation;
mod conversation_manager;
mod hooks;
#[cfg(feature = "mock")]
pub mod mock;
mod pipeline;
mod providers;
mod skills;
pub mod state;
pub mod storage;
pub mod test_runner;
mod tools;
pub mod ui;
pub mod vision;

pub use config::{
    AgentDefinition, BashConfig, Config, ContextConfig, ConversationConfig, McpServerConfig,
    McpTransportType, OllamaConfig, OpenRouterConfig, PipelineConfig, ProviderConfig, ProviderType,
    RetryConfig, SearXngConfig,
};
pub use context_manager::CompactionResult;
use context_manager::ContextManager;
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

use rig::completion::{Message, PromptError};
use rig::tool::ToolDyn;
use rig::tool::rmcp::McpTool;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{TokioChildProcess, streamable_http_client::StreamableHttpClientTransport};
pub use skills::{SkillRegistry, load_default_skills};
pub use state::StateManager;
pub use storage::{ConversationStorage, FileStorage};
pub use test_runner::{CompactionInfo, TestRunner};
pub use tools::{
    BashTool, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool, SearchTool, ThinkTool,
    TodoArgs, TodoItem, TodoStatus, TodoTool,
};
pub use ui::{Ui, UiAction};

use anyhow::{Result, anyhow};
use rmcp::service::{RoleClient, RunningService, ServiceExt};
use std::process::Stdio;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::debug;

/// Message types for internal queue between event loop and agent loop
enum QueueMessage {
    UserMessage(String),
    Command(String),
    StopMarker, // Signals that stop was requested
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
    if trimmed.starts_with('/') {
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

/// Build the system prompt dynamically with environment information
pub fn build_system_prompt(skills: &SkillRegistry) -> String {
    let mut prompt = SYSTEM_PROMPT.to_string();

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unknown".to_string());

    let current_time = chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S %Z")
        .to_string();

    // Load agents.md with case-insensitive filename matching
    let agents_md_content = std::fs::read_dir(".")
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

    let env_info = format!(
        "\n# Environment Information\n\n- **Current Working Directory**: {}\n- **Current Time**: {}\n",
        cwd, current_time
    );

    prompt.push_str(&skills_section);
    prompt.push_str(&env_info);
    prompt.push_str(&agents_md_content);

    debug!("System prompt:\n {}", prompt);

    prompt
}

/// Convert stored conversation messages to rig Messages for LLM chat history
pub fn convert_conversation_to_rig_messages(conv: &Conversation) -> Vec<Message> {
    use crate::conversation::Message as StoredMessage;
    use rig::completion::message::{
        AssistantContent, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent, UserContent,
    };
    use rig::one_or_many::OneOrMany;

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
                        content: OneOrMany::one(ToolResultContent::Text(Text {
                            text: result.clone(),
                        })),
                    })),
                });
            }
        }
    }

    messages
}

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
}

impl AgentRunner {
    pub fn new(
        agent: DynAgent,
        config: Config,
        provider_info: ProviderInfo,
        skills: SkillRegistry,
        event_receiver: Option<mpsc::UnboundedReceiver<AgentEvent>>,
        state_manager: Option<Arc<StateManager>>,
        session_hook: Arc<SessionHook>,
    ) -> Self {
        let agent = Arc::new(agent);
        let system_prompt = build_system_prompt(&skills);

        // Initialize ContextManager inside StateManager (StateManager owns it)
        if let Some(ref sm) = state_manager {
            // Create a tool-free compaction model from the same provider
            let compaction_model = crate::providers::create_compaction_model(
                &config.provider,
                config.context.compaction_model.as_deref(),
            )
            .ok()
            .map(Arc::new);

            let cm = ContextManager::new(
                config.context.clone(),
                provider_info.model.as_str(),
                sm.clone(),
                compaction_model,
            );
            sm.init_context_manager(cm, system_prompt.clone());
        }

        Self {
            agent,
            config,
            provider_info,
            skills,
            state_manager,
            session_hook,
            event_receiver,
        }
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

        // Completion notifications back to event loop
        let (completion_tx, _completion_rx) =
            tokio::sync::broadcast::channel::<CompletionResult>(8);

        // Extract fields we need to pass to spawned loops
        let state_manager = self.state_manager.clone();
        let session_hook = self.session_hook.clone();
        let config_model = self.config.model().to_string();
        let agent = self.agent.clone();
        let state_manager_for_agent = self.state_manager.clone();
        let config_for_agent = self.config.clone();
        let event_receiver = self.event_receiver.take();
        let provider_info = Arc::new(self.provider_info.clone());

        // Spawn event processor task (Phase 2: wire the event channel)
        let event_processor_handle = tokio::spawn({
            let state_manager = state_manager.clone();

            async move {
                if let Some(mut receiver) = event_receiver {
                    while let Some(event) = receiver.recv().await {
                        // Process event and update StateManager — the Controller (AgentRunner)
                        // decides how events affect state, not the Model (StateManager)
                        Self::process_event_for_ui(&state_manager, event);
                    }
                }
            }
        });

        // Spawn the two loops
        let event_handle = tokio::spawn({
            let msg_tx = msg_tx.clone();
            let completion_tx = completion_tx.clone();

            async move {
                Self::event_loop(
                    action_receiver,
                    msg_tx,
                    completion_tx,
                    state_manager,
                    session_hook,
                    config_model,
                    provider_info,
                )
                .await;
            }
        });

        let agent_handle = tokio::spawn({
            let msg_rx = tokio::sync::Mutex::new(msg_rx);
            let completion_tx = completion_tx.clone();

            async move {
                Self::agent_loop(
                    msg_rx,
                    completion_tx,
                    state_manager_for_agent,
                    agent,
                    config_for_agent,
                )
                .await;
            }
        });

        // Wait for event loop to exit (View closed)
        event_handle.await.ok();
        event_processor_handle.abort();
        agent_handle.abort();
    }

    /// Event loop - receives UiActions from View, queues messages for agent loop
    async fn event_loop(
        mut action_receiver: mpsc::UnboundedReceiver<UiAction>,
        msg_tx: tokio::sync::mpsc::Sender<QueueMessage>,
        _completion_tx: tokio::sync::broadcast::Sender<CompletionResult>,
        state_manager: Option<Arc<StateManager>>,
        session_hook: Arc<SessionHook>,
        config_model: String,
        provider_info: Arc<ProviderInfo>,
    ) {
        // Initialize conversation in StateManager (single source of truth)
        if let Some(ref sm) = state_manager {
            let name = format!(
                "Conversation {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M")
            );
            sm.create_conversation(name, config_model);
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
                            if state_manager.as_ref().is_some_and(|sm| sm.is_running()) {
                                session_hook.request_stop();
                                msg_tx.send(QueueMessage::StopMarker).await.ok();
                                if let Some(ref sm) = state_manager {
                                    sm.set_status(Some("Stop requested...".to_string()));
                                }
                            }
                        }
                        SubmitKind::Command(cmd) => {
                            // Dispatched by agent_loop via process_command_internal.
                            msg_tx.send(QueueMessage::Command(cmd)).await.ok();
                        }
                        SubmitKind::UserMessage(text) => {
                            if let Some(ref sm) = state_manager {
                                sm.add_user_message(text.clone());
                            }
                            // If agent is running, interrupt it first.
                            if state_manager.as_ref().is_some_and(|sm| sm.is_running()) {
                                session_hook.request_stop();
                            }
                            msg_tx.send(QueueMessage::UserMessage(text)).await.ok();
                        }
                        SubmitKind::MultimodalMessage { text, attachments } => {
                            // Capability guardrail — fail loud rather than drop images silently.
                            if !provider_info.supports_vision {
                                if let Some(ref sm) = state_manager {
                                    sm.add_system_message(format!(
                                        "❌ Model `{}` does not support vision. Switch to a \
                                         vision-capable model in config.yaml (e.g. \
                                         `anthropic/claude-3.5-sonnet`, `gpt-4o`, \
                                         `google/gemini-2.0-flash-001`).",
                                        provider_info.model
                                    ));
                                }
                                continue;
                            }
                            if let Some(ref sm) = state_manager {
                                sm.add_user_message_with_attachments(text.clone(), attachments);
                            }
                            if state_manager.as_ref().is_some_and(|sm| sm.is_running()) {
                                session_hook.request_stop();
                            }
                            // The String payload here is a display marker only —
                            // `process_message_internal` rebuilds the current-turn
                            // `Message` from `StateManager` state, so images are
                            // preserved even though this channel carries text.
                            msg_tx.send(QueueMessage::UserMessage(text)).await.ok();
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
                    // Only stop if agent is actually running
                    if state_manager.as_ref().is_some_and(|sm| sm.is_running()) {
                        session_hook.request_stop();
                        msg_tx.send(QueueMessage::StopMarker).await.ok();
                        if let Some(ref sm) = state_manager {
                            sm.set_status(Some("Stop requested...".to_string()));
                        }
                    }
                }
            }
        }
    }

    /// Agent loop - processes messages from event loop, sends completions back
    async fn agent_loop(
        msg_rx: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<QueueMessage>>,
        completion_tx: tokio::sync::broadcast::Sender<CompletionResult>,
        state_manager: Option<Arc<StateManager>>,
        agent: Arc<DynAgent>,
        config: Config,
    ) {
        loop {
            // Wait for a message
            let msg = msg_rx.lock().await.recv().await;

            match msg {
                Some(QueueMessage::UserMessage(content)) => {
                    // Mark as running via StateManager (broadcasts to all UIs)
                    if let Some(ref sm) = state_manager {
                        sm.set_running(true);
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
                            rig::completion::message::Message::from(content.as_str())
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
                }

                Some(QueueMessage::Command(cmd)) => {
                    if let Some(ref sm) = state_manager {
                        sm.set_running(true);
                    }
                    Self::process_command_internal(&cmd, &state_manager, &config).await;
                    if let Some(ref sm) = state_manager {
                        sm.set_running(false);
                    }
                    completion_tx.send(CompletionResult::CommandDone).ok();
                }

                Some(QueueMessage::StopMarker) => {
                    // This is just an acknowledgment that stop was requested
                    // The actual stopping happened in process_message_internal
                    if let Some(ref sm) = state_manager {
                        sm.set_status(None);
                        sm.add_system_message("Agent stopped by user".to_string());
                    }
                    completion_tx.send(CompletionResult::Stopped).ok();
                }

                None => {
                    // Channel closed, exit
                    break;
                }
            }
        }
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
    async fn process_message_internal(
        current_turn: rig::completion::message::Message,
        state_manager: &Option<Arc<StateManager>>,
        agent: &Arc<DynAgent>,
        config: &Config,
    ) -> CompletionResult {
        let mut retry_count = 0;

        loop {
            // Compaction is handled automatically by StateManager when messages
            // are added (add_user_message / add_assistant_message). No explicit
            // check needed here.

            // Call the agent with history from StateManager (single source of truth)
            let mut history = state_manager
                .as_ref()
                .map(|sm| sm.get_agent_history())
                .unwrap_or_default();
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
                    }

                    return CompletionResult::Success;
                }

                Err(PromptError::PromptCancelled { reason, .. }) => {
                    if reason == "stop" {
                        return CompletionResult::Stopped;
                    }
                    // Any other cancellation reason — return error instead of
                    // silently looping (which previously caused an infinite loop).
                    return CompletionResult::Error;
                }

                Err(e) => {
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
            "/stats" | "/context" | "/compact" | "/conversations" | "/history" => {
                // These commands return data that UI can access via StateManager
                // No system message needed — UI pulls this data
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
                // slot; the derived views (chat.messages, session stats, todo
                // list) are untouched. Without also clearing them the agent's
                // next turn still sees the prior history via
                // `get_agent_history()`, the token/cost counters keep
                // accumulating, AND the previous conversation's todos linger
                // in the side panel — i.e. /new would lie about starting
                // fresh. Clear all three explicitly.
                if let Some(sm) = state_manager {
                    sm.clear_chat();
                    sm.reset_stats();
                    sm.clear_all_todos();
                    let name = format!(
                        "Conversation {}",
                        chrono::Local::now().format("%Y-%m-%d %H:%M")
                    );
                    sm.create_conversation(name, config.model().to_string());
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
                if let Some(id_str) = cmd.strip_prefix("/load ") {
                    let id = match uuid::Uuid::parse_str(id_str) {
                        Ok(id) => id,
                        Err(_) => {
                            if let Some(sm) = state_manager {
                                sm.add_system_message("❌ Invalid conversation ID. Use /conversations to see available IDs.".to_string());
                            }
                            return;
                        }
                    };
                    if let Some(sm) = state_manager {
                        match sm.load_conversation(id) {
                            Ok(()) => {
                                if let Some(conv) = sm.get_current_conversation() {
                                    sm.add_system_message(format!(
                                        "Loaded conversation: '{}'",
                                        conv.name
                                    ));
                                }
                            }
                            Err(e) => {
                                sm.add_system_message(format!(
                                    "❌ Failed to load conversation: {}",
                                    e
                                ));
                            }
                        }
                    } else {
                        tracing::warn!("State manager not available for /load command");
                    }
                }
            }
            _ if cmd_lower.starts_with("/delete ") => {
                if let Some(id_str) = cmd.strip_prefix("/delete ") {
                    let id = match uuid::Uuid::parse_str(id_str) {
                        Ok(id) => id,
                        Err(_) => {
                            if let Some(sm) = state_manager {
                                sm.add_system_message("❌ Invalid conversation ID.".to_string());
                            }
                            return;
                        }
                    };
                    if let Some(sm) = state_manager {
                        match sm.delete_conversation(id) {
                            Ok(_) => {
                                sm.add_system_message("Conversation deleted.".to_string());
                            }
                            Err(e) => {
                                sm.add_system_message(format!("❌ Failed to delete: {}", e));
                            }
                        }
                    } else {
                        tracing::warn!("State manager not available for /delete command");
                    }
                }
            }
            _ if cmd_lower.starts_with("/export ") => {
                if let Some(args) = cmd.strip_prefix("/export ") {
                    let parts: Vec<&str> = args.splitn(2, ' ').collect();
                    if parts.len() == 2 {
                        let id_str = parts[0];
                        let format = parts[1].to_lowercase();
                        let id = match uuid::Uuid::parse_str(id_str) {
                            Ok(id) => id,
                            Err(_) => {
                                if let Some(sm) = state_manager {
                                    sm.add_system_message(
                                        "❌ Invalid conversation ID.".to_string(),
                                    );
                                }
                                return;
                            }
                        };
                        if let Some(sm) = state_manager {
                            match sm.export_conversation(id, &format) {
                                Ok(output) => {
                                    sm.add_system_message(format!("Export:\n{}", output));
                                }
                                Err(e) => {
                                    sm.add_system_message(format!("❌ Export failed: {}", e));
                                }
                            }
                        } else {
                            tracing::warn!("State manager not available for /export command");
                        }
                    } else {
                        if let Some(sm) = state_manager {
                            sm.add_system_message(
                                "Usage: /export <id> <json|markdown>".to_string(),
                            );
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
            _ => {
                // Unknown command — for now, let the agent handle it
                // The agent will respond via StateManager
            }
        }
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

    // Build the streamable-http config with optional bearer token + custom headers.
    let mut transport_config = StreamableHttpClientTransportConfig::with_uri(url.clone());

    if let Some(token) = config.auth_token.as_ref()
        && !token.is_empty()
    {
        transport_config = transport_config.auth_header(token.clone());
    }

    if let Some(headers) = config.headers.as_ref() {
        let mut parsed: std::collections::HashMap<http::HeaderName, http::HeaderValue> =
            std::collections::HashMap::with_capacity(headers.len());
        for (k, v) in headers {
            match (
                http::HeaderName::try_from(k.as_str()),
                http::HeaderValue::try_from(v.as_str()),
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

    let transport = StreamableHttpClientTransport::from_config(transport_config);

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

        sm.create_conversation("old one".to_string(), "test-model".to_string());
        let old_id = sm
            .get_current_conversation_id()
            .expect("seeded conversation");

        AgentRunner::process_command_internal("/new", &Some(sm.clone()), &config).await;

        let new_id = sm
            .get_current_conversation_id()
            .expect("/new must create a current conversation");
        assert_ne!(new_id, old_id, "/new must produce a fresh conversation id");
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
            headers: None,
            enabled: true,
        };

        let result = connect_mcp_server(&config).await;
        assert!(result.is_err(), "Expected error for invalid command");
    }

    #[tokio::test]
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
            headers: None,
            enabled: true,
        };
        assert!(config.validate().is_err());
    }
}
