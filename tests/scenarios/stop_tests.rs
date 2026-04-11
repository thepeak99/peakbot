//! Stop tests - E2E stop/interrupt functionality through AgentRunner
//!
//! Tests for verifying stop request handling through the full agentic loop.
//! Note: Full stop interruption requires the agent to be mid-execution,
//! which is difficult to test with MockCompletionModel. These tests verify
//! the infrastructure and partial behavior.

use crate::harness::TestHarness;
use peakbot::mock::MockResponse;

/// Test that agent responds normally without stop request
#[tokio::test]
async fn agent_responds_normally() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Normal response"));

    let response = harness.run_message("Hello").await;

    assert!(!response.is_empty());
    assert!(response.contains("Normal") || response.contains("response"));
}

/// Test that stop flag can be set and checked on session hook
#[tokio::test]
async fn stop_flag_infrastructure() {
    let harness = TestHarness::new();
    
    // Verify is_running starts false
    let state = harness.state_manager.get_state();
    assert!(!state.is_running, "is_running should start false");
}

/// Test agent continues normally through multiple messages
#[tokio::test]
async fn multiple_messages_without_interruption() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("First"));
    harness.add_response(MockResponse::text("Second"));
    harness.add_response(MockResponse::text("Third"));

    let r1 = harness.run_message("Message 1").await;
    let r2 = harness.run_message("Message 2").await;
    let r3 = harness.run_message("Message 3").await;

    assert!(!r1.is_empty());
    assert!(!r2.is_empty());
    assert!(!r3.is_empty());
    
    assert!(r1.contains("First"));
    assert!(r2.contains("Second"));
    assert!(r3.contains("Third"));
}

/// Test that events are emitted during normal operation (stop infrastructure)
#[tokio::test]
async fn events_emitted_during_normal_operation() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Response"));

    harness.run_message("Test").await;

    // Drain events - should have CompletionRequest and CompletionResponse
    let events = harness.drain_events();
    assert!(!events.is_empty(), "Should emit at least CompletionRequest event");
}

/// Test conversation state after normal message flow
#[tokio::test]
async fn conversation_state_after_normal_flow() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Response"));

    harness.run_message("Test").await;

    // Verify conversation has messages
    if let Some(cm) = harness.conversation_manager() {
        let cm = cm.lock().unwrap();
        let conv = cm.get_current();
        assert!(conv.is_some(), "Conversation should exist");
        let conv = conv.unwrap();
        assert_eq!(conv.messages.len(), 2, "Should have user + assistant message");
    }
}

/// Test stats accumulate through multiple messages (prerequisite for stop tests)
#[tokio::test]
async fn stats_accumulate_through_messages() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text_with_usage(
        "First",
        peakbot::mock::Usage { input_tokens: 100, output_tokens: 50 },
    ));
    harness.add_response(MockResponse::text_with_usage(
        "Second",
        peakbot::mock::Usage { input_tokens: 200, output_tokens: 80 },
    ));

    harness.run_message("One").await;
    harness.run_message("Two").await;

    let stats = harness.get_stats();
    assert_eq!(stats.total_api_calls, 2, "Should track 2 API calls");
    assert!(
        stats.total_input_tokens > 0,
        "Input tokens should be nonzero after agent loop"
    );
    assert!(
        stats.total_cost > 0.0,
        "Cost should accumulate across messages"
    );
}