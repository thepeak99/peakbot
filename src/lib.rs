//! PeakBot library - Core functionality for connecting to MCP servers and managing tools.

mod config;
mod context_manager;
mod conversation;
mod conversation_manager;
mod hooks;
mod providers;
mod skills;
mod tools;

pub use config::{
    BashConfig, Config, ContextConfig, ConversationConfig, McpServerConfig, OllamaConfig,
    OpenRouterConfig, ProviderConfig, ProviderType, RetryConfig, SearXngConfig,
};
pub use context_manager::{CompactionResult, ContextManager};
pub use conversation::{
    Conversation, ConversationMetadata, ConversationSummary, Message as ConversationMessage,
};
pub use conversation_manager::{ConversationManager, ConversationManagerConfig};
pub use hooks::{
    // Event types
    AgentEvent,
    ConversationHandler,
    CostHandler,
    EventChannel,
    EventHandler,
    EventProcessor,
    ModelPricing,
    SessionHook,
    SessionStats,
    StreamingConfig,
    StreamingOutputHandler,
    TextColor,
    TokenUsage,
    VerbosityLevel,
    create_event_channel,
    fetch_model_pricing,
};
pub use providers::{CostTracker, DynAgent, ProviderInfo, create_provider};
use rig::completion::Message;
use rig::tool::ToolDyn;
use rig::tool::rmcp::McpTool;
use rmcp::transport::TokioChildProcess;
pub use skills::{SkillRegistry, load_default_skills};
pub use tools::{
    BashTool, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool, LoggingToolDyn,
    SearchTool, ThinkTool, TodoList, TodoStatus, TodoTool,
};

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

    // Get current working directory
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unknown".to_string());

    // Get current time
    let current_time = chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S %Z")
        .to_string();

    // Try to read agents.md if it exists
    let agents_md_content = std::fs::read_to_string("agents.md")
        .map(|content| format!("\n# Agents.md Content\n\n--------------------------------------------------------\n{}\n", content.trim()))
        .unwrap_or_else(|_| String::new());

    // Add skills section if skills are loaded
    let skills_section = skills.to_system_prompt_section();

    // Build the environment information section
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
/// This is used when loading a conversation to restore the chat history
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
            // Skip tool calls and tool results - they're already embedded in assistant responses
            // and including them would be redundant
            StoredMessage::ToolCall { .. } => {}
            StoredMessage::ToolResult { .. } => {}
        }
    }

    messages
}

/// Print the last N messages from a conversation to the console
/// This is used when resuming an old conversation to show context
pub fn print_recent_messages(conv: &Conversation, count: usize) {
    use crate::conversation::Message as StoredMessage;

    let messages: Vec<_> = conv.messages.iter().rev().take(count).collect();

    if messages.is_empty() {
        println!("  (no messages in this conversation)");
        return;
    }

    // Print in reverse order (oldest first)
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

/// Truncate a string to a maximum length, adding "..." if truncated
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

/// A type that can handle agent prompts with history support
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
    event_receiver: Option<mpsc::UnboundedReceiver<AgentEvent>>,
}

impl AgentRunner {
    /// Create a new AgentRunner
    pub fn new(
        agent: DynAgent,
        config: Config,
        provider_info: ProviderInfo,
        skills: SkillRegistry,
        cost_tracker: CostTracker,
        todo_state: Option<Arc<Mutex<TodoList>>>,
        event_receiver: Option<mpsc::UnboundedReceiver<AgentEvent>>,
    ) -> Self {
        // Wrap agent in Arc so we can share it with ContextManager for summarization
        let agent = Arc::new(agent);

        // Build system prompt for context manager
        let system_prompt = build_system_prompt(&skills);

        // Estimate system prompt tokens (rough approximation: ~4 chars per token)
        let system_prompt_tokens = system_prompt.len() / 4;

        // Create context manager (always created, enabled flag controls actual usage)
        // Pass a clone of the agent Arc for summarization
        let context_manager = Some(ContextManager::new(
            config.context.clone(),
            provider_info.model.as_str(),
            cost_tracker.get_session_stats(),
            system_prompt_tokens,
            Some(agent.clone()),
        ));

        // Create conversation manager if persistence is enabled
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
            event_receiver,
        }
    }

    /// Print stats for the last request
    fn print_last_request_stats(&self) {
        if let Some(stats) = self.cost_tracker.get_last_request_stats() {
            println!("{}", stats);
        } else {
            // For providers without cost tracking (e.g., Ollama)
            println!("[Token tracking not available for this provider]");
        }
    }

    /// Print session summary
    fn print_stats(&self) {
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

    /// Reset session stats
    fn reset_stats(&self) {
        self.cost_tracker.reset_stats();
        println!("Stats reset.\n");
    }

    /// Print todo list summary
    fn print_todo_summary(&self) {
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

    /// Print context status
    fn print_context_status(&self, _chat_history: &[Message]) {
        if let Some(ref cm) = self.context_manager {
            println!("\n=== Context Status ===\n");
            // Uses actual token counts from provider - no need for chat_history
            println!("{}", cm.format_status());
            println!();
        } else {
            println!("\nContext compaction is not enabled.\n");
        }
    }

    /// Handle successful agent response by printing stats and todo summary
    fn handle_success(&self) {
        self.print_last_request_stats();
        self.print_todo_summary();
    }

    /// Force context compaction
    async fn force_compact(&mut self, chat_history: &mut Vec<Message>) {
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

    /// List all saved conversations
    fn list_conversations(&self) {
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

    /// Run the interactive REPL loop
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
        // Context compaction status - check the enabled flag
        if self.config.context.enabled {
            println!(
                "Context compaction: enabled (threshold: {:.0}%, keep_recent: {})",
                self.config.context.threshold * 100.0,
                self.config.context.keep_recent
            );
        } else {
            println!("Context compaction: disabled");
        }

        // Print conversation persistence status
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

        // Spawn event processing task if we have an event receiver
        // This handles cost tracking and conversation persistence via events from SessionHook
        if let Some(receiver) = self.event_receiver.take() {
            let mut handlers: Vec<Arc<dyn EventHandler>> = Vec::new();

            // Add cost handler
            let stats = self.cost_tracker.get_session_stats();
            let pricing = self.cost_tracker.get_pricing().clone();
            handlers.push(Arc::new(CostHandler::new(pricing, stats)));

            // Add conversation handler if enabled
            if let Some(ref cm) = self.conversation_manager {
                handlers.push(Arc::new(hooks::ConversationHandler::new(cm.clone())));
            }

            // Add streaming output handler for real-time agent output
            handlers.push(Arc::new(StreamingOutputHandler::new()));

            tokio::spawn(async move {
                let mut processor = EventProcessor::new(receiver, handlers);
                processor.run().await;
            });
        }

        // Check for auto-resume before starting the main loop
        let mut resumed = false;
        if let Some(ref cm) = self.conversation_manager {
            if self.config.conversation_auto_resume() {
                if let Ok(Some(latest)) = cm.lock().unwrap().get_latest() {
                    // Skip resuming if the conversation has 0 messages
                    if latest.messages.is_empty() {
                        tracing::debug!("Skipping auto-resume: latest conversation has 0 messages");
                    } else {
                        println!(
                            "\n[Found previous conversation: '{}' ({} messages)]",
                            latest.name,
                            latest.messages.len()
                        );
                        print!("Resume this conversation? (y/n): ");
                        stdout.flush().ok();

                        let mut confirm = String::new();
                        if stdin.lock().read_line(&mut confirm).is_ok() {
                            if confirm.trim().eq_ignore_ascii_case("y") || confirm.trim().is_empty()
                            {
                                // Load the conversation
                                if let Err(e) = cm.lock().unwrap().load_and_set_current(latest.id) {
                                    eprintln!("Failed to load conversation: {}", e);
                                } else {
                                    // Convert stored messages to rig messages and populate chat history
                                    chat_history = convert_conversation_to_rig_messages(&latest);
                                    resumed = true;
                                    println!("\n--- Resumed conversation: '{}' ---", latest.name);
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

        // If not resumed, create a new conversation
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
                // Show final stats before exiting
                self.print_stats();
                println!("Goodbye!");
                break;
            }

            // Handle /stats command
            if input.eq_ignore_ascii_case("/stats") {
                self.print_stats();
                continue;
            }

            // Handle /reset command
            if input.eq_ignore_ascii_case("/reset") {
                self.reset_stats();
                println!("Stats reset.\n");
                continue;
            }

            // Handle /context command
            if input.eq_ignore_ascii_case("/context") {
                self.print_context_status(&chat_history);
                continue;
            }

            // Handle /compact command
            if input.eq_ignore_ascii_case("/compact") {
                self.force_compact(&mut chat_history).await;
                continue;
            }

            // Handle /conversations or /history command
            if input.eq_ignore_ascii_case("/conversations")
                || input.eq_ignore_ascii_case("/history")
            {
                self.list_conversations();
                continue;
            }

            // Handle /new command - start a fresh conversation
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

            // Handle /save command - save current conversation
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

            // Handle /load <id> command
            if let Some(id_str) = input.strip_prefix("/load ") {
                if let Some(ref cm) = self.conversation_manager {
                    match uuid::Uuid::parse_str(id_str) {
                        Ok(id) => {
                            match cm.lock().unwrap().load(id) {
                                Ok(conv) => {
                                    // Load the conversation and populate chat history
                                    let _ = cm.lock().unwrap().load_and_set_current(id);
                                    chat_history = convert_conversation_to_rig_messages(&conv);
                                    println!("\n--- Loaded conversation: '{}' ---", conv.name);
                                    println!("Last {} messages:\n", 10.min(conv.messages.len()));
                                    print_recent_messages(&conv, 10);
                                    println!("\n");
                                }
                                Err(e) => {
                                    eprintln!("Failed to load conversation: {}\n", e);
                                }
                            }
                        }
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

            // Handle /delete <id> command
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

            // Handle /export <id> <format> command
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

            // Handle /rename <name> command
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

            // Auto-save user message before sending to model
            if let Some(ref cm) = self.conversation_manager {
                let _ = cm.lock().unwrap().add_user_message(input.to_string());
            }

            // Check if context compaction is needed before prompting
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

            // Use the agent to prompt with history
            let retry_config = self.config.retry();
            let mut last_error = self
                .agent
                .as_ref()
                .prompt_with_history(input, &mut chat_history)
                .await
                .err();

            let mut attempt = 0u32;
            while let Some(ref error) = last_error {
                // Check if we should retry
                if attempt >= retry_config.max_retries {
                    // Max retries exceeded, print the final error
                    eprintln!("\nError (after {} retries): {}\n", attempt, error);
                    last_error = None;
                    break;
                }

                // Calculate delay with exponential backoff
                let delay_ms = ((retry_config.initial_delay_ms as f64)
                    * (retry_config.backoff_factor.powi(attempt as i32)))
                .min(retry_config.max_delay_ms as f64)
                    as u64;

                if attempt > 0 {
                    eprintln!(
                        "\nError: {}. Retrying in {}ms (attempt {}/{})...\n",
                        error,
                        delay_ms,
                        attempt + 1,
                        retry_config.max_retries
                    );
                }

                // Wait before retrying
                sleep(Duration::from_millis(delay_ms)).await;

                // Attempt the request again
                last_error = self
                    .agent
                    .as_ref()
                    .prompt_with_history(input, &mut chat_history)
                    .await
                    .err();
                attempt += 1;
            }

            // Handle success (no error)
            if last_error.is_none() {
                self.handle_success();
            }
        }

        Ok(())
    }
}

/// A handle to an MCP server connection that keeps the service alive
/// while tools are being used.
pub struct McpServerHandle {
    /// The service - kept alive to maintain the connection
    /// Using type alias for the running service
    #[allow(unused)]
    service: rmcp::service::RunningService<rmcp::service::RoleClient, ()>,
    tools: Vec<McpTool>,
    name: String,
}

impl McpServerHandle {
    /// Get the tools from this MCP server handle
    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// Take ownership of the tools, converting them to dynamic tool trait objects
    pub fn take_tools(self) -> Vec<Box<dyn ToolDyn>> {
        self.tools
            .into_iter()
            .map(|tool| LoggingToolDyn::new(tool, &self.name))
            .map(|tool| Box::new(tool) as Box<dyn ToolDyn>)
            .collect()
    }
}

/// Connect to an MCP server and return its tools (wrapped with LoggingToolDyn)
pub async fn connect_mcp_server(config: &McpServerConfig) -> Result<McpServerHandle> {
    // Only stdio transport is supported for now
    let command = &config.command;

    let mut cmd = tokio::process::Command::new(command);
    if let Some(args) = &config.args {
        cmd.args(args);
    }
    if let Some(env) = &config.env {
        for (key, value) in env {
            cmd.env(key, value);
        }
    }

    // Use TokioChildProcess transport
    let (transport, _stderr) = TokioChildProcess::builder(cmd)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to create child process: {}", e))?;

    // Connect to the MCP server
    let service = ()
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to MCP server: {}", e))?;

    // Get server info
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
        service,
        tools,
        name: config.name.clone(),
    })
}

/// Load and connect to all configured MCP servers
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

    /// Test that connect_mcp_server handles an invalid command gracefully
    #[tokio::test]
    async fn test_connect_mcp_server_invalid_command() {
        let mut env = HashMap::new();
        env.insert("TEST_VAR".to_string(), "test_value".to_string());

        let config = McpServerConfig {
            name: "test_invalid".to_string(),
            command: "nonexistent_command_xyz123".to_string(),
            args: None,
            env: Some(env),
            enabled: true,
        };

        let result = connect_mcp_server(&config).await;
        assert!(result.is_err(), "Expected error for invalid command");
    }

    /// Test that connect_mcp_server works with a real MCP server
    #[tokio::test]
    async fn test_connect_mcp_server_hello() {
        let config = McpServerConfig {
            name: "hello-mcp-server".to_string(),
            command: "uvx".to_string(),
            args: Some(vec![
                "--from".to_string(),
                "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
                "hello-mcp-server".to_string(),
            ]),
            env: None,
            enabled: true,
        };

        let result = connect_mcp_server(&config).await;

        // This should succeed and return a handle with tools
        let handle = result.expect("Failed to connect to hello-mcp-server");
        let tools = handle.tools();
        assert!(!tools.is_empty(), "Expected at least one tool");

        println!("Connected to hello-mcp-server with {} tools", tools.len());
    }

    /// Test that connect_mcp_server works with environment variables
    #[tokio::test]
    async fn test_connect_mcp_server_with_env() {
        let mut env = HashMap::new();
        env.insert("TEST_ENV_VAR".to_string(), "test_value".to_string());

        let config = McpServerConfig {
            name: "hello-mcp-server-with-env".to_string(),
            command: "uvx".to_string(),
            args: Some(vec![
                "--from".to_string(),
                "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
                "hello-mcp-server".to_string(),
            ]),
            env: Some(env),
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

    /// Test that we can actually call a tool on the MCP server
    /// This is the key test - keeping the service alive while calling tools
    #[tokio::test]
    async fn test_connect_mcp_server_call_tool() {
        let config = McpServerConfig {
            name: "hello-mcp-server".to_string(),
            command: "uvx".to_string(),
            args: Some(vec![
                "--from".to_string(),
                "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
                "hello-mcp-server".to_string(),
            ]),
            env: None,
            enabled: true,
        };

        // Connect and get the handle (which keeps the service alive)
        let handle = connect_mcp_server(&config)
            .await
            .expect("Failed to connect to hello-mcp-server");

        // Get tools from the handle - the service is kept alive by the handle
        let tools = handle.tools();
        assert!(!tools.is_empty(), "Expected at least one tool");

        // Call the first tool
        let first_tool = &tools[0];
        println!("Calling tool: {}", first_tool.name());

        let result = first_tool
            .call("{}".to_string())
            .await
            .expect("Failed to call tool");

        println!("Tool call result: {:?}", result);

        // Verify we got a response
        assert!(
            !result.is_empty(),
            "Expected non-empty result from tool call"
        );
    }
}
