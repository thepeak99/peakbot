//! UI Trait Definition
//!
//! This module defines the `Ui` trait that all UI implementations must implement.
//! It provides a clean abstraction layer that allows PeakBot to support multiple
//! UI backends (TUI, Web, Mobile) without coupling the core logic to any specific UI.

use crate::TodoStatus;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Actions that can be performed through any UI
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UiAction {
    /// Send a message to the agent
    SendMessage(String),
    
    /// Execute a slash command (e.g., /stats, /context)
    ExecuteCommand(String),
    
    /// Toggle the TODO panel visibility
    ToggleTodoPanel,
    
    /// Update a TODO item
    UpdateTodoItem(usize, TodoItemAction),
    
    /// Add a new TODO item
    AddTodoItem(String),
    
    /// Clear completed TODO items
    ClearCompletedTodos,
    
    /// Exit the application
    Exit,
    
    /// Navigate in command popup (up/down)
    NavigatePopup(i32),
    
    /// Select current popup item
    SelectPopupItem,
    
    /// Cancel popup
    CancelPopup,
}

/// Actions that can be performed on a TODO item
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TodoItemAction {
    /// Toggle completion status
    ToggleComplete,
    
    /// Update status to a specific value
    UpdateStatus(TodoStatus),
    
    /// Delete the item
    Delete,
}

/// Trait that all UI implementations must implement
///
/// This trait provides a clean abstraction between the core PeakBot logic
/// and various UI implementations. Each UI type (TUI, Web, REPL, Mobile)
/// implements this trait to provide its own rendering and input handling.
pub trait Ui: Send + 'static {
    /// Initialize the UI (setup terminal, HTTP server, etc.)
    fn init(&mut self) -> Result<()>;
    
    /// Run the UI event loop (blocking)
    /// 
    /// This is the main loop that handles:
    /// - Rendering the UI
    /// - Processing user input
    /// - Updating state from the StateManager
    fn run(&mut self) -> Result<()>;
    
    /// Shutdown the UI gracefully
    fn shutdown(&mut self) -> Result<()>;
    
    /// Handle a user action (from input)
    /// 
    /// This is called when the user performs an action through the UI.
    /// The UI implementation should process the action and potentially
    /// update the state or send commands to the agent.
    fn on_action(&mut self, action: UiAction) -> Result<()>;
}

/// Slash command definition for the command popup
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlashCommand {
    /// The command name (without the leading /)
    pub name: String,
    
    /// Short description of what the command does
    pub description: String,
    
    /// Whether the command takes arguments
    pub takes_args: bool,
}

impl SlashCommand {
    /// Create a new slash command
    pub fn new(name: &str, description: &str, takes_args: bool) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            takes_args,
        }
    }
}

/// Get built-in slash commands
pub fn builtin_commands() -> Vec<SlashCommand> {
    vec![
        SlashCommand::new("stats", "Show session statistics (tokens, cost)", false),
        SlashCommand::new("context", "Show context usage status", false),
        SlashCommand::new("compact", "Force context compaction", false),
        SlashCommand::new("conversations", "List saved conversations", false),
        SlashCommand::new("help", "Show help message", false),
    ]
}

/// State for a command popup
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommandPopupState {
    /// The prefix being typed (e.g., "/stat")
    pub prefix: String,
    
    /// Currently selected index in the filtered list
    pub selected_index: usize,
}

impl CommandPopupState {
    /// Create a new command popup with the given prefix
    pub fn new(prefix: String) -> Self {
        Self {
            prefix,
            selected_index: 0,
        }
    }
    
    /// Get filtered commands that match the prefix
    pub fn filtered_commands(&self) -> Vec<SlashCommand> {
        if self.prefix.is_empty() {
            builtin_commands()
        } else {
            builtin_commands()
                .into_iter()
                .filter(|cmd| cmd.name.starts_with(&self.prefix))
                .collect()
        }
    }
    
    /// Get the currently selected command
    pub fn selected_command(&self) -> Option<SlashCommand> {
        self.filtered_commands().get(self.selected_index).cloned()
    }
    
    /// Navigate up in the list
    pub fn navigate_up(&mut self) {
        let count = self.filtered_commands().len();
        if count > 0 {
            self.selected_index = (self.selected_index.saturating_sub(1)) % count;
        }
    }
    
    /// Navigate down in the list
    pub fn navigate_down(&mut self) {
        let count = self.filtered_commands().len();
        if count > 0 {
            self.selected_index = (self.selected_index + 1) % count;
        }
    }
}
