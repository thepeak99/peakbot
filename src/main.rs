mod config;
mod tools;

use anyhow::Result;
use tracing_subscriber::EnvFilter;
use rig::completion::message::Message;
use rig::completion::Prompt;
use rig::prelude::*;
use rig::providers::openrouter;
use rig::tool::rmcp::McpTool;
use rig::tool::ToolDyn;
use rmcp::service::ServiceExt;
use std::io::{self, BufRead, Write};

use config::{Config, McpServerConfig};
use tools::{BashTool, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool, LoggingToolDyn};

const SYSTEM_PROMPT: &str = include_str!("system_prompt.txt");

/// Connect to an MCP server and return its tools (wrapped with LoggingToolDyn)
async fn connect_mcp_server(config: &McpServerConfig) -> Result<Vec<Box<dyn ToolDyn>>> {
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
    let transport = rmcp::transport::TokioChildProcess::new(cmd)
        .map_err(|e| anyhow::anyhow!("Failed to create child process: {}", e))?;
    
    // Connect to the MCP server
    let service = ().serve(transport).await
        .map_err(|e| anyhow::anyhow!("Failed to connect to MCP server: {}", e))?;
    
    // Get server info
    let server_info = service.peer_info();
    tracing::info!("Connected to MCP server '{}': {:?}", config.name, server_info);
    
    // List available tools (use None for paginated request)
    let tools = service.list_tools(None).await
        .map_err(|e| anyhow::anyhow!("Failed to list tools: {}", e))?;
    
    tracing::info!("MCP server '{}' has {} tools", config.name, tools.tools.len());
    
    // Create wrapped McpTool for each tool with logging
    let mcp_tools: Vec<Box<dyn ToolDyn>> = tools.tools.into_iter().map(|tool| {
        let inner = Box::new(McpTool::from_mcp_server(tool, service.clone())) as Box<dyn ToolDyn>;
        Box::new(LoggingToolDyn::new(inner, &config.name)) as Box<dyn ToolDyn>
    }).collect();
    
    Ok(mcp_tools)
}

/// Load and connect to all configured MCP servers
async fn load_mcp_servers(config: &Config) -> Result<Vec<Box<dyn ToolDyn>>> {
    let mut all_tools = Vec::new();
    
    let mcp_servers_json = match &config.mcp_servers {
        Some(json) => json,
        None => {
            tracing::info!("No MCP servers configured");
            return Ok(all_tools);
        }
    };
    
    let servers: Vec<McpServerConfig> = serde_json::from_str(mcp_servers_json)
        .map_err(|e| anyhow::anyhow!("Failed to parse MCP_SERVERS JSON: {}", e))?;
    
    for server_config in servers {
        tracing::info!("Connecting to MCP server: {}", server_config.name);
        match connect_mcp_server(&server_config).await {
            Ok(tools) => {
                tracing::info!("Loaded {} tools from MCP server '{}'", tools.len(), server_config.name);
                all_tools.extend(tools);
            }
            Err(e) => {
                tracing::error!("Failed to connect to MCP server '{}': {}", server_config.name, e);
            }
        }
    }
    
    Ok(all_tools)
}



#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_env_filter(EnvFilter::new("peakbot=info"))
        .init();

    // Load configuration from environment variables
    let config = Config::load().unwrap_or_default();

    // Get API key from config
    let api_key = config.openrouter_api_key.clone().unwrap_or_default();
    if api_key.is_empty() {
        anyhow::bail!("OpenRouter API key not configured. Set OPENROUTER_API_KEY env var");
    }

    use rig::providers::openrouter::Client;
    
    let client: Client = openrouter::Client::builder()
        .api_key(&api_key)
        .build()
        .expect("Failed to create OpenRouter client");

    // Create completion model with configured model name
    let model_name = config.openrouter_model.clone();

    // Use embedded system prompt
    let system_prompt = SYSTEM_PROMPT;

    // Load MCP servers and get their tools (already wrapped with LoggingToolDyn)
    let mcp_tools = load_mcp_servers(&config).await?;
    let mcp_tool_count = mcp_tools.len();
    tracing::info!("Loaded {} total MCP tools", mcp_tool_count);

    // Build the agent with all tools
    let agent = client
        .agent(model_name)
        .preamble(&system_prompt)
        .max_tokens(config.openrouter_max_tokens as u64)
        .tool(FileEditTool::default())
        .tool(FileReadTool)
        .tool(BashTool)
        .tool(ListDirectoryTool)
        .tool(FetchUrlTool)
        .tools(mcp_tools)
        .build();

    let cwd = std::env::current_dir()?;
    println!("PeakBot coding agent ready.");
    println!("Model: {}", config.openrouter_model);
    if mcp_tool_count > 0 {
        println!("MCP tools: {}", mcp_tool_count);
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

        match agent
            .prompt(input)
            .with_history(&mut chat_history)
            .max_turns(config.agent_max_turns as usize)
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
