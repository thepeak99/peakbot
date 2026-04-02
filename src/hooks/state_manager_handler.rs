//! StateManager handler for forwarding live events to the UI.
//!
//! This handler consumes AgentEvent stream from SessionHook and writes
//! tool calls and tool results to StateManager for live UI updates.

use crate::hooks::channel::EventHandler;
use crate::hooks::events::AgentEvent;
use crate::ui::app_state::ChatMessage;
use crate::ui::state_manager::StateManager;
use std::sync::Arc;

/// Forwards live agent events (tool calls, tool results) to StateManager
pub struct StateManagerHandler {
    state_manager: Arc<StateManager>,
}

impl StateManagerHandler {
    pub fn new(state_manager: Arc<StateManager>) -> Self {
        Self { state_manager }
    }
}

impl EventHandler for StateManagerHandler {
    fn handle_event(&self, event: &AgentEvent) {
        match event {
            AgentEvent::ToolCall {
                tool_name,
                arguments,
                ..
            } => {
                self.state_manager
                    .update_chat(ChatMessage::tool_call(tool_name, arguments, ""));
            }
            AgentEvent::ToolResult {
                tool_name,
                result,
                ..
            } => {
                self.state_manager
                    .update_chat(ChatMessage::tool_result(tool_name, result));
            }
            // All other events (CompletionRequest, CompletionResponse, SessionStart, SessionEnd)
            // are handled elsewhere (CostHandler, StreamingOutputHandler, or AgentRunner)
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "StateManagerHandler"
    }
}
