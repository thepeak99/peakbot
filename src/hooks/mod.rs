//! Hooks module for PeakBot.
//!
//! Provides event-driven hook system for tracking agent activity:
//! - SessionHook: Emits events for LLM calls, responses, and tool usage
//! - EventChannel: Async channel for streaming events
//! - EventProcessor: Processes events with configurable handlers

pub mod events;
pub mod session_hook;

// Re-exports
//pub use channel::{CostHandler, EventChannel, EventHandler, EventProcessor, create_event_channel};
pub use events::{AgentEvent, TokenUsage};
pub use session_hook::{ModelPricing, SessionHook, SessionStats, fetch_model_pricing};
