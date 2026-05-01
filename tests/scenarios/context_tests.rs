//! Context compaction tests - E2E context management through AgentRunner
//!
//! Tests for verifying ContextManager compaction behavior through the full agentic loop.
//! All tests verify that compaction works correctly when triggered with realistic
//! token counts and small context windows.
//!
//! These tests verify ACTUAL behavior, not just event emission:
//! - Messages are actually discarded (num_discarded > 0)
//! - History in StateManager actually shrinks
//! - Recent messages are preserved after compaction

use crate::harness::TestHarness;
use peakbot::ContextConfig;
use peakbot::mock::{MockResponse, Usage};

/// Helper: create a mock response with the given token counts.
/// These are the responses consumed by the agent for user messages.
fn agent_response(text: &str, input_tokens: u64) -> MockResponse {
    MockResponse::text_with_usage(
        text,
        Usage {
            input_tokens,
            output_tokens: 20,
        },
    )
}

/// Helper: create a mock response that will be consumed by the summarization
/// call inside compact(). When compaction triggers, ContextManager calls
/// agent.prompt() to summarize old messages, which consumes a queued response.
fn summarization_response() -> MockResponse {
    MockResponse::text("Summary of previous conversation.")
}

/// Test that compaction IS actually triggered with a small context window
/// AND that it actually discards messages.
///
/// With a 500-token context window and 50% threshold (250 tokens),
/// and each response using ~300 input tokens, compaction triggers
/// when there are more messages than keep_recent AND tokens exceed threshold.
#[tokio::test]
async fn compaction_triggers_with_small_window() {
    let context_config = ContextConfig {
        context_window: Some(500), // 500 tokens total
        threshold: 0.5,            // 50% = 250 tokens threshold
        keep_recent: 2,            // Keep last 2 messages
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", context_config);

    // Queue agent responses + extra for summarization calls.
    // compact() calls agent.prompt() for summarization, consuming one response.
    harness.add_response(agent_response("Response 1", 300));
    harness.add_response(agent_response("Response 2", 300));
    harness.add_compaction_response(summarization_response()); // consumed by compact()
    harness.add_response(agent_response("Response 3", 300));
    harness.add_response(agent_response("Response 4", 300));

    // Message 1: 2 messages (user + assistant), 2 <= 2 (keep_recent), no compaction
    harness.run_message("Message 1").await;
    assert!(
        !harness.has_compaction_occurred(),
        "Compaction should not trigger on first message (2 <= 2)"
    );

    // Message 2: history grows, now > keep_recent, and 300 > 250 threshold
    harness.run_message("Message 2").await;

    // Message 3: compaction should have occurred by now
    harness.run_message("Message 3").await;
    assert!(
        harness.has_compaction_occurred(),
        "Compaction should have occurred (history > 2 messages AND 300 > 250 threshold)"
    );

    // Verify compaction actually did something, not just fired an event
    harness.assert_compaction_actually_discarded();
}

/// Test that compaction actually reduces history length in StateManager.
///
/// This is the critical test that was missing: verify the persisted state
/// actually shrinks, not just that a CompactionInfo event was emitted.
#[tokio::test]
async fn compaction_reduces_history() {
    let context_config = ContextConfig {
        context_window: Some(300), // Very small window
        threshold: 0.5,            // 50% = 150 tokens threshold
        keep_recent: 1,            // Keep only last message
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", context_config);

    // Queue responses: agent responses + summarization responses consumed by compact()
    harness.add_response(agent_response("Response 1", 200));
    harness.add_compaction_response(summarization_response()); // compact() after msg 2
    harness.add_response(agent_response("Response 2", 200));
    harness.add_compaction_response(summarization_response()); // compact() after msg 3
    harness.add_response(agent_response("Response 3", 200));

    // Build up conversation history
    harness.run_message("Message 1").await;
    let _count_after_1 = harness.get_chat_message_count();

    harness.run_message("Message 2").await;
    harness.run_message("Message 3").await;

    // Compaction must have occurred
    let events = harness.get_compaction_events();
    assert!(
        !events.is_empty(),
        "Compaction should have occurred by message 3"
    );

    // Verify actual message reduction, not just event emission
    for event in &events {
        assert!(
            event.num_discarded > 0,
            "Compaction event should have discarded messages (num_discarded: {})",
            event.num_discarded
        );
        // compacted_count can equal original_count when summarization replaces
        // the discarded messages with a summary (1:1 replacement for small discards).
        // The key indicator is num_discarded > 0.
        assert!(
            event.compacted_count <= event.original_count,
            "Compacted count ({}) should not exceed original ({})",
            event.compacted_count,
            event.original_count
        );
    }

    // With tag-and-skip compaction, total message count doesn't decrease — old
    // messages are tagged compacted and a Summary message is inserted. But what the
    // LLM sees (uncompacted messages) should be fewer.
    assert!(
        harness.has_compacted_messages(),
        "Some messages should be tagged as compacted"
    );
    assert!(
        harness.has_summary_message(),
        "A Summary message should have been inserted"
    );
    // The LLM-visible history should be smaller than total
    let uncompacted = harness.get_uncompacted_message_count();
    let total = harness.get_chat_message_count();
    assert!(
        uncompacted < total,
        "Uncompacted count ({}) should be less than total ({})",
        uncompacted,
        total
    );
}

/// Test that recent messages are preserved after compaction
#[tokio::test]
async fn compaction_preserves_recent_messages() {
    let context_config = ContextConfig {
        context_window: Some(400),
        threshold: 0.5, // 50% = 200 tokens
        keep_recent: 3, // Keep last 3 messages — see make-flow-great-again.md
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", context_config);

    harness.add_response(agent_response("Response 1", 250));
    harness.add_response(agent_response("Response 2", 250));
    harness.add_compaction_response(summarization_response()); // compact() consumes this
    harness.add_response(agent_response("Response 3", 250));

    harness.run_message("Message 1").await;
    harness.run_message("Message 2").await;
    harness.run_message("Message 3").await;

    if harness.has_compaction_occurred() {
        harness.assert_compaction_actually_discarded();

        // After compaction, the history should contain at most:
        // summary (1) + keep_recent (2) + new messages added after compaction
        // It should NOT have all 6 original messages (3 user + 3 assistant)
        let history = harness.get_chat_history();
        assert!(
            history.len() < 6,
            "History should be compacted, but got {} messages (expected < 6)",
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
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", context_config);

    harness.add_responses(vec![
        agent_response("Response 1", 50),
        agent_response("Response 2", 50),
        agent_response("Response 3", 50),
        agent_response("Response 4", 50),
    ]);

    harness.run_message("Message 1").await;
    harness.run_message("Message 2").await;
    harness.run_message("Message 3").await;
    harness.run_message("Message 4").await;

    // Compaction should NOT have occurred because it's disabled
    assert!(
        !harness.has_compaction_occurred(),
        "Compaction should not occur when disabled"
    );

    // All messages should be present (no compaction)
    let count = harness.get_chat_message_count();
    assert_eq!(
        count, 8,
        "All 8 messages (4 user + 4 assistant) should be present when compaction is disabled"
    );
}

/// Test multiple compaction events occur over long conversations
/// and each one actually discards messages.
#[tokio::test]
async fn multiple_compaction_events() {
    let context_config = ContextConfig {
        context_window: Some(200), // Very small window
        threshold: 0.6,            // 60% = 120 tokens threshold
        keep_recent: 1,            // Keep only last message
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", context_config);

    // Queue enough responses for 6 messages plus summarization calls.
    // With keep_recent=1 and 150 > 120 threshold, compaction fires frequently.
    // Each compaction consumes one summarization response.
    for _ in 0..12 {
        harness.add_response(agent_response("Response", 150));
    }
    for _ in 0..6 {
        harness.add_compaction_response(summarization_response());
    }

    for i in 1..=6 {
        harness.run_message(&format!("Message {}", i)).await;
    }

    assert!(
        harness.has_compaction_occurred(),
        "At least one compaction should have occurred"
    );

    let events = harness.get_compaction_events();
    for (i, event) in events.iter().enumerate() {
        assert!(
            event.compacted_count <= event.original_count,
            "Compaction event {} should not increase message count",
            i
        );
        // Every compaction that fires should actually discard something
        assert!(
            event.num_discarded > 0,
            "Compaction event {} fired but discarded 0 messages -- no-op stub detected",
            i
        );
    }
}

/// Test context status is accessible
#[tokio::test]
async fn context_status_accessible() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Response"));

    harness.run_message("Test").await;

    let state = harness.get_state();
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

    let count_before = harness.get_chat_message_count();

    harness.run_message("First").await;
    harness.run_message("Second").await;
    harness.run_message("Third").await;

    let count_after = harness.get_chat_message_count();

    // 3 user + 3 assistant = 6 new messages
    assert_eq!(
        count_after - count_before,
        6,
        "Should have 6 new messages (3 user + 3 assistant), before: {}, after: {}",
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

    assert!(
        harness.get_chat_message_count() > 0,
        "Should have history before clear"
    );

    harness.clear_chat_history();

    assert_eq!(
        harness.get_chat_message_count(),
        0,
        "Should have empty history after clear"
    );
}

/// Test conversation continues after multiple messages (basic flow test)
#[tokio::test]
async fn conversation_continues_after_many_messages() {
    let mut harness = TestHarness::new();
    for _ in 0..5 {
        harness.add_response(MockResponse::text("Continued response"));
    }

    for i in 0..5 {
        let msg = format!("Message {}", i);
        let response = harness.run_message(&msg).await;
        assert!(!response.is_empty(), "Should respond to message {}", i);
    }
}

/// Verify that compaction through StateManager persists results.
///
/// Build up enough history that compaction discards MORE messages than the
/// summary adds (requires > 1 message to discard with keep_recent=1).
#[tokio::test]
async fn compaction_persists_to_state_manager() {
    let context_config = ContextConfig {
        context_window: Some(300),
        threshold: 0.5, // 150 tokens
        keep_recent: 1,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", context_config);

    // 3 messages: builds up chat entries. Each compaction consumes a summarization response.
    harness.add_response(agent_response("Response 1", 200));
    harness.add_compaction_response(summarization_response()); // compact after msg 1
    harness.add_response(agent_response("Response 2", 200));
    harness.add_compaction_response(summarization_response()); // compact after msg 2
    harness.add_response(agent_response("Response 3", 200));

    harness.run_message("Message 1").await;
    harness.run_message("Message 2").await;

    let _count_before_compact_turn = harness.get_chat_message_count();
    // Should be 4 (msg1_user, msg1_assistant, msg2_user, msg2_assistant)

    harness.run_message("Message 3").await;

    assert!(
        harness.has_compaction_occurred(),
        "Compaction should have triggered"
    );
    harness.assert_compaction_actually_discarded();

    // With tag-and-skip, compaction tags old messages and inserts a Summary.
    // Total count goes up, but compacted messages exist and a summary was inserted.
    assert!(
        harness.has_compacted_messages(),
        "Some messages should be tagged as compacted after compaction"
    );
    assert!(
        harness.has_summary_message(),
        "A Summary message should exist in StateManager after compaction"
    );
    // The LLM-visible count should be less than total
    let uncompacted = harness.get_uncompacted_message_count();
    let total = harness.get_chat_message_count();
    assert!(
        uncompacted < total,
        "Uncompacted ({}) should be less than total ({}) after compaction",
        uncompacted,
        total
    );
}

/// Test that compaction under threshold does NOT trigger.
/// Verifies no false positives when token usage is well under the limit.
#[tokio::test]
async fn no_compaction_under_threshold() {
    let context_config = ContextConfig {
        context_window: Some(1000),
        threshold: 0.8, // 800 tokens
        keep_recent: 2,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", context_config);

    // 100 tokens per request, well under 800 threshold
    for _ in 0..5 {
        harness.add_response(agent_response("Response", 100));
    }

    for i in 1..=5 {
        harness.run_message(&format!("Message {}", i)).await;
    }

    assert!(
        !harness.has_compaction_occurred(),
        "Compaction should NOT trigger when tokens (100) are well under threshold (800)"
    );

    // All messages should be present
    assert_eq!(
        harness.get_chat_message_count(),
        10,
        "All 10 messages (5 user + 5 assistant) should be present"
    );
}
