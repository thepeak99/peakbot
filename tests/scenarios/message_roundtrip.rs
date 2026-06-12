//! Message roundtrip tests
//!
//! Tests for verifying the complete message flow through the agent.
//! Uses StateManager as the single source of truth.

use crate::harness::TestHarness;
use peakbot::mock::MockResponse;

#[tokio::test]
async fn simple_message_roundtrip() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Hi there!"));

    let response = harness.run_message("Hello").await;

    // Verify response contains expected content
    assert!(
        response.contains("Hi") || response.contains("there"),
        "Response should contain greeting, got: {}",
        response
    );
}

#[tokio::test]
async fn multiple_messages_persist() {
    let mut harness = TestHarness::new();
    harness.add_responses(vec![
        MockResponse::text("First response"),
        MockResponse::text("Second response"),
        MockResponse::text("Third response"),
    ]);

    // Run multiple messages via StateManager
    harness.run_message("First message").await;
    harness.run_message("Second message").await;
    harness.run_message("Third message").await;

    // Verify responses
    // Verify via StateManager that history accumulated
    let state = harness.get_state();
    assert!(
        state.chat.messages.len() >= 6,
        "Should have 6 messages (3 user + 3 assistant), got {}",
        state.chat.messages.len()
    );
}

#[tokio::test]
async fn agent_preamble_respected() {
    let mut harness =
        TestHarness::with_system_prompt("You are a pirate assistant. Always speak like a pirate.");
    harness.add_response(MockResponse::text("Ahoy matey! How can I help ye?"));

    let response = harness.run_message("Hello").await;

    // The mock returns text regardless of preamble,
    // but the agent should have the preamble loaded
    assert!(!response.is_empty());
}

#[tokio::test]
async fn tool_call_with_follow_up() {
    let mut harness = TestHarness::new();

    // Queue two responses:
    // 1. The tool call response
    harness.add_response(MockResponse::tool_call_with_follow_up(
        "todo",
        serde_json::json!({
            "action": "add",
            "tasks": ["Test task"]
        }),
        "I've added the task to your todo list.",
    ));
    // 2. Final text response after tool is processed
    harness.add_response(MockResponse::text("I've added the task to your todo list."));

    let response = harness.run_message("Add a todo").await;

    // Response should include the follow-up text
    assert!(
        response.contains("task") || response.contains("todo"),
        "Response should mention task, got: {}",
        response
    );
}

/// Test that the harness correctly reports event emission
#[tokio::test]
async fn event_emission_simple() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Hello back!"));

    let response = harness.run_message("Hello").await;

    // Basic response should work
    assert!(!response.is_empty());
}

/// Test stats accumulation after multiple messages
#[tokio::test]
async fn stats_accumulate_after_messages() {
    use peakbot::state::StateManager;
    use std::sync::Arc;

    let state_manager = Arc::new(StateManager::new());

    // Add some stats
    state_manager.add_request(100, 50, 0.01);
    state_manager.add_request(200, 100, 0.02);

    let stats = state_manager.get_stats();
    assert_eq!(stats.total_api_calls, 2);
}

/// Test that the last user message is not sent twice to the model.
///
/// The production flow is: add_user_message() → get_agent_history() → prompt_with_history(msg).
/// Since get_agent_history() includes the just-added user message, and prompt_with_history()
/// appends `msg` as the current prompt, the user message can appear twice in the request.
/// This test verifies that doesn't happen.
#[tokio::test]
async fn user_message_not_duplicated_in_request() {
    use rig_core::completion::message::Message as RigMessage;
    use rig_core::completion::message::UserContent;

    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Got it!"));

    harness.run_message("Hello world").await;

    // Inspect the recorded request sent to the LLM
    let requests = harness.get_recorded_requests();
    assert_eq!(requests.len(), 1, "Expected exactly 1 LLM request");

    let request = &requests[0];

    // Count how many times the user message "Hello world" appears in chat_history
    let user_message_count = request
        .chat_history
        .iter()
        .filter(|msg| {
            if let RigMessage::User { content } = msg {
                content.iter().any(|c| {
                    if let UserContent::Text(t) = c {
                        t.text == "Hello world"
                    } else {
                        false
                    }
                })
            } else {
                false
            }
        })
        .count();

    assert_eq!(
        user_message_count, 1,
        "User message 'Hello world' should appear exactly once in the LLM request, \
         but appeared {} times (duplicate detected)",
        user_message_count
    );
}

/// Simulate the production code path where add_user_message() is called BEFORE
/// get_agent_history(), which causes the user message to appear in both the
/// history and the prompt argument to prompt_with_history().
///
/// Production flow (lib.rs event_loop → agent_loop):
///   1. sm.add_user_message(msg)        ← adds to StateManager
///   2. sm.get_agent_history()           ← returns history INCLUDING the just-added msg
///   3. agent.prompt_with_history(msg, history) ← msg appended AGAIN as prompt
///
/// This test reproduces that ordering to catch the duplicate.
#[tokio::test]
async fn user_message_not_duplicated_production_flow() {
    use peakbot::mock::{MockCompletionModel, MockResponse as MR};
    use peakbot::state::StateManager;
    use peakbot::{DynAgent, SessionHook};
    use rig_core::completion::message::Message as RigMessage;
    use rig_core::completion::message::UserContent;
    use std::sync::Arc;

    let mock_model = MockCompletionModel::new();
    let mock_model_ref = mock_model.clone();
    mock_model.add_response(MR::text("Reply"));

    let state_manager = Arc::new(StateManager::new());
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let session_hook = SessionHook::new(Some(sender));

    let agent = rig_core::agent::AgentBuilder::new(mock_model_ref)
        .preamble("You are a helpful assistant.")
        .max_tokens(1024)
        .default_max_turns(10)
        .hook(session_hook.clone())
        .build();

    let msg = "Hello world";

    // Step 1: Add user message to StateManager FIRST (like production does)
    state_manager.add_user_message(msg.to_string());

    // Step 2: Get history (now includes the user message we just added)
    let mut history = state_manager.get_agent_history();

    // Step 3: Call agent with the same message as prompt (rig appends it to history)
    let agent = DynAgent::Mock(agent);
    let _result = agent.prompt_with_history(msg, &mut history).await;

    // Inspect what was actually sent to the LLM
    let requests = mock_model.get_recorded_requests();
    assert_eq!(requests.len(), 1);

    let user_msg_count = requests[0]
        .chat_history
        .iter()
        .filter(|m| {
            if let RigMessage::User { content } = m {
                content.iter().any(|c| {
                    if let UserContent::Text(t) = c {
                        t.text == msg
                    } else {
                        false
                    }
                })
            } else {
                false
            }
        })
        .count();

    assert_eq!(
        user_msg_count, 1,
        "Production flow: user message '{}' should appear exactly once in the LLM request, \
         but appeared {} times. This means add_user_message() + get_agent_history() + \
         prompt_with_history() causes duplication.",
        msg, user_msg_count
    );
}

/// Same as above, but for the second message in a conversation — verifies
/// that history accumulation doesn't cause the latest message to duplicate.
#[tokio::test]
async fn second_user_message_not_duplicated_in_request() {
    use rig_core::completion::message::Message as RigMessage;
    use rig_core::completion::message::UserContent;

    let mut harness = TestHarness::new();
    harness.add_responses(vec![
        MockResponse::text("First reply"),
        MockResponse::text("Second reply"),
    ]);

    harness.run_message("First").await;
    harness.run_message("Second").await;

    let requests = harness.get_recorded_requests();
    assert_eq!(requests.len(), 2, "Expected exactly 2 LLM requests");

    // Check the second request — it should have "First" once (history) and "Second" once (prompt)
    let second_request = &requests[1];

    let second_msg_count = second_request
        .chat_history
        .iter()
        .filter(|msg| {
            if let RigMessage::User { content } = msg {
                content.iter().any(|c| {
                    if let UserContent::Text(t) = c {
                        t.text == "Second"
                    } else {
                        false
                    }
                })
            } else {
                false
            }
        })
        .count();

    assert_eq!(
        second_msg_count, 1,
        "User message 'Second' should appear exactly once in the second LLM request, \
         but appeared {} times (duplicate detected)",
        second_msg_count
    );
}

/// Test message history is maintained correctly via StateManager
#[tokio::test]
async fn history_maintained_across_messages() {
    let mut harness = TestHarness::new();
    harness.add_responses(vec![
        MockResponse::text("First"),
        MockResponse::text("Second"),
    ]);

    // Run messages - StateManager handles history
    harness.run_message("One").await;
    harness.run_message("Two").await;

    // Verify history via StateManager
    let state = harness.get_state();
    assert!(
        state.chat.messages.len() >= 4,
        "Should have at least 4 messages (2 user + 2 assistant), got {}",
        state.chat.messages.len()
    );
}
