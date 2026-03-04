mod config;
mod tools;

use anyhow::Result;
use rig::completion::Prompt;
use rig::completion::message::Message;
use rig::prelude::*;
use rig::providers::openrouter;
use std::io::{self, BufRead, Write};
use tracing_subscriber::EnvFilter;

use peakbot::{
    BashTool, Config, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool, load_mcp_servers,
};

const SYSTEM_PROMPT: &str = include_str!("system_prompt.txt");

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter(EnvFilter::new("peakbot=debug,envy=debug"))
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
        .preamble(system_prompt)
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
