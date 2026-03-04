mod config;
mod tools;

use anyhow::Result;
use rig::completion::message::Message;
use rig::completion::Prompt;
use rig::prelude::*;
use rig::providers::openrouter;
use std::fs;
use std::io::{self, BufRead, Write};

use config::Config;
use tools::{BashTool, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool};

/// Load the base system prompt from system_prompt.txt
fn load_base_system_prompt() -> String {
    fs::read_to_string("system_prompt.txt").unwrap_or_else(|e| {
        eprintln!("Warning: Could not read system_prompt.txt: {}", e);
        "You are PeakBot, a coding agent.".to_string()
    })
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
    let api_key = config.openrouter_api_key.unwrap_or_default();
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

    let agent = client
        .agent(model_name)
        .preamble(&system_prompt)
        .max_tokens(config.openrouter_max_tokens as u64)
        .tool(FileEditTool::default())
        .tool(FileReadTool)
        .tool(BashTool)
        .tool(ListDirectoryTool)
        .tool(FetchUrlTool)
        .build();

    let cwd = std::env::current_dir()?;
    println!("PeakBot coding agent ready.");
    println!("Model: {}", config.openrouter_model);
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
