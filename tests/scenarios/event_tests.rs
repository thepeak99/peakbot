//! Event tests - E2E event emission verification
//!
//! Tests for verifying SessionHook event emission through the full agentic loop.
//! All tests verify that events are correctly emitted and contain expected data.

use crate::harness::TestHarness;
use peakbot::AgentEvent;
use peakbot::mock::{MockResponse, Usage};

/// Test that CompletionResponse event is emitted after agent response
#[tokio::test]
async fn completion_response_event_emitted() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Hello! How can I help you?"));

    let response = harness.run_message("Hello").await;

    assert!(!response.is_empty());

    // Drain all events and verify CompletionResponse was emitted
    let events = harness.drain_events();
    let has_completion = events
        .iter()
        .any(|e| matches!(e, AgentEvent::CompletionResponse { .. }));
    assert!(has_completion, "Should emit CompletionResponse event");
}

/// Test that ToolCall event is emitted when model requests a tool
#[tokio::test]
async fn tool_call_event_emitted() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::tool_call(
        "todo",
        serde_json::json!({
            "action": "add",
            "tasks": ["Test task"]
        }),
    ));
    harness.add_response(MockResponse::text("Task added."));

    harness.run_message("Add a todo: Test task").await;

    // Drain all events and verify ToolCall was emitted
    let events = harness.drain_events();
    let has_tool_call = events.iter().any(|e| match e {
        AgentEvent::ToolCall { tool_name, .. } => tool_name == "todo",
        _ => false,
    });
    assert!(has_tool_call, "Should emit ToolCall event for 'todo' tool");
}

/// Test that ToolResult event is emitted after tool execution
#[tokio::test]
async fn tool_result_event_emitted() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::tool_call(
        "todo",
        serde_json::json!({
            "action": "add",
            "tasks": ["Test task"]
        }),
    ));
    harness.add_response(MockResponse::text("Task added."));

    harness.run_message("Add a todo: Test task").await;

    // Drain all events and verify ToolResult was emitted
    let events = harness.drain_events();
    let has_tool_result = events.iter().any(|e| match e {
        AgentEvent::ToolResult {
            tool_name, success, ..
        } => tool_name == "todo" && *success,
        _ => false,
    });
    assert!(
        has_tool_result,
        "Should emit ToolResult event with success=true"
    );
}

/// Test that stats accumulate from events (token counting)
#[tokio::test]
async fn stats_accumulate_from_events() {
    let mut harness = TestHarness::new();

    // Add response with specific token usage
    let usage = Usage {
        input_tokens: 100,
        output_tokens: 50,
    };
    harness.add_response(MockResponse::text_with_usage("Response text", usage));

    harness.run_message("Test message").await;

    // Drain events and verify token usage is tracked
    let events = harness.drain_events();

    // Find CompletionResponse event with usage
    let completion_event = events.iter().find_map(|e| match e {
        AgentEvent::CompletionResponse { usage, .. } => Some(usage),
        _ => None,
    });

    assert!(
        completion_event.is_some(),
        "Should have CompletionResponse with usage"
    );
    let usage = completion_event.unwrap();
    assert_eq!(usage.input_tokens, 100, "Input tokens should be tracked");
    assert_eq!(usage.output_tokens, 50, "Output tokens should be tracked");
}

/// Test multiple events are emitted in correct order
#[tokio::test]
async fn events_emitted_in_order() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Response"));

    harness.run_message("Test").await;

    let events = harness.drain_events();

    // Verify we have events
    assert!(!events.is_empty(), "Should have emitted at least one event");

    // If we have both request and response, response should come after request
    let request_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::CompletionRequest { .. }));
    let response_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::CompletionResponse { .. }));

    if let (Some(req_idx), Some(resp_idx)) = (request_idx, response_idx) {
        assert!(
            resp_idx > req_idx,
            "CompletionResponse should come after CompletionRequest"
        );
    }
}
