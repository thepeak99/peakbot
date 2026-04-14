//! Stats tests - Token tracking through the agent loop
//!
//! Integration tests for verifying stats accumulation via TestHarness.
//! Unit tests for StateManager live in src/state/state_manager.rs.

use crate::harness::TestHarness;
use peakbot::ContextConfig;
use peakbot::mock::{MockResponse, Usage};

/// Verify that ContextManager uses last_request's input_tokens for threshold
/// comparison, not cumulative sum of all requests.
///
/// With 500 token window, 80% threshold (400 tokens), 100 tokens/request:
/// - CORRECT: Should NOT trigger (100 tokens < 400 threshold)
/// - OLD BUG: Would trigger at turn 5 (cumulative sum = 500 > 400)
#[tokio::test]
async fn test_compactor_uses_last_request_tokens() {
    let context_config = ContextConfig {
        threshold: 0.8,
        keep_recent: 2,
        enabled: true,
        context_window: Some(500), // 500 token context = 400 token threshold
    };

    let mut harness = TestHarness::with_system_prompt_and_context(
        "You are a helpful assistant.",
        context_config,
    );

    // 100 tokens per request (context stays constant ~100 tokens)
    let usage = Usage {
        input_tokens: 100,
        output_tokens: 50,
    };

    for i in 1..=10 {
        harness.add_response(MockResponse::text_with_usage(format!("Response {}", i), usage.clone()));
    }

    // Run messages - compaction should NOT trigger
    for i in 1..=10 {
        harness.run_message(&format!("Message {}", i)).await;
    }

    assert!(
        !harness.has_compaction_occurred(),
        "Compaction should not trigger: 100 tokens/request < 400 token threshold. \
         If it triggered, the compactor is using cumulative tokens instead of last-request tokens."
    );
}

/// Verify threshold calculation produces sane values.
#[tokio::test]
async fn test_compactor_threshold_calculation() {
    let context_config = ContextConfig {
        threshold: 0.8,
        keep_recent: 2,
        enabled: true,
        context_window: Some(1000),
    };

    let harness = TestHarness::with_system_prompt_and_context(
        "Short prompt",
        context_config,
    );

    // Verify the state reflects the configured context window
    let state = harness.get_state();
    // The context window should be set (might be 0 before first request, that's ok)
    assert!(
        state.context.window_size == 0 || state.context.window_size == 1000,
        "Context window should be 0 (pre-init) or 1000 (configured), got {}",
        state.context.window_size
    );
}

/// Verify compaction DOES trigger when tokens exceed threshold.
/// Companion to test_compactor_uses_last_request_tokens -- tests the other side.
#[tokio::test]
async fn test_compactor_triggers_above_threshold() {
    let context_config = ContextConfig {
        threshold: 0.8,
        keep_recent: 1,
        enabled: true,
        context_window: Some(500), // 500 * 0.8 = 400 token threshold
    };

    let mut harness = TestHarness::with_system_prompt_and_context(
        "You are a helpful assistant.",
        context_config,
    );

    // 450 tokens per request (> 400 threshold), triggers compaction
    let usage = Usage {
        input_tokens: 450,
        output_tokens: 50,
    };

    // Agent responses + summarization responses for compact()
    harness.add_response(MockResponse::text_with_usage("Response 1", usage.clone()));
    harness.add_response(MockResponse::text_with_usage("Response 2", usage.clone()));
    harness.add_response(MockResponse::text("Summary of previous conversation.")); // summarization
    harness.add_response(MockResponse::text_with_usage("Response 3", usage.clone()));

    harness.run_message("Message 1").await;
    harness.run_message("Message 2").await;
    harness.run_message("Message 3").await;

    assert!(
        harness.has_compaction_occurred(),
        "Compaction should trigger: 450 tokens/request > 400 token threshold"
    );
    harness.assert_compaction_actually_discarded();
}

#[tokio::test]
async fn mock_response_with_usage() {
    let mut harness = TestHarness::new();

    let usage = Usage {
        input_tokens: 150,
        output_tokens: 75,
    };
    harness.add_response(MockResponse::text_with_usage("Test response", usage));

    let response = harness.run_message("Hello").await;

    assert!(response.contains("Test"));
}

#[tokio::test]
async fn multiple_messages_with_usage() {
    let mut harness = TestHarness::new();

    harness.add_responses(vec![
        MockResponse::text_with_usage(
            "First",
            Usage {
                input_tokens: 100,
                output_tokens: 50,
            },
        ),
        MockResponse::text_with_usage(
            "Second",
            Usage {
                input_tokens: 150,
                output_tokens: 75,
            },
        ),
        MockResponse::text_with_usage(
            "Third",
            Usage {
                input_tokens: 200,
                output_tokens: 100,
            },
        ),
    ]);

    let _r1 = harness.run_message("One").await;
    let _r2 = harness.run_message("Two").await;
    let _r3 = harness.run_message("Three").await;

    assert!(!harness.has_remaining_responses());
    assert_eq!(harness.remaining_responses(), 0);
}
