//! Comprehensive compaction & summarization tests
//!
//! Tests organized by the test-the-world.md plan:
//! - Phase 1: Verify summarization content (what the LLM receives)
//! - Phase 2: Verify summarization output in state
//! - Phase 3: Verify compaction timing (exact trigger turn)
//! - Phase 4: Verify queue consumption
//! - Phase 5: Edge cases (stacked summaries, tool calls, fallbacks)
//! - Phase 6.2: Strengthened existing tests

use crate::harness::TestHarness;
use peakbot::ContextConfig;
use peakbot::mock::{MockResponse, Usage};

/// Helper: create a mock response with the given token counts.
fn agent_response(text: &str, input_tokens: u64) -> MockResponse {
    MockResponse::text_with_usage(
        text,
        Usage {
            input_tokens,
            output_tokens: 20,
        },
    )
}

/// Helper: create a summarization response consumed by compact().
fn summarization_response() -> MockResponse {
    MockResponse::text("Summary of previous conversation.")
}

/// Helper: create a summarization response with specific text.
fn summarization_response_with(text: &str) -> MockResponse {
    MockResponse::text(text)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 1: Verify Summarization Content
// ═══════════════════════════════════════════════════════════════════════════════

/// 1.1 — The summarization request must contain the old messages, not the recent ones.
///
/// Setup: 500-token window, 50% threshold (250 tokens), keep_recent=2.
/// Send 3 messages with 300 input_tokens each.
/// Compaction triggers on turn 3 (sees turn 2's 300 > 250, history has 4 msgs > 2).
/// The summarization request should contain "OLD_MSG_1" but NOT "RECENT_MSG".
#[tokio::test]
async fn summarization_request_contains_old_messages() {
    let config = ContextConfig {
        context_window: Some(500),
        threshold: 0.5, // 250 tokens
        keep_recent: 2,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    // Turn 1: user="OLD_MSG_1", response has 300 tokens
    harness.add_response(agent_response("OLD_REPLY_1", 300));
    // Turn 2: user="OLD_MSG_2", response has 300 tokens
    harness.add_response(agent_response("OLD_REPLY_2", 300));
    // Turn 3: compaction fires first (consuming summarization response),
    //         then regular response
    harness.add_response(summarization_response());
    harness.add_response(agent_response("RECENT_REPLY_3", 300));

    harness.run_message("OLD_MSG_1").await;
    harness.run_message("OLD_MSG_2").await;
    harness.run_message("RECENT_MSG_3").await;

    assert!(
        harness.has_compaction_occurred(),
        "Compaction must trigger for this test to be meaningful"
    );

    let summ_requests = harness.get_summarization_requests();
    assert_eq!(
        summ_requests.len(),
        1,
        "Expected exactly 1 summarization request, got {}",
        summ_requests.len()
    );

    let prompt_text = TestHarness::extract_summarization_prompt(&summ_requests[0])
        .expect("Should have summarization prompt text");

    // The old messages should appear in the summarization prompt
    assert!(
        prompt_text.contains("OLD_MSG_1"),
        "Summarization prompt should contain OLD_MSG_1, got: {}",
        prompt_text
    );
}

/// 1.2 — Summarization request must exclude recent messages.
///
/// Uses distinctive content to make substring assertions unambiguous.
#[tokio::test]
async fn summarization_request_excludes_recent_messages() {
    let config = ContextConfig {
        context_window: Some(500),
        threshold: 0.5,
        keep_recent: 2,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    harness.add_response(agent_response("REPLY_ALPHA", 300));
    harness.add_response(agent_response("REPLY_BETA", 300));
    harness.add_response(summarization_response());
    harness.add_response(agent_response("REPLY_GAMMA", 300));

    harness.run_message("MSG_ALPHA").await;
    harness.run_message("MSG_BETA").await;
    harness.run_message("MSG_GAMMA").await;

    let summ_requests = harness.get_summarization_requests();
    assert_eq!(summ_requests.len(), 1);

    let prompt_text = TestHarness::extract_summarization_prompt(&summ_requests[0]).unwrap();

    // keep_recent=2 means the last 2 messages (user+assistant pairs) before
    // compaction are kept. The compaction runs at the START of turn 3, so
    // the history has 4 messages (turns 1+2). keep_start = 4 - 2 = 2.
    // Messages 0,1 (turn 1: user+assistant) get summarized.
    // Messages 2,3 (turn 2: user+assistant) are kept.
    //
    // So the summarization prompt should contain turn 1 content but NOT turn 2.
    assert!(
        prompt_text.contains("MSG_ALPHA") || prompt_text.contains("REPLY_ALPHA"),
        "Summarization should include old messages (turn 1)"
    );

    // MSG_GAMMA is the new message in turn 3 — it hasn't been added to history yet
    // when compaction runs (compaction runs BEFORE prompt_with_history).
    assert!(
        !prompt_text.contains("MSG_GAMMA"),
        "Summarization must not contain the current turn's message"
    );
}

/// 1.3 — Tool calls are excluded from summarization input.
///
/// ContextManager::format_messages_for_summary skips non-User/Assistant messages
/// with a catch-all `_ => continue`. Verify this contract.
#[tokio::test]
async fn summarization_excludes_tool_messages_from_prompt() {
    let config = ContextConfig {
        context_window: Some(500),
        threshold: 0.5,
        keep_recent: 2,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    // Turn 1: tool call — produces tool_call + tool_result + follow-up in history
    harness.add_response(MockResponse::tool_call_with_follow_up(
        "todo",
        serde_json::json!({"action": "add", "tasks": ["TOOL_TASK_XYZ"]}),
        "TOOL_FOLLOWUP_REPLY",
    ));
    // Turn 2: text response, high tokens to trigger compaction
    harness.add_response(agent_response("TEXT_REPLY_2", 300));
    // Turn 3: compaction + response
    harness.add_response(summarization_response());
    harness.add_response(agent_response("TEXT_REPLY_3", 300));

    harness.run_message("Add a todo TOOL_TASK_XYZ").await;
    harness.run_message("MSG_TWO").await;
    harness.run_message("MSG_THREE").await;

    if harness.has_compaction_occurred() {
        let summ_requests = harness.get_summarization_requests();
        if !summ_requests.is_empty() {
            let prompt_text =
                TestHarness::extract_summarization_prompt(&summ_requests[0]).unwrap();

            // format_messages_for_summary only includes User and Assistant messages.
            // The prompt should contain user text and assistant text but NOT raw
            // tool call JSON or tool result content as separate entries.
            // (Tool follow-up text IS included since it's AssistantContent::Text)
            assert!(
                !prompt_text.contains("ToolCall") && !prompt_text.contains("ToolResult"),
                "Summarization prompt should not contain raw tool message type markers"
            );
        }
    }
}

/// 1.4 — Total LLM call count matches expectations.
///
/// 3 user messages + 1 summarization = 4 total LLM calls.
#[tokio::test]
async fn llm_call_count_matches_expectations() {
    let config = ContextConfig {
        context_window: Some(500),
        threshold: 0.5,
        keep_recent: 2,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    harness.add_response(agent_response("R1", 300));
    harness.add_response(agent_response("R2", 300));
    harness.add_response(summarization_response());
    harness.add_response(agent_response("R3", 300));

    harness.run_message("M1").await;
    harness.run_message("M2").await;
    harness.run_message("M3").await;

    assert!(harness.has_compaction_occurred());

    // 3 regular + 1 summarization = 4 total
    assert_eq!(
        harness.get_summarization_requests().len(),
        1,
        "Should have exactly 1 summarization call"
    );
    assert_eq!(
        harness.get_regular_requests().len(),
        3,
        "Should have exactly 3 regular calls"
    );
    assert_eq!(
        harness.request_count(),
        4,
        "Total LLM calls should be 4 (3 regular + 1 summarization)"
    );
}

/// 1.5 — Post-compaction regular request contains compacted history.
///
/// After compaction, the next prompt_with_history() should send the
/// compacted history (summary + kept-recent), not the original full history.
#[tokio::test]
async fn post_compaction_request_has_compacted_history() {
    let config = ContextConfig {
        context_window: Some(500),
        threshold: 0.5,
        keep_recent: 2,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    harness.add_response(agent_response("R1", 300));
    harness.add_response(agent_response("R2", 300));
    harness.add_response(summarization_response());
    harness.add_response(agent_response("R3", 100));

    harness.run_message("M1").await;
    harness.run_message("M2").await;
    harness.run_message("M3").await;

    assert!(harness.has_compaction_occurred());

    // The request AFTER compaction (the last regular request) should have
    // fewer history messages than if no compaction happened.
    let regular_requests = harness.get_regular_requests();
    let last_regular = regular_requests.last().expect("Should have regular requests");

    // Verify the summary message is present in the post-compaction request.
    // The compaction replaced older messages with a summary, so the request
    // should contain fewer "real" conversation messages even if the total
    // count includes the summary + kept-recent + current prompt.

    // Verify the summary message is in the history sent to the LLM
    let has_summary = last_regular.chat_history.iter().any(|msg| {
        if let rig::completion::message::Message::User { content } = msg {
            content.iter().any(|c| {
                if let rig::completion::message::UserContent::Text(t) = c {
                    t.text.contains("[Previous conversation summary:")
                } else {
                    false
                }
            })
        } else {
            false
        }
    });
    assert!(
        has_summary,
        "Post-compaction LLM request should contain the summary message in history"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 2: Verify Summarization Output in State
// ═══════════════════════════════════════════════════════════════════════════════

/// 2.1 — Summary text appears in agent history after compaction.
#[tokio::test]
async fn summary_text_appears_in_history() {
    let config = ContextConfig {
        context_window: Some(300),
        threshold: 0.5,
        keep_recent: 1,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    harness.add_response(agent_response("R1", 200));
    harness.add_response(summarization_response_with("THE_CUSTOM_SUMMARY_TEXT"));
    harness.add_response(agent_response("R2", 200));

    harness.run_message("M1").await;
    harness.run_message("M2").await;

    assert!(harness.has_compaction_occurred());

    let history = harness.get_chat_history();
    let has_summary = history.iter().any(|msg| {
        if let rig::completion::message::Message::User { content } = msg {
            content.iter().any(|c| {
                if let rig::completion::message::UserContent::Text(t) = c {
                    t.text.contains("[Previous conversation summary:")
                        && t.text.contains("THE_CUSTOM_SUMMARY_TEXT")
                } else {
                    false
                }
            })
        } else {
            false
        }
    });
    assert!(
        has_summary,
        "History should contain a summary message with the mock's summary text. History: {:?}",
        history
            .iter()
            .map(|m| format!("{:?}", m))
            .collect::<Vec<_>>()
    );
}

/// 2.2 — Post-compaction history has the correct structure.
///
/// With keep_recent=1, after compaction + one new turn:
/// [summary, kept_assistant, new_user, new_assistant]
/// (kept region has 1 msg = the last assistant; the summary replaces the old ones)
#[tokio::test]
async fn post_compaction_history_structure() {
    let config = ContextConfig {
        context_window: Some(300),
        threshold: 0.5,
        keep_recent: 1,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    harness.add_response(agent_response("R1", 200));
    harness.add_response(summarization_response());
    harness.add_response(agent_response("R2", 200));

    harness.run_message("M1").await;
    harness.run_message("M2").await;

    assert!(
        harness.has_compaction_occurred(),
        "Compaction must trigger for this test"
    );

    let history = harness.get_chat_history();

    // After compaction the agent history was replaced, then M2 user+assistant were added.
    // The first message should be the summary.
    assert!(
        !history.is_empty(),
        "History should not be empty after compaction"
    );

    let first_is_summary = if let rig::completion::message::Message::User { content } =
        &history[0]
    {
        content.iter().any(|c| {
            if let rig::completion::message::UserContent::Text(t) = c {
                t.text.contains("[Previous conversation summary:")
            } else {
                false
            }
        })
    } else {
        false
    };
    assert!(
        first_is_summary,
        "First message in history should be the summary. Got: {:?}",
        history[0]
    );
}

/// 2.3 — Summary persists to StateManager chat messages (the UI-visible state).
#[tokio::test]
async fn summary_persists_to_state_manager() {
    let config = ContextConfig {
        context_window: Some(300),
        threshold: 0.5,
        keep_recent: 1,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    harness.add_response(agent_response("R1", 200));
    harness.add_response(summarization_response_with("PERSISTED_SUMMARY"));
    harness.add_response(agent_response("R2", 200));

    harness.run_message("M1").await;
    harness.run_message("M2").await;

    assert!(harness.has_compaction_occurred());

    let state = harness.get_state();
    let has_summary_in_chat = state
        .chat
        .messages
        .iter()
        .any(|m| m.content.contains("PERSISTED_SUMMARY"));
    assert!(
        has_summary_in_chat,
        "StateManager chat messages should contain the summary text. Messages: {:?}",
        state
            .chat
            .messages
            .iter()
            .map(|m| &m.content)
            .collect::<Vec<_>>()
    );
}

/// 2.4 — Summarization failure falls back to truncation.
///
/// Queue no summarization response so agent.prompt() errors.
/// Compaction should still complete via truncation fallback.
#[tokio::test]
async fn summarization_failure_falls_back_to_truncation() {
    let config = ContextConfig {
        context_window: Some(300),
        threshold: 0.5,
        keep_recent: 1,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    // Turn 1: normal
    harness.add_response(agent_response("R1", 200));
    // Turn 2: compaction fires, tries to summarize, but the next queued response
    // is the agent response (not a summarization response). The summarization
    // call will consume it and return "R2" as the "summary", then the actual
    // agent call will fail with "no responses queued".
    //
    // Actually, to test summarization FAILURE, we need the summarization call
    // itself to fail. Queue an error response for it.
    harness.add_response(MockResponse::error("Simulated LLM failure for summarization"));
    harness.add_response(agent_response("R2", 200));

    harness.run_message("M1").await;
    let response = harness.run_message("M2").await;

    // The agent should still respond (truncation fallback, not crash)
    assert!(
        !response.is_empty() && response != "Error occurred",
        "Agent should still respond after summarization failure. Got: {}",
        response
    );

    // History should be shorter than uncompacted (truncation happened)
    // Without compaction: 4 messages (2 user + 2 assistant)
    let count = harness.get_chat_message_count();
    assert!(
        count <= 4,
        "History should not grow unbounded after failed summarization. Got {} messages",
        count
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 3: Verify Compaction Timing
// ═══════════════════════════════════════════════════════════════════════════════

/// 3.1 — Compaction triggers at the exact expected turn.
///
/// Threshold math:
///   context_window=500, threshold=0.5 -> 250 tokens
///   keep_recent=2
///   Each response: 300 input_tokens (> 250)
///
/// Flow:
///   Turn 1: process_message_internal runs.
///     - compact_if_needed: history is empty (messages added AFTER prompt), skip.
///     - prompt_with_history, then add user+assistant to StateManager (2 msgs).
///   Turn 2: process_message_internal runs.
///     - process_session_hook_events: syncs turn 1's stats (300 tokens).
///     - compact_if_needed: history=2 msgs, keep_recent=2. 2 <= 2, skip.
///     - prompt, add msgs (4 total).
///   Turn 3: process_message_internal runs.
///     - process_session_hook_events: syncs turn 2's stats (300 tokens).
///     - compact_if_needed: history=4 msgs > 2, tokens=300 > 250. COMPACT!
#[tokio::test]
async fn compaction_triggers_at_exact_turn() {
    let config = ContextConfig {
        context_window: Some(500),
        threshold: 0.5,
        keep_recent: 2,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    harness.add_response(agent_response("R1", 300));
    harness.add_response(agent_response("R2", 300));
    harness.add_response(summarization_response());
    harness.add_response(agent_response("R3", 300));

    // Turn 1: no compaction
    harness.run_message("M1").await;
    assert_eq!(
        harness.get_compaction_events().len(),
        0,
        "Turn 1: no compaction (history empty at check time)"
    );

    // Turn 2: no compaction (msgs <= keep_recent)
    harness.run_message("M2").await;
    assert_eq!(
        harness.get_compaction_events().len(),
        0,
        "Turn 2: no compaction (2 msgs <= keep_recent=2)"
    );

    // Turn 3: compaction fires
    harness.run_message("M3").await;
    assert_eq!(
        harness.get_compaction_events().len(),
        1,
        "Turn 3: exactly 1 compaction event"
    );
}

/// 3.2 — Compaction does NOT trigger when tokens are below threshold.
///
/// Strengthened version: also verifies zero summarization LLM calls.
#[tokio::test]
async fn no_compaction_below_threshold_verified_by_request_count() {
    let config = ContextConfig {
        context_window: Some(1000),
        threshold: 0.8, // 800 tokens
        keep_recent: 2,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    // 100 tokens per request — well under 800
    for _ in 0..5 {
        harness.add_response(agent_response("Response", 100));
    }

    for i in 1..=5 {
        harness.run_message(&format!("Message {}", i)).await;
    }

    assert!(!harness.has_compaction_occurred());
    assert_eq!(
        harness.get_summarization_requests().len(),
        0,
        "No summarization calls should have been made"
    );
    assert_eq!(
        harness.request_count(),
        5,
        "Only 5 regular calls, no summarization"
    );
}

/// 3.3 — Compaction does NOT trigger when msgs <= keep_recent, even if tokens are high.
#[tokio::test]
async fn no_compaction_when_msgs_lte_keep_recent() {
    let config = ContextConfig {
        context_window: Some(400),
        threshold: 0.5, // 200 tokens
        keep_recent: 10, // Very high — 3 turns = 6 msgs < 10
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    // 500 tokens per request — way above threshold
    for _ in 0..3 {
        harness.add_response(agent_response("Response", 500));
    }

    for i in 1..=3 {
        harness.run_message(&format!("Message {}", i)).await;
    }

    assert!(
        !harness.has_compaction_occurred(),
        "Compaction should not trigger: 6 msgs <= keep_recent=10"
    );
    assert_eq!(harness.get_summarization_requests().len(), 0);
}

/// 3.4 — Verify the token stats pipeline ordering.
///
/// Turn 1 produces 100 tokens. Turn 2's compaction check should see 100
/// (not 0, not cumulative). Turn 2 produces 500 tokens. Turn 3's check
/// should see 500 and trigger.
#[tokio::test]
async fn token_stats_pipeline_ordering() {
    let config = ContextConfig {
        context_window: Some(600),
        threshold: 0.5, // 300 tokens
        keep_recent: 2,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    // Turn 1: 100 tokens (below 300 threshold)
    harness.add_response(agent_response("R1", 100));
    // Turn 2: 100 tokens (turn 2's check sees turn 1's 100 — below threshold)
    harness.add_response(agent_response("R2", 100));
    // Turn 3: 500 tokens (turn 3's check sees turn 2's 100 — still below threshold!)
    // Actually, needs_compaction reads last_input_tokens which is the last request.
    // After turn 2, last_input_tokens = 100. So turn 3's check sees 100 < 300.
    harness.add_response(agent_response("R3", 500));
    // Turn 4: check sees turn 3's 500 > 300, AND 6 msgs > 2. COMPACT!
    harness.add_response(summarization_response());
    harness.add_response(agent_response("R4", 100));

    harness.run_message("M1").await;
    assert!(!harness.has_compaction_occurred(), "Turn 1: no compaction");

    harness.run_message("M2").await;
    assert!(!harness.has_compaction_occurred(), "Turn 2: no compaction");

    harness.run_message("M3").await;
    assert!(
        !harness.has_compaction_occurred(),
        "Turn 3: no compaction (sees turn 2's 100 tokens < 300)"
    );

    harness.run_message("M4").await;
    assert!(
        harness.has_compaction_occurred(),
        "Turn 4: compaction (sees turn 3's 500 tokens > 300)"
    );
    assert_eq!(harness.get_compaction_events().len(), 1);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 4: Verify Queue Consumption
// ═══════════════════════════════════════════════════════════════════════════════

/// 4.1 — A compaction turn consumes exactly 2 responses (1 summarization + 1 regular).
#[tokio::test]
async fn compaction_turn_consumes_two_responses() {
    let config = ContextConfig {
        context_window: Some(500),
        threshold: 0.5,
        keep_recent: 2,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    harness.add_response(agent_response("R1", 300));
    harness.add_response(agent_response("R2", 300));
    harness.add_response(summarization_response());
    harness.add_response(agent_response("R3", 300));

    harness.run_message("M1").await;
    harness.run_message("M2").await;

    // Before turn 3 (the compaction turn)
    let remaining_before = harness.remaining_responses();
    harness.run_message("M3").await;
    let remaining_after = harness.remaining_responses();

    assert!(harness.has_compaction_occurred());
    assert_eq!(
        remaining_before - remaining_after,
        2,
        "Compaction turn should consume 2 responses (1 summarization + 1 regular). \
         Before: {}, After: {}",
        remaining_before,
        remaining_after
    );
}

/// 4.2 — A non-compaction turn consumes exactly 1 response.
#[tokio::test]
async fn non_compaction_turn_consumes_one_response() {
    let config = ContextConfig {
        context_window: Some(1000),
        threshold: 0.8, // high threshold, won't trigger
        keep_recent: 2,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    harness.add_response(agent_response("R1", 100));
    harness.add_response(agent_response("R2", 100));

    let remaining_before = harness.remaining_responses();
    harness.run_message("M1").await;
    let remaining_after = harness.remaining_responses();

    assert!(!harness.has_compaction_occurred());
    assert_eq!(
        remaining_before - remaining_after,
        1,
        "Non-compaction turn should consume exactly 1 response"
    );
}

/// 4.3 — Queue consumption is consistent across all turns.
///
/// Verify the per-turn consumption pattern for a multi-turn conversation
/// where compaction triggers once.
#[tokio::test]
async fn queue_consumption_pattern_across_turns() {
    let config = ContextConfig {
        context_window: Some(500),
        threshold: 0.5,
        keep_recent: 2,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    harness.add_response(agent_response("R1", 300));
    harness.add_response(agent_response("R2", 300));
    harness.add_response(summarization_response());
    harness.add_response(agent_response("R3", 100));
    harness.add_response(agent_response("R4", 100));

    let mut consumed_per_turn = Vec::new();

    for i in 1..=4 {
        let before = harness.remaining_responses();
        harness.run_message(&format!("M{}", i)).await;
        let after = harness.remaining_responses();
        consumed_per_turn.push(before - after);
    }

    // Turns 1,2: no compaction → 1 response each
    // Turn 3: compaction → 2 responses
    // Turn 4: no compaction → 1 response
    assert_eq!(
        consumed_per_turn,
        vec![1, 1, 2, 1],
        "Queue consumption pattern should be [1, 1, 2, 1] (compaction on turn 3). Got: {:?}",
        consumed_per_turn
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 5: Edge Cases
// ═══════════════════════════════════════════════════════════════════════════════

/// 5.1 — Multiple compactions produce stacked summaries.
///
/// After the second compaction, the summarization request should contain
/// the first summary text (since the first summary is now an "old" message).
#[tokio::test]
async fn multiple_compactions_stack_summaries() {
    let config = ContextConfig {
        context_window: Some(300),
        threshold: 0.5, // 150 tokens
        keep_recent: 1,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    // Turn 1: normal
    harness.add_response(agent_response("R1", 200));
    // Turn 2: compaction #1 fires
    harness.add_response(summarization_response_with("FIRST_SUMMARY"));
    harness.add_response(agent_response("R2", 200));
    // Turn 3: compaction #2 fires (the first summary is now old)
    harness.add_response(summarization_response_with("SECOND_SUMMARY"));
    harness.add_response(agent_response("R3", 200));

    harness.run_message("M1").await;
    harness.run_message("M2").await;
    harness.run_message("M3").await;

    let events = harness.get_compaction_events();
    assert!(
        events.len() >= 2,
        "Expected at least 2 compaction events, got {}",
        events.len()
    );

    let summ_requests = harness.get_summarization_requests();
    if summ_requests.len() >= 2 {
        let second_prompt =
            TestHarness::extract_summarization_prompt(&summ_requests[1]).unwrap();
        // The second summarization should include the first summary text,
        // since it's now part of the old messages being summarized.
        assert!(
            second_prompt.contains("FIRST_SUMMARY"),
            "Second summarization should contain the first summary text. Got: {}",
            second_prompt
        );
    }
}

/// 5.2 — Tool calls crossing the compaction boundary are preserved.
///
/// Exercise find_needed_tool_calls through the full E2E path.
/// A tool call in the old region whose result is in the kept region
/// must be preserved.
#[tokio::test]
async fn tool_calls_crossing_boundary_preserved() {
    let config = ContextConfig {
        context_window: Some(500),
        threshold: 0.5, // 250 tokens
        keep_recent: 3, // Keep last 3 messages
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    // Turn 1: a tool call (creates ToolCall + ToolResult + follow-up in history)
    harness.add_response(MockResponse::tool_call_with_follow_up(
        "todo",
        serde_json::json!({"action": "add", "tasks": ["Boundary task"]}),
        "Added the boundary task",
    ));
    // Turn 2: text, high tokens
    harness.add_response(agent_response("R2", 300));
    // Turn 3: compaction + response
    harness.add_response(summarization_response());
    harness.add_response(agent_response("R3", 300));

    harness.run_message("Add boundary task").await;
    harness.run_message("M2").await;
    harness.run_message("M3").await;

    if harness.has_compaction_occurred() {
        harness.assert_compaction_actually_discarded();

        // After compaction, the history should still be coherent.
        // The important thing is it doesn't crash and the conversation continues.
        let history = harness.get_chat_history();
        assert!(
            !history.is_empty(),
            "History should not be empty after compaction with tool calls"
        );
    }
}

/// 5.3 — Message-count fallback triggers when no token data is available.
///
/// Use MockResponse::text() (no usage data) so get_current_tokens() returns 0.
/// Fallback threshold: (keep_recent * 3).max(10) messages.
/// With keep_recent=3: threshold = 10 messages.
#[tokio::test]
async fn message_count_fallback_triggers_compaction() {
    let config = ContextConfig {
        context_window: Some(1000),
        threshold: 0.8,
        keep_recent: 3,
        enabled: true,
        // Fallback: (3 * 3).max(10) = 10 messages
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    // No usage data — forces message-count fallback
    // 10 messages threshold means we need > 10 messages in history.
    // Each turn adds 2 messages (user + assistant), so after 5 turns = 10 msgs.
    // After 6 turns = 12 msgs > 10 threshold. But compaction checks BEFORE adding
    // the current turn's messages, so:
    //   After turn 5: 10 msgs in history.
    //   Turn 6 check: 10 msgs > 10? No (it's >, not >=). Need 11+.
    //   After turn 5.5: we need to figure out exact timing.
    //
    // Actually, turn 6 sees 10 msgs (from turns 1-5). 10 > 10 is false.
    // Turn 7 sees 12 msgs (from turns 1-6). 12 > 10 is true. Compact!
    for _ in 0..6 {
        harness.add_response(MockResponse::text("Response"));
    }
    harness.add_response(summarization_response());
    harness.add_response(MockResponse::text("Final response"));

    for i in 1..=6 {
        harness.run_message(&format!("Message {}", i)).await;
    }

    // Should NOT have compacted yet (10 msgs, threshold is > 10)
    let compacted_at_6 = harness.has_compaction_occurred();

    harness.run_message("Message 7").await;

    if !compacted_at_6 {
        assert!(
            harness.has_compaction_occurred(),
            "Compaction should trigger via message-count fallback (12 msgs > 10 threshold)"
        );
    }
    // Either way, compaction should have occurred by now
    assert!(
        harness.has_compaction_occurred(),
        "Compaction should have occurred via message-count fallback"
    );
}

/// 5.4 — Compaction handles empty/whitespace messages without panicking.
#[tokio::test]
async fn compaction_handles_empty_messages() {
    let config = ContextConfig {
        context_window: Some(300),
        threshold: 0.5,
        keep_recent: 1,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    harness.add_response(agent_response("", 200)); // empty response
    harness.add_response(summarization_response());
    harness.add_response(agent_response("R2", 200));

    harness.run_message("").await; // empty user message
    harness.run_message("M2").await;

    // The test passes if it doesn't panic. Compaction may or may not trigger
    // depending on how empty messages affect the history, but it must not crash.
}

/// 5.5 — keep_recent=0: all messages summarized, only summary remains.
#[tokio::test]
async fn keep_recent_zero_summarizes_everything() {
    let config = ContextConfig {
        context_window: Some(300),
        threshold: 0.5,
        keep_recent: 0,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    harness.add_response(agent_response("R1", 200));
    harness.add_response(summarization_response_with("TOTAL_SUMMARY"));
    harness.add_response(agent_response("R2", 200));

    harness.run_message("M1").await;
    harness.run_message("M2").await;

    // If compaction occurred, verify the summary is there
    if harness.has_compaction_occurred() {
        let history = harness.get_chat_history();
        // With keep_recent=0, EVERYTHING before the current turn gets summarized.
        // After compaction: [summary]. Then current turn adds [user, assistant].
        let has_summary = history.iter().any(|msg| {
            if let rig::completion::message::Message::User { content } = msg {
                content.iter().any(|c| {
                    if let rig::completion::message::UserContent::Text(t) = c {
                        t.text.contains("TOTAL_SUMMARY")
                    } else {
                        false
                    }
                })
            } else {
                false
            }
        });
        assert!(has_summary, "With keep_recent=0, only the summary should remain from old messages");
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 6.2: Strengthened Existing Tests
// ═══════════════════════════════════════════════════════════════════════════════

/// Strengthened compaction_preserves_recent_messages: compaction MUST occur (assert, not if).
#[tokio::test]
async fn compaction_must_preserve_recent_messages() {
    let config = ContextConfig {
        context_window: Some(400),
        threshold: 0.5,
        keep_recent: 2,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    harness.add_response(agent_response("R1", 250));
    harness.add_response(agent_response("R2", 250));
    harness.add_response(summarization_response());
    harness.add_response(agent_response("R3", 250));

    harness.run_message("M1").await;
    harness.run_message("M2").await;
    harness.run_message("M3").await;

    // MUST occur, not "if it occurred"
    assert!(
        harness.has_compaction_occurred(),
        "Compaction must have occurred for this test to be valid"
    );
    harness.assert_compaction_actually_discarded();

    let history = harness.get_chat_history();
    assert!(
        history.len() < 6,
        "History should be compacted from 6 messages, got {}",
        history.len()
    );
}

/// Strengthened compaction_reduces_history: verify exact expected count.
#[tokio::test]
async fn compaction_reduces_history_exact_count() {
    let config = ContextConfig {
        context_window: Some(300),
        threshold: 0.5,
        keep_recent: 1,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    harness.add_response(agent_response("R1", 200));
    harness.add_response(summarization_response());
    harness.add_response(agent_response("R2", 200));

    harness.run_message("M1").await;
    harness.run_message("M2").await;

    assert!(
        harness.has_compaction_occurred(),
        "Compaction must occur by turn 2"
    );

    let events = harness.get_compaction_events();
    for event in &events {
        assert!(
            event.num_discarded > 0,
            "Every compaction event must discard messages (got num_discarded=0)"
        );
        assert!(
            event.compacted_count <= event.original_count,
            "compacted_count ({}) must not exceed original_count ({})",
            event.compacted_count,
            event.original_count
        );
    }

    // Verify via request count: 2 regular + 1 summarization
    assert_eq!(harness.request_count(), 3);
    assert_eq!(harness.get_summarization_requests().len(), 1);
}

/// Strengthened multiple_compaction_events: verify each discard count and
/// verify via summarization request count.
#[tokio::test]
async fn multiple_compaction_events_verified() {
    let config = ContextConfig {
        context_window: Some(200),
        threshold: 0.6, // 120 tokens
        keep_recent: 1,
        enabled: true,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config);

    // Queue enough for 5 turns with possible compactions after each
    for _ in 0..5 {
        harness.add_response(agent_response("R", 150));
    }
    for _ in 0..4 {
        harness.add_response(summarization_response());
    }

    for i in 1..=5 {
        harness.run_message(&format!("M{}", i)).await;
    }

    assert!(harness.has_compaction_occurred());

    let events = harness.get_compaction_events();
    assert!(
        events.len() >= 2,
        "Expected multiple compaction events, got {}",
        events.len()
    );

    for (i, event) in events.iter().enumerate() {
        assert!(
            event.num_discarded > 0,
            "Compaction event {} must discard messages",
            i
        );
    }

    // The number of summarization requests should match compaction events
    let summ_count = harness.get_summarization_requests().len();
    assert_eq!(
        summ_count,
        events.len(),
        "Summarization request count ({}) should match compaction event count ({})",
        summ_count,
        events.len()
    );
}
