//! PeakBot library - Core functionality for connecting to MCP servers and managing tools.

mod config;
mod context_manager;
mod hooks;
mod providers;
mod skills;
mod tools;

pub use config::{Config, ContextConfig, McpServerConfig, OllamaConfig, OpenRouterConfig, ProviderConfig, ProviderType, SearXngConfig};
pub use context_manager::{CompactionResult, ContextManager};
pub use hooks::{
    CostTrackingStats, ModelPricing, SessionStats, TokenCostHook, fetch_model_pricing,
};
pub use providers::{create_provider, CostTracker, DynAgent, ProviderInfo};
use rig::completion::message::Message;
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

/// A type that can handle agent prompts with history support
pub struct AgentRunner {
    agent: Arc<DynAgent>,
    config: Config,
    provider_info: ProviderInfo,
    skills: SkillRegistry,
    context_manager: Option<ContextManager>,
    system_prompt: String,
    cost_tracker: CostTracker,
    todo_state: Option<Arc<Mutex<TodoList>>>,
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
            cost_tracker.get_session_stats().unwrap_or_else(|| Arc::new(Mutex::new(SessionStats::new()))),
            system_prompt_tokens,
            Some(agent.clone()),
        ));
        
        Self {
            agent,
            config,
            provider_info,
            skills,
            context_manager,
            system_prompt,
            cost_tracker,
            todo_state,
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

    /// Force context compaction
    async fn force_compact(&mut self, chat_history: &mut Vec<Message>) {
        if let Some(ref mut cm) = self.context_manager {
            match cm.compact(chat_history, &self.system_prompt).await {
                Ok(result) => {
                    println!(
                        "\n[Context compacted: {} → {} messages, {} messages discarded]\n",
                        result.original_count,
                        result.compacted_count,
                        result.num_discarded
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

    /// Run the interactive REPL loop
    pub async fn run(&mut self) -> Result<()> {
        let cwd = std::env::current_dir()?;
        println!("PeakBot coding agent ready.");
        println!("Provider: {} | Model: {}", self.config.provider_name(), self.config.model());
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
        println!("Working directory: {}", cwd.display());
        println!("Type /stats to see session stats, /context for context status, /compact to force compaction, or 'exit' to quit.\n");

        let stdin = io::stdin();
        let mut stdout = io::stdout();
        let mut chat_history: Vec<Message> = Vec::new();

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

            // Check if context compaction is needed before prompting
            if let Some(ref mut cm) = self.context_manager {
                if cm.needs_compaction(&chat_history) {
                    println!("[Context approaching limit, compacting before prompt...]");
                    cm.compact(&mut chat_history, &self.system_prompt)
                        .await
                        .map(|result| {
                            println!(
                                "[Compacted: {} → {} messages, {} discarded]\n",
                                result.original_count,
                                result.compacted_count,
                                result.num_discarded
                            );
                        })
                        .unwrap_or_else(|e| {
                            eprintln!("[Warning: compaction failed: {}]", e);
                        });
                }
            }

            // Use the agent to prompt with history
            match self
                .agent
                .as_ref()
                .prompt_with_history(input, &mut chat_history)
                .await
            {
                Ok(response) => {
                    println!("\n{}", response);
                    // Display token stats after each response
                    self.print_last_request_stats();
                    // Display todo summary after each response
                    self.print_todo_summary();
                }
                Err(e) => {
                    eprintln!("\nError: {}\n", e);
                }
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
