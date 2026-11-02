//! N8 — `StateManager::turn_has_reply` scope (ticket #294 rework).
//!
//! Written FIRST against the locked design: `turn_has_reply` scans the
//! transcript backwards over ORCHESTRATOR-LANE rows, stopping at the first
//! User row (any user row — human or background — starts a turn), and
//! answers "does this turn already have an Agent row?".
//!
//! This is a SEPARATE integration target on purpose: `turn_has_reply` does
//! not exist yet, so this file fails to COMPILE — the intended RED — while
//! every other test target still compiles and runs.
use peakbot::state::StateManager;
use peakbot::ui::app_state::MessageSource;

/// (a) A previous turn's reply does not count: once the next user message
/// lands, the fresh turn has no reply yet.
#[test]
fn previous_turn_reply_does_not_count() {
    let sm = StateManager::new();
    sm.add_user_message("first".to_string());
    sm.add_assistant_message("answer one".to_string());
    assert!(
        sm.turn_has_reply(),
        "a turn with an agent row reports a reply"
    );

    sm.add_user_message("second".to_string());
    assert!(
        !sm.turn_has_reply(),
        "the new user message starts a turn with no reply yet"
    );
}

/// (b) A Background user row starts a turn, exactly like a human one.
#[test]
fn background_user_row_starts_a_turn() {
    let sm = StateManager::new();
    sm.add_user_message("first".to_string());
    sm.add_assistant_message("answer one".to_string());
    sm.add_user_message_from_background(
        "[bg output] ─── #1 `npm test` (3 new lines) ───".to_string(),
        vec![1],
    );
    assert!(
        !sm.turn_has_reply(),
        "a background user row is a turn start — no reply yet"
    );
}

/// (c) Sub-agent Agent rows do not count: only orchestrator-lane rows are
/// replies.
#[test]
fn sub_agent_agent_rows_do_not_count() {
    let sm = StateManager::new();
    sm.add_user_message("first".to_string());
    sm.add_assistant_message_sourced(
        MessageSource::SubAgent {
            role: "junior".to_string(),
        },
        "sub-agent chatter".to_string(),
    );
    assert!(
        !sm.turn_has_reply(),
        "a sub-agent row is not an orchestrator reply"
    );
}

/// (d) Compacted rows count: a compacted Agent row still answers "the turn
/// had a reply".
#[test]
fn compacted_rows_count() {
    let sm = StateManager::new();
    sm.add_user_message("first".to_string());
    sm.add_assistant_message("answer one".to_string());

    let mut chat = sm.get_state().chat;
    // Compact the reply (the user row stays live — the turn start).
    chat.messages
        .iter_mut()
        .filter(|m| m.role == peakbot::ui::app_state::MessageRole::Agent)
        .for_each(|m| m.compacted = true);
    sm.update_chat_state(chat);

    assert!(
        sm.turn_has_reply(),
        "a compacted agent row still counts as the turn's reply"
    );
}
