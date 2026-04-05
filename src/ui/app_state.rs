//! Application State Definitions
//!
//! This module defines the centralized state that all UIs observe.
//! It mirrors the patterns from ui-example.rs while being compatible
//! with existing PeakBot types (TodoList, SessionStats, etc.).

use crate::ui::ui_trait::{CommandPopupState, TodoItemAction};
use crate::tools::todo::TodoItem as CoreTodoItem;
use crate::TodoStatus;
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
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
    
    /// Active command popup (for slash commands)
    pub command_popup: Option<CommandPopupState>,
    
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

    /// Pending notifications for the UI to display
    #[serde(default)]
    pub notifications: Vec<Notification>,
    
    /// Agent status message (e.g., "Compacting...", "Stopped")
    pub status_message: Option<String>,
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
    
    /// Message content
    pub content: String,
    
    /// Timestamp when message was created
    pub timestamp: DateTime<Local>,
}

impl ChatMessage {
    /// Create a new user message
    pub fn user(content: String) -> Self {
        Self {
            role: MessageRole::User,
            content,
            timestamp: Local::now(),
        }
    }
    
    /// Create a new agent message
    pub fn agent(content: String) -> Self {
        Self {
            role: MessageRole::Agent,
            content,
            timestamp: Local::now(),
        }
    }
    
    /// Create a new system message
    pub fn system(content: String) -> Self {
        Self {
            role: MessageRole::System,
            content,
            timestamp: Local::now(),
        }
    }
    
    /// Create a new tool call message
    pub fn tool_call(tool_name: &str, args: &str, _result: &str) -> Self {
        let content = format!("🔧 {}({})", tool_name, args);
        Self {
            role: MessageRole::ToolCall,
            content,
            timestamp: Local::now(),
        }
    }
    
    /// Create a new tool result message
    pub fn tool_result(tool_name: &str, result: &str) -> Self {
        let content = format!("📋 {} result: {}", tool_name, result);
        Self {
            role: MessageRole::ToolResult,
            content,
            timestamp: Local::now(),
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
        }
    }
}

/// Role of a message sender
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
}

impl fmt::Display for MessageRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageRole::User => write!(f, "User"),
            MessageRole::Agent => write!(f, "Agent"),
            MessageRole::System => write!(f, "System"),
            MessageRole::ToolCall => write!(f, "Tool Call"),
            MessageRole::ToolResult => write!(f, "Tool Result"),
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

/// Notification for user-facing messages (confirmations, status updates)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub message: String,
    pub kind: NotificationKind,
    pub timestamp: DateTime<Local>,
}

impl Notification {
    pub fn new(message: String, kind: NotificationKind) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            message,
            kind,
            timestamp: Local::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NotificationKind {
    Info,
    Success,
    Warning,
    Error,
}

impl NotificationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            NotificationKind::Info => "info",
            NotificationKind::Success => "success",
            NotificationKind::Warning => "warning",
            NotificationKind::Error => "error",
        }
    }
}
