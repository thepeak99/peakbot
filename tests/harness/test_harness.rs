//! Test harness for integration testing
//!
//! TestHarness provides a unified way to set up tests with:
//! - StateManager for tracking state changes
//! - MockCompletionModel for simulating LLM responses
//! - Storage abstraction for conversation persistence
//! - Event collection for verifying agent behavior
//!
//! This harness creates a proper TestRunner that flows through the real agentic loop,
//! enabling true E2E tests where only the LLM provider is mocked.

use peakbot::ContextConfig;
use peakbot::mock::{MockCompletionModel, MockResponse};
use peakbot::storage::InMemoryStorage;
use peakbot::test_runner::TestRunner;
use peakbot::ui::AppState;
use peakbot::{AgentEvent, ConversationManager, SessionStats, StateManager, TodoItem};
use rig::completion::message::Message;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// Test harness for integration tests
///
/// This harness creates real components (StateManager, tools) with a mock LLM,
/// allowing true E2E testing where:
/// - Tool calls are real (modify actual state)
/// - ContextManager compaction is real
/// - Event emission is real
/// - Only the LLM provider is mocked
///
/// Uses TestRunner internally to ensure messages flow through the full agentic loop.
pub struct TestHarness {
    /// State manager for tracking state changes
    pub state_manager: Arc<StateManager>,
    /// Mock completion model for simulating LLM responses
    pub mock_model: MockCompletionModel,
    /// Event receiver for collecting agent events
    pub event_receiver: tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    /// Conversation storage for persistence tests
    pub storage: Arc<InMemoryStorage>,
    /// Temp directory for file-based tests
    _temp_dir: Option<TempDir>,
    /// Internal test runner for processing messages through the full loop
    runner: TestRunner,
}

impl TestHarness {
    /// Create a new TestHarness with default configuration
    pub fn new() -> Self {
        Self::with_system_prompt("You are a helpful assistant.")
    }

    /// Create a new TestHarness with a custom system prompt
    pub fn with_system_prompt(preamble: &str) -> Self {
        Self::with_system_prompt_and_context(preamble, ContextConfig::default())
    }

    /// Create a new TestHarness with custom system prompt and context configuration.
    ///
    /// This allows tests to configure a small context window to trigger compaction
    /// with fewer messages. For example, with a 500-token context window and 80%
    /// threshold, compaction triggers at ~400 tokens.
    pub fn with_system_prompt_and_context(preamble: &str, context_config: ContextConfig) -> Self {
        let mock_model = MockCompletionModel::new();
        let mock_model_clone = mock_model.clone();
        let state_manager = Arc::new(StateManager::new());

        // Create event channel - both TestHarness and TestRunner share the sender
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let session_hook = peakbot::SessionHook::new(Some(sender.clone()));

        // Build agent with mock model, session hook, and built-in tools
        let agent = rig::agent::AgentBuilder::new(mock_model_clone)
            .preamble(preamble)
            .max_tokens(1024)
            .default_max_turns(10)
            .hook(session_hook.clone())
            .tool(peakbot::TodoTool::new(state_manager.clone()))
            .tool(peakbot::BashTool::default())
            .tool(peakbot::FileEditTool::default())
            .tool(peakbot::FileReadTool)
            .tool(peakbot::ListDirectoryTool)
            .tool(peakbot::FetchUrlTool)
            .tool(peakbot::ThinkTool)
            .build();

        // Create Arc for shared access in TestRunner
        let session_hook_arc = Arc::new(session_hook);

        // Create TestRunner with custom context config
        let runner = TestRunner::new_with_context(
            peakbot::DynAgent::Mock(agent),
            state_manager.clone(),
            session_hook_arc,
            preamble.to_string(),
            context_config,
        );

        Self {
            state_manager,
            mock_model,
            event_receiver: receiver,
            storage: Arc::new(InMemoryStorage::new()),
            _temp_dir: None,
            runner,
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
    ///
    /// This flows through the full agentic loop via TestRunner, including:
    /// - Context compaction checks
    /// - Tool execution (real tools modify state)
    /// - Event emission (SessionHook emits events)
    /// - History management
    pub async fn run_message(&mut self, message: &str) -> String {
        self.runner.run_message(message).await
    }

    /// Run a message with history (backward compatible)
    pub async fn run_message_with_history(
        &mut self,
        message: &str,
        history: &mut Vec<Message>,
    ) -> String {
        self.runner.run_message_with_history(message, history).await
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

    /// Get a clone of the chat history (for verification)
    pub async fn get_chat_history(&self) -> Vec<Message> {
        self.runner.get_chat_history().await
    }

    /// Clear the chat history
    pub async fn clear_chat_history(&self) {
        self.runner.clear_history().await;
    }

    /// Get a reference to the state manager for assertions
    ///
    /// Allows tests to verify state changes after tool execution,
    /// todo modifications, stats accumulation, etc.
    pub fn state_manager(&self) -> &Arc<StateManager> {
        &self.state_manager
    }

    /// Get a reference to the conversation manager for assertions
    ///
    /// Allows tests to verify conversation persistence behavior.
    /// Returns None if conversation manager was not initialized.
    pub fn conversation_manager(
        &self,
    ) -> Option<&Arc<Mutex<ConversationManager<InMemoryStorage>>>> {
        self.runner.conversation_manager()
    }

    /// Drain all emitted events from the event receiver
    ///
    /// Collects all events that have been emitted during agent execution.
    /// Useful for verifying event emission behavior in tests.
    pub fn drain_events(&mut self) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.event_receiver.try_recv() {
            events.push(event);
        }
        events
    }

    // ── Compaction Tracking ─────────────────────────────────────────────────────

    /// Get all compaction events that occurred during testing
    pub fn get_compaction_events(&self) -> Vec<peakbot::CompactionInfo> {
        self.runner.get_compaction_events()
    }

    /// Check if any compaction occurred during testing
    pub fn has_compaction_occurred(&self) -> bool {
        self.runner.has_compaction_occurred()
    }

    /// Get the number of compaction events that occurred
    pub fn compaction_count(&self) -> usize {
        self.runner.compaction_count()
    }

    /// Clear compaction events (for resetting test state)
    pub fn clear_compaction_events(&self) {
        self.runner.clear_compaction_events();
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
    use peakbot::mock::MockResponse;

    #[tokio::test]
    async fn test_simple_message_roundtrip() {
        let mut harness = TestHarness::new();
        harness.add_response(MockResponse::text("Hello! How can I help?"));

        let response = harness.run_message("Hello").await;

        assert!(response.contains("Hello"));
        assert!(response.contains("help") || response.contains("How can I"));
    }

    #[tokio::test]
    async fn test_multiple_responses() {
        let mut harness = TestHarness::new();
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
        let mut harness = TestHarness::new();
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
