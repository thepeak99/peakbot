//! TestRunner - A test-friendly wrapper around AgentRunner functionality.
//!
//! This module provides a simplified interface for E2E testing that allows
//! tests to flow through the real agentic loop while only mocking the LLM provider.
//!
//! Key differences from AgentRunner:
//! - Does not spawn loops (tests call methods directly)
//! - Uses StateManager as the single source of truth for history
//! - Compaction is owned by StateManager (same as production)
//! - Simplified configuration for testing

use crate::config::ContextConfig;
use crate::context_manager::ContextManager;
use crate::hooks::SessionHook;
use crate::providers::DynAgent;
use crate::state::StateManager;
use rig::completion::PromptError;
use rig::completion::message::Message;
use std::sync::{Arc, Mutex};

/// Result of processing a message through the TestRunner
#[derive(Debug)]
pub enum ProcessResult {
    /// Message processed successfully with response
    Success(String),
    /// Agent was stopped
    Stopped,
    /// Error occurred
    Error,
}

/// Information about a compaction event that occurred during testing
#[derive(Debug, Clone)]
pub struct CompactionInfo {
    /// Number of messages before compaction
    pub original_count: usize,
    /// Number of messages after compaction
    pub compacted_count: usize,
    /// Number of messages discarded
    pub num_discarded: usize,
}

/// TestRunner provides a test-friendly interface to the agent processing pipeline.
///
/// Mirrors AgentRunner::process_message_internal but exposes it directly for
/// synchronous testing. Compaction is handled by StateManager (same as production).
pub struct TestRunner {
    /// The agent (with mock LLM in tests)
    pub agent: Arc<DynAgent>,
    /// State manager for tracking state changes (single source of truth)
    pub state_manager: Arc<StateManager>,
    /// Session hook for event emission
    pub session_hook: Arc<SessionHook>,
    /// System prompt
    pub system_prompt: String,
    /// Track compaction events for testing verification
    compaction_events: Arc<Mutex<Vec<CompactionInfo>>>,
    /// Mock model used for compaction summarization (tests can queue responses on this)
    #[cfg(feature = "mock")]
    pub compaction_mock: Option<crate::mock::MockCompletionModel>,
}

impl TestRunner {
    /// Default context window size for tests that don't care about it.
    /// Pinned here so test fixtures can rely on a stable threshold.
    pub const DEFAULT_CONTEXT_WINDOW: usize = 128_000;

    /// Create a new TestRunner with default context configuration
    pub fn new(
        agent: DynAgent,
        state_manager: Arc<StateManager>,
        session_hook: Arc<SessionHook>,
        system_prompt: String,
    ) -> Self {
        Self::new_with_context(
            agent,
            state_manager,
            session_hook,
            system_prompt,
            ContextConfig::default(),
            Self::DEFAULT_CONTEXT_WINDOW,
        )
    }

    /// Create a TestRunner that shares an event channel with the caller
    pub fn new_with_shared_events(
        agent: DynAgent,
        state_manager: Arc<StateManager>,
        session_hook: Arc<SessionHook>,
        system_prompt: String,
    ) -> Self {
        Self::new_with_context(
            agent,
            state_manager,
            session_hook,
            system_prompt,
            ContextConfig::default(),
            Self::DEFAULT_CONTEXT_WINDOW,
        )
    }

    /// Create a TestRunner with custom context configuration.
    ///
    /// Initializes the ContextManager inside StateManager (same as production).
    /// `context_size` is the resolved usize that drives compaction; tests
    /// pin it directly rather than going through registry resolution.
    pub fn new_with_context(
        agent: DynAgent,
        state_manager: Arc<StateManager>,
        session_hook: Arc<SessionHook>,
        system_prompt: String,
        context_config: ContextConfig,
        context_size: usize,
    ) -> Self {
        let agent = Arc::new(agent);

        // Initialize ContextManager inside StateManager (same as AgentRunner)
        // Create a mock compaction model for testing
        #[cfg(feature = "mock")]
        let (compaction_model, compaction_mock_model) = {
            let (model, mock) = crate::providers::create_mock_compaction_model();
            (Some(Arc::new(model)), Some(mock))
        };
        #[cfg(not(feature = "mock"))]
        let compaction_model = None;

        let cm = ContextManager::new(
            context_config,
            context_size,
            state_manager.clone(),
            compaction_model,
        );
        state_manager.init_context_manager(cm, system_prompt.clone());

        Self {
            agent,
            state_manager,
            session_hook,
            system_prompt,
            compaction_events: Arc::new(Mutex::new(Vec::new())),
            #[cfg(feature = "mock")]
            compaction_mock: compaction_mock_model,
        }
    }

    /// Process a message through the full agentic loop.
    pub async fn run_message(&mut self, message: &str) -> String {
        let history = self.state_manager.get_agent_history();
        let result = self.process_message_internal(message, history).await;

        match result {
            ProcessResult::Success(response) => response,
            ProcessResult::Stopped => "Agent stopped".to_string(),
            ProcessResult::Error => "Error occurred".to_string(),
        }
    }

    /// Internal message processing.
    ///
    /// Mirrors the production agent_loop order (see
    /// `make-flow-great-again.md`): the user message is appended to chat
    /// **before** `prompt_with_history`, so any tool calls/results emitted
    /// during the turn land *after* it — never wedged between the user
    /// message and a future tool result.
    ///
    /// **In-loop compaction.** When the hook terminates with reason
    /// `"compact"`, this fn runs `force_compact().await` synchronously
    /// and re-enters `prompt_with_history` with the post-compaction
    /// state. Mirrors the production handler in `lib.rs`. See
    /// `mid-compaction.md`.
    async fn process_message_internal(
        &mut self,
        msg: &str,
        mut history: Vec<Message>,
    ) -> ProcessResult {
        let current_msg = msg.to_string();

        // Single-writer-for-user-input invariant: append BEFORE the prompt,
        // mirroring agent_loop in production. The legacy ordering (append
        // after) is what allowed user-text to wedge between an in-flight
        // ToolCall and its ToolResult.
        self.state_manager.add_user_message(current_msg.clone());

        // Mark as running
        self.state_manager.set_running(true);

        // Sync stats from session hook BEFORE compaction check, so
        // needs_compaction sees the latest token counts from previous turns.
        self.process_session_hook_events();

        loop {
            // Pre-prompt compaction check (synchronous so tests can verify
            // events). The hook also gates mid-loop, but this front-loads
            // the very first request when it's already over budget from
            // history alone.
            if let Some(result) = self.state_manager.compact_if_needed().await {
                self.compaction_events.lock().unwrap().push(CompactionInfo {
                    original_count: result.original_count,
                    compacted_count: result.compacted_count,
                    num_discarded: result.num_discarded,
                });
                // Re-read history after compaction
                history = self.state_manager.get_agent_history();
            }

            // Call the agent with history
            let result = self
                .agent
                .prompt_with_history(&current_msg, &mut history)
                .await;

            // Process events from the session hook to update stats
            self.process_session_hook_events();

            match result {
                Ok(response) => {
                    // Append the assistant response. The user message was
                    // appended at the top of this fn — see invariant above.
                    self.state_manager.add_assistant_message(response.clone());
                    self.state_manager.set_running(false);
                    return ProcessResult::Success(response);
                }
                Err(PromptError::PromptCancelled { reason, .. }) if reason == "compact" => {
                    // Mid-loop compaction request from the hook: compact
                    // synchronously, then re-enter the loop. The loop guard
                    // (cleared last_input_tokens in apply_compaction)
                    // prevents the hook from re-terminating immediately.
                    let compacted = self.state_manager.force_compact().await;
                    if let Some(result) = compacted {
                        self.compaction_events.lock().unwrap().push(CompactionInfo {
                            original_count: result.original_count,
                            compacted_count: result.compacted_count,
                            num_discarded: result.num_discarded,
                        });
                    } else {
                        // Compaction couldn't make progress (e.g. no
                        // summarizer responses queued in the test, or
                        // empty body). Manually clear the staleness signal
                        // so the gate stops firing — break the loop and
                        // let the request go to the wire. Mirrors the
                        // production handler in `lib.rs`.
                        self.state_manager.clear_last_input_tokens();
                    }
                    history = self.state_manager.get_agent_history();
                    continue;
                }
                Err(PromptError::PromptCancelled { reason, .. }) => {
                    self.state_manager.set_running(false);
                    if reason == "stop" {
                        return ProcessResult::Stopped;
                    }
                    return ProcessResult::Error;
                }
                Err(_) => {
                    self.state_manager.set_running(false);
                    return ProcessResult::Error;
                }
            }
        }
    }

    /// Process events from the session hook to update StateManager stats.
    fn process_session_hook_events(&self) {
        let hook_stats = self.session_hook.get_stats();
        self.state_manager.reset_stats();
        let pricing = crate::hooks::ModelPricing::default();
        for req in hook_stats.all_requests() {
            let cost = (req.input_tokens as f64 * pricing.input_per_token)
                + (req.output_tokens as f64 * pricing.output_per_token);
            self.state_manager
                .add_request(req.input_tokens, req.output_tokens, cost);
        }
    }

    /// Get all compaction events that occurred during testing
    pub fn get_compaction_events(&self) -> Vec<CompactionInfo> {
        self.compaction_events.lock().unwrap().clone()
    }

    /// Check if any compaction occurred
    pub fn has_compaction_occurred(&self) -> bool {
        !self.compaction_events.lock().unwrap().is_empty()
    }

    /// Get current chat history from StateManager (single source of truth)
    pub fn get_chat_history(&self) -> Vec<Message> {
        self.state_manager.get_agent_history()
    }

    /// Clear chat history via StateManager
    pub fn clear_history(&self) {
        self.state_manager.clear_chat();
    }

    /// Get current state
    pub fn get_state(&self) -> crate::ui::AppState {
        self.state_manager.get_state()
    }

    /// Get todo list
    pub fn get_todos(&self) -> Vec<crate::TodoItem> {
        self.state_manager.get_todo_list().list().to_vec()
    }

    /// Get stats
    pub fn get_stats(&self) -> crate::hooks::SessionStats {
        self.state_manager.get_stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MockCompletionModel, MockResponse};

    #[tokio::test]
    async fn test_simple_message_roundtrip() {
        let mock_model = MockCompletionModel::new();
        mock_model.add_response(MockResponse::text("Hello! How can I help?"));

        let state_manager = Arc::new(StateManager::new());
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let session_hook = SessionHook::new(Some(sender.clone()));

        let agent = rig::agent::AgentBuilder::new(mock_model)
            .preamble("You are a helpful assistant.")
            .max_tokens(1024)
            .default_max_turns(10)
            .hook(session_hook.clone())
            .tool(crate::TodoTool::new(state_manager.clone()))
            .build();

        let session_hook_arc = Arc::new(session_hook);
        let mut runner = TestRunner::new_with_shared_events(
            DynAgent::Mock(agent),
            state_manager,
            session_hook_arc,
            "You are a helpful assistant.".to_string(),
        );

        let response = runner.run_message("Hello").await;
        assert!(response.contains("Hello") || response.contains("help"));
    }

    #[tokio::test]
    async fn test_stats_accumulate() {
        let mock_model = MockCompletionModel::new();
        mock_model.add_response(MockResponse::text_with_usage(
            "Response",
            crate::mock::Usage {
                input_tokens: 100,
                output_tokens: 50,
            },
        ));

        let state_manager = Arc::new(StateManager::new());
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let session_hook = SessionHook::new(Some(sender.clone()));

        let agent = rig::agent::AgentBuilder::new(mock_model)
            .preamble("You are a helpful assistant.")
            .max_tokens(1024)
            .default_max_turns(10)
            .hook(session_hook.clone())
            .tool(crate::TodoTool::new(state_manager.clone()))
            .build();

        let session_hook_arc = Arc::new(session_hook);
        let mut runner = TestRunner::new_with_shared_events(
            DynAgent::Mock(agent),
            state_manager.clone(),
            session_hook_arc,
            "You are a helpful assistant.".to_string(),
        );

        runner.run_message("Test").await;
    }
}
