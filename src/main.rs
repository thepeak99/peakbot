mod config;
mod tools;

use anyhow::Result;
use rig::completion::message::Message;
use rig::completion::Prompt;
use rig::prelude::*;
use rig::providers::openrouter;
use rig::tool::rmcp::McpTool;
use rig::tool::ToolDyn;
use rmcp::service::ServiceExt;
use std::fs;
use std::io::{self, BufRead, Write};

use config::{Config, McpServerConfig};
use tools::{BashTool, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool};

/// Load the base system prompt from system_prompt.txt
fn load_base_system_prompt() -> String {
    fs::read_to_string("system_prompt.txt").unwrap_or_else(|e| {
        eprintln!("Warning: Could not read system_prompt.txt: {}", e);
        "You are PeakBot, a coding agent.".to_string()
    })
}

/// Connect to an MCP server and return its tools
async fn connect_mcp_server(config: &McpServerConfig) -> Result<Vec<McpTool>> {
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
    
    // Create McpTool for each tool
    let mcp_tools: Vec<McpTool> = tools.tools.into_iter().map(|tool| {
        McpTool::from_mcp_server(tool, service.clone())
    }).collect();
    
    Ok(mcp_tools)
}

/// Load and connect to all configured MCP servers
async fn load_mcp_servers(config: &Config) -> Result<Vec<McpTool>> {
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

/// Check for agents.md in the current directory (case insensitive) and load its contents
fn load_agents_md() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    
    // List all entries in current directory and find one that matches "agents.md" (case insensitive)
    let entries = match std::fs::read_dir(&cwd) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("Warning: Could not read current directory: {}", e);
            return None;
        }
    };
    
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                if file_name.to_lowercase() == "agents.md" {
                    match std::fs::read_to_string(&path) {
                        Ok(contents) => {
                            println!("Loaded agents.md from: {}", path.display());
                            return Some(contents);
                        }
                        Err(e) => {
                            eprintln!("Warning: Could not read {}: {}", path.display(), e);
                        }
                    }
                }
            }
        }
    }
    
    None
}

/// Build the full system prompt by combining the base prompt with agents.md if present
fn build_system_prompt() -> String {
    let mut prompt = load_base_system_prompt();
    
    if let Some(agents_content) = load_agents_md() {
        prompt.push_str("\n\n---\n\n");
        prompt.push_str("## Additional Agent Configuration (from agents.md)\n\n");
        prompt.push_str(&agents_content);
    }
    
    prompt
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
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

    // Build system prompt (combines base prompt with agents.md if present in current directory)
    let system_prompt = build_system_prompt();

    // Load MCP servers and get their tools
    let mcp_tools = load_mcp_servers(&config).await?;
    let mcp_tool_count = mcp_tools.len();
    tracing::info!("Loaded {} total MCP tools", mcp_tool_count);

    // Convert MCP tools to boxed dyn ToolDyn
    let mcp_tools_boxed: Vec<Box<dyn ToolDyn>> = mcp_tools
        .into_iter()
        .map(|t| Box::new(t) as Box<dyn ToolDyn>)
        .collect();

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
        .tools(mcp_tools_boxed)
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
