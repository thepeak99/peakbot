//! REPL UI Implementation
//!
//! This module provides a REPL (Read-Eval-Print Loop) UI implementation
//! that wraps the existing synchronous stdin/stdout interface.

use anyhow::Result;
use std::io::{self, BufRead, Write};
use std::sync::mpsc::Sender;

use crate::ui::app_state::{AppState, ChatMessage};
use crate::ui::state_manager::StateManager;
use crate::ui::ui_trait::{Ui, UiAction, TodoItemAction};

/// REPL UI implementation for PeakBot
///
/// This wraps the existing synchronous REPL interface as a Ui trait
/// implementation for backward compatibility.
pub struct ReplUi {
    /// Reference to the state manager
    state_manager: StateManager,
    
    /// Channel to send actions to the agent
    action_sender: Option<Sender<UiAction>>,
    
    /// Whether the REPL should continue running
    running: bool,
}

impl ReplUi {
    /// Create a new ReplUi instance
    pub fn new(state_manager: StateManager, action_sender: Option<Sender<UiAction>>) -> Self {
        Self {
            state_manager,
            action_sender,
            running: true,
        }
    }

    /// Get current state
    fn get_state(&self) -> AppState {
        self.state_manager.get_state()
    }

    /// Print a chat message to stdout

    /// Print session stats
    fn print_stats(&self) {
        let state = self.get_state();
        let stats = &state.stats;
        
        println!("\n=== Session Statistics ===\n");
        println!("Model: {}", stats.model);
        println!("Total input tokens: {}", stats.format_tokens(stats.total_input_tokens));
        println!("Total output tokens: {}", stats.format_tokens(stats.total_output_tokens));
        println!("Total tokens: {}", stats.format_tokens(stats.total_tokens()));
        println!("Total API calls: {}", stats.total_api_calls);
        println!("Total cost: ${}", stats.format_cost());
        println!();
    }

    /// Print context status
    fn print_context_status(&self) {
        let state = self.get_state();
        let context = &state.context;
        
        println!("\n=== Context Status ===\n");
        println!("Current usage: {} tokens", context.current_usage);
        println!("Window size: {} tokens", context.window_size);
        println!("Usage percentage: {:.1}%", context.usage_percentage());
        println!("Compaction enabled: {}", context.compaction_enabled);
        println!("Compaction threshold: {:.0}%", context.compaction_threshold * 100.0);
        println!();
    }

    /// Print TODO list
    fn print_todo_list(&self) {
        let state = self.get_state();
        let todo = &state.todo;
        
        if todo.items.is_empty() {
            println!("\nNo TODO items.\n");
            return;
        }
        
        let (pending, in_progress, completed, cancelled) = todo.count_by_status();
        println!("\n=== TODO List ===\n");
        println!("Total: {} items ({} pending, {} in-progress, {} completed, {} cancelled)\n",
            todo.items.len(), pending, in_progress, completed, cancelled);
        
        for (i, item) in todo.items.iter().enumerate() {
            let checkbox = if item.completed { "■" } else { "☐" };
            let status_str = match item.status {
                crate::tools::todo::TodoStatus::Pending => "Pending",
                crate::tools::todo::TodoStatus::InProgress => "In Progress",
                crate::tools::todo::TodoStatus::Completed => "Completed",
                crate::tools::todo::TodoStatus::Cancelled => "Cancelled",
            };
            println!("  [{}] {} {} - {}", i, checkbox, status_str, item.text);
        }
        println!();
    }
}

impl Ui for ReplUi {
    /// Initialize the REPL (nothing to do for stdin/stdout)
    fn init(&mut self) -> Result<()> {
        // REPL doesn't need initialization
        Ok(())
    }

    /// Run the REPL event loop (blocking)
    fn run(&mut self) -> Result<()> {
        println!("PeakBot REPL ready.");
        println!("Type 'exit' or 'quit' to quit.");
        println!("Use /stats, /context, /todo for commands.\n");

        let stdin = io::stdin();
        let mut stdout = io::stdout();

        while self.running {
            print!("> ");
            stdout.flush()?;

            let mut input = String::new();
            stdin.lock().read_line(&mut input)?;
            let input = input.trim();

            if input.is_empty() {
                continue;
            }

            // Handle exit commands
            if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
                self.print_stats();
                println!("Goodbye!");
                break;
            }

            // Handle commands
            match input.to_lowercase().as_str() {
                "/stats" => {
                    self.print_stats();
                }
                "/context" => {
                    self.print_context_status();
                }
                "/todo" | "/todos" => {
                    self.print_todo_list();
                }
                "/todo toggle <n>" if input.len() > 13 => {
                    // Parse the index
                    if let Ok(index) = input[13..].trim().parse::<usize>() {
                        let _ = self.action_sender.as_ref().map(|s| {
                            s.send(UiAction::UpdateTodoItem(index, TodoItemAction::ToggleComplete))
                        });
                    } else {
                        println!("Invalid index. Usage: /todo toggle <index>\n");
                    }
                }
                "/todo clear" => {
                    let _ = self.action_sender.as_ref().map(|s| s.send(UiAction::ClearCompletedTodos));
                    self.print_todo_list();
                }
                _ => {
                    // Regular message - send to agent
                    if !input.starts_with('/') {
                        let _ = self.action_sender.as_ref().map(|s| s.send(UiAction::SendMessage(input.to_string())));
                    } else {
                        println!("Unknown command: {}\n", input);
                    }
                }
            }
        }

        Ok(())
    }

    /// Shutdown the REPL gracefully
    fn shutdown(&mut self) -> Result<()> {
        self.running = false;
        Ok(())
    }

    /// Handle a user action
    fn on_action(&mut self, action: UiAction) -> Result<()> {
        match action {
            UiAction::SendMessage(msg) => {
                // Add user message to chat
                let message = ChatMessage::user(msg);
                self.state_manager.update_chat(message);
            }
            UiAction::ExecuteCommand(cmd) => {
                // Handle command execution
                match cmd.to_lowercase().as_str() {
                    "/stats" => self.print_stats(),
                    "/context" => self.print_context_status(),
                    "/todo" | "/todos" => self.print_todo_list(),
                    _ => {
                        // Forward to agent if not a known command
                        let _ = self.action_sender.as_ref().map(|s| s.send(UiAction::SendMessage(cmd)));
                    }
                }
            }
            UiAction::ToggleTodoPanel => {
                // REPL doesn't have panels, but we can update state
                let mut state = self.state_manager.get_state();
                state.todo.toggle_visibility();
                let todo_state = state.todo.clone();
                drop(state);
                self.state_manager.update_todo_state(todo_state);
            }
            UiAction::UpdateTodoItem(index, item_action) => {
                let mut state = self.state_manager.get_state();
                match item_action {
                    TodoItemAction::ToggleComplete => {
                        state.todo.update_item(index, TodoItemAction::ToggleComplete);
                    }
                    TodoItemAction::UpdateStatus(status) => {
                        state.todo.update_item(index, TodoItemAction::UpdateStatus(status));
                    }
                    TodoItemAction::Delete => {
                        state.todo.update_item(index, TodoItemAction::Delete);
                    }
                }
                let todo_state = state.todo.clone();
                drop(state);
                self.state_manager.update_todo_state(todo_state);
                self.print_todo_list();
            }
            UiAction::AddTodoItem(text) => {
                let mut state = self.state_manager.get_state();
                state.todo.add_item(text);
                let todo_state = state.todo.clone();
                drop(state);
                self.state_manager.update_todo_state(todo_state);
                self.print_todo_list();
            }
            UiAction::ClearCompletedTodos => {
                let mut state = self.state_manager.get_state();
                let removed = state.todo.clear_completed();
                let todo_state = state.todo.clone();
                drop(state);
                self.state_manager.update_todo_state(todo_state);
                println!("Cleared {} completed TODO items.\n", removed);
                self.print_todo_list();
            }
            UiAction::Exit => {
                self.running = false;
            }
            UiAction::NavigatePopup(_) | UiAction::SelectPopupItem | UiAction::CancelPopup => {
                // REPL doesn't use popups
            }
        }

        Ok(())
    }
}
