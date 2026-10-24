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
    assert!(
        !events.is_empty(),
        "Should emit at least CompletionRequest event"
    );
}

/// Test conversation state after normal message flow
#[tokio::test]
async fn conversation_state_after_normal_flow() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Response"));

    harness.run_message("Test").await;

    // Verify conversation has messages via StateManager
    let state = harness.get_state();
    assert_eq!(
        state.chat.messages.len(),
        2,
        "Should have user + assistant message"
    );
}

/// Test stats accumulate through multiple messages (prerequisite for stop tests)
#[tokio::test]
async fn stats_accumulate_through_messages() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text_with_usage(
        "First",
        peakbot::mock::Usage {
            input_tokens: 100,
            output_tokens: 50,
        },
    ));
    harness.add_response(MockResponse::text_with_usage(
        "Second",
        peakbot::mock::Usage {
            input_tokens: 200,
            output_tokens: 80,
        },
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

// ── Two-stage Stop (TDD: written before the implementation) ────────────────
//
// Scenario-level coverage through the public `TestHarness` (real agent loop,
// mocked LLM). The per-press state machine itself is pinned at the unit level
// (`src/state/state_manager.rs::two_stage_stop_tests`) and the full graceful /
// escalation e2e is pinned at the lib level (`src/lib.rs::tests::two_stage_stop`,
// which drives the real `AgentRunner::run_loop` + `process_message_internal` —
// those are crate-private, so the production loop can't be reached from this
// integration crate).

use peakbot::ui::app_state::MessageRole;

/// A normal, unstopped turn leaves no stop residue: `stop_pending` is false
/// before and after, and the next turn's first press is graceful again from a
/// clean slate.
#[tokio::test]
async fn stop_flag_resets_between_turns() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("first answer"));

    // Idle: pressing is a no-op and arms nothing.
    assert_eq!(
        harness.state_manager.request_stop(),
        peakbot::state::StopStep::Noop,
        "idle press must be a no-op"
    );
    assert!(!harness.state_manager.stop_pending());
    assert!(!harness.get_state().stop_pending);

    // A normal turn.
    let r1 = harness.run_message("one").await;
    assert!(r1.contains("first answer"));
    assert!(
        !harness.state_manager.stop_pending(),
        "no stop was pressed: the flag must stay false after a normal turn"
    );
    assert!(!harness.get_state().stop_pending);

    // A second turn runs cleanly — nothing stale leaked from the first.
    harness.add_response(MockResponse::text("second answer"));
    let r2 = harness.run_message("two").await;
    assert!(r2.contains("second answer"));
    assert!(!harness.state_manager.stop_pending());
}

/// E2E graceful stop: the in-flight tool finishes with its REAL result and the
/// turn ends before the next LLM call.
///
/// `#[ignore]`d for the harness reason in the module header, NOT because the
/// behaviour is untestable: the faithful version drives the production
/// `AgentRunner::process_message_internal`, which is crate-private. The public
/// `TestHarness`/`TestRunner` have no mid-tool stop/cancel seam (their loop has
/// no cancellation token), so a press here cannot end the turn before the next
/// LLM call and the test would fail even once the feature is implemented. The
/// real e2e is covered at the lib level (`src/lib.rs::tests::two_stage_stop`).
/// Enable this once a public mid-turn stop seam exists; the body below is
/// written against the public API so it only needs the seam, not a rewrite.
#[tokio::test]
#[ignore = "two-stage stop: enable after impl (needs a public mid-turn stop seam; e2e is covered at the lib level)"]
async fn graceful_stop_end_to_end() {
    let mut harness = TestHarness::new();
    // First reply requests a real tool (finishes in ms); the second reply is
    // only reachable if the loop wrongly continues after the graceful stop.
    harness.add_response(MockResponse::tool_call(
        "think",
        serde_json::json!({ "thought": "in flight" }),
    ));
    harness.add_response(MockResponse::text("SHOULD NOT BE SENT"));

    let sm = harness.state_manager.clone();
    let model = harness.mock_model.clone();

    // Drive the turn in a task so the test can press Stop mid-tool.
    let turn = tokio::spawn(async move { harness.run_message("go").await });

    // Wait for the tool to be in flight (its call row is the tail).
    let call_id = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let msgs = sm.get_state().chat.messages;
            if let Some(tail) = msgs.last()
                && tail.role == MessageRole::ToolCall
                && tail.tool_name.as_deref() == Some("think")
            {
                return tail.call_id.clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the think ToolCall row must land within 10s");

    // The 1st press: graceful.
    assert_eq!(sm.request_stop(), peakbot::state::StopStep::Graceful);

    let _resp = tokio::time::timeout(std::time::Duration::from_secs(10), turn)
        .await
        .expect("the turn must end after the in-flight tool finishes")
        .expect("the turn task must not panic");

    // The in-flight call recorded its REAL result, not INTERRUPTED.
    let msgs = sm.get_state().chat.messages;
    let tool_result = msgs
        .iter()
        .find(|m| m.role == MessageRole::ToolResult && m.call_id == call_id)
        .expect("the in-flight tool call must be answered");
    assert!(
        !tool_result
            .tool_result
            .as_deref()
            .unwrap_or_default()
            .contains("INTERRUPTED"),
        "graceful stop records the REAL result, not INTERRUPTED: {tool_result:?}"
    );
    // …and the turn ended BEFORE the next LLM call.
    assert_eq!(
        model.request_count(),
        1,
        "the turn must end before the 2nd completion request"
    );
    assert!(
        !sm.stop_pending(),
        "stop_pending must be clear after the turn ends"
    );
    assert_eq!(sm.request_stop(), peakbot::state::StopStep::Noop);
}
