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

use crate::TodoStatus;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Trait that all UI implementations must implement — View in MVC
#[allow(async_fn_in_trait)]
pub trait Ui: Send + 'static {
    async fn init(&mut self) -> Result<()>;
    async fn run(&mut self) -> Result<()>;
    async fn shutdown(&mut self) -> Result<()>;
}

/// User input actions — flow from View to Controller
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UiAction {
    /// Send a message to the agent, or — if the text starts with `/` —
    /// a slash command to be dispatched internally. The event loop in
    /// `AgentRunner` classifies via `classify_submission`.
    SendMessage(String),

    /// Request the agent to stop (Esc key).
    RequestStop,
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

/// Get built-in slash commands.
///
/// This list is the **single source of truth** for:
/// - the autocomplete popup in the REPL,
/// - the `/help` handler in `lib.rs::process_command_internal`,
/// - and the dispatcher arms in the same file.
///
/// If you add a command here, add a dispatcher arm. If you remove one from
/// the dispatcher, remove it here. The ordering is what the popup shows on
/// an empty prefix — keep the most commonly-used commands near the top.
///
/// See `allehailmenu.md` §4 for the reasoning behind this specific list.
pub fn builtin_commands() -> Vec<SlashCommand> {
    vec![
        SlashCommand::new("help", "List available commands", false),
        SlashCommand::new("stats", "Show session statistics (tokens, cost)", false),
        SlashCommand::new("context", "Show context usage status", false),
        SlashCommand::new("compact", "Force context compaction", false),
        SlashCommand::new("conversations", "List saved conversations", false),
        SlashCommand::new("history", "Show conversation history", false),
        SlashCommand::new("reset", "Reset session statistics", false),
        SlashCommand::new("new", "Start a new conversation", false),
        SlashCommand::new("save", "Save the current conversation", false),
        SlashCommand::new("load", "Load a conversation by id", true),
        SlashCommand::new("delete", "Delete a conversation by id", true),
        SlashCommand::new("export", "Export a conversation (json|markdown)", true),
        SlashCommand::new("rename", "Rename the current conversation", true),
        SlashCommand::new("stop", "Stop the agent (interrupt current task)", false),
        SlashCommand::new("exit", "Quit PeakBot (no confirmation)", false),
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
            .get(self.selected_index.min(filtered.len().saturating_sub(1)))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the exact set of built-in slash commands. The popup UI, the
    /// `/help` handler, and the dispatcher in `lib.rs::process_command_internal`
    /// all read from this list. If a command is added/removed/reordered,
    /// this test fails loudly so we remember to update the dispatcher too.
    ///
    /// See `allehailmenu.md` §4 for the reality-check that produced this list.
    #[test]
    fn builtin_commands_exact_list_and_order() {
        let cmds = builtin_commands();
        let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "help",
                "stats",
                "context",
                "compact",
                "conversations",
                "history",
                "reset",
                "new",
                "save",
                "load",
                "delete",
                "export",
                "rename",
                "stop",
                "exit",
            ],
        );
    }

    #[test]
    fn builtin_commands_takes_args_flags_match_dispatcher() {
        let cmds = builtin_commands();
        let by_name = |n: &str| -> bool {
            cmds.iter()
                .find(|c| c.name == n)
                .expect("command in list")
                .takes_args
        };
        // No-arg commands — process_command_internal dispatches on exact match
        assert!(!by_name("help"));
        assert!(!by_name("stats"));
        assert!(!by_name("context"));
        assert!(!by_name("compact"));
        assert!(!by_name("conversations"));
        assert!(!by_name("history"));
        assert!(!by_name("reset"));
        assert!(!by_name("new"));
        assert!(!by_name("save"));
        assert!(!by_name("stop"));
        assert!(!by_name("exit"));
        // Arg-taking commands — dispatcher uses `starts_with("/name ")`
        assert!(by_name("load"));
        assert!(by_name("delete"));
        assert!(by_name("export"));
        assert!(by_name("rename"));
    }

    #[test]
    fn builtin_commands_no_phantom_entries() {
        // These were in the old list but have no handler anywhere.
        // See allehailmenu.md §4.
        //
        // NOTE: /exit used to be on this list too, but was implemented
        // 2026-04-24 as a no-confirmation quit alongside the Ctrl+C
        // confirm-dialog path. /quit stays phantom — it's redundant with
        // /exit and we're not picking two aliases for the same thing.
        let cmds = builtin_commands();
        let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        assert!(!names.contains(&"clear"), "/clear has no handler");
        assert!(!names.contains(&"quit"), "/quit has no handler — use /exit");
    }

    #[test]
    fn builtin_commands_includes_exit_with_handler() {
        // Pin the inverse of the old phantom assertion: /exit MUST be in
        // the list because it has a live dispatcher arm. If this ever
        // flips back (command removed), the popup and /help will silently
        // stop advertising a working command.
        let cmds = builtin_commands();
        let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"exit"), "/exit must be in the popup list");
    }

    #[test]
    fn filtered_commands_empty_prefix_returns_all() {
        let popup = CommandPopupState::new(String::new());
        assert_eq!(popup.filtered_commands().len(), builtin_commands().len());
    }

    #[test]
    fn filtered_commands_prefix_filters_by_starts_with() {
        let popup = CommandPopupState::new("c".to_string());
        let names: Vec<String> = popup
            .filtered_commands()
            .iter()
            .map(|c| c.name.clone())
            .collect();
        // "c" matches: context, compact, conversations
        assert_eq!(names, vec!["context", "compact", "conversations"]);
    }

    #[test]
    fn filtered_commands_no_matches_returns_empty() {
        let popup = CommandPopupState::new("zzz".to_string());
        assert!(popup.filtered_commands().is_empty());
    }
}
