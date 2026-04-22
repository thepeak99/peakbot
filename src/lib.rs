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
    #[allow(unused)]
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
        use crate::ui::app_state::NotificationKind;

        if let Some(ref sm) = self.state_manager {
            match sm.force_compact().await {
                Some(result) => {
                    sm.push_notification(
                        format!(
                            "Context compacted: {} → {} messages, {} discarded",
                            result.original_count, result.compacted_count, result.num_discarded
                        ),
                        NotificationKind::Info,
                    );
                }
                None => {
                    sm.push_notification("Nothing to compact.".to_string(), NotificationKind::Info);
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
                    // Add user message to chat AND persist (StateManager handles persistence)
                    if let Some(ref sm) = state_manager {
                        sm.add_user_message(msg.clone());
                    }

                    // If agent is running, interrupt it first
                    if state_manager.as_ref().is_some_and(|sm| sm.is_running()) {
                        session_hook.request_stop();
                    }

                    // Queue the new message for the agent
                    msg_tx.send(QueueMessage::UserMessage(msg)).await.ok();
                }

                UiAction::ExecuteCommand(cmd) => {
                    if cmd == "/stop" {
                        // Only stop if agent is actually running
                        if state_manager.as_ref().is_some_and(|sm| sm.is_running()) {
                            session_hook.request_stop();
                            msg_tx.send(QueueMessage::StopMarker).await.ok();
                            if let Some(ref sm) = state_manager {
                                sm.push_notification(
                                    "Stop requested...".to_string(),
                                    crate::ui::app_state::NotificationKind::Info,
                                );
                            }
                        }
                    } else {
                        // Queue command for agent loop
                        msg_tx.send(QueueMessage::Command(cmd)).await.ok();
                    }
                }

                UiAction::RequestStop => {
                    // Only stop if agent is actually running
                    if state_manager.as_ref().is_some_and(|sm| sm.is_running()) {
                        session_hook.request_stop();
                        msg_tx.send(QueueMessage::StopMarker).await.ok();
                        if let Some(ref sm) = state_manager {
                            sm.push_notification(
                                "Stop requested...".to_string(),
                                crate::ui::app_state::NotificationKind::Info,
                            );
                        }
                    }
                }

                UiAction::Exit => {
                    break;
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

                    let result =
                        Self::process_message_internal(&content, &state_manager, &agent, &config)
                            .await;

                    // Mark as done
                    if let Some(ref sm) = state_manager {
                        sm.set_running(false);
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
                        sm.push_notification(
                            "Agent stopped by user".to_string(),
                            crate::ui::app_state::NotificationKind::Info,
                        );
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

    /// Internal process_message - returns CompletionResult instead of handling directly
    async fn process_message_internal(
        msg: &str,
        state_manager: &Option<Arc<StateManager>>,
        agent: &Arc<DynAgent>,
        config: &Config,
    ) -> CompletionResult {
        let mut retry_count = 0;
        let current_msg = msg.to_string();

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
                .prompt_with_history(&current_msg, &mut history)
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

                Err(_) => {
                    if retry_count == config.retry().max_retries {
                        if let Some(sm) = state_manager {
                            sm.push_notification(
                                "Max number of retries exceeded".to_string(),
                                crate::ui::app_state::NotificationKind::Error,
                            );
                        }
                        return CompletionResult::Error;
                    }
                    if let Some(sm) = state_manager {
                        sm.push_notification(
                            "Retrying...".to_string(),
                            crate::ui::app_state::NotificationKind::Warning,
                        );
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
        use crate::ui::app_state::NotificationKind;

        let cmd_lower = cmd.to_lowercase();

        match cmd_lower.as_str() {
            "/stats" | "/reset" | "/context" | "/compact" | "/conversations" | "/history" => {
                // These commands return data that UI can access via StateManager
                // No notification needed - UI pulls this data
            }
            "/new" => {
                // Create new conversation via StateManager (single source of truth)
                if let Some(sm) = state_manager {
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
                        sm.push_notification(
                            format!("Conversation saved: {}", conv.name),
                            NotificationKind::Success,
                        );
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
                                sm.push_notification("Invalid conversation ID. Use /conversations to see available IDs.".to_string(), NotificationKind::Error);
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
                                sm.push_notification(
                                    format!("Failed to load conversation: {}", e),
                                    NotificationKind::Error,
                                );
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
                                sm.push_notification(
                                    "Invalid conversation ID.".to_string(),
                                    NotificationKind::Error,
                                );
                            }
                            return;
                        }
                    };
                    if let Some(sm) = state_manager {
                        match sm.delete_conversation(id) {
                            Ok(_) => {
                                sm.push_notification(
                                    "Conversation deleted.".to_string(),
                                    NotificationKind::Success,
                                );
                            }
                            Err(e) => {
                                sm.push_notification(
                                    format!("Failed to delete: {}", e),
                                    NotificationKind::Error,
                                );
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
                                    sm.push_notification(
                                        "Invalid conversation ID.".to_string(),
                                        NotificationKind::Error,
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
                                    sm.push_notification(
                                        format!("Export failed: {}", e),
                                        NotificationKind::Error,
                                    );
                                }
                            }
                        } else {
                            tracing::warn!("State manager not available for /export command");
                        }
                    } else {
                        if let Some(sm) = state_manager {
                            sm.push_notification(
                                "Usage: /export <id> <json|markdown>".to_string(),
                                NotificationKind::Info,
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
                                sm.push_notification(
                                    format!("Conversation renamed to: {}", name),
                                    NotificationKind::Success,
                                );
                            }
                            Err(e) => {
                                sm.push_notification(
                                    format!("Failed to rename: {}", e),
                                    NotificationKind::Error,
                                );
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
    let mut transport_config =
        StreamableHttpClientTransportConfig::with_uri(url.clone());

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
