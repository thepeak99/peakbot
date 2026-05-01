//! Queued-input regression guards (see make-flow-great-again.md).
//!
//! ## Bug class being pinned
//!
//! When the user typed a message while a tool call was in flight, the
//! production event loop used to call `add_user_message` synchronously —
//! splicing user text **between** the recorded `ToolCall(X)` and the
//! not-yet-arrived `ToolResult(X)`. The next prompt's `chat_history` then
//! carried a `tool_result` separated from its `tool_use` by a stray user
//! message, which Anthropic refuses outright and OpenAI flags as an unknown
//! `tool_call_id`.
//!
//! ## Fix
//!
//! `event_loop` no longer writes user input to chat. The single writer is
//! `agent_loop`, which appends `add_user_message{,_with_attachments}` at
//! dequeue time, **between** turns. The fix is structural — once the call
//! site moves to a task that only runs between turns, the bug becomes
//! impossible to express.
//!
//! ## Test layering
//!
//! - **State-manager unit tests** (`src/state/state_manager.rs::tests`):
//!   pin the `pending_input_count` mutators in isolation.
//! - **This file**: pins the *event* and *AppState* ordering through the
//!   `TestRunner`-shaped flow (which now mirrors agent_loop ordering).
//! - **`tests/scenarios/stop_tests.rs`**: existing /stop coverage.

use peakbot::AgentEvent;
use peakbot::mock::MockResponse;
use peakbot::ui::app_state::MessageRole;

use super::super::harness::TestHarness;

/// **The new ordering invariant.** Under the agent_loop / TestRunner mirror,
/// the user message lands in `state.chat.messages` *before* the agent's
/// reply, never after. The legacy "append after prompt" ordering would have
/// produced `[Agent, User]`; the fix produces `[User, Agent]`.
#[tokio::test]
async fn user_message_precedes_assistant_in_chat_after_run() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("hi back"));

    harness.run_message("hello").await;

    let messages = harness.get_state().chat.messages;
    assert!(
        messages.len() >= 2,
        "expected at least User + Agent messages, got {} ({:?})",
        messages.len(),
        messages.iter().map(|m| m.role).collect::<Vec<_>>(),
    );

    // Find the first User and the first Agent message and assert ordering.
    let first_user = messages.iter().position(|m| m.role == MessageRole::User);
    let first_agent = messages.iter().position(|m| m.role == MessageRole::Agent);
    assert!(
        matches!((first_user, first_agent), (Some(u), Some(a)) if u < a),
        "User message must appear before Agent reply; got roles {:?}",
        messages.iter().map(|m| m.role).collect::<Vec<_>>(),
    );
}

/// Multi-turn variant: each new user message lands AFTER the previous turn's
/// assistant reply, in queue order. The legacy bug shape allowed user
/// messages from later turns to wedge in earlier turns; the fix makes that
/// impossible.
#[tokio::test]
async fn multi_turn_chat_preserves_user_assistant_alternation() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("reply 1"));
    harness.add_response(MockResponse::text("reply 2"));
    harness.add_response(MockResponse::text("reply 3"));

    harness.run_message("first").await;
    harness.run_message("second").await;
    harness.run_message("third").await;

    let messages = harness.get_state().chat.messages;
    let roles: Vec<MessageRole> = messages.iter().map(|m| m.role).collect();

    // Filter out non-conversation roles (System "worked for ..." messages
    // would appear in production but TestRunner doesn't emit them).
    let convo: Vec<MessageRole> = roles
        .iter()
        .copied()
        .filter(|r| matches!(r, MessageRole::User | MessageRole::Agent))
        .collect();

    assert_eq!(
        convo,
        vec![
            MessageRole::User,
            MessageRole::Agent,
            MessageRole::User,
            MessageRole::Agent,
            MessageRole::User,
            MessageRole::Agent,
        ],
        "expected strict User/Agent alternation across 3 turns; got {convo:?} \
         (full roles: {roles:?})",
    );
}

/// **Event-channel ordering.** A tool-call response produces, in order,
/// `ToolCall → ToolResult → CompletionResponse`. The legacy bug-class
/// happens at the consumer side (event_loop or event_processor calling
/// `add_user_message` between the ToolCall and ToolResult writes). Pinning
/// the producer side here documents what the consumer must preserve and
/// catches regressions in MockCompletionModel that would silently reorder
/// events.
#[tokio::test]
async fn tool_call_events_arrive_in_call_then_result_order() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::tool_call_with_follow_up(
        "todo",
        serde_json::json!({"action": "add", "tasks": ["X"]}),
        "Added X.",
    ));
    harness.add_response(MockResponse::text("Added X."));

    harness.run_message("Add a todo X").await;

    let events = harness.drain_events();

    // Find ToolCall and ToolResult positions.
    let call_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCall { .. }));
    let result_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolResult { .. }));

    let call_idx = call_idx.expect("expected at least one ToolCall event");
    let result_idx = result_idx.expect("expected at least one ToolResult event");

    assert!(
        call_idx < result_idx,
        "ToolCall must arrive before its ToolResult; got events {:?}",
        events
            .iter()
            .map(|e| format!("{e:?}").chars().take(40).collect::<String>())
            .collect::<Vec<_>>(),
    );
}

/// **Pending-input counter end-to-end.** Mirrors the event_loop /
/// agent_loop / /stop sequence at the StateManager level. This is the data
/// the `⏳ N queued` status-bar hint reads in `render_status_bar`.
#[tokio::test]
async fn pending_input_counter_lifecycles_correctly() {
    use peakbot::StateManager;
    use std::sync::Arc;

    let sm = Arc::new(StateManager::new());

    // Mirrors event_loop: three sends in a row.
    sm.increment_pending_input();
    sm.increment_pending_input();
    sm.increment_pending_input();
    assert_eq!(sm.get_state().pending_input_count, 3);

    // Mirrors agent_loop dequeue.
    sm.decrement_pending_input();
    assert_eq!(sm.get_state().pending_input_count, 2);

    // Mirrors event_loop /stop drain — zero immediately, regardless of
    // remaining queued items.
    sm.set_pending_input_count(0);
    assert_eq!(sm.get_state().pending_input_count, 0);

    // Underflow is saturating, not panicking.
    sm.decrement_pending_input();
    assert_eq!(sm.get_state().pending_input_count, 0);
}

/// Pins that queued user messages are dispatched as separate turns, NOT
/// concatenated. See full rationale in the doc-comment on
/// `queued_messages_are_sent_as_separate_turns_not_glued` below.
#[tokio::test]
async fn queued_messages_are_sent_as_separate_turns_not_glued() {
    use rig::completion::message::{Message as RigMessage, UserContent};

    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("ack 1"));
    harness.add_response(MockResponse::text("ack 2"));
    harness.add_response(MockResponse::text("ack 3"));

    // Three queued messages (e.g. user typed three follow-ups while a
    // tool was running). TestRunner::run_message mirrors agent_loop's
    // per-message dequeue path, so calling it three times back-to-back
    // is the closest faithful repro of three QueueMessage::UserMessage
    // items dequeued in order.
    harness.run_message("first message").await;
    harness.run_message("second message").await;
    harness.run_message("third message").await;

    // 1) One LLM request per queued message - no batching.
    let requests = harness.get_recorded_requests();
    assert_eq!(
        requests.len(),
        3,
        "expected exactly 3 LLM requests (one per queued message); got {} \
         - if this is < 3, queued messages were batched into a single prompt",
        requests.len(),
    );

    // 2) Each request's own prompt is exactly the one message text -
    // never a glued concatenation. The prompt is the last message in
    // chat_history.
    let expected_prompts = ["first message", "second message", "third message"];
    for (i, expected) in expected_prompts.iter().enumerate() {
        let last = requests[i]
            .chat_history
            .last()
            .unwrap_or_else(|| panic!("request {i} has empty chat_history"));
        let RigMessage::User { content } = last else {
            panic!("request {i}'s last message is not a User message: {last:?}");
        };
        let text = content
            .iter()
            .find_map(|c| {
                if let UserContent::Text(t) = c {
                    Some(t.text.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("request {i}'s User message has no Text content"));

        assert_eq!(
            text, *expected,
            "request {i}: prompt text should be exactly {expected:?} (the one queued \
             message), not a glued/concatenated blob. Got {text:?}.",
        );

        // Defence-in-depth: if the equality above ever loosens, these
        // explicit negative assertions still catch the specific gluing
        // shapes worried about ("a b c", "a\nb\nc", "abc", etc).
        assert!(
            !text.contains("first message") || i == 0,
            "request {i}: prompt unexpectedly contains 'first message' (glued?). text = {text:?}",
        );
        assert!(
            !text.contains("second message") || i == 1,
            "request {i}: prompt unexpectedly contains 'second message' (glued?). text = {text:?}",
        );
        assert!(
            !text.contains("third message") || i == 2,
            "request {i}: prompt unexpectedly contains 'third message' (glued?). text = {text:?}",
        );
    }

    // 3) By the third turn, chat_history visible to the model contains
    // all three user messages as DISTINCT User entries, in order - not
    // collapsed into one. A glued history would yield a single entry
    // like "first message\nsecond message\nthird message".
    let third = &requests[2];
    let user_entries: Vec<String> = third
        .chat_history
        .iter()
        .filter_map(|msg| {
            if let RigMessage::User { content } = msg {
                content.iter().find_map(|c| {
                    if let UserContent::Text(t) = c {
                        Some(t.text.clone())
                    } else {
                        None
                    }
                })
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        user_entries,
        vec![
            "first message".to_string(),
            "second message".to_string(),
            "third message".to_string(),
        ],
        "third request's chat_history should contain three separate User entries in order, \
         one per queued message. A single glued entry would prove batching.",
    );
}
