//! Conversation persistence - data structures for storing conversation history.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Metadata about a conversation (stats, etc.)
///
/// Token + cost fields use `#[serde(default)]` so conversations saved before
/// stats persistence existed (only `message_count`) still deserialize cleanly
/// — they just come back with zeros, which is the right answer for them.
///
/// `model_alias` carries the user-facing handle of the model that was
/// active when this conversation ran. It is required for `/load` to
/// re-activate the right model. Pre-v4 files (which never wrote this
/// field) load with the reserved sentinel
/// [`crate::config::RESERVED_UNAVAILABLE_ALIAS`] (`"unknown"`), which
/// `/load` then rejects with the canonical `Model 'unknown' not
/// available.` message — an honest answer, not a silent guess.
/// *(persisted artifacts must carry every field needed to be re-activated)*
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMetadata {
    /// Number of messages in the conversation
    #[serde(default)]
    pub message_count: usize,
    /// Input tokens from the last request (mirrors SessionStats — overwritten,
    /// not accumulated; doubles as the live context-size indicator on resume)
    #[serde(default)]
    pub total_input_tokens: u64,
    /// Output tokens from the last request (mirrors SessionStats — overwritten)
    #[serde(default)]
    pub total_output_tokens: u64,
    /// Cumulative number of API calls across the conversation
    #[serde(default)]
    pub total_api_calls: u64,
    /// Cumulative cost in USD across the conversation
    #[serde(default)]
    pub total_cost: f64,
    /// User-facing alias of the model active when this conversation
    /// was last updated. Defaults to the reserved `unknown` sentinel
    /// for pre-v4 files. See struct-level doc comment.
    #[serde(default = "default_unknown_alias")]
    pub model_alias: String,
}

/// Sentinel-default for [`ConversationMetadata::model_alias`] on
/// pre-v4 conversation files. Returns the same literal as
/// [`crate::config::RESERVED_UNAVAILABLE_ALIAS`] but kept inline here
/// so this module doesn't need to import the config module just to
/// pin a default. The two MUST stay equal — pinned by
/// `pre_v4_default_matches_reserved_unavailable_alias`.
fn default_unknown_alias() -> String {
    "unknown".to_string()
}

impl Default for ConversationMetadata {
    fn default() -> Self {
        Self {
            message_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_api_calls: 0,
            total_cost: 0.0,
            model_alias: default_unknown_alias(),
        }
    }
}

/// A message in the conversation history
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum Message {
    /// User message
    User {
        /// Message content
        content: String,
        /// Timestamp when message was sent
        timestamp: DateTime<Utc>,
    },
    /// Assistant (AI) message
    Assistant {
        /// Message content
        content: String,
        /// Timestamp when message was generated
        timestamp: DateTime<Utc>,
    },
    /// Tool invocation (captures what the model asked the tool to do)
    ToolCall {
        /// Name of the tool being called
        tool_name: String,
        /// Arguments passed to the tool (JSON string)
        arguments: String,
        /// Tool call ID for correlating calls with results
        #[serde(skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        /// Timestamp when tool was called
        timestamp: DateTime<Utc>,
    },
    /// Tool execution result
    ToolResult {
        /// Name of the tool that was executed
        tool_name: String,
        /// Arguments that were passed to the tool
        arguments: String,
        /// Tool execution result (or error message)
        result: String,
        /// Tool call ID for correlating calls with results
        #[serde(skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        /// Timestamp when tool was executed
        timestamp: DateTime<Utc>,
    },
}

impl Message {
    /// Create a new user message
    pub fn user(content: String) -> Self {
        Message::User {
            content,
            timestamp: Utc::now(),
        }
    }

    /// Create a new assistant message
    pub fn assistant(content: String) -> Self {
        Message::Assistant {
            content,
            timestamp: Utc::now(),
        }
    }

    /// Create a new tool call message
    pub fn tool_call(tool_name: String, arguments: String, call_id: Option<String>) -> Self {
        Message::ToolCall {
            tool_name,
            arguments,
            call_id,
            timestamp: Utc::now(),
        }
    }

    /// Create a new tool result message
    pub fn tool_result(
        tool_name: String,
        arguments: String,
        result: String,
        call_id: Option<String>,
    ) -> Self {
        Message::ToolResult {
            tool_name,
            arguments,
            result,
            call_id,
            timestamp: Utc::now(),
        }
    }

    /// Get the content of the message
    pub fn content(&self) -> &str {
        match self {
            Message::User { content, .. } => content,
            Message::Assistant { content, .. } => content,
            Message::ToolResult { result, .. } => result,
            Message::ToolCall { .. } => "",
        }
    }
}

/// A complete conversation with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    /// Unique identifier for this conversation
    pub id: Uuid,
    /// Human-readable name for the conversation
    pub name: String,
    /// When the conversation was created
    pub created_at: DateTime<Utc>,
    /// When the conversation was last updated
    pub updated_at: DateTime<Utc>,
    /// Message history
    pub messages: Vec<Message>,
    /// Model used for this conversation
    pub model: String,
    /// Additional metadata (token count, cost, etc.)
    pub metadata: ConversationMetadata,
}

impl Conversation {
    /// Create a new conversation with the given name and model wire id.
    ///
    /// **Sets `model_alias` to the sentinel `"unknown"`** — call sites
    /// that know the alias should use [`Conversation::new_with_alias`]
    /// (which stamps the right value) or set
    /// `metadata.model_alias` immediately after construction. Tests
    /// that don't care about alias-driven `/load` may use this directly.
    pub fn new(name: String, model: String) -> Self {
        Self::new_with_alias(name, model, default_unknown_alias())
    }

    /// Create a new conversation, stamping the user-facing model alias
    /// into the metadata. This is the production constructor — callers
    /// in the boot path, `/new` handler, and `/model` switch must use
    /// this so `/load` can later reactivate the right model.
    pub fn new_with_alias(name: String, model: String, model_alias: String) -> Self {
        let now = Utc::now();
        let metadata = ConversationMetadata {
            model_alias,
            ..ConversationMetadata::default()
        };
        Conversation {
            id: Uuid::new_v4(),
            name,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            model,
            metadata,
        }
    }

    /// Add a user message to the conversation
    pub fn add_user_message(&mut self, content: String) {
        self.messages.push(Message::user(content));
        self.metadata.message_count = self.messages.len();
        self.updated_at = Utc::now();
    }

    /// Add an assistant message to the conversation
    pub fn add_assistant_message(&mut self, content: String) {
        self.messages.push(Message::assistant(content));
        self.metadata.message_count = self.messages.len();
        self.updated_at = Utc::now();
    }

    /// Add a tool call to the conversation
    pub fn add_tool_call(&mut self, tool_name: String, arguments: String, call_id: Option<String>) {
        self.messages
            .push(Message::tool_call(tool_name, arguments, call_id));
        self.metadata.message_count = self.messages.len();
        self.updated_at = Utc::now();
    }

    /// Add a tool result to the conversation
    pub fn add_tool_result(
        &mut self,
        tool_name: String,
        arguments: String,
        result: String,
        call_id: Option<String>,
    ) {
        self.messages
            .push(Message::tool_result(tool_name, arguments, result, call_id));
        self.metadata.message_count = self.messages.len();
        self.updated_at = Utc::now();
    }

    /// Rename the conversation
    pub fn rename(&mut self, name: String) {
        self.name = name;
        self.updated_at = Utc::now();
    }
}

/// Summary of a conversation (for listing)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSummary {
    /// Unique identifier
    pub id: Uuid,
    /// Human-readable name
    pub name: String,
    /// When the conversation was created
    pub created_at: DateTime<Utc>,
    /// When the conversation was last updated
    pub updated_at: DateTime<Utc>,
    /// Number of messages
    pub message_count: usize,
    /// Model wire id used (e.g. `anthropic/claude-3.7-sonnet`).
    pub model: String,
    /// User-facing model alias (the handle from
    /// [`crate::config::ModelRegistry`]). Pre-v4 files default to
    /// `"unknown"`. Used by `/conversations` to mark unavailable
    /// rows and by `/load` to validate before activation.
    #[serde(default = "default_summary_alias")]
    pub model_alias: String,
}

fn default_summary_alias() -> String {
    "unknown".to_string()
}

impl From<&Conversation> for ConversationSummary {
    fn from(conv: &Conversation) -> Self {
        ConversationSummary {
            id: conv.id,
            name: conv.name.clone(),
            created_at: conv.created_at,
            updated_at: conv.updated_at,
            message_count: conv.metadata.message_count,
            model: conv.model.clone(),
            model_alias: conv.metadata.model_alias.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conversation_creation() {
        let conv = Conversation::new("Test".to_string(), "claude-3".to_string());
        assert_eq!(conv.name, "Test");
        assert_eq!(conv.model, "claude-3");
        assert!(conv.messages.is_empty());
    }

    #[test]
    fn test_add_messages() {
        let mut conv = Conversation::new("Test".to_string(), "claude-3".to_string());

        conv.add_user_message("Hello".to_string());
        assert_eq!(conv.messages.len(), 1);

        conv.add_assistant_message("Hi there!".to_string());
        assert_eq!(conv.messages.len(), 2);

        conv.add_tool_result(
            "bash".to_string(),
            r#"{"command": "ls"}"#.to_string(),
            "output".to_string(),
            Some("call_1".to_string()),
        );
        assert_eq!(conv.messages.len(), 3);
        assert_eq!(conv.metadata.message_count, 3);

        // Test adding a tool call
        conv.add_tool_call(
            "file_read".to_string(),
            r#"{"path": "/test/file.txt"}"#.to_string(),
            Some("call_2".to_string()),
        );
        assert_eq!(conv.messages.len(), 4);
    }

    #[test]
    fn test_serialization() {
        let mut conv = Conversation::new("Test".to_string(), "claude-3".to_string());
        conv.add_user_message("Hello".to_string());
        conv.add_assistant_message("Hi!".to_string());

        let json = serde_json::to_string(&conv).unwrap();
        let loaded: Conversation = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.id, conv.id);
        assert_eq!(loaded.name, conv.name);
        assert_eq!(loaded.messages.len(), 2);
    }

    #[test]
    fn test_serialization_with_tool_calls() {
        let mut conv = Conversation::new("Test".to_string(), "claude-3".to_string());
        conv.add_user_message("List files".to_string());
        conv.add_tool_call(
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            Some("call_1".to_string()),
        );
        conv.add_tool_result(
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            "file1.txt\nfile2.txt".to_string(),
            Some("call_1".to_string()),
        );
        conv.add_assistant_message("Here are the files.".to_string());

        let json = serde_json::to_string_pretty(&conv).unwrap();
        let loaded: Conversation = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.messages.len(), 4);

        // Verify tool call roundtrip
        match &loaded.messages[1] {
            Message::ToolCall {
                tool_name,
                arguments,
                call_id,
                ..
            } => {
                assert_eq!(tool_name, "bash");
                assert_eq!(arguments, r#"{"command":"ls"}"#);
                assert_eq!(call_id.as_deref(), Some("call_1"));
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }

        // Verify tool result roundtrip
        match &loaded.messages[2] {
            Message::ToolResult {
                tool_name,
                arguments,
                result,
                call_id,
                ..
            } => {
                assert_eq!(tool_name, "bash");
                assert_eq!(arguments, r#"{"command":"ls"}"#);
                assert_eq!(result, "file1.txt\nfile2.txt");
                assert_eq!(call_id.as_deref(), Some("call_1"));
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }

    /// Verify backward compatibility: JSON without call_id fields still deserializes
    #[test]
    fn test_backward_compat_no_call_id() {
        let json = r#"{
            "id": "10da8b9d-f242-4786-9c75-c3fbc2530f1f",
            "name": "Test",
            "created_at": "2026-04-14T09:14:07Z",
            "updated_at": "2026-04-14T09:15:41Z",
            "messages": [
                {"role": "ToolCall", "tool_name": "bash", "arguments": "{}", "timestamp": "2026-04-14T09:15:41Z"},
                {"role": "ToolResult", "tool_name": "bash", "arguments": "{}", "result": "ok", "timestamp": "2026-04-14T09:15:41Z"}
            ],
            "model": "test",
            "metadata": {"message_count": 2}
        }"#;

        let conv: Conversation = serde_json::from_str(json).unwrap();
        assert_eq!(conv.messages.len(), 2);

        match &conv.messages[0] {
            Message::ToolCall { call_id, .. } => assert!(call_id.is_none()),
            other => panic!("Expected ToolCall, got {:?}", other),
        }
        match &conv.messages[1] {
            Message::ToolResult { call_id, .. } => assert!(call_id.is_none()),
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }

    // === v4: model_alias persistence + pre-v4 fallback ===============

    /// Pre-v4 conversation files don't have `model_alias` in their
    /// metadata. Loading them must default to the reserved `"unknown"`
    /// sentinel — `/load` then rejects with the canonical error rather
    /// than guessing a model.
    #[test]
    fn pre_v4_metadata_loads_with_unknown_alias() {
        let json = r#"{
            "id": "10da8b9d-f242-4786-9c75-c3fbc2530f1f",
            "name": "Old convo",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "messages": [],
            "model": "anthropic/claude-3.7-sonnet",
            "metadata": {"message_count": 0}
        }"#;
        let conv: Conversation = serde_json::from_str(json).unwrap();
        assert_eq!(conv.metadata.model_alias, "unknown");
    }

    /// The `serde(default)` literal MUST equal the
    /// [`crate::config::RESERVED_UNAVAILABLE_ALIAS`] constant. If
    /// either drifts, pre-v4 files would deserialize to a sentinel
    /// `/load` doesn't recognise, breaking the locked failure path.
    #[test]
    fn pre_v4_default_matches_reserved_unavailable_alias() {
        assert_eq!(
            super::default_unknown_alias(),
            crate::config::RESERVED_UNAVAILABLE_ALIAS
        );
    }

    /// New conversations stamped via `new_with_alias` round-trip the
    /// alias through serde without loss.
    #[test]
    fn new_with_alias_roundtrips_through_serde() {
        let conv = Conversation::new_with_alias(
            "Test".into(),
            "anthropic/claude-3.7-sonnet".into(),
            "sonnet".into(),
        );
        let json = serde_json::to_string(&conv).unwrap();
        let parsed: Conversation = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.metadata.model_alias, "sonnet");
        assert_eq!(parsed.model, "anthropic/claude-3.7-sonnet");
    }

    /// Default constructor stamps the unknown sentinel — keeps test
    /// helpers compatible without forcing every site through
    /// `new_with_alias`.
    #[test]
    fn legacy_new_constructor_stamps_unknown_alias() {
        let conv = Conversation::new("Test".into(), "test-model".into());
        assert_eq!(conv.metadata.model_alias, "unknown");
    }
}
