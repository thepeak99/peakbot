//! Conversation persistence handler for event-driven message saving.
//!
//! This handler captures agent events and persists messages in real-time,
//! preventing data loss on interruption.

use crate::hooks::{AgentEvent, EventHandler};
use crate::ConversationManager;
use std::sync::{Arc, Mutex};

/// Event handler that persists conversation messages in real-time
pub struct ConversationHandler {
    manager: Arc<Mutex<ConversationManager>>,
}

impl ConversationHandler {
    pub fn new(manager: Arc<Mutex<ConversationManager>>) -> Self {
        Self { manager }
    }
}

impl EventHandler for ConversationHandler {
    fn handle_event(&self, event: &AgentEvent) {
        // Use blocking_lock since EventHandler::handle_event is sync
        // (trait cannot be async)
        let mut manager = self.manager.lock().unwrap();

        match event {
            AgentEvent::ToolCall {
                tool_name,
                arguments,
                ..
            } => {
                if let Err(e) = manager.add_tool_call(tool_name.clone(), arguments.clone()) {
                    tracing::error!("Failed to save tool call: {}", e);
                }
            }
            AgentEvent::ToolResult {
                tool_name,
                arguments,
                result,
                ..
            } => {
                if let Err(e) =
                    manager.add_tool_result(tool_name.clone(), arguments.clone(), result.clone())
                {
                    tracing::error!("Failed to save tool result: {}", e);
                }
            }
            AgentEvent::CompletionResponse {
                content,
                usage,
                ..
            } => {
                // Only save if content is non-empty
                if !content.trim().is_empty() {
                    if let Err(e) = manager.add_assistant_message(content.clone()) {
                        tracing::error!("Failed to save assistant message: {}", e);
                    }
                }
                if let Err(e) = manager.update_tokens(usage.total_tokens, usage.cost) {
                    tracing::error!("Failed to update tokens: {}", e);
                }
            }
            _ => {} // Ignore other events
        }
    }

    fn name(&self) -> &str {
        "ConversationHandler"
    }
}
