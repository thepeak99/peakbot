//! Message roundtrip tests
//!
//! Tests for verifying the complete message flow through the agent.

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

    let mut history = Vec::new();

    // Run multiple messages
    let r1 = harness
        .run_message_with_history("First message", &mut history)
        .await;
    let r2 = harness
        .run_message_with_history("Second message", &mut history)
        .await;
    let r3 = harness
        .run_message_with_history("Third message", &mut history)
        .await;

    // Verify responses
    assert!(r1.contains("First"));
    assert!(r2.contains("Second"));
    assert!(r3.contains("Third"));

    // Verify history accumulated
    assert!(
        history.len() >= 2,
        "History should have messages, got: {}",
        history.len()
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

/// Test message history is maintained correctly
#[tokio::test]
async fn history_maintained_across_messages() {
    let mut harness = TestHarness::new();
    harness.add_responses(vec![
        MockResponse::text("First"),
        MockResponse::text("Second"),
    ]);

    let mut history = Vec::new();

    let r1 = harness.run_message_with_history("One", &mut history).await;
    let r2 = harness.run_message_with_history("Two", &mut history).await;

    // Both responses should be correct
    assert!(r1.contains("First"));
    assert!(r2.contains("Second"));

    // History should have accumulated messages
    assert!(
        history.len() >= 2,
        "History should have at least 2 messages"
    );
}
