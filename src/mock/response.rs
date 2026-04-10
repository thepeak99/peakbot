//! Mock response types for testing
//!
//! Provides typed MockResponse enum that can't represent invalid states.

use serde::{Deserialize, Serialize};

/// Token usage for a response (for cost tracking tests)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Default for Usage {
    fn default() -> Self {
        Self {
            input_tokens: 100,
            output_tokens: 50,
        }
    }
}

/// A mock response that the agent can return.
/// This enum makes illegal states unrepresentable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MockResponse {
    /// Simple text response from the assistant
    Text {
        content: String,
        #[serde(default)]
        usage: Option<Usage>,
    },
    /// A tool call (function invocation)
    ToolCall {
        tool: String,
        args: serde_json::Value,
        /// Optional follow-up text after tool execution
        follow_up: Option<String>,
        #[serde(default)]
        usage: Option<Usage>,
    },
    /// An error response
    Error { message: String },
}

impl MockResponse {
    /// Create a simple text response
    pub fn text<S: Into<String>>(content: S) -> Self {
        Self::Text {
            content: content.into(),
            usage: None,
        }
    }

    /// Create a text response with specific usage
    pub fn text_with_usage<S: Into<String>>(content: S, usage: Usage) -> Self {
        Self::Text {
            content: content.into(),
            usage: Some(usage),
        }
    }

    /// Create a tool call response
    pub fn tool_call(tool: &str, args: impl Serialize) -> Self {
        Self::ToolCall {
            tool: tool.to_string(),
            args: serde_json::to_value(args).expect("args must serialize"),
            follow_up: None,
            usage: None,
        }
    }

    /// Create a tool call with follow-up text
    pub fn tool_call_with_follow_up(tool: &str, args: impl Serialize, follow_up: &str) -> Self {
        Self::ToolCall {
            tool: tool.to_string(),
            args: serde_json::to_value(args).expect("args must serialize"),
            follow_up: Some(follow_up.to_string()),
            usage: None,
        }
    }

    /// Create a tool call with specific usage
    pub fn tool_call_with_usage(tool: &str, args: impl Serialize, usage: Usage) -> Self {
        Self::ToolCall {
            tool: tool.to_string(),
            args: serde_json::to_value(args).expect("args must serialize"),
            follow_up: None,
            usage: Some(usage),
        }
    }

    /// Create an error response
    pub fn error<S: Into<String>>(message: S) -> Self {
        Self::Error {
            message: message.into(),
        }
    }

    /// Set usage for this response (builder pattern)
    pub fn with_usage(self, usage: Usage) -> Self {
        match self {
            Self::Text { content, .. } => Self::Text {
                content,
                usage: Some(usage),
            },
            Self::ToolCall {
                tool,
                args,
                follow_up,
                ..
            } => Self::ToolCall {
                tool,
                args,
                follow_up,
                usage: Some(usage),
            },
            Self::Error { .. } => self,
        }
    }
}
