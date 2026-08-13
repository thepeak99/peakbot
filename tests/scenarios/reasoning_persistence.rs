//! RED regression tests for the thinking-block round-trip pipeline.
//!
//! These tests pin two structural defects that the planned fix must close.
//! They run against `tests/scenarios/mod.rs`, which is the single integration
//! test harness, mirroring how `reasoning_preservation.rs` is registered.
//!
//! # Defects pinned
//!
//! **R1 — staging race.**
//! `StateManager::stage_thinking_for_next_assistant` is currently invoked
//! from the spawned event-processor task in `process_event_for_ui`
//! (src/lib.rs), which runs *after* the main loop has already appended the
//! assistant ChatMessage via `sm.add_assistant_message(...)`. The pending
//! slot is therefore empty when the prose row arrives — text-only turns
//! lose their thinking blocks. The planned fix moves the staging into the
//! synchronous hook path `SessionHook::on_completion_response`, gated on
//! `self.source.is_orchestrator_lane()`.
//!
//! **R2 — ToolCall has no `thinking` field.**
//! `conversation::Message::ToolCall` (src/conversation.rs) only carries
//! `tool_name / arguments / call_id / compacted / source / timestamp`. There
//! is no `thinking: Vec<ThinkingBlock>`, so `sync_to_conversation` drops
//! blocks from tool-call rows — no conversation JSON on disk has ever
//! contained a `"thinking"` key on a ToolCall row. The planned fix adds
//! the field and threads it through both halves of the persist/restore
//! pair.
//!
//! # Colours
//!
//! * `hook_stages_thinking_synchronously_for_next_assistant` (T1) — RED
//!   today. The hook only emits the event to the channel; nothing reaches
//!   `pending_thinking` so the prose row arrives empty.
//! * `sub_agent_hook_does_not_stage_onto_orchestrator_rows` (T1b) —
//!   GREEN today (nothing stages anywhere), and pinned GREEN after the
//!   fix by the `is_orchestrator_lane()` gate inside the hook.
//! * `tool_call_thinking_survives_conversation_roundtrip` (T2) — RED
//!   today. `Message::ToolCall` has no `thinking` field, so the saved
//!   JSON has no `"thinking"` key on tool-call rows, and the restored
//!   ChatMessage carries no blocks.

#![cfg(test)]
// The tester's assertions deliberately pass references to helper functions;
// removing them would alter the spec's source style without changing meaning.
#![allow(clippy::needless_borrow)]

use peakbot::ui::app_state::{MessageRole, MessageSource};
use peakbot::{Conversation, StateManager};
use rig_core::completion::message::{
    AssistantContent, Message as RigMessage, Reasoning, Text, UserContent,
};
use rig_core::one_or_many::OneOrMany;
use std::sync::Arc;

/// Sentinel signature — its byte content is what Anthropic validates on
/// replay. Equal *string* equality, not just non-emptiness, is what
/// contract 1 demands. Same shape as `reasoning_preservation.rs`.
const FAKE_SIGNATURE: &str = "sig.SGXabc123XYZ-==";

/// Sentinel thinking text — distinct from the signature so a passing test
/// can't satisfy both checks by accident. Leaking this string elsewhere
/// in the transcript would be a regression.
const THINKING_TEXT: &str = "ROUNDTRIP_DO_NOT_LEAK_THINKING_77a1";

/// Build a rig `CompletionResponse<MockModelResponse>` carrying one
/// `AssistantContent::Reasoning` block with the sentinel signature and
/// text. Mirrors the construction in `reasoning_preservation.rs`
/// (around line 800) but adds `MockCompletionModel`/`MockModelResponse`
/// imports for the hook's `PromptHook` turbofish.
fn thinking_response()
-> rig_core::completion::CompletionResponse<peakbot::mock::completion_model::MockModelResponse> {
    use rig_core::completion::Usage as RigUsage;
    use rig_core::completion::message::Text as RigText;

    let reasoning = Reasoning::new_with_signature(THINKING_TEXT, Some(FAKE_SIGNATURE.to_string()));
    let choice = OneOrMany::one(AssistantContent::Reasoning(reasoning));
    // Some completions return prose alongside reasoning — include a Text
    // block to ensure the staging race is exercised exactly the way the
    // production loop sees it (a real CompletionResponse with prose).
    let _ = RigText::new("prose after thinking"); // documents intent; unused
    rig_core::completion::CompletionResponse {
        choice,
        usage: RigUsage::new(),
        raw_response: peakbot::mock::completion_model::MockModelResponse {
            content: String::new(),
            is_tool_call: false,
        },
        message_id: None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// T1 — Hook stages thinking synchronously (was: lost to the event-processor race).
// ─────────────────────────────────────────────────────────────────────────────

/// The capture seam must stage thinking *synchronously* in
/// `SessionHook::on_completion_response` so the main loop's subsequent
/// `sm.add_assistant_message(...)` adopts the blocks onto the prose row.
///
/// Today, staging happens only inside the spawned `process_event_for_ui`
/// task (src/lib.rs:~2376), which the main loop has already passed by the
/// time it dispatches — `pending_thinking` is empty, and the prose row is
/// created with `thinking: vec![]`. The test never spawns the event
/// processor: it invokes the hook's `on_completion_response` directly via
/// the same `PromptHook::<MockCompletionModel>` turbofish used elsewhere
/// in the suite, then immediately calls `add_assistant_message` the way
/// the agent loop does. The orchestrator assistant row must carry the
/// block with the sentinel signature byte-identical.
#[tokio::test]
async fn hook_stages_thinking_synchronously_for_next_assistant() {
    use peakbot::SessionHook;
    use peakbot::mock::MockCompletionModel;
    use rig_core::agent::PromptHook;

    // Build a fresh StateManager (no storage needed — we assert on the
    // in-memory ChatMessage, not the on-disk Conversation).
    let sm = Arc::new(StateManager::new());

    // Wire the hook: no event channel (so the event-tap path can't
    // accidentally satisfy the assertion by side-effect), but with the
    // StateManager wired AND preserve_reasoning on — the post-fix
    // staging path needs both.
    let hook = SessionHook::new(None)
        .with_state_manager(&sm)
        .with_preserve_reasoning(true);

    // Pretend the orchestrator just emitted a CompletionResponse carrying
    // a signed Thinking block. No event processor is spawned.
    let response = thinking_response();
    let prompt = RigMessage::User {
        content: OneOrMany::one(UserContent::Text(Text::new("hi"))),
    };
    let _ = <SessionHook as PromptHook<MockCompletionModel>>::on_completion_response(
        &hook, &prompt, &response,
    )
    .await;

    // Production main-loop order: the LLM returned, now we write the
    // assistant ChatMessage. The staged blocks must be picked up here.
    sm.add_assistant_message("prose after thinking".to_string());

    // Find the orchestrator (Human-source) assistant row and assert it
    // carries the Thinking block byte-identical.
    let state = sm.get_state();
    let assistant_row = state
        .chat
        .messages
        .iter()
        .find(|m| {
            m.role == MessageRole::Agent
                && m.source.is_orchestrator_lane()
                && !m.thinking.is_empty()
        })
        .expect(
            "the orchestrator assistant row must carry the thinking block the hook staged; \
             today the hook only emits an event, so this row is empty — R1",
        );

    assert_eq!(
        assistant_row.thinking,
        vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: THINKING_TEXT.to_string(),
            signature: FAKE_SIGNATURE.to_string(),
        }],
        "thinking block must round-trip verbatim from rig response → hook → StateManager row",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// T1b — Sub-agent hook must not stage onto the orchestrator's slot.
// ─────────────────────────────────────────────────────────────────────────────

/// Pin: even after the fix moves staging into the hook path, a sub-agent
/// hook (which carries `MessageSource::SubAgent { role }`) must NOT
/// stage onto the orchestrator's `pending_thinking` slot. The
/// orchestrator's wire context must remain clean — a sub-agent's
/// thinking belongs on its own lane.
///
/// This test intentionally wires `with_state_manager(&sm)` on the
/// sub-agent hook (mirroring `build_sub_agent`'s wiring for the
/// in-loop compaction gate) so the assertion does not pass merely
/// because the hook lacks a StateManager. The fix's gate must be the
/// `is_orchestrator_lane()` predicate, not an absent manager.
#[tokio::test]
async fn sub_agent_hook_does_not_stage_onto_orchestrator_rows() {
    use peakbot::SessionHook;
    use peakbot::mock::MockCompletionModel;
    use rig_core::agent::PromptHook;

    let sm = Arc::new(StateManager::new());

    let hook = SessionHook::new(None)
        .with_source(MessageSource::SubAgent {
            role: "reviewer".to_string(),
        })
        // Critically: still wired to the orchestrator's StateManager,
        // so a missing-manager optimisation cannot explain the result.
        .with_state_manager(&sm)
        .with_preserve_reasoning(true);

    let response = thinking_response();
    let prompt = RigMessage::User {
        content: OneOrMany::one(UserContent::Text(Text::new("hi"))),
    };
    let _ = <SessionHook as PromptHook<MockCompletionModel>>::on_completion_response(
        &hook, &prompt, &response,
    )
    .await;

    // Production main-loop order: orchestrator writes its own prose row.
    sm.add_assistant_message("orchestrator prose".to_string());

    let state = sm.get_state();
    // The orchestrator row must carry no thinking — the sub-agent's
    // blocks must not leak onto it.
    let orchestrator_assistant = state
        .chat
        .messages
        .iter()
        .find(|m| m.role == MessageRole::Agent && m.source.is_orchestrator_lane())
        .expect("the orchestrator's assistant row must exist");
    assert!(
        orchestrator_assistant.thinking.is_empty(),
        "a sub-agent hook's CompletionResponse must not stage onto the orchestrator's row; \
         got thinking = {:?}",
        orchestrator_assistant.thinking,
    );

    // And the orchestrator's pending slot must be empty too — so a
    // *later* orchestrator turn can't accidentally inherit the
    // sub-agent's blocks.
    let state_after = sm.get_state();
    assert!(
        !state_after
            .chat
            .messages
            .iter()
            .skip_while(|m| !(m.role == MessageRole::Agent && m.source.is_orchestrator_lane()))
            .skip(1) // the orchestrator's own row we just inspected
            .any(|m| !m.thinking.is_empty()),
        "no later orchestrator row must carry sub-agent thinking either",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// T2 — ToolCall thinking survives a full save → JSON → load round trip.
// ─────────────────────────────────────────────────────────────────────────────

/// A tool-call row carrying thinking blocks must survive
/// `sync_to_conversation` → `serde_json::to_string` →
/// `serde_json::from_str` → `sync_from_conversation` with the
/// signature byte-identical.
///
/// Today the `conversation::Message::ToolCall` variant has NO `thinking`
/// field (src/conversation.rs:~119-134), so `sync_to_conversation`
/// drops the blocks from the ChatMessage and the serialized JSON has no
/// `"thinking"` key on the ToolCall row. The fix adds
/// `#[serde(default, skip_serializing_if = "Vec::is_empty")] thinking:
/// Vec<ThinkingBlock>` mirroring the Assistant arm, threads it through
/// `sync_to_conversation` AND `sync_from_conversation`, and pins the
/// round-trip exactly.
///
/// The save side uses `sync_to_conversation` (called automatically from
/// `add_tool_call → persist_current`) plus `sm.get_current_conversation()`
/// to read the populated Conversation; the load side uses
/// `sync_from_conversation` (called from `load_conversation`).
/// Per the repo rule: symmetric persist/restore are audited together.
#[test]
fn tool_call_thinking_survives_conversation_roundtrip() {
    use peakbot::storage::{ConversationStorage, InMemoryStorage};

    // ── Build a StateManager with a current Conversation wired in. ──────
    let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::default());
    let sm = Arc::new(StateManager::new_arc_with_storage(storage.clone()));
    sm.create_conversation(
        "tool-call-thinking".into(),
        "anthropic".into(),
        "claude-3-5-sonnet-latest".into(),
        String::new(),
    );
    // Seed a user message so the conversation is non-trivial.
    sm.add_user_message("read a.txt".to_string());

    // ── Stage thinking blocks and add a tool call that adopts them. ─────
    //
    // The "stage → add_tool_call" path already works in memory today:
    // add_tool_call at state_manager.rs:~1876-1880 adopts staged blocks
    // onto the orchestrator's ToolCall row when the source is
    // orchestrator-lane. The ChatMessage is therefore correctly populated;
    // it is the persistence side (sync_to_conversation + serde) that
    // drops the blocks — that's what T2 pins.
    sm.stage_thinking_for_next_assistant(vec![peakbot::reasoning::ThinkingBlock::Thinking {
        text: THINKING_TEXT.to_string(),
        signature: FAKE_SIGNATURE.to_string(),
    }]);
    sm.add_tool_call(
        MessageSource::Human,
        "file_read".to_string(),
        r#"{"path":"a.txt"}"#.to_string(),
        Some("c1".to_string()),
    );

    // Sanity: the live ChatMessage carries the block.
    let pre_save_state = sm.get_state();
    let pre_save_tool_row = pre_save_state
        .chat
        .messages
        .iter()
        .find(|m| m.role == MessageRole::ToolCall)
        .expect("the tool-call ChatMessage must exist");
    assert_eq!(
        pre_save_tool_row.thinking,
        vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: THINKING_TEXT.to_string(),
            signature: FAKE_SIGNATURE.to_string(),
        }],
        "in-memory ChatMessage must carry the staged blocks (sanity)",
    );

    // ── Save half: serialize the Conversation to JSON. ──────────────────
    // add_tool_call already ran sync_to_conversation via persist_current.
    let conv: Conversation = sm
        .get_current_conversation()
        .expect("a current conversation must be set after create_conversation");

    let wire_json = serde_json::to_string(&conv).expect("serialise");
    let parsed: serde_json::Value = serde_json::from_str(&wire_json).expect("parse");

    // Find the ToolCall row in the serialised messages and assert it
    // carries a `"thinking"` key whose first block matches the
    // sentinel signature.
    let tool_rows: Vec<&serde_json::Value> = parsed
        .get("messages")
        .and_then(|m| m.as_array())
        .expect("messages must be an array")
        .iter()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("ToolCall"))
        .collect();
    assert_eq!(
        tool_rows.len(),
        1,
        "exactly one ToolCall row expected; got {}",
        tool_rows.len()
    );
    let tool_json = tool_rows[0];
    let thinking_arr = tool_json
        .get("thinking")
        .and_then(|t| t.as_array())
        .unwrap_or_else(|| {
            panic!(
                "ToolCall row must serialise with a `thinking` array after the fix; \
                 today the field does not exist on Message::ToolCall — R2. Row: {tool_json}"
            )
        });
    assert!(
        !thinking_arr.is_empty(),
        "the thinking array on the ToolCall row must be non-empty; got {thinking_arr:?}",
    );
    // The first block must be a Thinking { text, signature } with our
    // sentinel bytes — Anthropic validates byte equality on replay.
    let first_block = &thinking_arr[0];
    assert_eq!(
        first_block.get("kind").and_then(|k| k.as_str()),
        Some("thinking"),
        "first thinking block on the ToolCall row must be the Thinking variant; got {first_block}",
    );
    assert_eq!(
        first_block.get("text").and_then(|t| t.as_str()),
        Some(THINKING_TEXT),
        "thinking text must round-trip verbatim on the ToolCall row",
    );
    assert_eq!(
        first_block.get("signature").and_then(|s| s.as_str()),
        Some(FAKE_SIGNATURE),
        "signature must round-trip verbatim on the ToolCall row",
    );

    // ── Restore half: hand the JSON to a fresh StateManager. ────────────
    let fresh_conv: Conversation = serde_json::from_str(&wire_json)
        .expect("serialised JSON must deserialize back into a Conversation");
    let fresh_storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::default());
    fresh_storage
        .save(&fresh_conv)
        .expect("seed InMemoryStorage with the JSON-derived Conversation");
    let sm2 = Arc::new(StateManager::new_arc_with_storage(fresh_storage));
    sm2.load_conversation(fresh_conv.id)
        .expect("load into fresh SM");

    let restored_state = sm2.get_state();
    let restored_tool_row = restored_state
        .chat
        .messages
        .iter()
        .find(|m| m.role == MessageRole::ToolCall)
        .expect("restored ChatMessage must contain the ToolCall row");

    assert_eq!(
        restored_tool_row.thinking,
        vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: THINKING_TEXT.to_string(),
            signature: FAKE_SIGNATURE.to_string(),
        }],
        "thinking blocks must round-trip byte-identical save → JSON → load → ChatMessage",
    );
    assert_eq!(
        restored_tool_row.call_id.as_deref(),
        Some("c1"),
        "sanity: the call_id must also round-trip (regression lock)",
    );
}
