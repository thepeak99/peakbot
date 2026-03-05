//! PeakBot library - Core functionality for connecting to MCP servers and managing tools.

mod config;
mod skills;
mod tools;

pub use config::{Config, McpServerConfig};
use rig::agent::PromptHook;
use rig::client::{Capabilities, Capable, Client};
use rig::completion::{CompletionModel, Prompt};
use rig::tool::ToolDyn;
use rig::tool::rmcp::McpTool;
use rmcp::transport::TokioChildProcess;
pub use skills::{SkillRegistry, load_default_skills};
pub use tools::{
    BashTool, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool, LoggingToolDyn,
};

use anyhow::{Result, anyhow};
use core::prelude;
use rig::providers::openrouter;
use rig::{agent::Agent, completion::message::Message};
use rmcp::service::ServiceExt;
use std::io::{self, BufRead, Write};
use std::process::Stdio;
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
pub async fn build_agent<M, Ext>(
    client: &Client<Ext>,
    config: &Config,
    mcp_server_handles: &[McpServerHandle],
    skills: &SkillRegistry,
) -> Agent<M>
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
        .map(|handle| handle.dyn_tools())
        .flatten()
        .collect();

    // Build the agent with all tools
    client
        .agent(model_name)
        .preamble(&system_prompt)
        .max_tokens(config.openrouter_max_tokens)
        .default_max_turns(config.agent_max_turns)
        .tool(FileEditTool::default())
        .tool(FileReadTool)
        .tool(BashTool)
        .tool(ListDirectoryTool)
        .tool(FetchUrlTool)
        .tools(mcp_tools)
        .build()
}

/// A type that can handle agent prompts with history support
pub struct AgentRunner<M: CompletionModel, P: PromptHook<M>> {
    agent: Agent<M, P>,
    config: Config,
    skills: SkillRegistry,
}

impl<M: CompletionModel, P: PromptHook<M> + 'static> AgentRunner<M, P> {
    /// Create a new AgentRunner
    pub fn new(agent: Agent<M, P>, config: Config, skills: SkillRegistry) -> Self {
        Self {
            agent,
            config,
            skills,
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
        println!("Working directory: {}", cwd.display());
        println!("Type your message (or 'exit' to quit).\n");

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
                println!("Goodbye!");
                break;
            }

            // Clone history since chat() takes ownership
            match self
                .agent
                .prompt(input)
                .with_history(&mut chat_history)
                .await
            {
                Ok(response) => {
                    println!("\n{}\n", response);
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
