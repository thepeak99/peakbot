//! Hooks module for PeakBot.
//!
//! Provides event-driven hook system for tracking agent activity:
//! - SessionHook: Emits events for LLM calls, responses, and tool usage
//! - EventChannel: Async channel for streaming events
//! - EventProcessor: Processes events with configurable handlers

pub mod channel;
pub mod conversation_handler;
pub mod events;
pub mod session_hook;
pub mod streaming_output_handler;

// Re-exports
pub use channel::{CostHandler, create_event_channel, EventChannel, EventProcessor, EventHandler};
pub use conversation_handler::ConversationHandler;
pub use events::{AgentEvent, TokenUsage};
pub use session_hook::{
    ModelPricing,
    SessionHook,
    SessionStats,
    fetch_model_pricing,
};

pub use streaming_output_handler::{StreamingConfig, StreamingOutputHandler, TextColor, VerbosityLevel};
