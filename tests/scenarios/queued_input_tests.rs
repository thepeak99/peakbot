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
