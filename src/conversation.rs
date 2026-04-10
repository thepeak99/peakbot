//! Conversation persistence - data structures for storing conversation history.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Metadata about a conversation (stats, etc.)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConversationMetadata {
    /// Number of messages in the conversation
    pub message_count: usize,
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
    pub fn tool_call(tool_name: String, arguments: String) -> Self {
        Message::ToolCall {
            tool_name,
            arguments,
            timestamp: Utc::now(),
        }
    }

    /// Create a new tool result message
    pub fn tool_result(tool_name: String, arguments: String, result: String) -> Self {
        Message::ToolResult {
            tool_name,
            arguments,
            result,
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
    /// Create a new conversation with the given name and model
    pub fn new(name: String, model: String) -> Self {
        let now = Utc::now();
        Conversation {
            id: Uuid::new_v4(),
            name,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            model,
            metadata: ConversationMetadata::default(),
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
    pub fn add_tool_call(&mut self, tool_name: String, arguments: String) {
        self.messages.push(Message::tool_call(tool_name, arguments));
        self.metadata.message_count = self.messages.len();
        self.updated_at = Utc::now();
    }

    /// Add a tool result to the conversation
    pub fn add_tool_result(&mut self, tool_name: String, arguments: String, result: String) {
        self.messages
            .push(Message::tool_result(tool_name, arguments, result));
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
    /// Model used
    pub model: String,
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
        );
        assert_eq!(conv.messages.len(), 3);
        assert_eq!(conv.metadata.message_count, 3);

        // Test adding a tool call
        conv.add_tool_call(
            "file_read".to_string(),
            r#"{"path": "/test/file.txt"}"#.to_string(),
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
}
