//! PeakBot library - Core functionality for connecting to MCP servers and managing tools.

mod config;
mod context_manager;
mod hooks;
mod skills;
mod token_estimator;
mod tools;

pub use config::{Config, ContextConfig, McpServerConfig, SearXngConfig};
pub use context_manager::{CompactionResult, ContextManager};
pub use hooks::{
    CostTrackingStats, ModelPricing, SessionStats, TokenCostHook, fetch_model_pricing,
};
use rig::agent::PromptHook;
use rig::client::{Capabilities, Capable, Client};
use rig::completion::{CompletionModel, Prompt};
use rig::tool::ToolDyn;
use rig::tool::rmcp::McpTool;
use rmcp::transport::TokioChildProcess;
pub use skills::{SkillRegistry, load_default_skills};
pub use token_estimator::{get_default_estimator, get_model_context_window, SimpleEstimator, TiktokenEstimator, TokenEstimator};
pub use tools::{
    BashTool, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool, LoggingToolDyn,
    SearchTool, ThinkTool,
};

use anyhow::{Result, anyhow};
use rig::providers::openrouter;
use rig::{agent::Agent, completion::message::Message};
use rmcp::service::ServiceExt;
use std::io::{self, BufRead, Write};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tracing::debug;

const SYSTEM_PROMPT: &str = include_str!("system_prompt.txt");

/// Build the system prompt dynamically with environment information
fn build_system_prompt(skills: &SkillRegistry) -> String {
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

/// Create the OpenRouter client from config
pub fn create_openrouter_client(config: &Config) -> Result<openrouter::Client> {
    let api_key = config.openrouter_api_key.clone().unwrap_or_default();
    if api_key.is_empty() {
        anyhow::bail!("OpenRouter API key not configured. Set OPENROUTER_API_KEY env var");
    }

    let client = openrouter::Client::builder()
        .api_key(&api_key)
        .build()
        .expect("Failed to create OpenRouter client");

    Ok(client)
}

use rig::client::completion::CompletionClient;
/// Build the agent with all tools (built-in and MCP)
/// Returns the agent and a reference to the session stats
pub async fn build_agent<M, Ext>(
    client: &Client<Ext>,
    config: &Config,
    mcp_server_handles: &[McpServerHandle],
    skills: &SkillRegistry,
) -> Result<(Agent<M, TokenCostHook>, Arc<Mutex<SessionStats>>)>
where
    M: CompletionModel<Client = Client<Ext>>, //Type bending
    Ext: Capabilities<Completion = Capable<M>>,
{
    // Create completion model with configured model name
    let model_name = config.openrouter_model.clone();

    // Build the system prompt dynamically with environment info and skills
    let system_prompt = build_system_prompt(skills);

    let mcp_tools = mcp_server_handles
        .iter()
        .flat_map(|handle| handle.dyn_tools())
        .collect();

    // Create the token cost hook if enabled
    let hook = if config.cost_tracking {
        TokenCostHook::new(
            model_name.clone(),
            fetch_model_pricing(
                config.openrouter_api_key.as_deref().unwrap_or(""),
                &model_name,
            )
            .await?,
        )
    } else {
        // Create a no-op hook with zero pricing
        TokenCostHook::new(
            model_name.clone(),
            ModelPricing {
                input_per_token: 0.0,
                output_per_token: 0.0,
            },
        )
    };

    // Get the stats reference before moving the hook into the agent
    let stats = hook.get_stats();

    // Build the agent with all tools and the hook
    let mut agent_builder = client
        .agent(model_name)
        .preamble(&system_prompt)
        .max_tokens(config.openrouter_max_tokens)
        .default_max_turns(config.agent_max_turns)
        .hook(hook)
        .tool(FileEditTool::default())
        .tool(FileReadTool)
        .tool(BashTool)
        .tool(ListDirectoryTool)
        .tool(FetchUrlTool)
        .tool(ThinkTool);

    // Conditionally add search tool if SearXNG is configured
    if config.searxng_enabled() {
        if let Some(searxng_config) = &config.searxng {
            agent_builder = agent_builder.tool(SearchTool::new(searxng_config));
            tracing::info!("SearXNG search enabled: {}", searxng_config.base_url);
        }
    }

    let agent = agent_builder.tools(mcp_tools).build();

    Ok((agent, stats))
}

/// A type that can handle agent prompts with history support
pub struct AgentRunner<M: CompletionModel, P: PromptHook<M>> {
    agent: Agent<M, P>,
    config: Config,
    skills: SkillRegistry,
    stats: Arc<Mutex<SessionStats>>,
    context_manager: Option<ContextManager>,
    system_prompt: String,
}

impl<M: CompletionModel, P: PromptHook<M> + 'static> AgentRunner<M, P> {
    /// Create a new AgentRunner
    pub fn new(
        agent: Agent<M, P>,
        config: Config,
        skills: SkillRegistry,
        stats: Arc<Mutex<SessionStats>>,
    ) -> Self {
        // Build system prompt for context manager
        let system_prompt = build_system_prompt(&skills);
        
        // Create context manager (always created, enabled flag controls actual usage)
        let context_manager = Some(ContextManager::new(config.context.clone(), &config.openrouter_model));
        
        Self {
            agent,
            config,
            skills,
            stats,
            context_manager,
            system_prompt,
        }
    }

    /// Print stats for the last request
    fn print_last_request_stats(&self) {
        if let Ok(stats) = self.stats.lock() {
            if let Some(last) = stats.last_request() {
                println!(
                    "{}",
                    stats.format_per_request(last.input_tokens, last.output_tokens, last.cost)
                );
            }
        }
    }

    /// Print session summary
    fn print_stats(&self) {
        if let Ok(stats) = self.stats.lock() {
            println!("\n=== Session Statistics ===\n");
            println!("{}", stats.summary());
            println!();
        }
    }

    /// Reset session stats
    fn reset_stats(&self) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.reset();
        }
    }

    /// Print context status
    fn print_context_status(&self, chat_history: &[Message]) {
        if let Some(ref cm) = self.context_manager {
            println!("\n=== Context Status ===\n");
            println!("{}", cm.format_status(chat_history, &self.system_prompt));
            println!();
        } else {
            println!("\nContext compaction is not enabled.\n");
        }
    }

    /// Force context compaction
    fn force_compact(&mut self, chat_history: &mut Vec<Message>) {
        if let Some(ref mut cm) = self.context_manager {
            match cm.compact(chat_history, &self.system_prompt) {
                Ok(result) => {
                    println!(
                        "\n[Context compacted: {} → {} messages, {} messages summarized]\n",
                        result.original_count,
                        result.compacted_count,
                        result.num_summarized
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
        println!("Model: {}", self.config.openrouter_model);
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
            if self.config.cost_tracking {
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
                self.force_compact(&mut chat_history);
                continue;
            }

            // Check if context compaction is needed before prompting
            if let Some(ref mut cm) = self.context_manager {
                if cm.needs_compaction(&chat_history) {
                    println!("[Context approaching limit, compacting before prompt...]");
                    cm.compact(&mut chat_history, &self.system_prompt)
                        .map(|result| {
                            println!(
                                "[Compacted: {} → {} messages, {} summarized]\n",
                                result.original_count,
                                result.compacted_count,
                                result.num_summarized
                            );
                        })
                        .unwrap_or_else(|e| {
                            eprintln!("[Warning: compaction failed: {}]", e);
                        });
                }
            }

            // Clone history since chat() takes ownership
            match self
                .agent
                .prompt(input)
                .with_history(&mut chat_history)
                .await
            {
                Ok(response) => {
                    println!("\n{}", response);
                    // Display token stats after each response
                    self.print_last_request_stats();
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

    fn dyn_tools(&self) -> Vec<Box<dyn ToolDyn>> {
        self.tools
            .iter()
            .map(|tool| LoggingToolDyn::new(tool.to_owned(), &self.name))
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
