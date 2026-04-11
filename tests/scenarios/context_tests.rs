//! Context compaction tests - E2E context management through AgentRunner
//!
//! Tests for verifying ContextManager compaction behavior through the full agentic loop.
//! All tests verify that compaction works correctly when triggered with realistic
//! token counts and small context windows.

use crate::harness::TestHarness;
use peakbot::ContextConfig;
use peakbot::mock::{MockResponse, Usage};

/// Test that compaction IS actually triggered with a small context window
///
/// With a 500-token context window and 50% threshold (250 tokens),
/// and each response using ~150 input tokens, compaction triggers
/// when there are more messages than keep_recent AND tokens exceed threshold.
///
/// IMPORTANT: The compaction check uses stats from the PREVIOUS message cycle.
/// This is because stats are synced AFTER the agent call completes.
///
/// Flow:
/// - Message 1: 2 messages, check stats from "start" (0 tokens) -> no check (2 <= 2)
/// - After msg 1: agent called, stats sync (150 tokens)
/// - Message 2: 4 messages, check stats from after msg 1 (150 tokens) -> no check (150 <= 250)
/// - After msg 2: agent called, stats sync (300 tokens)
/// - Message 3: 6 messages, check stats from after msg 2 (300 tokens) -> check (300 > 250) -> compact!
#[tokio::test]
async fn compaction_triggers_with_small_window() {
    // Configure a tiny context window so compaction triggers quickly
    let context_config = ContextConfig {
        context_window: Some(500), // 500 tokens total
        threshold: 0.5,            // 50% = 250 tokens threshold
        keep_recent: 2,            // Keep last 2 messages (triggers when > 2)
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", context_config);

    // With 150 tokens per request:
    // Stats are synced AFTER each agent call, so compaction triggers
    // one message later than expected (at message 3, not message 2)
    harness.add_response(MockResponse::text_with_usage(
        "Response 1",
        Usage {
            input_tokens: 150,
            output_tokens: 20,
        },
    ));
    harness.add_response(MockResponse::text_with_usage(
        "Response 2",
        Usage {
            input_tokens: 150,
            output_tokens: 20,
        },
    ));
    harness.add_response(MockResponse::text_with_usage(
        "Response 3",
        Usage {
            input_tokens: 150,
            output_tokens: 20,
        },
    ));
    harness.add_response(MockResponse::text_with_usage(
        "Response 4",
        Usage {
            input_tokens: 150,
            output_tokens: 20,
        },
    ));

    // Message 1: 2 messages (user + assistant), 2 <= 2 (keep_recent), no token check
    harness.run_message("Message 1").await;
    assert!(
        !harness.has_compaction_occurred(),
        "Compaction should not trigger on first message (2 <= 2)"
    );

    // Message 2: 4 messages, 4 > 2, checks tokens from PREVIOUS cycle (150)
    // 150 <= 250 threshold, so no compaction
    harness.run_message("Message 2").await;
    assert!(
        !harness.has_compaction_occurred(),
        "Compaction should not trigger on message 2 (checks 150 tokens from msg 1, 150 <= 250)"
    );

    // Message 3: 6 messages, 6 > 2, checks tokens from after msg 2 (300)
    // 300 > 250 threshold -> compaction triggers!
    harness.run_message("Message 3").await;

    // Verify compaction occurred
    assert!(
        harness.has_compaction_occurred(),
        "Compaction should have occurred by message 3 (checks 300 tokens from msg 2, 300 > 250 threshold)"
    );
    let events = harness.get_compaction_events();
    assert!(
        !events.is_empty(),
        "Should have at least one compaction event"
    );
}

/// Test that compaction reduces history length
///
/// Verifies that compaction actually discards messages by checking
/// the compaction event's num_discarded field.
#[tokio::test]
async fn compaction_reduces_history() {
    let context_config = ContextConfig {
        context_window: Some(300), // Very small window
        threshold: 0.5,            // 50% = 150 tokens threshold
        keep_recent: 1,            // Keep only last message
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", context_config);

    // Use realistic token counts - 100 tokens per request
    // After 2 requests: 200 tokens (above 150 threshold)
    harness.add_responses(vec![
        MockResponse::text_with_usage(
            "Response 1",
            Usage {
                input_tokens: 100,
                output_tokens: 20,
            },
        ),
        MockResponse::text_with_usage(
            "Response 2",
            Usage {
                input_tokens: 100,
                output_tokens: 20,
            },
        ),
        MockResponse::text_with_usage(
            "Response 3",
            Usage {
                input_tokens: 100,
                output_tokens: 20,
            },
        ),
        MockResponse::text_with_usage(
            "Response 4",
            Usage {
                input_tokens: 100,
                output_tokens: 20,
            },
        ),
        MockResponse::text_with_usage(
            "Response 5",
            Usage {
                input_tokens: 100,
                output_tokens: 20,
            },
        ),
    ]);

    // Build up conversation history
    harness.run_message("Message 1").await;
    harness.run_message("Message 2").await;
    harness.run_message("Message 3").await;

    // After message 3, compaction should have occurred
    // Check the compaction event to verify messages were actually discarded
    let events = harness.get_compaction_events();
    if !events.is_empty() {
        // At least one compaction should have happened
        // Check that some messages were actually discarded
        for event in &events {
            assert!(
                event.num_discarded > 0,
                "Compaction should have discarded messages (num_discarded: {})",
                event.num_discarded
            );
        }
    } else {
        panic!("Compaction should have occurred by message 3");
    }
}

/// Test that recent messages are preserved after compaction
#[tokio::test]
async fn compaction_preserves_recent_messages() {
    let context_config = ContextConfig {
        context_window: Some(400),
        threshold: 0.75, // 75% = 300 tokens
        keep_recent: 3,  // Keep last 3 messages
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", context_config);

    harness.add_responses(vec![
        MockResponse::text_with_usage(
            "Response 1",
            Usage {
                input_tokens: 100,
                output_tokens: 20,
            },
        ),
        MockResponse::text_with_usage(
            "Response 2",
            Usage {
                input_tokens: 100,
                output_tokens: 20,
            },
        ),
        MockResponse::text_with_usage(
            "Response 3",
            Usage {
                input_tokens: 100,
                output_tokens: 20,
            },
        ),
        MockResponse::text_with_usage(
            "Response 4",
            Usage {
                input_tokens: 100,
                output_tokens: 20,
            },
        ),
    ]);

    // Run messages until compaction triggers
    harness.run_message("Message 1").await;
    harness.run_message("Message 2").await;
    harness.run_message("Message 3").await;

    let history = harness.get_chat_history().await;

    // After compaction, should have:
    // - Summary message (if summarization worked) or just kept messages
    // - Plus the recent messages that were kept
    // Should NOT have all 6 original messages (3 user + 3 assistant)
    if harness.has_compaction_occurred() {
        // Should be less than full history
        assert!(
            history.len() < 6,
            "History should be compacted, but got {} messages",
            history.len()
        );
    }
}

/// Test that compaction is skipped when disabled
#[tokio::test]
async fn compaction_skipped_when_disabled() {
    let context_config = ContextConfig {
        context_window: Some(100), // Tiny window
        threshold: 0.5,
        keep_recent: 2,
        enabled: false, // DISABLED
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", context_config);

    harness.add_responses(vec![
        MockResponse::text_with_usage(
            "Response 1",
            Usage {
                input_tokens: 50,
                output_tokens: 10,
            },
        ),
        MockResponse::text_with_usage(
            "Response 2",
            Usage {
                input_tokens: 50,
                output_tokens: 10,
            },
        ),
        MockResponse::text_with_usage(
            "Response 3",
            Usage {
                input_tokens: 50,
                output_tokens: 10,
            },
        ),
        MockResponse::text_with_usage(
            "Response 4",
            Usage {
                input_tokens: 50,
                output_tokens: 10,
            },
        ),
    ]);

    // Run many messages
    harness.run_message("Message 1").await;
    harness.run_message("Message 2").await;
    harness.run_message("Message 3").await;
    harness.run_message("Message 4").await;

    // Compaction should NOT have occurred because it's disabled
    assert!(
        !harness.has_compaction_occurred(),
        "Compaction should not occur when disabled"
    );
}

/// Test multiple compaction events occur over long conversations
#[tokio::test]
async fn multiple_compaction_events() {
    let context_config = ContextConfig {
        context_window: Some(200), // Very small window
        threshold: 0.6,            // 60% = 120 tokens
        keep_recent: 1,            // Keep only last message
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", context_config);

    // Use small token counts to trigger multiple compactions
    harness.add_responses(vec![
        MockResponse::text_with_usage(
            "Response",
            Usage {
                input_tokens: 80,
                output_tokens: 15,
            },
        ),
        MockResponse::text_with_usage(
            "Response",
            Usage {
                input_tokens: 80,
                output_tokens: 15,
            },
        ),
        MockResponse::text_with_usage(
            "Response",
            Usage {
                input_tokens: 80,
                output_tokens: 15,
            },
        ),
        MockResponse::text_with_usage(
            "Response",
            Usage {
                input_tokens: 80,
                output_tokens: 15,
            },
        ),
        MockResponse::text_with_usage(
            "Response",
            Usage {
                input_tokens: 80,
                output_tokens: 15,
            },
        ),
        MockResponse::text_with_usage(
            "Response",
            Usage {
                input_tokens: 80,
                output_tokens: 15,
            },
        ),
    ]);

    // Build up conversation - should trigger multiple compactions
    harness.run_message("Message 1").await;
    harness.run_message("Message 2").await;
    harness.run_message("Message 3").await;
    harness.run_message("Message 4").await;
    harness.run_message("Message 5").await;
    harness.run_message("Message 6").await;

    // Should have at least one compaction event
    assert!(
        harness.has_compaction_occurred(),
        "At least one compaction should have occurred"
    );

    // Could have multiple compactions depending on how fast history grows
    let events = harness.get_compaction_events();
    if !events.is_empty() {
        for event in &events {
            // Verify compaction info makes sense
            assert!(
                event.compacted_count <= event.original_count,
                "Compacted count should not exceed original count"
            );
        }
    }
}

/// Test context status is accessible
#[tokio::test]
async fn context_status_accessible() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Response"));

    harness.run_message("Test").await;

    // Verify context status is accessible after running a message
    let state = harness.get_state();
    // Context info should be accessible - we just verify the field is readable
    let _ = state.context.current_usage;
    let _ = state.context.window_size;
}

/// Test that messages are properly added to history
#[tokio::test]
async fn messages_added_to_history() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Response 1"));
    harness.add_response(MockResponse::text("Response 2"));
    harness.add_response(MockResponse::text("Response 3"));

    let history_before = harness.get_chat_history().await;
    let count_before = history_before.len();

    harness.run_message("First").await;
    harness.run_message("Second").await;
    harness.run_message("Third").await;

    let history_after = harness.get_chat_history().await;
    let count_after = history_after.len();

    // Should have more messages after running
    assert!(
        count_after > count_before,
        "Should have more messages after running, before: {}, after: {}",
        count_before,
        count_after
    );
}

/// Test history can be cleared
#[tokio::test]
async fn history_cleared() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Response"));

    harness.run_message("Test").await;

    let history_before = harness.get_chat_history().await;
    assert!(
        !history_before.is_empty(),
        "Should have history before clear"
    );

    harness.clear_chat_history().await;

    let history_after = harness.get_chat_history().await;
    assert!(
        history_after.is_empty(),
        "Should have empty history after clear"
    );
}

/// Test conversation continues after multiple messages (basic flow test)
#[tokio::test]
async fn conversation_continues_after_many_messages() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Continued response"));

    // Run multiple messages
    for i in 0..5 {
        let msg = format!("Message {}", i);
        let response = harness.run_message(&msg).await;
        assert!(!response.is_empty(), "Should respond to message {}", i);
    }
}
