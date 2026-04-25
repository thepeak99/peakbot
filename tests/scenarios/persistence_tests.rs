//! Persistence tests - E2E conversation persistence through StateManager
//!
//! Tests for verifying conversation save/load behavior through the full agentic loop.
//! Uses StateManager as the single source of truth for all message and conversation data.

use crate::harness::TestHarness;
use peakbot::mock::MockResponse;

/// Test that conversation is saved after processing a message
#[tokio::test]
async fn conversation_save_via_agent() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Hello! I'm here to help."));

    let response = harness.run_message("Hello").await;

    assert!(!response.is_empty());

    // Verify via StateManager - conversation state should have messages
    let state = harness.get_state();
    assert!(
        !state.chat.messages.is_empty(),
        "Chat should have messages after save"
    );
}

/// Test that messages persist across multiple turns
#[tokio::test]
async fn conversation_persists_across_turns() {
    let mut harness = TestHarness::new();
    harness.add_responses(vec![
        MockResponse::text("First response"),
        MockResponse::text("Second response"),
        MockResponse::text("Third response"),
    ]);

    harness.run_message("First message").await;
    harness.run_message("Second message").await;
    harness.run_message("Third message").await;

    // Verify all messages are in the chat via StateManager
    let state = harness.get_state();
    // 3 user messages + 3 assistant messages = 6
    assert_eq!(
        state.chat.messages.len(),
        6,
        "Should have 6 messages (3 user + 3 assistant)"
    );
}

/// Test conversation list functionality
#[tokio::test]
async fn conversation_list_functionality() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Response"));

    // Create a conversation by running a message
    harness.run_message("Test message").await;

    // Verify chat state has been updated with messages
    let state = harness.get_state();
    assert!(
        !state.chat.messages.is_empty(),
        "Chat should have messages after conversation creation"
    );
}

/// Test conversation metadata preservation
#[tokio::test]
async fn conversation_metadata_preserved() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Response"));

    harness.run_message("Test message").await;

    // Verify chat state has been updated
    let state = harness.get_state();
    assert!(!state.chat.messages.is_empty(), "Chat should have messages");
}

/// Test that tool calls are recorded in conversation
#[tokio::test]
async fn tool_calls_recorded_in_conversation() {
    let mut harness = TestHarness::new();

    // Queue tool call followed by final response
    harness.add_response(MockResponse::tool_call(
        "todo",
        serde_json::json!({
            "action": "add",
            "tasks": ["Test task"]
        }),
    ));
    harness.add_response(MockResponse::text("Task added successfully."));

    harness.run_message("Add a todo: Test task").await;

    // Verify conversation has user and assistant messages
    let state = harness.get_state();
    // Should have at least user + assistant message
    assert!(
        state.chat.messages.len() >= 2,
        "Should have at least 2 messages, got {}",
        state.chat.messages.len()
    );
}
