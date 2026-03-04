mod tools;

use anyhow::Result;
use rig::completion::message::Message;
use rig::completion::Prompt;
use rig::prelude::*;
use rig::providers::anthropic;
use std::io::{self, BufRead, Write};

use tools::{BashTool, FileEditTool, FileReadTool, ListDirectoryTool};

const SYSTEM_PROMPT: &str = include_str!("system_prompt.txt");

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    let client = anthropic::Client::from_env();

    let agent = client
        .agent(anthropic::completion::CLAUDE_4_SONNET)
        .preamble(SYSTEM_PROMPT)
        .max_tokens(4096)
        .tool(FileEditTool::default())
        .tool(FileReadTool)
        .tool(BashTool)
        .tool(ListDirectoryTool)
        .build();

    let cwd = std::env::current_dir()?;
    println!("PeakBot coding agent ready.");
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
            .max_turns(15)
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
