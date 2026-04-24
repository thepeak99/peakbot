//! Application State Definitions
//!
//! This module defines the centralized state that all UIs observe.
//! It mirrors the patterns from ui-example.rs while being compatible
//! with existing PeakBot types (TodoList, SessionStats, etc.).

use crate::TodoStatus;
use crate::tools::todo::TodoItem as CoreTodoItem;
use crate::ui::ui_trait::TodoItemAction;
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use serde_json;
use std::fmt;

/// Centralized state that all UIs observe
///
/// This is the single source of truth for all UI-renderable state.
/// The StateManager keeps this in sync with the core PeakBot state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppState {
    /// Chat messages
    pub chat: ChatState,

    /// TODO items
    pub todo: TodoState,

    /// Input field state
    pub input: InputState,

    /// Session statistics (tokens, cost, etc.)
    pub stats: SessionState,

    /// Context usage
    pub context: ContextState,

    /// Current conversation info
    pub conversation: Option<ConversationState>,

    /// UI preferences
    pub preferences: UiPreferences,

    /// Whether the agent is currently processing
    #[serde(default)]
    pub is_running: bool,

    /// Whether the agent is currently loading (alias for is_running, kept for compatibility)
    #[serde(default)]
    #[doc(hidden)]
    pub is_loading: bool,

    /// Welcome banner — populated once at startup, never changes
    pub welcome: Option<WelcomeState>,

    /// Whether this state update is the final broadcast after a prompt completed
    #[serde(default)]
    pub is_final: bool,

    /// Agent status message (e.g., "Compacting...", "Stopped")
    pub status_message: Option<String>,

    /// When the current run started. `Some` iff `is_running`.
    ///
    /// Local-only (`Instant` isn't `Serialize`). The TUI reads this to render
    /// a "working" spinner and elapsed timer in the input block title. If a
    /// cross-process UI ever needs this, migrate to `SystemTime` or epoch
    /// millis — do NOT pre-build that bridge.
    #[serde(skip)]
    pub run_started_at: Option<std::time::Instant>,
}

impl AppState {
    /// Create a new empty AppState
    pub fn new() -> Self {
        Self::default()
    }
}

/// Chat message state
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatState {
    /// Chat messages
    pub messages: Vec<ChatMessage>,

    /// Whether to auto-scroll to latest message
    pub auto_scroll: bool,

    /// Manual scroll offset (when auto_scroll is disabled)
    pub scroll_offset: usize,
}

impl ChatState {
    /// Create a new empty ChatState
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a message to the chat
    pub fn add_message(&mut self, message: ChatMessage) {
        self.messages.push(message);
        // Auto-scroll when new messages are added
        self.auto_scroll = true;
    }

    /// Clear all messages
    pub fn clear(&mut self) {
        self.messages.clear();
        self.auto_scroll = true;
    }

    /// Get the number of messages
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }
}

/// A single chat message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Role of the message sender
    pub role: MessageRole,

    /// Display content (formatted for UI rendering)
    pub content: String,

    /// Timestamp when message was created
    pub timestamp: DateTime<Local>,

    // ── Structured tool data (lossless) ──────────────────────────────
    // These fields preserve the original data from rig so that
    // ChatMessage → Conversation::Message and ChatMessage → rig::Message
    // roundtrips are lossless.

    /// Tool name (for ToolCall and ToolResult roles)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,

    /// Raw tool arguments JSON string (for ToolCall and ToolResult roles)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_args: Option<String>,

    /// Tool execution result (for ToolResult role)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<String>,

    /// Tool call ID for correlating calls with results
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,

    /// Whether this message has been compacted (summarized).
    /// Compacted messages are kept for UI display but skipped when
    /// building the rig message array sent to the LLM.
    #[serde(default)]
    pub compacted: bool,
}

impl ChatMessage {
    /// Create a new user message
    pub fn user(content: String) -> Self {
        Self {
            role: MessageRole::User,
            content,
            timestamp: Local::now(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            call_id: None,
            compacted: false,
        }
    }

    /// Create a new agent message
    pub fn agent(content: String) -> Self {
        Self {
            role: MessageRole::Agent,
            content,
            timestamp: Local::now(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            call_id: None,
            compacted: false,
        }
    }

    /// Create a new system message
    pub fn system(content: String) -> Self {
        Self {
            role: MessageRole::System,
            content,
            timestamp: Local::now(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            call_id: None,
            compacted: false,
        }
    }

    /// Create a compaction summary message
    pub fn summary(content: String) -> Self {
        Self {
            role: MessageRole::Summary,
            content,
            timestamp: Local::now(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            call_id: None,
            compacted: false,
        }
    }

    /// Create a new tool call message with structured display:
    /// - Shows thought intent first
    /// - Shows key params (2-3 lines max)
    /// Stores raw tool_name and args for lossless persistence.
    pub fn tool_call(tool_name: &str, args: &str, call_id: Option<String>) -> Self {
        let content = format_tool_call(tool_name, args);
        Self {
            role: MessageRole::ToolCall,
            content,
            timestamp: Local::now(),
            tool_name: Some(tool_name.to_string()),
            tool_args: Some(args.to_string()),
            tool_result: None,
            call_id,
            compacted: false,
        }
    }

    /// Create a new tool result message with truncation to top 2-3 lines.
    /// Stores raw tool_name, args, result, and call_id for lossless persistence.
    pub fn tool_result(
        tool_name: &str,
        args: &str,
        result: &str,
        call_id: Option<String>,
    ) -> Self {
        let content = format_tool_result(tool_name, result);
        Self {
            role: MessageRole::ToolResult,
            content,
            timestamp: Local::now(),
            tool_name: Some(tool_name.to_string()),
            tool_args: Some(args.to_string()),
            tool_result: Some(result.to_string()),
            call_id,
            compacted: false,
        }
    }

    /// Create a new assistant message (alias for agent)
    pub fn assistant(content: String) -> Self {
        Self::agent(content)
    }

    /// Create a new error message
    pub fn error(content: String) -> Self {
        Self {
            role: MessageRole::System,
            content,
            timestamp: Local::now(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            call_id: None,
            compacted: false,
        }
    }

    /// Create a message with a fixed timestamp (for testing)
    pub fn with_timestamp(role: MessageRole, content: String, timestamp_str: &str) -> Self {
        use chrono::NaiveDateTime;
        Self {
            role,
            content,
            timestamp: NaiveDateTime::parse_from_str(timestamp_str, "%Y-%m-%d %H:%M:%S")
                .unwrap()
                .and_local_timezone(Local)
                .unwrap(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            call_id: None,
            compacted: false,
        }
    }
}

/// Format tool call with structured output: thought intent first, then params
pub(crate) fn format_tool_call(tool_name: &str, args: &str) -> String {
    // Try to parse JSON args to extract thought and key params
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(args) {
        let mut lines = Vec::new();

        // Line 1: Thought intent (always first if present)
        if let Some(thought) = parsed.get("thought").and_then(|v| v.as_str())
            && !thought.is_empty()
        {
            lines.push(format!("💭 {}", truncate_str(thought, 100)));
        }

        // Line 2: Tool name with key params
        let mut params = Vec::new();
        for (key, value) in parsed.as_object().unwrap_or(&serde_json::Map::new()) {
            if key == "thought" {
                continue; // Already shown
            }
            let value_str = match value {
                serde_json::Value::String(s) => truncate_str(s, 60),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Null => "null".to_string(),
                _ => truncate_str(&value.to_string(), 40),
            };
            params.push(format!("{}={}", key, value_str));
        }

        let params_str = params.join(", ");
        lines.push(format!("🔧 {}({})", tool_name, params_str));

        // Limit to 2-3 lines total
        if lines.len() > 3 {
            lines.truncate(3);
        }

        lines.join("\n")
    } else {
        // Fallback: just show tool name with raw args
        format!("🔧 {}({})", tool_name, truncate_str(args, 150))
    }
}

/// Format tool result with truncation to top 2-3 lines
pub(crate) fn format_tool_result(tool_name: &str, result: &str) -> String {
    // Special handling per tool type
    match tool_name {
        "bash" => format_bash_result(result),
        "file_read" => format_file_read_result(result),
        "list_directory" => format_list_directory_result(result),
        "web_search" => format_search_result(result),
        _ => format_generic_result(result),
    }
}

/// Truncate a string to max_len chars, adding "..." if truncated
pub(crate) fn truncate_str(s: &str, max_len: usize) -> String {
    let s_len = s.chars().count();

    if s_len <= max_len {
        s.to_string()
    } else if max_len < 3 {
        // Not enough room for "...", just truncate to what fits
        s.chars().take(max_len).collect()
    } else {
        s.chars().take(max_len - 3).collect::<String>() + "..."
    }
}

/// Truncate a single line to max chars, adding "..." if truncated
pub(crate) fn truncate_line(s: &str, max_len: usize) -> String {
    let s_len = s.chars().count();

    if s_len <= max_len {
        s.to_string()
    } else if max_len < 3 {
        // Not enough room for "...", just truncate to what fits
        s.chars().take(max_len).collect()
    } else {
        s.chars().take(max_len - 3).collect::<String>() + "..."
    }
}

/// Truncate each line to max chars, then truncate to top N lines
fn truncate_lines(s: &str, max_lines: usize, max_chars: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let total = lines.len();

    // First truncate each line
    let truncated: Vec<String> = lines.iter().map(|l| truncate_line(l, max_chars)).collect();

    if total <= max_lines {
        truncated.join("\n")
    } else {
        let preview: Vec<&str> = truncated
            .iter()
            .take(max_lines)
            .map(|s| s.as_str())
            .collect();
        format!(
            "{}\n... [{} lines truncated]",
            preview.join("\n"),
            total - max_lines
        )
    }
}

/// Truncate result to top N lines
fn truncate_to_lines(s: &str, max_lines: usize) -> String {
    truncate_lines(s, max_lines, 60)
}

fn format_bash_result(result: &str) -> String {
    // Parse bash output format: "Exit code: X\nSTDOUT:\n...\nSTDERR:\n..."
    let lines: Vec<&str> = result.lines().collect();

    // Extract exit code
    let exit_code = lines
        .iter()
        .find(|l| l.starts_with("Exit code:"))
        .map(|l| l.split_whitespace().last().unwrap_or("0"))
        .unwrap_or("0");

    // Find stdout/stderr sections
    let mut stdout_lines = Vec::new();
    let mut stderr_lines = Vec::new();
    let mut in_stdout = false;
    let mut in_stderr = false;

    for line in &lines {
        if line.starts_with("STDOUT:") {
            in_stdout = true;
            in_stderr = false;
        } else if line.starts_with("STDERR:") {
            in_stdout = false;
            in_stderr = true;
        } else if line.starts_with("Exit code:") || line.starts_with("Full output saved") {
            in_stdout = false;
            in_stderr = false;
        } else if in_stdout {
            stdout_lines.push(*line);
        } else if in_stderr {
            stderr_lines.push(*line);
        }
    }

    let exit_icon = if exit_code == "0" { "✅" } else { "❌" };
    let mut output = format!("{} Exit {}", exit_icon, exit_code);

    // Show first 2-3 lines of stdout (truncated to 60 chars each)
    let stdout_preview: Vec<String> = stdout_lines
        .iter()
        .take(2)
        .map(|l| truncate_line(l, 60))
        .collect();
    if !stdout_preview.is_empty() {
        output.push_str(&format!(" | {}", stdout_preview.join(" | ")));
    }

    // Show stderr if present (1 line max)
    if !stderr_lines.is_empty() {
        output.push_str(&format!(" | ⚠️ {}", truncate_str(stderr_lines[0], 50)));
    }

    // Add truncation notice if there was more
    let total_lines = stdout_lines.len() + stderr_lines.len();
    if total_lines > 3 {
        output.push_str(&format!(" ... [{} more lines]", total_lines - 3));
    }

    output
}

fn format_file_read_result(result: &str) -> String {
    // Parse: "     1\tcontent\n     2\tcontent2\n..."
    let lines: Vec<&str> = result.lines().collect();

    // Extract total lines from last line format or count
    let total_lines = lines.len();

    // Truncate lines and show first 3 only
    let truncated: Vec<String> = lines.iter().take(3).map(|l| truncate_line(l, 60)).collect();
    let preview_str = truncated.join("\n");

    let mut output = format!("📄 {} lines\n{}", total_lines, preview_str);

    if lines.len() > 3 {
        output.push_str(&format!("\n... [{} more lines]", lines.len() - 3));
    }

    output
}

fn format_list_directory_result(result: &str) -> String {
    let lines: Vec<&str> = result.lines().collect();
    let total = lines.len();

    // Truncate each entry and show first 3
    let preview: Vec<String> = lines.iter().take(3).map(|l| truncate_line(l, 60)).collect();
    let preview_str = preview.join(", ");

    let mut output = format!("📁 {} entries\n{}", total, preview_str);

    if lines.len() > 3 {
        output.push_str(&format!("\n... [{} more]", lines.len() - 3));
    }

    output
}

fn format_search_result(result: &str) -> String {
    // Truncate each line and show first 3 results
    let lines: Vec<&str> = result.lines().collect();
    let preview: Vec<String> = lines.iter().take(3).map(|l| truncate_line(l, 60)).collect();
    let preview_str = preview.join("\n");

    let mut output = preview_str;

    if lines.len() > 3 {
        output.push_str(&format!("\n... [{} more results]", lines.len() - 3));
    }

    output
}

fn format_generic_result(result: &str) -> String {
    // Generic truncation to 2-3 lines
    truncate_to_lines(result, 3)
}

/// Role of a message sender
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// User message
    User,

    /// Agent (AI) message
    Agent,

    /// System message
    System,

    /// Tool invocation
    ToolCall,

    /// Tool execution result
    ToolResult,

    /// Compaction summary (injected by the compactor)
    Summary,
}

impl fmt::Display for MessageRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageRole::User => write!(f, "User"),
            MessageRole::Agent => write!(f, "Agent"),
            MessageRole::System => write!(f, "System"),
            MessageRole::ToolCall => write!(f, "Tool Call"),
            MessageRole::ToolResult => write!(f, "Tool Result"),
            MessageRole::Summary => write!(f, "Summary"),
        }
    }
}

/// TODO panel state
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TodoState {
    /// Whether the TODO panel is visible
    pub visible: bool,

    /// TODO items
    pub items: Vec<TodoItem>,
}

impl TodoState {
    /// Create a new empty TodoState
    pub fn new() -> Self {
        Self::default()
    }

    /// Toggle visibility
    pub fn toggle_visibility(&mut self) {
        self.visible = !self.visible;
    }

    /// Add a TODO item
    pub fn add_item(&mut self, text: String) -> TodoItem {
        let item = TodoItem::new(text);
        self.items.push(item.clone());
        item
    }

    /// Update a TODO item by index
    pub fn update_item(&mut self, index: usize, action: TodoItemAction) -> Option<TodoItem> {
        if let Some(item) = self.items.get_mut(index) {
            match action {
                TodoItemAction::ToggleComplete => {
                    item.completed = !item.completed;
                    if item.completed {
                        item.status = TodoStatus::Completed;
                    } else {
                        item.status = TodoStatus::Pending;
                    }
                }
                TodoItemAction::UpdateStatus(status) => {
                    item.status = status.clone();
                    item.completed = matches!(status, TodoStatus::Completed);
                }
                TodoItemAction::Delete => {
                    return Some(self.items.remove(index));
                }
            }
            Some(item.clone())
        } else {
            None
        }
    }

    /// Remove a TODO item by index
    pub fn remove_item(&mut self, index: usize) -> Option<TodoItem> {
        Some(self.items.remove(index))
    }

    /// Clear completed items
    pub fn clear_completed(&mut self) -> usize {
        let initial = self.items.len();
        self.items.retain(|item| !item.completed);
        initial - self.items.len()
    }

    /// Get count of items by status
    pub fn count_by_status(&self) -> (usize, usize, usize, usize) {
        let mut pending = 0;
        let mut in_progress = 0;
        let mut completed = 0;
        let mut cancelled = 0;

        for item in &self.items {
            match item.status {
                TodoStatus::Pending => pending += 1,
                TodoStatus::InProgress => in_progress += 1,
                TodoStatus::Completed => completed += 1,
                TodoStatus::Cancelled => cancelled += 1,
            }
        }

        (pending, in_progress, completed, cancelled)
    }
}

/// A single TODO item (UI representation)
///
/// This is a simplified version of the core TodoItem that's optimized
/// for UI rendering. It's kept in sync with the core TodoItem by the
/// StateManager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    /// Unique ID (from core TodoItem)
    pub id: usize,

    /// Task description
    pub text: String,

    /// Whether the item is completed
    pub completed: bool,

    /// Status of the item
    pub status: TodoStatus,

    /// Whether this item is currently selected
    #[serde(default)]
    pub selected: bool,
}

impl TodoItem {
    /// Create a new TODO item
    pub fn new(text: String) -> Self {
        Self {
            id: 0, // Will be set by StateManager when syncing
            text,
            completed: false,
            status: TodoStatus::Pending,
            selected: false,
        }
    }
}

/// Convert from core TodoItem to UI TodoItem
impl From<&CoreTodoItem> for TodoItem {
    fn from(item: &CoreTodoItem) -> Self {
        Self {
            id: item.id,
            text: item.task.clone(),
            completed: matches!(item.status, TodoStatus::Completed),
            status: item.status.clone(),
            selected: false,
        }
    }
}

/// Input field state
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InputState {
    /// Current input buffer
    pub buffer: String,

    /// Cursor position in the buffer
    #[serde(default)]
    pub cursor_pos: usize,

    /// Whether the input is in command mode (after typing /)
    #[serde(default)]
    pub in_command_mode: bool,

    /// Number of wrapped lines in the input (for dynamic height)
    #[serde(default)]
    pub wrapped_lines: usize,
}

impl InputState {
    /// Create a new empty InputState
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear the input buffer
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor_pos = 0;
        self.in_command_mode = false;
        self.wrapped_lines = 0;
    }

    /// Check if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Get the current cursor position
    pub fn cursor(&self) -> usize {
        self.cursor_pos
    }

    /// Set the cursor position
    pub fn set_cursor(&mut self, pos: usize) {
        self.cursor_pos = pos.min(self.buffer.len());
    }

    /// Set the wrapped line count
    pub fn set_wrapped_lines(&mut self, lines: usize) {
        self.wrapped_lines = lines;
    }
}

/// Session statistics state
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionState {
    /// Total input tokens
    pub total_input_tokens: u64,

    /// Total output tokens
    pub total_output_tokens: u64,

    /// Total API calls
    pub total_api_calls: u64,

    /// Total cost in USD
    pub total_cost: f64,

    /// Current model name
    pub model: String,
}

impl SessionState {
    /// Create a new empty SessionState
    pub fn new() -> Self {
        Self::default()
    }

    /// Get total tokens
    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }

    /// Format cost as a string
    pub fn format_cost(&self) -> String {
        format!("{:.4}", self.total_cost)
    }

    /// Format tokens as a string with K/M suffix
    pub fn format_tokens(&self, tokens: u64) -> String {
        if tokens >= 1_000_000 {
            format!("{:.1}M", tokens as f64 / 1_000_000.0)
        } else if tokens >= 1_000 {
            format!("{:.1}k", tokens as f64 / 1_000.0)
        } else {
            format!("{}", tokens)
        }
    }
}

/// Context usage state
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextState {
    /// Current context usage (tokens)
    pub current_usage: u64,

    /// Context window size (tokens)
    pub window_size: u64,

    /// Whether compaction is enabled
    #[serde(default)]
    pub compaction_enabled: bool,

    /// Compaction threshold (0.0 - 1.0)
    #[serde(default = "default_threshold")]
    pub compaction_threshold: f64,
}

impl ContextState {
    /// Create a new ContextState
    pub fn new() -> Self {
        Self::default()
    }

    /// Get usage as a percentage
    pub fn usage_percentage(&self) -> f64 {
        if self.window_size == 0 {
            return 0.0;
        }
        (self.current_usage as f64 / self.window_size as f64) * 100.0
    }

    /// Check if compaction should be triggered
    pub fn should_compact(&self) -> bool {
        self.compaction_enabled && self.usage_percentage() >= (self.compaction_threshold * 100.0)
    }
}

fn default_threshold() -> f64 {
    0.8 // 80%
}

/// Conversation state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationState {
    /// Conversation ID
    pub id: String,

    /// Conversation name
    pub name: String,

    /// Model used
    pub model: String,

    /// Message count
    pub message_count: usize,

    /// Last updated timestamp
    pub updated_at: DateTime<Local>,
}

impl ConversationState {
    /// Create a new ConversationState
    pub fn new(id: String, name: String, model: String) -> Self {
        Self {
            id,
            name,
            model,
            message_count: 0,
            updated_at: Local::now(),
        }
    }
}

/// UI preferences
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UiPreferences {
    /// Theme (light/dark)
    #[serde(default = "default_theme")]
    pub theme: String,

    /// Font size
    #[serde(default = "default_font_size")]
    pub font_size: u8,

    /// Show timestamps
    #[serde(default)]
    pub show_timestamps: bool,

    /// Wrap long lines
    #[serde(default = "default_true")]
    pub wrap_lines: bool,
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_font_size() -> u8 {
    12
}

fn default_true() -> bool {
    true
}

/// Welcome banner state — populated once at startup
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WelcomeState {
    pub provider_name: String,
    pub model: String,
    pub max_tokens: usize,
    pub builtin_tools_count: usize,
    pub mcp_tools_count: usize,
    pub skills_count: usize,
    pub searxng_enabled: bool,
    pub searxng_url: Option<String>,
    pub cost_tracking_enabled: bool,
    pub compaction_enabled: bool,
    pub compaction_threshold: f64,
    pub compaction_keep_recent: usize,
    pub conversation_persistence_enabled: bool,
    pub cwd: std::path::PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_str_normal() {
        let s = "hello world"; // 11 characters
        // max_len = 10: take (10-3)=7 chars = "hello w", add "..." = "hello w..." (10 chars)
        assert_eq!(truncate_str(s, 10), "hello w...");
        // max_len = 8: take (8-3)=5 chars = "hello", add "..." = "hello..." (8 chars)
        assert_eq!(truncate_str(s, 8), "hello...");
        // max_len = 5: take (5-3)=2 chars = "he", add "..." = "he..." (5 chars)
        assert_eq!(truncate_str(s, 5), "he...");
    }

    #[test]
    fn test_truncate_str_no_truncation_needed() {
        let s = "hi";
        assert_eq!(truncate_str(s, 10), "hi");
        assert_eq!(truncate_str(s, 2), "hi");
    }

    #[test]
    fn test_truncate_str_exact_length() {
        let s = "hello";
        assert_eq!(truncate_str(s, 5), "hello");
    }

    #[test]
    fn test_truncate_str_bug_max_len_less_than_3() {
        // When max_len < 3, the output should NOT exceed max_len
        let s = "hello world";
        
        // max_len = 2 should return at most 2 characters
        let result = truncate_str(s, 2);
        assert!(
            result.chars().count() <= 2,
            "truncate_str(s, 2) returned '{}' with {} chars, expected <= 2",
            result,
            result.chars().count()
        );

        // max_len = 1 should return at most 1 character
        let result = truncate_str(s, 1);
        assert!(
            result.chars().count() <= 1,
            "truncate_str(s, 1) returned '{}' with {} chars, expected <= 1",
            result,
            result.chars().count()
        );

        // max_len = 0 should return empty string
        let result = truncate_str(s, 0);
        assert!(
            result.chars().count() <= 0,
            "truncate_str(s, 0) returned '{}' with {} chars, expected <= 0",
            result,
            result.chars().count()
        );
    }

    #[test]
    fn test_truncate_str_edge_cases() {
        // max_len = 3 should return at most 3 characters (no room for "...")
        let s = "hello world";
        let result = truncate_str(s, 3);
        assert!(
            result.chars().count() <= 3,
            "truncate_str(s, 3) returned '{}' with {} chars, expected <= 3",
            result,
            result.chars().count()
        );
    }

    #[test]
    fn test_truncate_line_bug_max_len_less_than_3() {
        // Same bug should exist in truncate_line
        let s = "hello world";
        
        let result = truncate_line(s, 2);
        assert!(
            result.chars().count() <= 2,
            "truncate_line(s, 2) returned '{}' with {} chars, expected <= 2",
            result,
            result.chars().count()
        );
    }
}
