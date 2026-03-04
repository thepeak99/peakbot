mod config;
mod tools;

use anyhow::Result;
use rig::completion::message::Message;
use rig::completion::Prompt;
use rig::prelude::*;
use rig::providers::openrouter;
use std::io::{self, BufRead, Write};

use config::Config;
use tools::{BashTool, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool};

const SYSTEM_PROMPT: &str = include_str!("system_prompt.txt");

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

    let agent = client
        .agent(model_name)
        .preamble(SYSTEM_PROMPT)
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
