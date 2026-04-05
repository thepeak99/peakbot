//! PeakBot library - Core functionality for connecting to MCP servers and managing tools.

mod config;
mod context_manager;
mod conversation;
mod conversation_manager;
mod hooks;
mod pipeline;
mod providers;
mod skills;
mod tools;
pub mod ui;

pub use config::{
    AgentDefinition, BashConfig, Config, ContextConfig, ConversationConfig, McpServerConfig,
    McpTransportType, OllamaConfig, OpenRouterConfig, PipelineConfig, ProviderConfig, ProviderType,
    RetryConfig, SearXngConfig,
};
pub use context_manager::{CompactionResult, ContextManager};
pub use conversation::{
    Conversation, ConversationMetadata, ConversationSummary, Message as ConversationMessage,
};
pub use conversation_manager::{ConversationManager, ConversationManagerConfig};
pub use hooks::{
    AgentEvent, CostHandler, EventChannel, EventHandler, EventProcessor, ModelPricing, SessionHook,
    SessionStats, StateManagerHandler, TokenUsage, create_event_channel, fetch_model_pricing,
};
pub use pipeline::{DelegateTool, SubAgentRegistry};
pub use providers::{CostTracker, DynAgent, ProviderInfo, create_provider};
use rig::completion::{Message, PromptError};
use rig::tool::ToolDyn;
use rig::tool::rmcp::McpTool;
use rmcp::transport::{TokioChildProcess, streamable_http_client::StreamableHttpClientTransport};
pub use skills::{SkillRegistry, load_default_skills};
pub use tools::{
    BashTool, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool, SearchTool, ThinkTool,
    TodoList, TodoStatus, TodoTool,
};
pub use ui::{Ui, UiAction};

use anyhow::{Result, anyhow};
use rmcp::service::ServiceExt;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
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
    Success(String),
    Stopped,
    Error(String),
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

    let mut messages = Vec::new();

    for msg in &conv.messages {
        match msg {
            StoredMessage::User { content, .. } => {
                messages.push(Message::user(content.clone()));
            }
            StoredMessage::Assistant { content, .. } => {
                messages.push(Message::assistant(content.clone()));
            }
            StoredMessage::ToolCall { .. } => {}
            StoredMessage::ToolResult { .. } => {}
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
    context_manager: Option<ContextManager>,
    conversation_manager: Option<Arc<Mutex<ConversationManager>>>,
    system_prompt: String,
    cost_tracker: CostTracker,
    todo_state: Option<Arc<Mutex<TodoList>>>,
    state_manager: Option<Arc<ui::StateManager>>,
    // Shared session hook for interrupt/queue state
    session_hook: Arc<SessionHook>,
    // Retained for streaming output handler (view concern, set up by main.rs)
    _event_receiver: Option<mpsc::UnboundedReceiver<AgentEvent>>,
}

impl AgentRunner {
    pub fn new(
        agent: DynAgent,
        config: Config,
        provider_info: ProviderInfo,
        skills: SkillRegistry,
        cost_tracker: CostTracker,
        todo_state: Option<Arc<Mutex<TodoList>>>,
        event_receiver: Option<mpsc::UnboundedReceiver<AgentEvent>>,
        state_manager: Option<Arc<ui::StateManager>>,
        session_hook: Arc<SessionHook>,
    ) -> Self {
        let agent = Arc::new(agent);
        let system_prompt = build_system_prompt(&skills);
        let system_prompt_tokens = system_prompt.len() / 4;

        let context_manager = Some(ContextManager::new(
            config.context.clone(),
            provider_info.model.as_str(),
            cost_tracker.get_session_stats(),
            system_prompt_tokens,
            Some(agent.clone()),
        ));

        let conversation_manager = if config.conversation_enabled() {
            match ConversationManager::new(ConversationManagerConfig {
                auto_save: true,
                storage_dir: config.conversation_storage_dir(),
                max_conversations: config.conversation_max(),
                auto_resume: config.conversation_auto_resume(),
            }) {
                Ok(cm) => Some(Arc::new(Mutex::new(cm))),
                Err(e) => {
                    tracing::warn!("Failed to initialize conversation manager: {}", e);
                    None
                }
            }
        } else {
            None
        };

        Self {
            agent,
            config,
            provider_info,
            skills,
            context_manager,
            conversation_manager,
            system_prompt,
            cost_tracker,
            todo_state,
            state_manager,
            session_hook,
            _event_receiver: event_receiver,
        }
    }

    /// Reset session stats — backend concern
    pub fn reset_stats(&self) {
        self.cost_tracker.reset_stats();
    }

    /// Sync current state to StateManager (Model)
    fn sync_state_to_manager(&self) {
        if let Some(ref sm) = self.state_manager {
            let stats = self.cost_tracker.get_session_stats();
            sm.update_stats(&stats);

            if let Some(ref todo) = self.todo_state {
                if let Ok(list) = todo.lock() {
                    sm.update_todo(&list);
                }
            }
        }
    }

    /// Force context compaction
    pub async fn force_compact(&mut self, chat_history: &mut Vec<Message>) {
        use crate::ui::app_state::NotificationKind;

        if let Some(ref mut cm) = self.context_manager {
            match cm.compact(chat_history, &self.system_prompt).await {
                Ok(result) => {
                    if let Some(ref sm) = self.state_manager {
                        sm.push_notification(
                            format!(
                                "Context compacted: {} → {} messages, {} discarded",
                                result.original_count, result.compacted_count, result.num_discarded
                            ),
                            NotificationKind::Info,
                        );
                    }
                }
                Err(e) => {
                    if let Some(ref sm) = self.state_manager {
                        sm.push_notification(
                            format!("Error compacting context: {}", e),
                            NotificationKind::Error,
                        );
                    }
                }
            }
        } else if let Some(ref sm) = self.state_manager {
            sm.push_notification(
                "Context compaction is not enabled.".to_string(),
                NotificationKind::Warning,
            );
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

        // Shared chat history (needed by both loops)
        let chat_history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));

        // Spawn event processing task for live state updates
        if let Some(receiver) = self._event_receiver.take() {
            let mut handlers: Vec<Arc<dyn EventHandler>> = Vec::new();

            let stats = self.cost_tracker.get_session_stats();
            let pricing = self.cost_tracker.get_pricing().clone();
            handlers.push(Arc::new(CostHandler::new(pricing, stats)));

            // StateManagerHandler forwards live events (tool calls, tool results) to StateManager
            if let Some(ref sm) = self.state_manager {
                handlers.push(Arc::new(StateManagerHandler::new(sm.clone())));
            }

            tokio::spawn(async move {
                let mut processor = EventProcessor::new(receiver, handlers);
                processor.run().await;
            });
        }

        // Extract fields we need to pass to spawned loops
        let conversation_manager = self.conversation_manager.clone();
        let state_manager = self.state_manager.clone();
        let session_hook = self.session_hook.clone();
        let config_model = self.config.model().to_string();
        let system_prompt = self.system_prompt.clone();
        let context_manager = self.context_manager.take();
        let conversation_manager_for_agent = self.conversation_manager.clone();
        let agent = self.agent.clone();
        let state_manager_for_agent = self.state_manager.clone();
        let config_for_agent = self.config.clone();

        // Spawn the two loops
        let event_handle = tokio::spawn({
            let msg_tx = msg_tx.clone();
            let completion_tx = completion_tx.clone();
            let chat_history = chat_history.clone();

            async move {
                Self::event_loop(
                    action_receiver,
                    msg_tx,
                    completion_tx,
                    chat_history,
                    conversation_manager,
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
            let chat_history = chat_history.clone();

            async move {
                Self::agent_loop(
                    msg_rx,
                    completion_tx,
                    chat_history,
                    context_manager,
                    conversation_manager_for_agent,
                    state_manager_for_agent,
                    agent,
                    config_for_agent,
                    system_prompt,
                )
                .await;
            }
        });

        // Wait for event loop to exit (View closed)
        event_handle.await.ok();
        agent_handle.abort();
    }

    /// Event loop - receives UiActions from View, queues messages for agent loop
    async fn event_loop(
        mut action_receiver: mpsc::UnboundedReceiver<UiAction>,
        msg_tx: tokio::sync::mpsc::Sender<QueueMessage>,
        _completion_tx: tokio::sync::broadcast::Sender<CompletionResult>,
        _chat_history: Arc<tokio::sync::Mutex<Vec<Message>>>,
        conversation_manager: Option<Arc<Mutex<ConversationManager>>>,
        state_manager: Option<Arc<ui::StateManager>>,
        session_hook: Arc<SessionHook>,
        config_model: String,
    ) {
        // Initialize conversation
        if let Some(ref cm) = conversation_manager {
            let name = format!(
                "Conversation {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M")
            );
            let _ = cm.lock().unwrap().create_new(name, config_model);
        }

        while let Some(action) = action_receiver.recv().await {
            match action {
                UiAction::SendMessage(msg) => {
                    // Auto-save user message
                    if let Some(ref cm) = conversation_manager {
                        let _ = cm.lock().unwrap().add_user_message(msg.clone());
                    }
                    // Add user message to chat for live rendering
                    if let Some(ref sm) = state_manager {
                        use crate::ui::app_state::ChatMessage;
                        sm.update_chat(ChatMessage::user(msg.clone()));
                    }

                    // If agent is running, interrupt it first
                    if state_manager.as_ref().map_or(false, |sm| sm.is_running()) {
                        session_hook.request_stop();
                    }

                    // Queue the new message for the agent
                    msg_tx.send(QueueMessage::UserMessage(msg)).await.ok();
                }

                UiAction::ExecuteCommand(cmd) => {
                    if cmd == "/stop" {
                        // Only stop if agent is actually running
                        if state_manager.as_ref().map_or(false, |sm| sm.is_running()) {
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
                    if state_manager.as_ref().map_or(false, |sm| sm.is_running()) {
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
        chat_history: Arc<tokio::sync::Mutex<Vec<Message>>>,
        mut context_manager: Option<ContextManager>,
        conversation_manager: Option<Arc<Mutex<ConversationManager>>>,
        state_manager: Option<Arc<ui::StateManager>>,
        agent: Arc<DynAgent>,
        config: Config,
        system_prompt: String,
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

                    let result = Self::process_message_internal(
                        &content,
                        chat_history.clone(),
                        &mut context_manager,
                        &conversation_manager,
                        &state_manager,
                        &agent,
                        &config,
                        &system_prompt,
                    )
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
                    Self::process_command_internal(
                        &cmd,
                        chat_history.clone(),
                        &mut context_manager,
                        &conversation_manager,
                        &state_manager,
                        &agent,
                        &config,
                        &system_prompt,
                    )
                    .await;
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

    /// Internal process_message - returns CompletionResult instead of handling directly
    async fn process_message_internal(
        msg: &str,
        chat_history: Arc<tokio::sync::Mutex<Vec<Message>>>,
        context_manager: &mut Option<ContextManager>,
        conversation_manager: &Option<Arc<Mutex<ConversationManager>>>,
        state_manager: &Option<Arc<ui::StateManager>>,
        agent: &Arc<DynAgent>,
        config: &Config,
        system_prompt: &str,
    ) -> CompletionResult {
        let mut retry_count = 0;
        let current_msg = msg.to_string();

        loop {
            // Context compaction check
            if let Some(cm) = context_manager.as_mut() {
                let mut history = chat_history.lock().await;
                if cm.needs_compaction(&history) {
                    // Set status message for UI
                    if let Some(sm) = state_manager {
                        sm.set_status(Some("Compacting context...".to_string()));
                    }
                    // Compact and continue
                    match cm.compact(&mut history, system_prompt).await {
                        Ok(result) => {
                            if let Some(sm) = state_manager {
                                sm.push_notification(
                                    format!(
                                        "Context compacted: {} → {} messages, {} discarded",
                                        result.original_count,
                                        result.compacted_count,
                                        result.num_discarded
                                    ),
                                    crate::ui::app_state::NotificationKind::Info,
                                );
                                sm.set_status(None);
                            }
                        }
                        Err(e) => {
                            if let Some(sm) = state_manager {
                                sm.push_notification(
                                    format!("Context compaction failed: {}", e),
                                    crate::ui::app_state::NotificationKind::Warning,
                                );
                                sm.set_status(None);
                            }
                        }
                    }
                }
            }

            // Call the agent
            let history = chat_history.lock().await;
            let result = agent
                .as_ref()
                .prompt_with_history(&current_msg, &history)
                .await;

            match result {
                Ok(response) => {
                    // Add assistant response to chat for live rendering
                    if let Some(sm) = state_manager.as_ref() {
                        use crate::ui::app_state::ChatMessage;
                        sm.update_chat(ChatMessage::agent(response.clone()));
                        sm.set_final_broadcast(true);
                    }

                    // Sync stats and todo to StateManager
                    // (simplified - would need cost_tracker and todo_state)

                    // Save conversation
                    if let Some(cm) = conversation_manager.as_ref() {
                        if let Err(e) = cm.lock().unwrap().save() {
                            tracing::warn!("Failed to save conversation: {}", e);
                        }
                    }

                    return CompletionResult::Success(response);
                }

                Err(PromptError::PromptCancelled { reason, .. }) => {
                    // On stop, just return Stopped. Let the loop handle the StopMarker.
                    if reason == "stop" {
                        return CompletionResult::Stopped;
                    }
                    // Other cancellations (compact, etc) - loop continues
                }

                Err(_) => {
                    if retry_count == config.retry().max_retries {
                        if let Some(sm) = state_manager {
                            sm.push_notification(
                                "Max number of retries exceeded".to_string(),
                                crate::ui::app_state::NotificationKind::Error,
                            );
                        }
                        return CompletionResult::Error(
                            "Maximum number of retries exceeded".to_string(),
                        );
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
        chat_history: Arc<tokio::sync::Mutex<Vec<Message>>>,
        _context_manager: &mut Option<ContextManager>,
        conversation_manager: &Option<Arc<Mutex<ConversationManager>>>,
        state_manager: &Option<Arc<ui::StateManager>>,
        _agent: &Arc<DynAgent>,
        config: &Config,
        _system_prompt: &str,
    ) {
        use crate::ui::app_state::NotificationKind;

        let cmd_lower = cmd.to_lowercase();

        match cmd_lower.as_str() {
            "/stats" | "/reset" | "/context" | "/compact" | "/conversations" | "/history" => {
                // These commands return data that UI can access via StateManager
                // No notification needed - UI pulls this data
            }
            "/new" => {
                if let Some(cm) = conversation_manager.as_ref() {
                    let name = format!(
                        "Conversation {}",
                        chrono::Local::now().format("%Y-%m-%d %H:%M")
                    );
                    let _ = cm
                        .lock()
                        .unwrap()
                        .create_new(name, config.model().to_string());
                    chat_history.lock().await.clear();
                    if let Some(sm) = state_manager {
                        sm.add_system_message("Started a new conversation.".to_string());
                    }
                } else if let Some(sm) = state_manager {
                    sm.push_notification(
                        "Conversation persistence is not enabled.".to_string(),
                        NotificationKind::Warning,
                    );
                }
            }
            "/save" => {
                if let Some(cm) = conversation_manager.as_ref() {
                    if let Some(conv) = cm.lock().unwrap().get_current() {
                        let _ = cm.lock().unwrap().save();
                        if let Some(sm) = state_manager {
                            sm.push_notification(
                                format!("Conversation saved: {}", conv.name),
                                NotificationKind::Success,
                            );
                        }
                    }
                } else if let Some(sm) = state_manager {
                    sm.push_notification(
                        "Conversation persistence is not enabled.".to_string(),
                        NotificationKind::Warning,
                    );
                }
            }
            _ if cmd_lower.starts_with("/load ") => {
                if let Some(id_str) = cmd.strip_prefix("/load ") {
                    if let Some(cm) = conversation_manager.as_ref() {
                        let id = match uuid::Uuid::parse_str(id_str) {
                            Ok(id) => id,
                            Err(_) => {
                                if let Some(sm) = state_manager {
                                    sm.push_notification("Invalid conversation ID. Use /conversations to see available IDs.".to_string(), NotificationKind::Error);
                                }
                                return;
                            }
                        };
                        let result = {
                            let guard = cm.lock().unwrap();
                            guard.load(id)
                        };
                        match result {
                            Ok(conv) => {
                                {
                                    let mut guard = cm.lock().unwrap();
                                    guard.load_and_set_current(id).ok();
                                }
                                chat_history.lock().await.clear();
                                *chat_history.lock().await =
                                    crate::convert_conversation_to_rig_messages(&conv);
                                if let Some(sm) = state_manager {
                                    sm.add_system_message(format!(
                                        "Loaded conversation: '{}'",
                                        conv.name
                                    ));
                                }
                            }
                            Err(e) => {
                                if let Some(sm) = state_manager {
                                    sm.push_notification(
                                        format!("Failed to load conversation: {}", e),
                                        NotificationKind::Error,
                                    );
                                }
                            }
                        }
                    } else if let Some(sm) = state_manager {
                        sm.push_notification(
                            "Conversation persistence is not enabled.".to_string(),
                            NotificationKind::Warning,
                        );
                    }
                }
            }
            _ if cmd_lower.starts_with("/delete ") => {
                if let Some(id_str) = cmd.strip_prefix("/delete ") {
                    if let Some(cm) = conversation_manager.as_ref() {
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
                        let result = {
                            let guard = cm.lock().unwrap();
                            guard.delete(id)
                        };
                        match result {
                            Ok(_) => {
                                if let Some(sm) = state_manager {
                                    sm.push_notification(
                                        "Conversation deleted.".to_string(),
                                        NotificationKind::Success,
                                    );
                                }
                            }
                            Err(e) => {
                                if let Some(sm) = state_manager {
                                    sm.push_notification(
                                        format!("Failed to delete: {}", e),
                                        NotificationKind::Error,
                                    );
                                }
                            }
                        }
                    } else if let Some(sm) = state_manager {
                        sm.push_notification(
                            "Conversation persistence is not enabled.".to_string(),
                            NotificationKind::Warning,
                        );
                    }
                }
            }
            _ if cmd_lower.starts_with("/export ") => {
                if let Some(args) = cmd.strip_prefix("/export ") {
                    let parts: Vec<&str> = args.splitn(2, ' ').collect();
                    if parts.len() == 2 {
                        if let Some(cm) = conversation_manager.as_ref() {
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
                            let conv_result = {
                                let guard = cm.lock().unwrap();
                                guard.load(id)
                            };
                            match conv_result {
                                Ok(conv) => {
                                    let output = {
                                        let guard = cm.lock().unwrap();
                                        match format.as_str() {
                                            "markdown" | "md" => guard.export_markdown(&conv),
                                            "json" => guard.export_json(&conv),
                                            _ => {
                                                if let Some(sm) = state_manager {
                                                    sm.push_notification(format!("Unknown format '{}'. Use 'json' or 'markdown'.", format), NotificationKind::Error);
                                                }
                                                return;
                                            }
                                        }
                                    };
                                    match output {
                                        Ok(s) => {
                                            if let Some(sm) = state_manager {
                                                sm.add_system_message(format!("Export:\n{}", s));
                                            }
                                        }
                                        Err(e) => {
                                            if let Some(sm) = state_manager {
                                                sm.push_notification(
                                                    format!("Export failed: {}", e),
                                                    NotificationKind::Error,
                                                );
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    if let Some(sm) = state_manager {
                                        sm.push_notification(
                                            format!("Failed to load conversation: {}", e),
                                            NotificationKind::Error,
                                        );
                                    }
                                }
                            }
                        } else if let Some(sm) = state_manager {
                            sm.push_notification(
                                "Conversation persistence is not enabled.".to_string(),
                                NotificationKind::Warning,
                            );
                        }
                    } else if let Some(sm) = state_manager {
                        sm.push_notification(
                            "Usage: /export <id> <json|markdown>".to_string(),
                            NotificationKind::Info,
                        );
                    }
                }
            }
            _ if cmd_lower.starts_with("/rename ") => {
                if let Some(name) = cmd.strip_prefix("/rename ") {
                    if let Some(cm) = conversation_manager.as_ref() {
                        let result = {
                            let mut guard = cm.lock().unwrap();
                            guard.rename(name.to_string())
                        };
                        match result {
                            Ok(_) => {
                                if let Some(sm) = state_manager {
                                    sm.push_notification(
                                        format!("Conversation renamed to: {}", name),
                                        NotificationKind::Success,
                                    );
                                }
                            }
                            Err(e) => {
                                if let Some(sm) = state_manager {
                                    sm.push_notification(
                                        format!("Failed to rename: {}", e),
                                        NotificationKind::Error,
                                    );
                                }
                            }
                        }
                    } else if let Some(sm) = state_manager {
                        sm.push_notification(
                            "Conversation persistence is not enabled.".to_string(),
                            NotificationKind::Warning,
                        );
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

pub struct McpServerHandle {
    #[allow(unused)]
    name: String,
    tools: Vec<McpTool>,
    // Box for type erasure - allows stdio and HTTP transports to coexist
    service: Box<dyn std::any::Any + Send + Sync>,
}

impl McpServerHandle {
    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    pub fn take_tools(self) -> Vec<Box<dyn ToolDyn>> {
        self.tools
            .into_iter()
            .map(|tool| Box::new(tool) as Box<dyn ToolDyn>)
            .collect()
    }
}

pub async fn connect_mcp_server(config: &McpServerConfig) -> Result<McpServerHandle> {
    // Validate configuration
    if let Err(e) = config.validate() {
        return Err(anyhow::anyhow!("Invalid MCP server config: {}", e));
    }

    match config.transport_type() {
        McpTransportType::Stdio => connect_mcp_stdio(config).await,
        McpTransportType::Sse | McpTransportType::StreamableHttp => connect_mcp_http(config).await,
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
        .map(|tool| McpTool::from_mcp_server(tool, service.clone()))
        .collect::<Vec<_>>();

    tracing::info!("MCP server '{}' has {} tools", config.name, tools.len());

    Ok(McpServerHandle {
        name: config.name.clone(),
        tools,
        service: Box::new(service),
    })
}

/// Connect to an MCP server using HTTP transport (remote server)
async fn connect_mcp_http(config: &McpServerConfig) -> Result<McpServerHandle> {
    let url = config
        .url
        .as_ref()
        .ok_or_else(|| anyhow!("HTTP transport requires 'url'"))?;

    tracing::info!("Connecting to MCP server '{}' at {}", config.name, url);

    // Create the HTTP transport using reqwest
    let transport = StreamableHttpClientTransport::from_uri(url.clone());

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
        .map(|tool| McpTool::from_mcp_server(tool, service.clone()))
        .collect::<Vec<_>>();

    tracing::info!("MCP server '{}' has {} tools", config.name, tools.len());

    Ok(McpServerHandle {
        name: config.name.clone(),
        tools,
        service: Box::new(service),
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
    fn test_mcp_config_validation() {
        // Valid stdio config
        let config = McpServerConfig {
            name: "test".to_string(),
            transport_type: McpTransportType::Stdio,
            command: Some("npx".to_string()),
            args: None,
            env: None,
            url: None,
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
            enabled: true,
        };
        assert!(config.validate().is_err());
    }
}
