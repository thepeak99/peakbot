//! Agent event definitions for the event-driven hook system.
//!
//! This module provides unified event types that capture the entire agent lifecycle:
//! - LLM completion requests and responses
//! - Tool calls and results
//! - Token usage and session metadata

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ui::app_state::MessageSource;

/// An [`AgentEvent`] paired with the lane that produced it.
///
/// This is the channel payload: the orchestrator's hook stamps
/// [`MessageSource::Human`]; a sub-agent's hook stamps
/// [`MessageSource::SubAgent`]. The single receiver reads `source` to route
/// the event — rolling cost up regardless of lane, and stamping the lane
/// onto any transcript message it produces.
#[derive(Debug, Clone)]
pub struct SourcedEvent {
    /// The lane that produced this event.
    pub source: MessageSource,
    /// The event itself.
    pub event: AgentEvent,
}

/// Token usage details for a request/response cycle
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Number of input tokens
    pub input_tokens: u64,
    /// Number of output tokens
    pub output_tokens: u64,
    /// Total tokens (input + output)
    pub total_tokens: u64,
    /// Estimated cost in USD (calculated by CostHandler, not the hook)
    pub cost: f64,
}

impl TokenUsage {
    /// Create a new TokenUsage from raw token counts (cost defaults to 0.0)
    pub fn from_raw(input: u64, output: u64) -> Self {
        Self {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input + output,
            cost: 0.0, // Cost = 0 here, calculated by CostHandler
        }
    }
}

/// Unified event types for the entire agent lifecycle
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    // ─── LLM Interaction Events ───────────────────────────────────────
    /// Request sent to the LLM (prompt + history)
    CompletionRequest {
        /// Number of messages in the request
        message_count: usize,
        /// Estimated token count (if available)
        estimated_tokens: Option<u64>,
        /// Timestamp
        timestamp: DateTime<Utc>,
    },

    /// Response received from the LLM
    CompletionResponse {
        /// Text content from the model. Prose only: empty when the turn
        /// produced no text (e.g. a pure tool call).
        content: String,
        /// Reasoning/thinking from the model (if present). Flattened
        /// display string — see [`CompletionResponse::thinking`] for
        /// the lossless wire-replay carrier.
        reasoning: Option<String>,
        /// Lossless Anthropic-style thinking blocks captured from the
        /// response. Empty on every non-Anthropic provider, when the
        /// model did not think, or when the `preserve_reasoning` knob
        /// is off at the SessionHook capture seam. Kept verbatim with
        /// their signatures so `get_agent_history` can replay them
        /// into the same rig assistant message as a `ToolCall`, per
        /// Anthropic's wire contract.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        thinking: Vec<crate::reasoning::ThinkingBlock>,
        /// Token usage for this response
        usage: TokenUsage,
        /// Timestamp
        timestamp: DateTime<Utc>,
    },

    // ─── Tool Events ──────────────────────────────────────────────────
    /// Tool was invoked by the model
    ToolCall {
        /// Name of the tool being called
        tool_name: String,
        /// Arguments passed to the tool (JSON string)
        arguments: String,
        /// Optional tool call ID
        call_id: Option<String>,
        /// Timestamp when tool was called
        timestamp: DateTime<Utc>,
    },

    /// Tool execution completed
    ToolResult {
        /// Name of the tool that was executed
        tool_name: String,
        /// Arguments that were passed to the tool
        arguments: String,
        /// Tool execution result (or error message)
        result: String,
        /// Whether the tool execution was successful
        success: bool,
        /// Optional tool call ID
        call_id: Option<String>,
        /// Timestamp when tool was executed
        timestamp: DateTime<Utc>,
    },

    // ─── Session Events ───────────────────────────────────────────────
    /// Session started
    SessionStart {
        /// Model being used
        model: String,
        /// Session ID (for correlation)
        session_id: String,
        /// Timestamp
        timestamp: DateTime<Utc>,
    },

    /// Session ended
    SessionEnd {
        /// Total API calls in session
        total_api_calls: u64,
        /// Total tokens used
        total_tokens: u64,
        /// Total cost
        total_cost: f64,
        /// Duration in seconds
        duration_seconds: f64,
        /// Timestamp
        timestamp: DateTime<Utc>,
    },
}
