//! UI Trait Definition
//!
//! This module defines the `Ui` trait that all UI implementations must implement.
//! It provides a clean abstraction layer that allows PeakBot to support multiple
//! UI backends (TUI, Web, Mobile) without coupling the core logic to any specific UI.
//!
//! ## MVC Architecture
//!
//! - **Model** (`StateManager`): single source of truth for UI state. Broadcasts to subscribers.
//! - **View** (`Ui` impls): read state from Model, render to screen, send user input to Controller.
//! - **Controller** (`AgentRunner`): receive input from View, call agent, write to Model.
//!
//! Data flows one way only:
//!   View ──UiAction──► Controller ──writes──► Model ──broadcasts──► View

use anyhow::Result;
use serde::{Deserialize, Serialize};
use crate::TodoStatus;

/// Trait that all UI implementations must implement — View in MVC
pub trait Ui: Send + 'static {
    fn init(&mut self) -> Result<()>;
    fn run(&mut self) -> Result<()>;
    fn shutdown(&mut self) -> Result<()>;
}

/// User input actions — flow from View to Controller
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UiAction {
    /// Send a message to the agent
    SendMessage(String),

    /// Execute a slash command (e.g., /stats, /context)
    ExecuteCommand(String),

    /// Request the agent to stop
    RequestStop,

    /// Exit the application
    Exit,
}

/// TUI-local actions — handled directly by the TUI View, never sent to Controller
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TuiAction {
    CancelPopup,
    SelectPopupItem,
    NavigatePopup(i32),
    ToggleTodoPanel,
}

/// Actions that can be performed on a TODO item (used by TodoState in app_state)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TodoItemAction {
    ToggleComplete,
    UpdateStatus(TodoStatus),
    Delete,
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
        SlashCommand::new("help", "Show available commands", false),
        SlashCommand::new("clear", "Clear chat history", false),
        SlashCommand::new("stop", "Stop the agent (interrupt current task)", false),
        SlashCommand::new("exit", "Exit the application", false),
        SlashCommand::new("quit", "Exit the application", false),
    ]
}

/// State for a command popup
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommandPopupState {
    /// The prefix being typed (e.g., "/stat")
    pub prefix: String,

    /// Currently selected index in the filtered list
    pub selected_index: usize,

    /// Scroll offset for visible items
    pub scroll_offset: usize,
}

impl CommandPopupState {
    /// Create a new command popup with the given prefix
    pub fn new(prefix: String) -> Self {
        Self {
            prefix,
            selected_index: 0,
            scroll_offset: 0,
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
        let filtered = self.filtered_commands();
        filtered
            .get(
                self.selected_index
                    .min(filtered.len().saturating_sub(1)),
            )
            .cloned()
    }

    /// Navigate up in the list
    pub fn navigate_up(&mut self) {
        let count = self.filtered_commands().len();
        if count > 0 {
            self.selected_index = self.selected_index.saturating_sub(1);
            if self.selected_index < self.scroll_offset {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
        }
    }

    /// Navigate down in the list
    pub fn navigate_down(&mut self) {
        let filtered = self.filtered_commands();
        let count = filtered.len();
        if count > 0 {
            self.selected_index = (self.selected_index + 1) % count;
            let visible_height = 8;
            if self.selected_index >= self.scroll_offset + visible_height {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
            }
        }
    }
}
