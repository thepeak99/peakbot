//! Test harness for integration testing
//!
//! TestHarness provides a unified way to set up tests with:
//! - StateManager for tracking state changes (single source of truth)
//! - MockCompletionModel for simulating LLM responses
//! - Storage abstraction for conversation persistence
//! - Event collection for verifying agent behavior
//!
//! This harness creates a proper TestRunner that flows through the real agentic loop,
//! enabling true E2E tests where only the LLM provider is mocked.

use peakbot::ContextConfig;
use peakbot::mock::{MockCompletionModel, MockResponse, RecordedRequest};
use peakbot::test_runner::TestRunner;
use peakbot::ui::AppState;
use peakbot::{AgentEvent, SessionStats, StateManager, TodoItem};
use rig::completion::message::Message;
use std::sync::Arc;
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
        Self::with_system_prompt_and_context(
            preamble,
            ContextConfig::default(),
            TestRunner::DEFAULT_CONTEXT_WINDOW,
        )
    }

    /// Create a new TestHarness with custom system prompt, context configuration,
    /// and an explicit context window.
    ///
    /// `context_window` is the resolved usize that drives compaction (tests pin
    /// this directly rather than threading a wire-id through `auto_detect_*`).
    /// For example, with a 500-token window and 80% threshold compaction
    /// triggers at ~400 tokens.
    pub fn with_system_prompt_and_context(
        preamble: &str,
        context_config: ContextConfig,
        context_window: usize,
    ) -> Self {
        let mock_model = MockCompletionModel::new();
        let mock_model_clone = mock_model.clone();
        let state_manager = Arc::new(StateManager::new());

        // Create event channel - both TestHarness and TestRunner share the sender
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        // Wire the hook to the StateManager so `on_completion_call` can
        // gate in-loop compaction (production parity — see `mid-compaction.md`).
        let session_hook =
            peakbot::SessionHook::new(Some(sender.clone())).with_state_manager(&state_manager);

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
            context_window,
        );

        Self {
            state_manager,
            mock_model,
            event_receiver: receiver,
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

    /// Get a clone of the chat history from StateManager (for verification)
    pub fn get_chat_history(&self) -> Vec<Message> {
        self.runner.get_chat_history()
    }

    /// Clear the chat history via StateManager
    pub fn clear_chat_history(&self) {
        self.runner.clear_history();
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

    /// Add a mock response for the compaction summarization model.
    /// When compaction fires, it uses a separate tool-free model.
    /// Queue responses here to control what the summarizer returns.
    pub fn add_compaction_response(&self, response: MockResponse) {
        if let Some(ref mock) = self.runner.compaction_mock {
            mock.add_response(response);
        }
    }

    /// Get all compaction events that occurred during testing
    pub fn get_compaction_events(&self) -> Vec<peakbot::CompactionInfo> {
        self.runner.get_compaction_events()
    }

    /// Check if any compaction occurred during testing
    pub fn has_compaction_occurred(&self) -> bool {
        self.runner.has_compaction_occurred()
    }

    /// Get the number of chat messages currently in StateManager.
    /// This reflects the ACTUAL persisted state, not just what the agent sees.
    pub fn get_chat_message_count(&self) -> usize {
        self.state_manager.get_state().chat.messages.len()
    }

    /// Get the number of uncompacted messages (what the LLM would see).
    pub fn get_uncompacted_message_count(&self) -> usize {
        self.state_manager
            .get_state()
            .chat
            .messages
            .iter()
            .filter(|m| !m.compacted)
            .count()
    }

    /// Check if any messages are tagged as compacted.
    pub fn has_compacted_messages(&self) -> bool {
        self.state_manager
            .get_state()
            .chat
            .messages
            .iter()
            .any(|m| m.compacted)
    }

    /// Get the number of messages tagged as compacted.
    #[allow(dead_code)] // test utility kept for future scenarios
    pub fn compacted_message_count(&self) -> usize {
        self.state_manager
            .get_state()
            .chat
            .messages
            .iter()
            .filter(|m| m.compacted)
            .count()
    }

    /// Check if a Summary message exists in the chat.
    pub fn has_summary_message(&self) -> bool {
        use peakbot::ui::app_state::MessageRole;
        self.state_manager
            .get_state()
            .chat
            .messages
            .iter()
            .any(|m| m.role == MessageRole::Summary)
    }

    // ── Request Recording ───────────────────────────────────────────────────────

    /// Get all recorded LLM requests (both regular and summarization).
    pub fn get_recorded_requests(&self) -> Vec<RecordedRequest> {
        self.mock_model.get_recorded_requests()
    }

    /// Get the total number of LLM calls made.
    pub fn request_count(&self) -> usize {
        self.mock_model.request_count()
    }

    /// Get recorded requests from the compaction model (summarization calls).
    pub fn get_compaction_requests(&self) -> Vec<RecordedRequest> {
        self.runner
            .compaction_mock
            .as_ref()
            .map(|m| m.get_recorded_requests())
            .unwrap_or_default()
    }

    /// Get only the summarization requests (sent to the compaction model).
    /// With the new architecture, summarization goes through a separate tool-free model.
    pub fn get_summarization_requests(&self) -> Vec<RecordedRequest> {
        self.get_compaction_requests()
    }

    /// Get only the regular (non-summarization) requests — all main agent requests.
    pub fn get_regular_requests(&self) -> Vec<RecordedRequest> {
        self.get_recorded_requests()
    }

    /// Extract the prompt text from a summarization request.
    /// The prompt is sent as the main prompt (not in chat_history) to the compaction model.
    pub fn extract_summarization_prompt(request: &RecordedRequest) -> Option<String> {
        // The compaction model receives the prompt as the last user message
        request.chat_history.last().and_then(|msg| {
            if let rig::completion::message::Message::User { content } = msg {
                content.iter().find_map(|c| {
                    if let rig::completion::message::UserContent::Text(t) = c {
                        if t.text.contains("Summarize this conversation concisely")
                            || t.text.contains("Previous conversation:")
                        {
                            Some(t.text.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
            } else {
                None
            }
        })
    }

    // ── Assertions ─────────────────────────────────────────────────────────────

    /// Assert that the last compaction event actually discarded messages.
    /// Panics with a descriptive message if no compaction occurred or if
    /// num_discarded was 0 (the stub behavior).
    ///
    /// Note: compacted_count can equal original_count when summarization replaces
    /// discarded messages 1:1 (e.g., 1 discarded message replaced by 1 summary).
    /// The reliable indicator is num_discarded > 0.
    pub fn assert_compaction_actually_discarded(&self) {
        let events = self.get_compaction_events();
        assert!(
            !events.is_empty(),
            "Expected compaction to have occurred, but no compaction events were recorded"
        );
        let last = events.last().unwrap();
        assert!(
            last.num_discarded > 0,
            "Compaction event fired but num_discarded is 0 -- compaction was a no-op stub. \
             original_count: {}, compacted_count: {}",
            last.original_count,
            last.compacted_count
        );
        assert!(
            last.compacted_count <= last.original_count,
            "Compaction event fired but compacted_count ({}) > original_count ({}) -- \
             compaction somehow ADDED messages",
            last.compacted_count,
            last.original_count
        );
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
