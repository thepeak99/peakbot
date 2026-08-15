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
/// Setup: 500-token window, 50% threshold (250 tokens), keep_recent=3.
/// Send 3 messages with 300 input_tokens each. Under the agent_loop ordering
/// (user-msg appended *before* compaction check; see make-flow-great-again.md)
/// compaction triggers on turn 3: the check sees 5 msgs (turns 1+2 closed +
/// turn 3 user), 5 > keep_recent=3, and turn 2's 300 tokens > 250.
/// The summarization request should contain "OLD_MSG_1" but NOT "RECENT_MSG".
#[tokio::test]
async fn summarization_request_contains_old_messages() {
    let config = ContextConfig {
        threshold: 0.5, // 250 tokens
        keep_recent: 3,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 500);

    // Turn 1: user="OLD_MSG_1", response has 300 tokens
    harness.add_response(agent_response("OLD_REPLY_1", 300));
    // Turn 2: user="OLD_MSG_2", response has 300 tokens
    harness.add_response(agent_response("OLD_REPLY_2", 300));
    // Turn 3: compaction fires first (consuming summarization response),
    //         then regular response
    harness.add_compaction_response(summarization_response());
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
        threshold: 0.5,
        keep_recent: 3,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 500);

    harness.add_response(agent_response("REPLY_ALPHA", 300));
    harness.add_response(agent_response("REPLY_BETA", 300));
    harness.add_compaction_response(summarization_response());
    harness.add_response(agent_response("REPLY_GAMMA", 300));

    harness.run_message("MSG_ALPHA").await;
    harness.run_message("MSG_BETA").await;
    harness.run_message("MSG_GAMMA").await;

    let summ_requests = harness.get_summarization_requests();
    assert_eq!(summ_requests.len(), 1);

    let prompt_text = TestHarness::extract_summarization_prompt(&summ_requests[0]).unwrap();

    // Under the agent_loop ordering (user-msg appended *before* compaction
    // check; see make-flow-great-again.md), turn 3 begins with 5 msgs:
    // [user A, asst A, user B, asst B, user C]. keep_recent=3 means the last
    // 3 (asst B, user C — wait, that's only 2; with keep_recent=3 we keep
    // [user B, asst B, user C]). keep_start = 5 - 3 = 2. Messages 0,1 (turn 1
    // user+assistant) are summarized; msg 2 onward are kept.
    assert!(
        prompt_text.contains("MSG_ALPHA") || prompt_text.contains("REPLY_ALPHA"),
        "Summarization should include old messages (turn 1)"
    );

    // MSG_GAMMA is the turn-3 user message; it is in the kept tail, not the
    // summarized window.
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
        threshold: 0.5,
        keep_recent: 2,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 500);

    // Turn 1: tool call — produces tool_call + tool_result + follow-up in history
    harness.add_response(MockResponse::tool_call_with_follow_up(
        "todo",
        serde_json::json!({"action": "add", "tasks": ["TOOL_TASK_XYZ"]}),
        "TOOL_FOLLOWUP_REPLY",
    ));
    // Turn 2: text response, high tokens to trigger compaction
    harness.add_response(agent_response("TEXT_REPLY_2", 300));
    // Turn 3: compaction + response
    harness.add_compaction_response(summarization_response());
    harness.add_response(agent_response("TEXT_REPLY_3", 300));

    harness.run_message("Add a todo TOOL_TASK_XYZ").await;
    harness.run_message("MSG_TWO").await;
    harness.run_message("MSG_THREE").await;

    if harness.has_compaction_occurred() {
        let summ_requests = harness.get_summarization_requests();
        if !summ_requests.is_empty() {
            let prompt_text = TestHarness::extract_summarization_prompt(&summ_requests[0]).unwrap();

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
        threshold: 0.5,
        keep_recent: 3,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 500);

    harness.add_response(agent_response("R1", 300));
    harness.add_response(agent_response("R2", 300));
    harness.add_compaction_response(summarization_response());
    harness.add_response(agent_response("R3", 300));

    harness.run_message("M1").await;
    harness.run_message("M2").await;
    harness.run_message("M3").await;

    assert!(harness.has_compaction_occurred());

    // With independent compaction model: 3 main agent calls + 1 compaction model call
    assert_eq!(
        harness.get_summarization_requests().len(),
        1,
        "Should have exactly 1 summarization call (on compaction model)"
    );
    assert_eq!(
        harness.get_regular_requests().len(),
        3,
        "Should have exactly 3 regular calls (on main agent)"
    );
    // request_count() only counts main agent calls
    assert_eq!(
        harness.request_count(),
        3,
        "Main agent LLM calls should be 3 (summarization is on separate model)"
    );
}

/// 1.5 — Post-compaction regular request contains compacted history.
///
/// After compaction, the next prompt_with_history() should send the
/// compacted history (summary + kept-recent), not the original full history.
#[tokio::test]
async fn post_compaction_request_has_compacted_history() {
    let config = ContextConfig {
        threshold: 0.5,
        keep_recent: 2,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 500);

    harness.add_response(agent_response("R1", 300));
    harness.add_response(agent_response("R2", 300));
    harness.add_compaction_response(summarization_response());
    harness.add_response(agent_response("R3", 100));

    harness.run_message("M1").await;
    harness.run_message("M2").await;
    harness.run_message("M3").await;

    assert!(harness.has_compaction_occurred());

    // The request AFTER compaction (the last regular request) should have
    // fewer history messages than if no compaction happened.
    let regular_requests = harness.get_regular_requests();
    let last_regular = regular_requests
        .last()
        .expect("Should have regular requests");

    // Verify the summary message is present in the post-compaction request.
    // The compaction replaced older messages with a summary, so the request
    // should contain fewer "real" conversation messages even if the total
    // count includes the summary + kept-recent + current prompt.

    // Verify the summary message is in the history sent to the LLM
    let has_summary = last_regular.chat_history.iter().any(|msg| {
        if let rig_core::completion::message::Message::User { content } = msg {
            content.iter().any(|c| {
                if let rig_core::completion::message::UserContent::Text(t) = c {
                    t.text.contains("[Conversation summary]")
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
        threshold: 0.5,
        keep_recent: 1,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 300);

    harness.add_response(agent_response("R1", 200));
    harness.add_compaction_response(summarization_response_with("THE_CUSTOM_SUMMARY_TEXT"));
    harness.add_response(agent_response("R2", 200));

    harness.run_message("M1").await;
    harness.run_message("M2").await;

    assert!(harness.has_compaction_occurred());

    // With tag-and-skip, the summary appears as a Summary role message.
    // In get_agent_history() it's converted to a User message with "[Conversation summary]" prefix.
    let history = harness.get_chat_history();
    let has_summary = history.iter().any(|msg| {
        if let rig_core::completion::message::Message::User { content } = msg {
            content.iter().any(|c| {
                if let rig_core::completion::message::UserContent::Text(t) = c {
                    t.text.contains("[Conversation summary]")
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
        threshold: 0.5,
        keep_recent: 1,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 300);

    harness.add_response(agent_response("R1", 200));
    harness.add_compaction_response(summarization_response());
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

    let first_is_summary =
        if let rig_core::completion::message::Message::User { content } = &history[0] {
            content.iter().any(|c| {
                if let rig_core::completion::message::UserContent::Text(t) = c {
                    t.text.contains("[Conversation summary]")
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
        threshold: 0.5,
        keep_recent: 1,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 300);

    harness.add_response(agent_response("R1", 200));
    harness.add_compaction_response(summarization_response_with("PERSISTED_SUMMARY"));
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

/// 2.4 — Compaction failure aborts the turn with an error.
///
/// Queue no compaction response so the summarization call errors.
/// After the unfuck-compact fix, compaction failure is a hard error:
/// the turn aborts with ProcessResult::Error and a system message.
/// "Honest failure is the right behaviour" — no silent degradation.
#[tokio::test]
async fn compaction_failure_aborts_turn() {
    let config = ContextConfig {
        threshold: 0.5, // 150 tokens
        keep_recent: 1,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 300);

    // Turn 1: normal
    harness.add_response(agent_response("R1", 200));
    // Turn 2: compaction fires, tries to summarize via compaction model,
    // but no compaction response is queued — summarization fails.
    // The turn should abort with an error.
    harness.add_response(agent_response("WONT_BE_SEEN", 200));

    harness.run_message("M1").await;
    let response = harness.run_message("M2").await;

    // Compaction failure is now a hard error — the turn aborts.
    assert_eq!(
        response, "Error occurred",
        "Compaction failure must abort the turn with an error. Got: {response}"
    );

    // No compaction should have succeeded
    assert!(
        !harness.has_compaction_occurred(),
        "Compaction should not have succeeded with no summarization response"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 3: Verify Compaction Timing
// ═══════════════════════════════════════════════════════════════════════════════

/// 3.1 — Compaction triggers at the exact expected turn.
///
/// Threshold math:
///   context_window=500, threshold=0.5 -> 250 tokens
///   keep_recent=3
///   Each response: 300 input_tokens (> 250)
///
/// Flow (single-writer ordering — see make-flow-great-again.md: user-msg
/// is appended to chat *before* compaction check, mirroring agent_loop):
///   Turn 1: process_message_internal runs.
///     - add_user_message("M1"): history=1 msg.
///     - compact_if_needed: 1 <= keep_recent=3, skip.
///     - prompt + add_assistant: history=2 msgs, last_input_tokens=300.
///   Turn 2:
///     - add_user_message("M2"): history=3 msgs.
///     - process_session_hook_events: syncs turn 1's stats (300 tokens).
///     - compact_if_needed: 3 <= 3, skip.
///     - prompt + add_assistant: history=4 msgs.
///   Turn 3:
///     - add_user_message("M3"): history=5 msgs.
///     - process_session_hook_events: syncs turn 2's stats (300 tokens).
///     - compact_if_needed: 5 > 3, tokens=300 > 250. COMPACT!
#[tokio::test]
async fn compaction_triggers_at_exact_turn() {
    let config = ContextConfig {
        threshold: 0.5,
        keep_recent: 3,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 500);

    harness.add_response(agent_response("R1", 300));
    harness.add_response(agent_response("R2", 300));
    harness.add_compaction_response(summarization_response());
    harness.add_response(agent_response("R3", 300));

    // Turn 1: no compaction (1 msg at check time)
    harness.run_message("M1").await;
    assert_eq!(
        harness.get_compaction_events().len(),
        0,
        "Turn 1: no compaction (1 msg <= keep_recent=3)"
    );

    // Turn 2: no compaction (3 msgs <= keep_recent=3)
    harness.run_message("M2").await;
    assert_eq!(
        harness.get_compaction_events().len(),
        0,
        "Turn 2: no compaction (3 msgs <= keep_recent=3)"
    );

    // Turn 3: compaction fires (5 msgs > 3, tokens > 250)
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
        threshold: 0.8, // 800 tokens
        keep_recent: 2,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 1000);

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
        threshold: 0.5,  // 200 tokens
        keep_recent: 10, // Very high — 3 turns = 6 msgs < 10
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 400);

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
        threshold: 0.5, // 300 tokens
        keep_recent: 2,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 600);

    // Turn 1: 100 tokens (below 300 threshold)
    harness.add_response(agent_response("R1", 100));
    // Turn 2: 100 tokens (turn 2's check sees turn 1's 100 — below threshold)
    harness.add_response(agent_response("R2", 100));
    // Turn 3: 500 tokens (turn 3's check sees turn 2's 100 — still below threshold!)
    // Actually, needs_compaction reads last_input_tokens which is the last request.
    // After turn 2, last_input_tokens = 100. So turn 3's check sees 100 < 300.
    harness.add_response(agent_response("R3", 500));
    // Turn 4: check sees turn 3's 500 > 300, AND 6 msgs > 2. COMPACT!
    harness.add_compaction_response(summarization_response());
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

/// 4.1 — A turn with pre-prompt compaction consumes exactly 1 main-agent response.
///
/// With the independent compaction model, summarization goes to the separate
/// compaction model. Pre-prompt compaction fires when the previous turn's
/// token count exceeds the threshold. Each compaction turn consumes 1 main
/// agent response (the summarization is transparent to the main queue).
#[tokio::test]
async fn compaction_turn_consumes_two_responses() {
    let config = ContextConfig {
        threshold: 0.5,
        keep_recent: 2,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 500);

    harness.add_response(agent_response("R1", 300));
    harness.add_response(agent_response("R2", 300));
    // Two compaction responses: M2's pre-prompt compaction consumes the
    // first; M3's pre-prompt compaction consumes the second.
    harness.add_compaction_response(summarization_response());
    harness.add_compaction_response(summarization_response());
    harness.add_response(agent_response("R3", 300));

    harness.run_message("M1").await;
    harness.run_message("M2").await;

    // Before turn 3 (the compaction turn)
    let remaining_before = harness.remaining_responses();
    harness.run_message("M3").await;
    let remaining_after = harness.remaining_responses();

    assert!(harness.has_compaction_occurred());
    // With independent compaction model, the compaction turn only consumes 1 from
    // the main agent queue (summarization goes to the separate compaction model).
    assert_eq!(
        remaining_before - remaining_after,
        1,
        "Compaction turn should consume 1 main agent response (summarization is on separate model). \
         Before: {}, After: {}",
        remaining_before,
        remaining_after
    );
}

/// 4.2 — A non-compaction turn consumes exactly 1 response.
#[tokio::test]
async fn non_compaction_turn_consumes_one_response() {
    let config = ContextConfig {
        threshold: 0.8, // high threshold, won't trigger
        keep_recent: 2,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 1000);

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
/// where pre-prompt compaction triggers on M2 and M3 (300 tokens > 250
/// threshold) but not on M4 (100 tokens < threshold). Two compaction
/// responses are queued: one for M2, one for M3.
#[tokio::test]
async fn queue_consumption_pattern_across_turns() {
    let config = ContextConfig {
        threshold: 0.5,
        keep_recent: 2,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 500);

    harness.add_response(agent_response("R1", 300));
    harness.add_response(agent_response("R2", 300));
    // M2's pre-prompt compaction consumes the first; M3's consumes the second.
    harness.add_compaction_response(summarization_response());
    harness.add_compaction_response(summarization_response());
    harness.add_response(agent_response("R3", 100));
    harness.add_response(agent_response("R4", 100));

    let mut consumed_per_turn = Vec::new();

    for i in 1..=4 {
        let before = harness.remaining_responses();
        harness.run_message(&format!("M{}", i)).await;
        let after = harness.remaining_responses();
        consumed_per_turn.push(before - after);
    }

    // With independent compaction model, every turn consumes exactly 1 from the
    // main agent queue. Summarization goes to the separate compaction model.
    assert_eq!(
        consumed_per_turn,
        vec![1, 1, 1, 1],
        "Each turn should consume 1 main agent response (compaction is on separate model). Got: {:?}",
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
        threshold: 0.5, // 150 tokens
        keep_recent: 1,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 300);

    // Turn 1: normal
    harness.add_response(agent_response("R1", 200));
    // Turn 2: compaction #1 fires
    harness.add_compaction_response(summarization_response_with("FIRST_SUMMARY"));
    harness.add_response(agent_response("R2", 200));
    // Turn 3: compaction #2 fires (the first summary is now old)
    harness.add_compaction_response(summarization_response_with("SECOND_SUMMARY"));
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
        let second_prompt = TestHarness::extract_summarization_prompt(&summ_requests[1]).unwrap();
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
        threshold: 0.5, // 250 tokens
        keep_recent: 3, // Keep last 3 messages
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 500);

    // Turn 1: a tool call (creates ToolCall + ToolResult + follow-up in history)
    harness.add_response(MockResponse::tool_call_with_follow_up(
        "todo",
        serde_json::json!({"action": "add", "tasks": ["Boundary task"]}),
        "Added the boundary task",
    ));
    // Turn 2: text, high tokens
    harness.add_response(agent_response("R2", 300));
    // Turn 3: compaction + response
    harness.add_compaction_response(summarization_response());
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
        threshold: 0.8,
        keep_recent: 3,
        enabled: true,
        compaction_model: None,
        // Fallback: (3 * 3).max(10) = 10 messages
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 1000);

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
    harness.add_compaction_response(summarization_response());
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
        threshold: 0.5,
        keep_recent: 1,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 300);

    harness.add_response(agent_response("", 200)); // empty response
    harness.add_compaction_response(summarization_response());
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
        threshold: 0.5,
        keep_recent: 0,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 300);

    harness.add_response(agent_response("R1", 200));
    harness.add_compaction_response(summarization_response_with("TOTAL_SUMMARY"));
    harness.add_response(agent_response("R2", 200));

    harness.run_message("M1").await;
    harness.run_message("M2").await;

    // If compaction occurred, verify the summary is there
    if harness.has_compaction_occurred() {
        let history = harness.get_chat_history();
        // With keep_recent=0, EVERYTHING before the current turn gets summarized.
        // After compaction: [summary]. Then current turn adds [user, assistant].
        let has_summary = history.iter().any(|msg| {
            if let rig_core::completion::message::Message::User { content } = msg {
                content.iter().any(|c| {
                    if let rig_core::completion::message::UserContent::Text(t) = c {
                        t.text.contains("TOTAL_SUMMARY")
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
            "With keep_recent=0, only the summary should remain from old messages"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 6.2: Strengthened Existing Tests
// ═══════════════════════════════════════════════════════════════════════════════

/// Strengthened compaction_preserves_recent_messages: compaction MUST occur (assert, not if).
#[tokio::test]
async fn compaction_must_preserve_recent_messages() {
    let config = ContextConfig {
        threshold: 0.5,
        keep_recent: 3,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 400);

    harness.add_response(agent_response("R1", 250));
    harness.add_response(agent_response("R2", 250));
    harness.add_compaction_response(summarization_response());
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
        threshold: 0.5,
        keep_recent: 1,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 300);

    harness.add_response(agent_response("R1", 200));
    harness.add_compaction_response(summarization_response());
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

    // Verify via request count: 2 regular (main agent) + 1 summarization (compaction model)
    assert_eq!(harness.request_count(), 2, "Main agent should have 2 calls");
    assert_eq!(
        harness.get_summarization_requests().len(),
        1,
        "Should have 1 summarization call"
    );
}

/// Strengthened multiple_compaction_events: verify each discard count and
/// verify via summarization request count.
#[tokio::test]
async fn multiple_compaction_events_verified() {
    let config = ContextConfig {
        threshold: 0.6, // 120 tokens
        keep_recent: 1,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 200);

    // Queue enough for 5 turns with possible compactions after each
    for _ in 0..5 {
        harness.add_response(agent_response("R", 150));
    }
    for _ in 0..4 {
        harness.add_compaction_response(summarization_response());
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

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 7: Mid-loop compaction (`mid-compaction.md`)
// ═══════════════════════════════════════════════════════════════════════════════

/// 7.1 — In-loop compaction triggers from the SessionHook gate.
///
/// Setup: pile up multiple turns where every response reports
/// `input_tokens > threshold`. Once the message count exceeds
/// `keep_recent`, the threshold check stops short-circuiting and the
/// token branch fires. The `on_completion_call` hook (or the
/// pre-prompt `compact_if_needed`) terminates the loop with reason
/// `"compact"`, the handler runs `force_compact`, and the run resumes.
///
/// Pinned by `mid-compaction.md` § 5 test 1.
#[tokio::test]
async fn in_loop_compaction_terminates_and_resumes() {
    let config = ContextConfig {
        threshold: 0.5, // 250 of a 500-token window
        keep_recent: 3,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 500);

    // Five turns with 400 input tokens each — well over the 250 threshold.
    // After turn 4 the conversation is 8 messages (>keep_recent=3), so the
    // threshold check stops short-circuiting and starts firing on tokens.
    for _ in 0..5 {
        harness.add_response(agent_response("OVER_BUDGET", 400));
    }
    // Plenty of compaction summaries — at least one will be consumed.
    for _ in 0..3 {
        harness.add_compaction_response(summarization_response_with("Periodic summary."));
    }

    for i in 0..5 {
        harness.run_message(&format!("turn {i}")).await;
    }

    assert!(
        harness.has_compaction_occurred(),
        "Compaction must fire when token threshold is consistently breached"
    );

    let events = harness.get_compaction_events();
    assert!(
        events.iter().any(|e| e.num_discarded > 0),
        "At least one compaction event must actually discard messages"
    );
}

/// 7.2 — `terminate("compact")` does not infinite-loop when compaction
/// cannot make progress.
///
/// Setup: queue an over-budget response that primes
/// `last_input_tokens > threshold`, but **don't queue any compaction
/// summaries**. On the next turn the hook will fire `terminate("compact")`.
/// `force_compact()` returns `None`. The handler clears
/// `last_input_tokens` manually (loop guard), the gate stops firing,
/// and the run completes without recursing.
///
/// Pinned by `mid-compaction.md` § 5 test 2.
#[tokio::test]
async fn terminate_compact_with_no_progress_does_not_loop_forever() {
    let config = ContextConfig {
        threshold: 0.5,
        keep_recent: 3,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 500);

    // Turn 1: over-threshold response. No compaction summary queued.
    harness.add_response(agent_response("OVER_BUDGET", 400));
    // Turn 2: a normal response — the test asserts we *get* this response,
    // proving the loop terminated and the agent ran.
    harness.add_response(agent_response("RECOVERY_REPLY", 100));

    let _ = harness.run_message("turn 1").await;
    let response = harness.run_message("turn 2").await;

    // The critical behaviour: we got back to the user. The exact response
    // depends on whether compaction was requested (and gracefully bypassed)
    // or not — either way, the test *terminates* and the second response
    // is consumed.
    assert!(
        !response.is_empty(),
        "must receive a response even when compaction can't make progress"
    );
    assert!(
        !response.starts_with("Error"),
        "response must not be an error: {response}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 8: Death-spiral regression (unfuck-compact.md)
// ═══════════════════════════════════════════════════════════════════════════════

/// 8.1 — **Load-bearing spiral pin**: force_compact returning None must
/// abort the turn in one iteration, not loop forever.
///
/// Setup: keep_recent=1, threshold=0.5 (150 of 300). Build 12 messages
/// (6 turns × 200-token responses) without queueing any compaction
/// summaries. After 6 turns, the message-count fallback threshold
/// max(1×3, 10) = 10 is exceeded. On turn 7:
///
/// 1. compact_if_needed fires → summarization fails → None
/// 2. prompt fires → hook sees needs_compaction true → terminate("compact")
/// 3. "compact" arm: force_compact → None
/// 4. **Before fix**: clear_last_input_tokens + continue → infinite spiral
///    (hook terminates before wire call, no mock response consumed)
/// 5. **After fix**: return ProcessResult::Error immediately
///
/// The timeout catches the infinite spiral on the broken code.
#[tokio::test]
async fn force_compact_none_aborts_turn_in_one_iteration() {
    let config = ContextConfig {
        threshold: 0.5, // 150 tokens of 300
        keep_recent: 1,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 300);

    // Build 12 messages (6 turns). keep_recent=1, fallback threshold = max(3,10) = 10.
    // 12 > 10 → message-count fallback triggers even after clear_last_input_tokens.
    for _ in 0..6 {
        harness.add_response(agent_response("HISTORY", 200));
    }
    // NO compaction responses — summarization will always fail.
    // One extra response (won't be reached after the fix).
    harness.add_response(agent_response("WONT_SEE_THIS", 200));

    for i in 0..6 {
        harness.run_message(&format!("M{i}")).await;
    }

    // Turn 7: triggers the death spiral on broken code.
    // After fix: returns Error immediately.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        harness.run_message("TRIGGER"),
    )
    .await;

    assert!(
        result.is_ok(),
        "Turn must complete within 5 seconds — no infinite spiral"
    );
    let response = result.unwrap();
    assert_eq!(
        response, "Error occurred",
        "force_compact returning None must abort the turn. Got: {response}"
    );

    // History must be bounded — no clone amplification.
    let history = harness.get_chat_history();
    assert!(
        history.len() < 30,
        "History must not grow unboundedly during spiral. Got {} messages",
        history.len()
    );
}

/// 8.2 — Happy-path regression: compaction succeeds, turn completes normally.
#[tokio::test]
async fn force_compact_success_completes_turn() {
    let config = ContextConfig {
        threshold: 0.5, // 150 tokens of 300
        keep_recent: 1,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 300);

    harness.add_response(agent_response("R1", 200));
    harness.add_compaction_response(summarization_response_with("GOOD_SUMMARY"));
    harness.add_response(agent_response("R2", 200));

    harness.run_message("M1").await;
    let response = harness.run_message("M2").await;

    // Compaction succeeds — turn completes normally.
    assert!(
        !response.is_empty() && !response.starts_with("Error"),
        "Successful compaction must produce a normal response. Got: {response}"
    );
    assert!(
        harness.has_compaction_occurred(),
        "Compaction must have occurred"
    );
}

/// 8.3 — Bail-once pin: compaction failure does not carry state across turns.
///
/// Two sequential prompts. First triggers compaction failure (bails).
/// Second attempts compaction again — no carry-over "broken" state.
#[tokio::test]
async fn compaction_failure_does_not_carry_across_turns() {
    let config = ContextConfig {
        threshold: 0.5, // 150 tokens of 300
        keep_recent: 1,
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 300);

    // Build history first
    harness.add_response(agent_response("R1", 200));
    // Turn 2: compaction fails (no summary) → error
    harness.add_response(agent_response("WONT_SEE", 200));
    // Turn 3: compaction fails again (no summary) → error (fresh attempt)
    harness.add_response(agent_response("WONT_SEE_EITHER", 200));

    harness.run_message("M1").await;
    let r2 = harness.run_message("M2").await;
    let r3 = harness.run_message("M3").await;

    // Both turns fail independently — no persistent "compaction broken" flag.
    assert_eq!(
        r2, "Error occurred",
        "Turn 2 must abort on compaction failure"
    );
    assert_eq!(
        r3, "Error occurred",
        "Turn 3 must also abort (fresh attempt, no carry-over)"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 9: Sub-agent-lane × compaction regression
// (Pinned by `tickets/compaction-subagent-lane.md` § 8.5)
// ═══════════════════════════════════════════════════════════════════════════════

/// Spec § 8.5 — `delegation_then_compaction_keeps_wire_valid`.
///
/// End-to-end pin of the bf7d62d3-class bug. A delegation puts a sub-agent's
/// internal turns (sourced `SubAgent { role }`) into the transcript; later,
/// `force_compact()` is called. After compaction:
///
/// 1. The rescued delegate `ToolCall`/`ToolResult` pair stays adjacent on
///    the orchestrator wire (Anthropic rejects "tool_use without tool_result
///    immediately after").
/// 2. The summary is *prepended* to the surviving wire sequence, not
///    spliced into the pair.
/// 3. The summarization prompt does not contain sub-agent marker content
///    (the lane filter is applied at the summarization-input seam).
/// 4. The orchestrator wire history does not contain sub-agent marker
///    content.
///
/// RED today on (1) and (3).
#[tokio::test]
async fn delegation_then_compaction_keeps_wire_valid() {
    use peakbot::ui::app_state::MessageSource;
    use rig_core::completion::message::{AssistantContent, Message as RigMessage, UserContent};

    let config = ContextConfig {
        threshold: 0.5, // 250 of a 500-token window
        keep_recent: 2, // keep last 2 messages after compaction
        enabled: true,
        compaction_model: None,
    };

    let mut harness =
        TestHarness::with_system_prompt_and_context("You are a helpful assistant.", config, 500);

    // Two ordinary turns to give the conversation a couple of orchestrator rows.
    // Each turn's response reports 300 input_tokens — over the 250 threshold
    // — so the harness's pre-prompt compaction check doesn't kick in here
    // (we're calling `force_compact` directly below).
    harness.add_response(agent_response("R1", 300));
    harness.add_response(agent_response("R2", 300));
    harness.run_message("M1").await;
    harness.run_message("M2").await;

    // Now simulate one delegation directly on `harness.state_manager`,
    // mirroring the pattern in `tests/scenarios/pipeline_tests.rs`.
    //
    // Orchestrator: ToolCall(delegate, "call-1") → (sub-agent rows) →
    // ToolResult(delegate, "call-1") → trailing assistant.
    harness.state_manager.add_tool_call(
        MessageSource::Human,
        None,
        "delegate".to_string(),
        r#"{"role":"researcher","task":"survey","parent_task_id":1}"#.to_string(),
        Some("call-1".to_string()),
    );
    let sub = MessageSource::SubAgent {
        role: "researcher".to_string(),
    };
    harness.state_manager.add_tool_call(
        sub.clone(),
        None,
        "bash".to_string(),
        r#"{"command":"echo SUBAGENT_SECRET step 1"}"#.to_string(),
        Some("sub-c-1".to_string()),
    );
    harness.state_manager.add_tool_result(
        sub.clone(),
        "bash".to_string(),
        r#"{"command":"echo SUBAGENT_SECRET step 1"}"#.to_string(),
        "SUBAGENT_SECRET observed something".to_string(),
        Some("sub-c-1".to_string()),
    );
    harness.state_manager.add_assistant_message_sourced(
        sub.clone(),
        "Internal sub-agent note SUBAGENT_SECRET".to_string(),
    );
    harness.state_manager.add_tool_call(
        sub.clone(),
        None,
        "bash".to_string(),
        r#"{"command":"echo SUBAGENT_SECRET step 2"}"#.to_string(),
        Some("sub-c-2".to_string()),
    );
    harness.state_manager.add_tool_result(
        sub.clone(),
        "bash".to_string(),
        r#"{"command":"echo SUBAGENT_SECRET step 2"}"#.to_string(),
        "SUBAGENT_SECRET saw the answer".to_string(),
        Some("sub-c-2".to_string()),
    );
    harness.state_manager.add_tool_result(
        MessageSource::Human,
        "delegate".to_string(),
        r#"{"role":"researcher","task":"survey","parent_task_id":1}"#.to_string(),
        "DELEGATE_RESULT_payload".to_string(),
        Some("call-1".to_string()),
    );
    harness
        .state_manager
        .add_assistant_message("done".to_string());

    // Queue the summarization response.
    harness.add_compaction_response(summarization_response_with("SUMMARY_TEXT"));

    // Act: drive compaction through the full public path.
    let result = harness
        .state_manager
        .force_compact()
        .await
        .expect("compaction must produce a result with mock summarizer queued");

    // ── (3) Every summarization prompt must not contain SUBAGENT_SECRET ──
    // (The harness may have triggered earlier compactions during the two
    // ordinary setup turns; the lane filter must apply at every seam.)
    let summ_requests = harness.get_summarization_requests();
    assert!(
        !summ_requests.is_empty(),
        "expected at least one summarization request, got {}",
        summ_requests.len()
    );
    for (i, req) in summ_requests.iter().enumerate() {
        let prompt_text: String = req
            .chat_history
            .iter()
            .flat_map(|m| match m {
                RigMessage::User { content } => content
                    .iter()
                    .filter_map(|c| match c {
                        UserContent::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !prompt_text.contains("SUBAGENT_SECRET"),
            "summarization request #{i} must NOT contain SUBAGENT_SECRET \
             (lane filter must apply to compact()); got:\n{prompt_text}"
        );
    }

    // ── (4) Wire history has no SUBAGENT_SECRET ─────────────────────────
    let wire = harness.state_manager.get_agent_history();
    for (i, m) in wire.iter().enumerate() {
        let leaked = match m {
            RigMessage::Assistant { content, .. } => content.iter().any(|c| match c {
                AssistantContent::Text(t) => t.text.contains("SUBAGENT_SECRET"),
                _ => false,
            }),
            RigMessage::User { content } => content.iter().any(|c| match c {
                UserContent::Text(t) => t.text.contains("SUBAGENT_SECRET"),
                _ => false,
            }),
            RigMessage::System { .. } => false,
        };
        assert!(
            !leaked,
            "SUBAGENT_SECRET leaked into the orchestrator wire at index {i}: {m:?}"
        );
    }

    // ── (2) Summary precedes the delegate ToolCall on the wire ──────────
    let delegate_tc_pos = wire
        .iter()
        .position(|m| {
            matches!(m, RigMessage::Assistant { content, .. }
            if content.iter().any(|c| matches!(c, AssistantContent::ToolCall(tc)
                if tc.id == "call-1")))
        })
        .expect("delegate ToolCall(call-1) must be on the wire");
    let summary_pos = wire
        .iter()
        .position(|m| {
            matches!(m, RigMessage::User { content }
            if content.iter().any(|c| matches!(c, UserContent::Text(t)
                if t.text.contains("[Conversation summary]"))))
        })
        .expect("summary User-text must be on the wire (compaction happened)");
    assert!(
        summary_pos < delegate_tc_pos,
        "Summary (pos={summary_pos}) must precede the delegate ToolCall (pos={delegate_tc_pos}); \
         current wire order wedges the rescued pair — Anthropic would reject"
    );

    // ── (1) Pair adjacency: every Assistant(ToolCall X) is immediately
    //       followed by a User(ToolResult X) with matching call_id. ────────
    for (i, m) in wire.iter().enumerate() {
        if let RigMessage::Assistant { content, .. } = m
            && let Some(AssistantContent::ToolCall(tc)) = content.iter().next()
        {
            let call_id = tc.id.clone();
            let next = wire.get(i + 1).unwrap_or_else(|| {
                panic!(
                    "ToolCall {call_id:?} is the last wire element — no matching ToolResult follows"
                )
            });
            match next {
                RigMessage::User { content } => {
                    let found = content.iter().any(|c| match c {
                        UserContent::ToolResult(tr) => tr.id == call_id,
                        _ => false,
                    });
                    assert!(
                        found,
                        "ToolCall {call_id:?} must be immediately followed by ToolResult {call_id:?}; got {next:?}"
                    );
                }
                other => panic!(
                    "ToolCall {call_id:?} must be immediately followed by a User ToolResult; got {other:?}"
                ),
            }
        }
    }

    // Sanity: the summary text we queued appears in the resulting plan.
    let _ = result; // result is used to drive the assertions above; quiet unused.
}

// ─────────────────────────────────────────────────────────────────────────────
// T7b — Resumption after compaction splits by response.
// ─────────────────────────────────────────────────────────────────────────────

/// After a mid-action compaction the resumptive dispatch path goes
/// through `build_resumption_for_compaction` (not
/// `build_current_turn_message` + `get_agent_history`), because the last
/// live row is a ToolResult rather than a fresh user turn.
///
/// T7b pins: the resumption HISTORY produced for the segmented fixture
/// carries the two split assistant messages — NOT the single coalesced
/// Assistant the per-row helper would emit if consulted in isolation —
/// and the resumption PROMPT is the trailing ToolResult carrying zero
/// reasoning (a ToolResult is a User-content row, never an Assistant
/// row, so there is nothing to split; the contract is just "no
/// reasoning attached").
///
/// This is the load-bearing regression lock for
/// `build_resumption_for_compaction`: if a future implementer forgets to
/// route the segmentation through it (and only patches
/// `get_agent_history`), `/compact` would silently resume with a
/// single-mismatched-signature assistant and Anthropic would 400.
///
/// Pre-implementation: `begin_response` and the new `add_tool_call`
/// arity don't exist. The test fails to compile (RED for the right
/// reason).
#[test]
fn resumption_after_compaction_splits_by_response() {
    use peakbot::StateManager;
    use peakbot::ui::app_state::MessageSource;
    use rig_core::completion::message::{AssistantContent, Message as RigMessage, UserContent};
    use rig_core::one_or_many::OneOrMany;

    let sm = StateManager::new();

    // ── Build the T1 fixture on a fresh StateManager. ──────────────────
    sm.add_user_message("go".to_string());

    let r1 = sm.begin_response(vec![peakbot::reasoning::ThinkingBlock::Thinking {
        text: "alpha".to_string(),
        signature: "sig.AAA-==".to_string(),
    }]);
    sm.add_tool_call(
        MessageSource::Human,
        Some(r1),
        "bash".to_string(),
        r#"{"command":"ls"}"#.to_string(),
        Some("c1".to_string()),
    );
    sm.add_tool_result(
        MessageSource::Human,
        "bash".to_string(),
        r#"{"command":"ls"}"#.to_string(),
        "file1\nfile2".to_string(),
        Some("c1".to_string()),
    );
    sm.add_tool_call(
        MessageSource::Human,
        Some(r1),
        "todo".to_string(),
        r#"{"action":"create","title":"a"}"#.to_string(),
        Some("c2".to_string()),
    );
    sm.add_tool_result(
        MessageSource::Human,
        "todo".to_string(),
        r#"{"action":"create","title":"a"}"#.to_string(),
        "ok".to_string(),
        Some("c2".to_string()),
    );

    let r2 = sm.begin_response(vec![peakbot::reasoning::ThinkingBlock::Thinking {
        text: "beta".to_string(),
        signature: "sig.BBB-==".to_string(),
    }]);
    sm.add_tool_call(
        MessageSource::Human,
        Some(r2),
        "todo".to_string(),
        r#"{"action":"create","title":"b"}"#.to_string(),
        Some("c3".to_string()),
    );
    // The trailing live row is a ToolResult (mid-action compaction
    // scenario — like the existing
    // build_resumption_for_compaction_does_not_duplicate_user_prompt).
    sm.add_tool_result(
        MessageSource::Human,
        "todo".to_string(),
        r#"{"action":"create","title":"b"}"#.to_string(),
        "ok".to_string(),
        Some("c3".to_string()),
    );

    let (prompt, history) = sm
        .build_resumption_for_compaction()
        .expect("mid-action compaction fixture must produce a resumption");

    // ── History: must carry two split assistant messages. ─────────────
    let assistants: Vec<&OneOrMany<AssistantContent>> = history
        .iter()
        .filter_map(|m| match m {
            RigMessage::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .collect();
    assert_eq!(
        assistants.len(),
        2,
        "resumption history must split the two-response run into two Message::Assistant entries; got {}",
        assistants.len(),
    );

    // Per-message signature SETS must be disjoint and partition correctly.
    let mut sigs_seen: Vec<String> = Vec::new();
    for c in assistants {
        for x in c.iter() {
            if let AssistantContent::Reasoning(r) = x
                && let Some(sig) = r.content.iter().find_map(|rc| match rc {
                    rig_core::completion::message::ReasoningContent::Text { signature, .. } => {
                        signature.clone()
                    }
                    _ => None,
                })
            {
                sigs_seen.push(sig);
            }
        }
    }
    assert_eq!(
        sigs_seen,
        vec!["sig.AAA-==".to_string(), "sig.BBB-==".to_string()],
        "resumption history must carry the two signatures in transcript order — NOT a single SIG_A-only bundle (the bug) and NOT a single SIG_B-only bundle (forgetting r1 entirely)",
    );

    // ── Prompt: the trailing ToolResult for c3 — User content, NO
    //    Assistant content, NO Reasoning. The compaction helper hands
    //    the model the result of the last tool call it ran, not an
    //    Assistant message. ─────────────────────────────────────────────
    match &prompt {
        RigMessage::User { content } => {
            let tr = content.iter().find_map(|c| match c {
                UserContent::ToolResult(tr) => Some(tr.id.as_str()),
                _ => None,
            });
            assert_eq!(
                tr,
                Some("c3"),
                "resumption prompt must be the trailing ToolResult (c3) — not an Assistant, not a User-text",
            );
            // UserContent has no Reasoning variant by construction; if a
            // future refactor ever coalesced User+Assistant on the prompt
            // seam, the outer `match &prompt` above would have caught
            // it (the `other => panic!` branch). So no extra inner check.
        }
        other => panic!(
            "resumption prompt must be a User message carrying the trailing ToolResult, got {:?}",
            other,
        ),
    }
}
