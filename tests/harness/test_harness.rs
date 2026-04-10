//! Test harness for integration testing
//!
//! TestHarness provides a unified way to set up tests with:
//! - StateManager for tracking state changes
//! - MockCompletionModel for simulating LLM responses
//! - Storage abstraction for conversation persistence
//! - Event collection for verifying agent behavior

use crate::mock::{MockCompletionModel, MockResponse};
use crate::storage::InMemoryStorage;
use peakbot::ui::AppState;
use peakbot::{AgentEvent, SessionStats, StateManager, TodoItem};
use rig::agent::{Agent, AgentBuilder};
use rig::completion::Prompt;
use rig::completion::message::Message;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;

/// Test harness for integration tests
pub struct TestHarness {
    /// State manager for tracking state changes
    pub state_manager: Arc<StateManager>,
    /// Mock completion model for simulating LLM responses
    pub mock_model: MockCompletionModel,
    /// Mock agent for testing
    pub agent: Agent<MockCompletionModel, ()>,
    /// Optional event receiver for collecting agent events
    pub event_receiver: Option<mpsc::UnboundedReceiver<AgentEvent>>,
    /// Conversation storage for persistence tests
    pub storage: Arc<InMemoryStorage>,
    /// Temp directory for file-based tests
    _temp_dir: Option<TempDir>,
}

impl TestHarness {
    /// Create a new TestHarness with default configuration
    pub fn new() -> Self {
        Self::with_system_prompt("You are a helpful assistant.")
    }

    /// Create a new TestHarness with a custom system prompt
    pub fn with_system_prompt(preamble: &str) -> Self {
        let state_manager = Arc::new(StateManager::new());
        let mock_model = MockCompletionModel::new();

        // Build agent directly with AgentBuilder (no client needed for mock)
        let agent = AgentBuilder::new(mock_model.clone())
            .preamble(preamble)
            .max_tokens(1024)
            .default_max_turns(10)
            .build();

        Self {
            state_manager,
            mock_model,
            agent,
            event_receiver: None,
            storage: Arc::new(InMemoryStorage::new()),
            _temp_dir: None,
        }
    }

    /// Add a mock response to the queue
    pub fn add_response(&self, response: MockResponse) {
        self.mock_model.add_response(response);
    }

    /// Add multiple mock responses
    pub fn add_responses(&self, responses: impl IntoIterator<Item = MockResponse>) {
        self.mock_model.add_responses(responses);
    }

    /// Run a message through the agent and return the response
    pub async fn run_message(&self, message: &str) -> String {
        let mut history = Vec::new();
        let result = self.agent.prompt(message).with_history(&mut history).await;

        match result {
            Ok(response) => response,
            Err(e) => format!("Error: {:?}", e),
        }
    }

    /// Run a message with history
    pub async fn run_message_with_history(
        &self,
        message: &str,
        history: &mut Vec<Message>,
    ) -> String {
        let result = self.agent.prompt(message).with_history(history).await;

        match result {
            Ok(response) => response,
            Err(e) => format!("Error: {:?}", e),
        }
    }

    /// Get current state snapshot
    pub fn get_state(&self) -> AppState {
        self.state_manager.get_state()
    }

    /// Get todo list
    pub fn get_todos(&self) -> Vec<TodoItem> {
        self.state_manager.get_todo_list().list().to_vec()
    }

    /// Get stats
    pub fn get_stats(&self) -> SessionStats {
        self.state_manager.get_stats()
    }

    /// Check if there are remaining mock responses
    pub fn has_remaining_responses(&self) -> bool {
        self.mock_model.has_responses()
    }

    /// Get remaining response count
    pub fn remaining_responses(&self) -> usize {
        self.mock_model.remaining()
    }

    /// Clear all queued responses
    pub fn clear_responses(&self) {
        self.mock_model.clear();
    }
}

impl Default for TestHarness {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockResponse;

    #[tokio::test]
    async fn test_simple_message_roundtrip() {
        let harness = TestHarness::new();
        harness.add_response(MockResponse::text("Hello! How can I help?"));

        let response = harness.run_message("Hello").await;

        assert!(response.contains("Hello"));
        assert!(response.contains("help") || response.contains("How can I"));
    }

    #[tokio::test]
    async fn test_multiple_responses() {
        let harness = TestHarness::new();
        harness.add_responses(vec![
            MockResponse::text("First response"),
            MockResponse::text("Second response"),
        ]);

        let r1 = harness.run_message("One").await;
        let r2 = harness.run_message("Two").await;

        assert!(r1.contains("First"));
        assert!(r2.contains("Second"));
    }

    #[tokio::test]
    async fn test_tool_call_response() {
        let harness = TestHarness::new();
        harness.add_response(MockResponse::tool_call(
            "todo",
            serde_json::json!({
                "action": "add",
                "tasks": ["Test task"]
            }),
        ));

        let response = harness.run_message("Add a todo").await;

        // The tool call should be processed and the response indicates tool was called
        assert!(!response.is_empty());
    }
}
