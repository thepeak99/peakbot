//! REPL UI Implementation — a View in MVC
//!
//! The REPL View:
//! - Reads user input and sends UiActions to the Controller (AgentRunner)
//! - Subscribes to StateManager and renders state to stdout
//! - Never calls the agent directly
//!
//! Data flow:
//!   User input → UiAction → Controller → Model (StateManager) → broadcast → View (render)

use anyhow::Result;
use std::io::{self, BufRead, Write};
use std::sync::Arc;
use std::thread;
use tokio::sync::mpsc::UnboundedSender;

use crate::ui::app_state::{AppState, MessageRole, TodoState, WelcomeState};
use crate::ui::state_manager::StateManager;
use crate::ui::ui_trait::{Ui, UiAction};

/// REPL View — subscribes to StateManager and renders to stdout
pub struct ReplUi {
    state_manager: Arc<StateManager>,
    /// Send user actions to the Controller
    action_sender: UnboundedSender<UiAction>,
    /// Whether the view is running
    running: bool,
    /// Last API call count — used to detect prompt completions
    last_api_calls: u64,
    /// Last message count — only render new messages since last render
    last_message_count: usize,
    /// Whether the welcome message has been printed
    welcome_printed: bool,
}

impl ReplUi {
    pub fn new(state_manager: Arc<StateManager>, action_sender: UnboundedSender<UiAction>) -> Self {
        Self {
            state_manager,
            action_sender,
            running: true,
            last_api_calls: 0,
            last_message_count: 0,
            welcome_printed: false,
        }
    }

    /// Send a UiAction to the Controller
    fn send_action(&self, action: UiAction) {
        let _ = self.action_sender.send(action);
    }
}

impl Ui for ReplUi {
    async fn init(&mut self) -> Result<()> {
        Ok(())
    }

    /// Run the REPL view loop:
    /// 1. Spawn a thread to read stdin and send UiActions to Controller
    /// 2. Subscribe to StateManager and render on every state update
    async fn run(&mut self) -> Result<()> {
        // Spawn stdin reader thread
        let action_sender = self.action_sender.clone();
        thread::spawn(move || {
            let stdin = io::stdin();
            for line in stdin.lock().lines() {
                match line {
                    Ok(input) => {
                        let input = input.trim();
                        if input.is_empty() {
                            continue;
                        }
                        let action = if input.starts_with('/') {
                            UiAction::ExecuteCommand(input.to_string())
                        } else {
                            UiAction::SendMessage(input.to_string())
                        };
                        let _ = action_sender.send(action);
                    }
                    Err(_) => {
                        break;
                    }
                }
            }
            let _ = action_sender.send(UiAction::Exit);
        });

        // Subscribe to state updates
        let state_receiver = self.state_manager.subscribe();

        // Initial prompt
        print!("> ");
        std::io::stdout().flush().ok();

        loop {
            // Wait for next state update
            match state_receiver.recv() {
                Ok(state) => {
                    self.render(&state);
                    print!("> ");
                    std::io::stdout().flush().ok();
                }
                Err(_) => {
                    // Channel closed — controller has exited
                    break;
                }
            }

            // Check if we've been signaled to exit
            if !self.running {
                break;
            }
        }

        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        self.running = false;
        Ok(())
    }
}

impl ReplUi {
    /// Render the current state to stdout
    fn render(&mut self, state: &AppState) {
        // Print welcome banner (once, on first render)
        if !self.welcome_printed && state.welcome.is_some() {
            self.print_welcome(state.welcome.as_ref().unwrap());
            self.welcome_printed = true;
        }

        // Only print NEW messages since last render
        let new_messages: Vec<_> = state.chat.messages
            .iter()
            .rev()
            .take(state.chat.messages.len().saturating_sub(self.last_message_count))
            .collect();

        for msg in new_messages.iter().rev() {
            match msg.role {
                MessageRole::Agent => {
                    println!("\n{}", msg.content);
                }
                MessageRole::ToolCall => {
                    // Show compact tool call indicator
                    if let Some(name) = msg.content.strip_prefix("🔧 ") {
                        if let Some(name) = name.split('(').next() {
                            print!("[🔧 {}] ", name);
                        }
                    }
                }
                MessageRole::ToolResult => {
                    // Skip verbose tool results in REPL
                }
                MessageRole::User => {
                    // User messages shown elsewhere (just print a separator)
                }
                MessageRole::System => {}
            }
        }

        // Token report — print when API call count increases (prompt completed)
        if state.stats.total_api_calls > self.last_api_calls {
            self.last_api_calls = state.stats.total_api_calls;
            self.print_token_report(&state.stats);
        }

        // Print todo list if non-empty
        if !state.todo.items.is_empty() {
            self.render_todo(&state.todo);
        }

        // Update last message count
        self.last_message_count = state.chat.messages.len();
    }

    /// Print the welcome banner
    fn print_welcome(&self, welcome: &WelcomeState) {
        println!();
        println!("╔══════════════════════════════════════════════════════════╗");
        println!("║                    PeakBot Ready                        ║");
        println!("╠══════════════════════════════════════════════════════════╣");
        println!("║  Provider:  {}                                      ║", pad_right(&welcome.provider_name, 41));
        println!("║  Model:     {}                                 ║", pad_right(&welcome.model, 41));
        println!("║  Max Tokens: {}                                           ║", pad_right(&welcome.max_tokens.to_string(), 41));
        println!("║  Tools:     {} built-in, {} MCP                      ║",
            welcome.builtin_tools_count, welcome.mcp_tools_count);
        println!("║  Skills:    {}                                           ║",
            welcome.skills_count);
        if welcome.searxng_enabled {
            if let Some(ref url) = welcome.searxng_url {
                println!("║  SearXNG:   {} (enabled)              ║",
                    pad_right(url, 36));
            } else {
                println!("║  SearXNG:   enabled                                   ║");
            }
        } else {
            println!("║  SearXNG:   not configured                             ║");
        }
        if welcome.cost_tracking_enabled {
            println!("║  Cost:      enabled                                    ║");
        } else {
            println!("║  Cost:      not available                               ║");
        }
        if welcome.compaction_enabled {
            println!("║  Compaction: {:.0}% threshold, keep {} recent           ║",
                welcome.compaction_threshold * 100.0,
                welcome.compaction_keep_recent);
        } else {
            println!("║  Compaction: disabled                                  ║");
        }
        println!("║  CWD:       {} ║", pad_right(&welcome.cwd.to_string_lossy(), 47));
        println!("╚══════════════════════════════════════════════════════════╝");
        println!();
        println!("Type your message or /help for commands. Press Ctrl+C to exit.");
        println!();
    }

    /// Print the token report
    fn print_token_report(&self, stats: &crate::ui::app_state::SessionState) {
        let total = stats.total_tokens();
        let cost_str = if stats.total_cost > 0.0001 {
            format!("${:.4}", stats.total_cost)
        } else {
            "N/A".to_string()
        };
        println!(
            "\n[Tokens: {} in / {} out | Cost: {} | Total: {}]\n",
            stats.total_input_tokens,
            stats.total_output_tokens,
            cost_str,
            total
        );
    }

    /// Render the todo list
    fn render_todo(&self, todo: &TodoState) {
        use crate::TodoStatus;

        let (pending, in_progress, completed, cancelled) = todo.count_by_status();
        println!("\n=== Todo List ===");
        println!(
            "Total: {} ({} pending, {} in-progress, {} completed, {} cancelled)\n",
            todo.items.len(),
            pending,
            in_progress,
            completed,
            cancelled
        );

        for item in &todo.items {
            let icon = match item.status {
                TodoStatus::Pending => "○",
                TodoStatus::InProgress => "◐",
                TodoStatus::Completed => "●",
                TodoStatus::Cancelled => "✗",
            };
            println!(
                "  {} #{} [{}] {}",
                icon,
                item.id,
                item.status,
                item.text
            );
        }
        println!();
    }
}

/// Pad a string to the right with spaces to fit within width
fn pad_right(s: &str, width: usize) -> String {
    if s.len() >= width {
        s[..width.min(s.len())].to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - s.len()))
    }
}
