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
    McpTransportType, OllamaConfig, OpenRouterConfig, PipelineConfig, ProviderConfig,
    ProviderType, RetryConfig, SearXngConfig,
};
pub use context_manager::{CompactionResult, ContextManager};
pub use conversation::{
    Conversation, ConversationMetadata, ConversationSummary, Message as ConversationMessage,
};
pub use conversation_manager::{ConversationManager, ConversationManagerConfig};
pub use hooks::{
    AgentEvent, CostHandler, EventChannel, EventHandler, EventProcessor, ModelPricing,
    SessionHook, SessionStats, StateManagerHandler, StreamingConfig, StreamingOutputHandler,
    TextColor, TokenUsage, VerbosityLevel, create_event_channel, fetch_model_pricing,
};
pub use pipeline::{DelegateTool, SubAgentRegistry};
pub use providers::{CostTracker, DynAgent, ProviderInfo, create_provider};
use rig::completion::{Message, PromptError};
use rig::tool::ToolDyn;
use rig::tool::rmcp::McpTool;
use rmcp::transport::{TokioChildProcess, streamable_http_client::StreamableHttpClientTransport};
pub use skills::{SkillRegistry, load_default_skills};
pub use tools::{
    BashTool, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool, LoggingToolDyn,
    SearchTool, ThinkTool, TodoList, TodoStatus, TodoTool,
};
pub use ui::{UiAction, Ui};

use anyhow::{Result, anyhow};
use rmcp::service::ServiceExt;
use std::io::{self, BufRead, Write};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep};
use tracing::debug;

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

/// Print the last N messages from a conversation to the console
pub fn print_recent_messages(conv: &Conversation, count: usize) {
    use crate::conversation::Message as StoredMessage;

    let messages: Vec<_> = conv.messages.iter().rev().take(count).collect();

    if messages.is_empty() {
        println!("  (no messages in this conversation)");
        return;
    }

    for msg in messages.iter().rev() {
        match msg {
            StoredMessage::User { content, timestamp } => {
                println!(
                    "  [{}] User: {}",
                    timestamp.format("%H:%M"),
                    truncate(content, 100)
                );
            }
            StoredMessage::Assistant { content, timestamp } => {
                println!(
                    "  [{}] Assistant: {}",
                    timestamp.format("%H:%M"),
                    truncate(content, 100)
                );
            }
            StoredMessage::ToolCall {
                tool_name,
                arguments,
                timestamp,
            } => {
                println!(
                    "  [{}] Tool Call: {} - {}",
                    timestamp.format("%H:%M"),
                    tool_name,
                    truncate(arguments, 60)
                );
            }
            StoredMessage::ToolResult {
                tool_name,
                result,
                timestamp,
                ..
            } => {
                println!(
                    "  [{}] Tool: {} - {}",
                    timestamp.format("%H:%M"),
                    tool_name,
                    truncate(result, 60)
                );
            }
        }
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

/// AgentRunner — the Controller in MVC.
///
/// Receives input (UiAction) from Views, calls the agent, writes results to
/// StateManager (Model). Never reads stdin or prints directly.
pub struct AgentRunner {
    agent: Arc<DynAgent>,
    config: Config,
    provider_info: ProviderInfo,
    skills: SkillRegistry,
    context_manager: Option<ContextManager>,
    conversation_manager: Option<Arc<Mutex<ConversationManager>>>,
    system_prompt: String,
    cost_tracker: CostTracker,
    todo_state: Option<Arc<Mutex<TodoList>>>,
    state_manager: Option<Arc<ui::StateManager>>,
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
            _event_receiver: event_receiver,
        }
    }

    /// Print session stats — used by REPL View
    pub fn print_stats(&self) {
        println!("\n=== Session Statistics ===\n");
        println!("Provider: {}", self.provider_info.name);
        println!("Model: {}", self.provider_info.model);
        if let Some(summary) = self.cost_tracker.get_session_summary() {
            println!("{}", summary);
        } else {
            println!("Token tracking not available for this provider.");
        }
        println!();
    }

    /// Print context status — used by REPL View
    pub fn print_context_status(&self) {
        if let Some(ref cm) = self.context_manager {
            println!("\n=== Context Status ===\n");
            println!("{}", cm.format_status());
            println!();
        } else {
            println!("\nContext compaction is not enabled.\n");
        }
    }

    /// Print todo list summary — used by REPL View
    pub fn print_todo_summary(&self) {
        if let Some(ref state) = self.todo_state {
            if let Ok(list) = state.lock() {
                let tasks = list.list();
                if !tasks.is_empty() {
                    let (pending, in_progress, completed, cancelled) = list.count_by_status();
                    println!(
                        "\n[Todo: {} pending, {} in-progress, {} completed, {} cancelled]\n",
                        pending, in_progress, completed, cancelled
                    );
                }
            }
        }
    }

    /// List saved conversations — used by REPL View
    pub fn list_conversations(&self) {
        if let Some(ref cm) = self.conversation_manager {
            match cm.lock().unwrap().list() {
                Ok(conversations) => {
                    if conversations.is_empty() {
                        println!("\nNo saved conversations.\n");
                    } else {
                        println!("\n=== Saved Conversations ===\n");
                        for conv in &conversations {
                            println!("ID: {}", conv.id);
                            println!("  Name: {}", conv.name);
                            println!("  Model: {}", conv.model);
                            println!("  Messages: {}", conv.message_count);
                            println!("  Created: {}", conv.created_at.format("%Y-%m-%d %H:%M"));
                            println!("  Updated: {}", conv.updated_at.format("%Y-%m-%d %H:%M"));
                            println!();
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to list conversations: {}\n", e);
                }
            }
        } else {
            println!("\nConversation persistence is not enabled.\n");
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
        if let Some(ref mut cm) = self.context_manager {
            match cm.compact(chat_history, &self.system_prompt).await {
                Ok(result) => {
                    println!(
                        "\n[Context compacted: {} → {} messages, {} messages discarded]\n",
                        result.original_count, result.compacted_count, result.num_discarded
                    );
                }
                Err(e) => {
                    eprintln!("\nError compacting context: {}\n", e);
                }
            }
        } else {
            println!("\nContext compaction is not enabled.\n");
        }
    }

    /// Handle a message — call the agent and process the response
    /// Returns the agent's response text on success, or the final error on failure
    async fn process_message(&mut self, msg: &str, chat_history: &mut Vec<Message>) -> Result<String, String> {
        let mut retry_count = 0;
        const MAX_COMPACT_RETRIES: usize = 3;

        loop {
            // Check if context compaction is needed before starting
            if let Some(ref mut cm) = self.context_manager {
                if cm.needs_compaction(chat_history) {
                    println!("[Context approaching limit, compacting before prompt...]");
                    cm.compact(chat_history, &self.system_prompt)
                        .await
                        .map(|result| {
                            println!(
                                "[Compacted: {} → {} messages, {} discarded]\n",
                                result.original_count, result.compacted_count, result.num_discarded
                            );
                        })
                        .unwrap_or_else(|e| {
                            eprintln!("[Warning: compaction failed: {}]", e);
                        });
                }
            }

            // Call the agent - may be interrupted by hook if context is exceeded
            let result = self.agent.as_ref().prompt_with_history(msg, chat_history).await;

            match result {
                Ok(response) => return Ok(response),
                
                Err(PromptError::PromptCancelled { chat_history: terminated_history, .. }) => {
                    // Agent was terminated by hook - history was returned
                    chat_history.clear();
                    chat_history.extend(terminated_history.iter().cloned());
                    retry_count += 1;

                    if retry_count > MAX_COMPACT_RETRIES {
                        return Err("Max compaction retries exceeded".to_string());
                    }

                    tracing::info!(
                        "Agent terminated due to context limit. Compacting (attempt {}/{})",
                        retry_count, MAX_COMPACT_RETRIES
                    );
                    println!("[Context limit reached, compacting...]");

                    // Compact the history
                    if let Some(ref mut cm) = self.context_manager {
                        if let Err(e) = cm.compact(chat_history, &self.system_prompt).await {
                            return Err(format!("Compaction failed: {}", e));
                        }
                        println!("[Compaction complete, resuming...]");
                    } else {
                        return Err("Context limit reached but compaction not available".to_string());
                    }

                    continue; // Retry with same user message
                }
                
                Err(e) => {
                    // Non-cancellation error - handle with retry logic
                    let retry_config = self.config.retry();
                    
                    if retry_count > 0 {
                        // We've already retried after a cancellation, don't retry again
                        return Err(e.to_string());
                    }

                    let mut attempt = 0u32;
                    let mut last_error = Err(e);
                    
                    while last_error.is_err() {
                        if attempt >= retry_config.max_retries {
                            let error = last_error.err().unwrap();
                            eprintln!("\nError (after {} retries): {}\n", attempt, error);
                            return Err(error.to_string());
                        }

                        let delay_ms = ((retry_config.initial_delay_ms as f64)
                            * (retry_config.backoff_factor.powi(attempt as i32)))
                        .min(retry_config.max_delay_ms as f64) as u64;

                        if attempt > 0 {
                            eprintln!(
                                "\nError: {}. Retrying in {}ms (attempt {}/{})...\n",
                                last_error.as_ref().err().unwrap(),
                                delay_ms,
                                attempt + 1,
                                retry_config.max_retries
                            );
                        }

                        sleep(Duration::from_millis(delay_ms)).await;

                        last_error = self.agent.as_ref().prompt_with_history(msg, chat_history).await;
                        attempt += 1;
                    }

                    return Ok(last_error.unwrap());
                }
            }
        }
    }

    /// Handle a slash command
    async fn process_command(&mut self, cmd: &str, chat_history: &mut Vec<Message>) {
        let cmd_lower = cmd.to_lowercase();

        match cmd_lower.as_str() {
            "/exit" | "/quit" => {
                // Don't exit here — signal back to the view loop
            }
            "/stats" => {
                self.print_stats();
            }
            "/reset" => {
                self.reset_stats();
                println!("Stats reset.\n");
            }
            "/context" => {
                self.print_context_status();
            }
            "/compact" => {
                self.force_compact(chat_history).await;
            }
            "/conversations" | "/history" => {
                self.list_conversations();
            }
            "/new" => {
                if let Some(ref cm) = self.conversation_manager {
                    let name = format!(
                        "Conversation {}",
                        chrono::Local::now().format("%Y-%m-%d %H:%M")
                    );
                    let _ = cm
                        .lock()
                        .unwrap()
                        .create_new(name, self.config.model().to_string());
                    chat_history.clear();
                    println!("Started a new conversation.\n");
                } else {
                    println!("Conversation persistence is not enabled.\n");
                }
            }
            "/save" => {
                if let Some(ref cm) = self.conversation_manager {
                    if let Some(ref conv) = cm.lock().unwrap().get_current() {
                        let _ = cm.lock().unwrap().save();
                        println!("Conversation saved: {}\n", conv.name);
                    }
                } else {
                    println!("Conversation persistence is not enabled.\n");
                }
            }
            _ if cmd_lower.starts_with("/load ") => {
                if let Some(id_str) = cmd.strip_prefix("/load ") {
                    if let Some(ref cm) = self.conversation_manager {
                        match uuid::Uuid::parse_str(id_str) {
                            Ok(id) => match cm.lock().unwrap().load(id) {
                                Ok(conv) => {
                                    let _ = cm.lock().unwrap().load_and_set_current(id);
                                    chat_history.clear();
                                    *chat_history = convert_conversation_to_rig_messages(&conv);
                                    println!("\n--- Loaded conversation: '{}' ---\n", conv.name);
                                }
                                Err(e) => {
                                    eprintln!("Failed to load conversation: {}\n", e);
                                }
                            },
                            Err(_) => {
                                eprintln!(
                                    "Invalid conversation ID. Use /conversations to see available IDs.\n"
                                );
                            }
                        }
                    } else {
                        println!("Conversation persistence is not enabled.\n");
                    }
                }
            }
            _ if cmd_lower.starts_with("/delete ") => {
                if let Some(id_str) = cmd.strip_prefix("/delete ") {
                    if let Some(ref cm) = self.conversation_manager {
                        match uuid::Uuid::parse_str(id_str) {
                            Ok(id) => match cm.lock().unwrap().delete(id) {
                                Ok(_) => println!("Conversation deleted.\n"),
                                Err(e) => eprintln!("Failed to delete: {}\n", e),
                            },
                            Err(_) => {
                                eprintln!("Invalid conversation ID.\n");
                            }
                        }
                    } else {
                        println!("Conversation persistence is not enabled.\n");
                    }
                }
            }
            _ if cmd_lower.starts_with("/export ") => {
                if let Some(args) = cmd.strip_prefix("/export ") {
                    let parts: Vec<&str> = args.splitn(2, ' ').collect();
                    if parts.len() == 2 {
                        if let Some(ref cm) = self.conversation_manager {
                            let id_str = parts[0];
                            let format = parts[1].to_lowercase();
                            match uuid::Uuid::parse_str(id_str) {
                                Ok(id) => match cm.lock().unwrap().load(id) {
                                    Ok(conv) => {
                                        let output = match format.as_str() {
                                            "markdown" | "md" => {
                                                cm.lock().unwrap().export_markdown(&conv)
                                            }
                                            "json" => cm.lock().unwrap().export_json(&conv),
                                            _ => {
                                                eprintln!(
                                                    "Unknown format '{}'. Use 'json' or 'markdown'.\n",
                                                    format
                                                );
                                                return;
                                            }
                                        };
                                        match output {
                                            Ok(s) => {
                                                println!("\n--- Export ---\n{}\n--- End ---\n", s);
                                            }
                                            Err(e) => eprintln!("Export failed: {}\n", e),
                                        }
                                    }
                                    Err(e) => eprintln!("Failed to load conversation: {}\n", e),
                                },
                                Err(_) => eprintln!("Invalid conversation ID.\n"),
                            }
                        } else {
                            println!("Conversation persistence is not enabled.\n");
                        }
                    } else {
                        println!("Usage: /export <id> <json|markdown>\n");
                    }
                }
            }
            _ if cmd_lower.starts_with("/rename ") => {
                if let Some(name) = cmd.strip_prefix("/rename ") {
                    if let Some(ref cm) = self.conversation_manager {
                        if let Err(e) = cm.lock().unwrap().rename(name.to_string()) {
                            eprintln!("Failed to rename: {}\n", e);
                        } else {
                            println!("Conversation renamed to: {}\n", name);
                        }
                    } else {
                        println!("Conversation persistence is not enabled.\n");
                    }
                }
            }
            _ => {
                // Unknown command — treat as a message to the agent (response discarded)
                let _ = self.process_message(cmd, chat_history).await;
            }
        }
    }

    /// Called after every successful agent response — update model with response and stats
    /// For MVC mode (run_loop): pass the response text
    /// For legacy mode (run): pass None (response is printed directly)
    fn handle_success(&self, response: Option<&str>) {
        // Add agent response to chat (MVC mode only)
        if let Some(response) = response {
            if let Some(ref sm) = self.state_manager {
                use crate::ui::app_state::ChatMessage;
                sm.update_chat(ChatMessage::agent(response.to_string()));
                sm.set_final_broadcast(true);
            }
        }

        // Sync stats and todo to StateManager
        self.sync_state_to_manager();

        // Save conversation (backend concern)
        if let Some(ref cm) = self.conversation_manager {
            if let Err(e) = cm.lock().unwrap().save() {
                tracing::warn!("Failed to save conversation: {}", e);
            }
        }
    }

    /// The controller loop — receives UiActions from Views, processes them.
    /// This is NOT the I/O loop — Views own the I/O.
    pub async fn run_loop(&mut self, mut action_receiver: mpsc::UnboundedReceiver<UiAction>) {
        let mut chat_history: Vec<Message> = Vec::new();

        // Spawn event processing task for streaming output (view concern)
        if let Some(receiver) = self._event_receiver.take() {
            let mut handlers: Vec<Arc<dyn EventHandler>> = Vec::new();

            let stats = self.cost_tracker.get_session_stats();
            let pricing = self.cost_tracker.get_pricing().clone();
            handlers.push(Arc::new(CostHandler::new(pricing, stats)));

            // StateManagerHandler forwards live events (tool calls, tool results) to StateManager
            if let Some(ref sm) = self.state_manager {
                handlers.push(Arc::new(StateManagerHandler::new(sm.clone())));
            }

            handlers.push(Arc::new(StreamingOutputHandler::new()));

            tokio::spawn(async move {
                let mut processor = EventProcessor::new(receiver, handlers);
                processor.run().await;
            });
        }

        // Initialize conversation
        if let Some(ref cm) = self.conversation_manager {
            let name = format!(
                "Conversation {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M")
            );
            let _ = cm
                .lock()
                .unwrap()
                .create_new(name, self.config.model().to_string());
        }

        while let Some(action) = action_receiver.recv().await {
            match action {
                UiAction::SendMessage(msg) => {
                    // Auto-save user message
                    if let Some(ref cm) = self.conversation_manager {
                        let _ = cm.lock().unwrap().add_user_message(msg.clone());
                    }
                    // Add user message to chat for live rendering
                    if let Some(ref sm) = self.state_manager {
                        use crate::ui::app_state::ChatMessage;
                        sm.update_chat(ChatMessage::user(msg.clone()));
                    }
                    // Process and handle response
                    match self.process_message(&msg, &mut chat_history).await {
                        Ok(response) => {
                            self.handle_success(Some(&response));
                        }
                        Err(_) => {
                            // Error already printed in process_message
                        }
                    }
                }
                UiAction::ExecuteCommand(cmd) => {
                    // Don't save commands to conversation history
                    self.process_command(&cmd, &mut chat_history).await;
                }
                UiAction::Exit => {
                    break;
                }
            }
        }
    }

    /// Run the REPL loop — kept for backward compatibility and for when no UI is used.
    /// This is the only method that owns the I/O loop.
    pub async fn run(&mut self) -> Result<()> {
        let cwd = std::env::current_dir()?;
        println!("PeakBot coding agent ready.");
        println!(
            "Provider: {} | Model: {} | Max-tokens: {}",
            self.config.provider_name(),
            self.config.model(),
            self.config.max_tokens()
        );
        if self.config.mcp_servers.as_ref().map_or(0, |s| s.len()) > 0 {
            println!(
                "MCP servers: {}",
                self.config.mcp_servers.as_ref().map_or(0, |s| s.len())
            );
        }
        if !self.skills.is_empty() {
            println!("Skills: {}", self.skills.len());
            for skill in self.skills.all() {
                println!("  - {}: {}", skill.name, skill.description);
            }
        }
        if self.config.searxng_enabled() {
            if let Some(ref searxng) = self.config.searxng {
                println!("SearXNG: {} (enabled)", searxng.base_url);
            }
        } else {
            println!("SearXNG: not configured");
        }
        println!(
            "Cost tracking: {}",
            if !self.config.supports_pricing() {
                "not supported by provider"
            } else if self.config.cost_tracking {
                "enabled"
            } else {
                "disabled"
            }
        );
        if self.config.context.enabled {
            println!(
                "Context compaction: enabled (threshold: {:.0}%, keep_recent: {})",
                self.config.context.threshold * 100.0,
                self.config.context.keep_recent
            );
        } else {
            println!("Context compaction: disabled");
        }

        if let Some(ref cm) = self.conversation_manager {
            println!(
                "Conversation persistence: enabled (auto-save, {} stored)",
                cm.lock().unwrap().storage_dir().display()
            );
        } else {
            println!("Conversation persistence: disabled");
        }

        println!("Working directory: {}", cwd.display());
        println!(
            "Type /stats to see session stats, /context for context status, /compact to force compaction, /conversations to list saved, or 'exit' to quit.\n"
        );

        let stdin = io::stdin();
        let mut stdout = io::stdout();
        let mut chat_history: Vec<Message> = Vec::new();

        // Spawn event processing task
        if let Some(receiver) = self._event_receiver.take() {
            let mut handlers: Vec<Arc<dyn EventHandler>> = Vec::new();
            let stats = self.cost_tracker.get_session_stats();
            let pricing = self.cost_tracker.get_pricing().clone();
            handlers.push(Arc::new(CostHandler::new(pricing, stats)));
            handlers.push(Arc::new(StreamingOutputHandler::new()));
            tokio::spawn(async move {
                let mut processor = EventProcessor::new(receiver, handlers);
                processor.run().await;
            });
        }

        // Check for auto-resume
        let mut resumed = false;
        if let Some(ref cm) = self.conversation_manager {
            if self.config.conversation_auto_resume() {
                if let Ok(Some(latest)) = cm.lock().unwrap().get_latest() {
                    if !latest.messages.is_empty() {
                        println!(
                            "\n[Found previous conversation: '{}' ({} messages)]",
                            latest.name,
                            latest.messages.len()
                        );
                        print!("Resume this conversation? (y/n): ");
                        stdout.flush().ok();

                        let mut confirm = String::new();
                        if stdin.lock().read_line(&mut confirm).is_ok() {
                            if confirm.trim().eq_ignore_ascii_case("y") || confirm.trim().is_empty() {
                                if let Err(e) = cm.lock().unwrap().load_and_set_current(latest.id) {
                                    eprintln!("Failed to load conversation: {}", e);
                                } else {
                                    chat_history = convert_conversation_to_rig_messages(&latest);
                                    resumed = true;
                                    println!("\n--- Resumed conversation: '{}' ---\n", latest.name);
                                    println!("Last {} messages:\n", 10.min(latest.messages.len()));
                                    print_recent_messages(&latest, 10);
                                    println!("\n");
                                }
                            }
                        }
                    }
                }
            }
        }

        if !resumed {
            if let Some(ref cm) = self.conversation_manager {
                let name = format!(
                    "Conversation {}",
                    chrono::Local::now().format("%Y-%m-%d %H:%M")
                );
                let _ = cm
                    .lock()
                    .unwrap()
                    .create_new(name, self.config.model().to_string());
            }
        }

        loop {
            print!("> ");
            stdout.flush()?;

            let mut input = String::new();
            stdin.lock().read_line(&mut input)?;
            let input = input.trim();

            if input.is_empty() {
                continue;
            }

            if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
                self.print_stats();
                println!("Goodbye!");
                break;
            }

            if input.eq_ignore_ascii_case("/stats") {
                self.print_stats();
                continue;
            }

            if input.eq_ignore_ascii_case("/reset") {
                self.reset_stats();
                println!("Stats reset.\n");
                continue;
            }

            if input.eq_ignore_ascii_case("/context") {
                self.print_context_status();
                continue;
            }

            if input.eq_ignore_ascii_case("/compact") {
                self.force_compact(&mut chat_history).await;
                continue;
            }

            if input.eq_ignore_ascii_case("/conversations")
                || input.eq_ignore_ascii_case("/history")
            {
                self.list_conversations();
                continue;
            }

            if input.eq_ignore_ascii_case("/new") {
                if let Some(ref cm) = self.conversation_manager {
                    let name = format!(
                        "Conversation {}",
                        chrono::Local::now().format("%Y-%m-%d %H:%M")
                    );
                    let _ = cm
                        .lock()
                        .unwrap()
                        .create_new(name, self.config.model().to_string());
                    chat_history.clear();
                    println!("Started a new conversation.\n");
                } else {
                    println!("Conversation persistence is not enabled.\n");
                }
                continue;
            }

            if input.eq_ignore_ascii_case("/save") {
                if let Some(ref cm) = self.conversation_manager {
                    if let Some(ref conv) = cm.lock().unwrap().get_current() {
                        let _ = cm.lock().unwrap().save();
                        println!("Conversation saved: {}\n", conv.name);
                    }
                } else {
                    println!("Conversation persistence is not enabled.\n");
                }
                continue;
            }

            if let Some(id_str) = input.strip_prefix("/load ") {
                if let Some(ref cm) = self.conversation_manager {
                    match uuid::Uuid::parse_str(id_str) {
                        Ok(id) => match cm.lock().unwrap().load(id) {
                            Ok(conv) => {
                                let _ = cm.lock().unwrap().load_and_set_current(id);
                                chat_history = convert_conversation_to_rig_messages(&conv);
                                println!("\n--- Loaded conversation: '{}' ---\n", conv.name);
                                println!("Last {} messages:\n", 10.min(conv.messages.len()));
                                print_recent_messages(&conv, 10);
                                println!("\n");
                            }
                            Err(e) => {
                                eprintln!("Failed to load conversation: {}\n", e);
                            }
                        },
                        Err(_) => {
                            eprintln!(
                                "Invalid conversation ID. Use /conversations to see available IDs.\n"
                            );
                        }
                    }
                } else {
                    println!("Conversation persistence is not enabled.\n");
                }
                continue;
            }

            if let Some(id_str) = input.strip_prefix("/delete ") {
                if let Some(ref cm) = self.conversation_manager {
                    match uuid::Uuid::parse_str(id_str) {
                        Ok(id) => match cm.lock().unwrap().delete(id) {
                            Ok(_) => println!("Conversation deleted.\n"),
                            Err(e) => eprintln!("Failed to delete: {}\n", e),
                        },
                        Err(_) => {
                            eprintln!("Invalid conversation ID.\n");
                        }
                    }
                } else {
                    println!("Conversation persistence is not enabled.\n");
                }
                continue;
            }

            if let Some(args) = input.strip_prefix("/export ") {
                let parts: Vec<&str> = args.splitn(2, ' ').collect();
                if parts.len() == 2 {
                    if let Some(ref cm) = self.conversation_manager {
                        let id_str = parts[0];
                        let format = parts[1].to_lowercase();
                        match uuid::Uuid::parse_str(id_str) {
                            Ok(id) => match cm.lock().unwrap().load(id) {
                                Ok(conv) => {
                                    let output = match format.as_str() {
                                        "markdown" | "md" => {
                                            cm.lock().unwrap().export_markdown(&conv)
                                        }
                                        "json" => cm.lock().unwrap().export_json(&conv),
                                        _ => {
                                            eprintln!(
                                                "Unknown format '{}'. Use 'json' or 'markdown'.\n",
                                                format
                                            );
                                            continue;
                                        }
                                    };
                                    match output {
                                        Ok(s) => {
                                            println!("\n--- Export ---\n{}\n--- End ---\n", s);
                                        }
                                        Err(e) => eprintln!("Export failed: {}\n", e),
                                    }
                                }
                                Err(e) => eprintln!("Failed to load conversation: {}\n", e),
                            },
                            Err(_) => eprintln!("Invalid conversation ID.\n"),
                        }
                    } else {
                        println!("Conversation persistence is not enabled.\n");
                    }
                } else {
                    println!("Usage: /export <id> <json|markdown>\n");
                }
                continue;
            }

            if let Some(name) = input.strip_prefix("/rename ") {
                if let Some(ref cm) = self.conversation_manager {
                    if let Err(e) = cm.lock().unwrap().rename(name.to_string()) {
                        eprintln!("Failed to rename: {}\n", e);
                    } else {
                        println!("Conversation renamed to: {}\n", name);
                    }
                } else {
                    println!("Conversation persistence is not enabled.\n");
                }
                continue;
            }

            // Auto-save user message
            if let Some(ref cm) = self.conversation_manager {
                let _ = cm.lock().unwrap().add_user_message(input.to_string());
            }

            // Context compaction check
            if let Some(ref mut cm) = self.context_manager {
                if cm.needs_compaction(&chat_history) {
                    println!("[Context approaching limit, compacting before prompt...]");
                    cm.compact(&mut chat_history, &self.system_prompt)
                        .await
                        .map(|result| {
                            println!(
                                "[Compacted: {} → {} messages, {} discarded]\n",
                                result.original_count, result.compacted_count, result.num_discarded
                            );
                        })
                        .unwrap_or_else(|e| {
                            eprintln!("[Warning: compaction failed: {}]", e);
                        });
                }
            }

            // Call agent - may be interrupted by hook
            let mut retry_count = 0;
            const MAX_COMPACT_RETRIES: usize = 3;
            
            loop {
                match self.agent.as_ref().prompt_with_history(input, &mut chat_history).await {
                    Ok(_) => {
                        self.handle_success(None);
                        break;
                    }
                    Err(PromptError::PromptCancelled { chat_history: terminated_history, .. }) => {
                        // Agent was terminated by hook - history was returned
                        chat_history.clear();
                        chat_history.extend(terminated_history.iter().cloned());
                        retry_count += 1;

                        if retry_count > MAX_COMPACT_RETRIES {
                            eprintln!("\nMax compaction retries exceeded\n");
                            break;
                        }

                        println!("[Context limit reached, compacting...]");
                        if let Some(ref mut cm) = self.context_manager {
                            if let Err(e) = cm.compact(&mut chat_history, &self.system_prompt).await {
                                eprintln!("\nCompaction failed: {}\n", e);
                                break;
                            }
                            println!("[Compaction complete, resuming...]");
                        } else {
                            eprintln!("\nContext limit reached but compaction not available\n");
                            break;
                        }
                        // Continue to retry
                    }
                    Err(mut e) => {
                        // Handle with retry logic
                        let retry_config = self.config.retry();
                        let mut attempt = 0u32;
                        
                        loop {
                            if attempt >= retry_config.max_retries {
                                eprintln!("\nError (after {} retries): {}\n", attempt, e);
                                break;
                            }

                            let delay_ms = ((retry_config.initial_delay_ms as f64)
                                * (retry_config.backoff_factor.powi(attempt as i32)))
                            .min(retry_config.max_delay_ms as f64) as u64;

                            if attempt > 0 {
                                eprintln!(
                                    "\nError: {}. Retrying in {}ms (attempt {}/{})...\n",
                                    e,
                                    delay_ms,
                                    attempt + 1,
                                    retry_config.max_retries
                                );
                            }

                            self.sync_state_to_manager();
                            sleep(Duration::from_millis(delay_ms)).await;

                            match self.agent.as_ref().prompt_with_history(input, &mut chat_history).await {
                                Ok(_) => {
                                    self.handle_success(None);
                                    break;
                                }
                                Err(new_e) => {
                                    e = new_e;
                                    attempt += 1;
                                }
                            }
                        }
                        break;
                    }
                }
            }
        }

        Ok(())
    }
}

/// A handle to an MCP server connection that keeps the service alive
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
            .map(|tool| LoggingToolDyn::new(tool, &self.name))
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
        McpTransportType::Sse | McpTransportType::StreamableHttp => {
            connect_mcp_http(config).await
        }
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

    tracing::info!(
        "Connecting to MCP server '{}' at {}",
        config.name,
        url
    );

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
