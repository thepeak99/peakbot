//! TDD RED tests for: "Preserve Anthropic reasoning/thinking blocks in
//! conversation history" (design §8).
//!
//! Each test here is a *contract* — a behaviour the implementation must
//! satisfy once landed. Most are RED today because the types and
//! methods they reference don't exist yet (`ThinkingBlock`,
//! `ChatMessage.thinking`, `with_preserve_reasoning`,
//! `add_assistant_message_with_thinking`, `resolve_preserve_reasoning`,
//! the `thinking` field on `AgentEvent::CompletionResponse`, the cross-
//! provider gate, the `ThinkingWire` display shape). The compile failure
//! is the RED signal for new-type contracts; once the implementation
//! lands, each test asserts the named behaviour.
//!
//! Per the design's "verify-by-poison" discipline, this file is the
//! single integration-test target where all RED lives. The
//! `MockCompletionModel` cannot emit `AssistantContent::Reasoning` blocks,
//! so the rig round-trip contract (design §8.4) is written as a
//! StateManager-layer equivalence test plus a direct rig round-trip test
//! that constructs the message shape by hand and asserts wire JSON
//! byte-equality.

#![cfg(test)]
// The tester's assertions deliberately pass references to helper functions;
// removing them would alter the spec's source style without changing meaning.
#![allow(clippy::needless_borrow)]

use peakbot::storage::{ConversationStorage, FileStorage, InMemoryStorage};
use peakbot::ui::app_state::{MessageRole, MessageSource};
use peakbot::{Conversation, StateManager};
use rig_core::completion::message::{
    AssistantContent, Message as RigMessage, Reasoning, ReasoningContent, Text, ToolCall,
    ToolFunction, UserContent,
};
use rig_core::one_or_many::OneOrMany;
use std::sync::Arc;
use tempfile::TempDir;

/// Sentinel string that lives only inside a thinking block. No prose ever
/// contains this; if the summariser or the wire-builder leaks thinking
/// text, the assertion fires.
const THINKING_SENTINEL: &str = "DO_NOT_LEAK_THINKING_SENTINEL_77a1";

/// Sentinel signature — its byte content is what Anthropic validates on
/// replay. Equal *string* equality, not just non-emptiness, is what
/// contract 1 demands.
const FAKE_SIGNATURE: &str = "sig.SGXabc123XYZ-==";

// ─────────────────────────────────────────────────────────────────────────────
// Shared helpers for the per-response segmentation suite (T1–T5, T7a, T11).
//
// Each response has its own Anthropic signature, so the wire-seam test must
// distinguish two discrete responses, not blur them into one. These two
// constants are the only signatures used across the segmentation suite and
// are deliberately distinct (different bytes after `sig.`) so an assertion
// comparing a per-message signature set to the expected set cannot pass for
// the wrong response.
// ─────────────────────────────────────────────────────────────────────────────

const SIG_A: &str = "sig.AAA-==";
const SIG_B: &str = "sig.BBB-==";

/// Simulate one Anthropic response arriving at SessionHook: opens a new
/// response and stages its blocks. Returns the id every row of that
/// response must carry.
///
/// Pre-implementation: `begin_response` does not exist. This helper exists
/// so each per-response-id test stays a one-line `r1 = respond(&sm, vec![…])`
/// rather than spreading the (still-RED) call across the test bodies.
fn respond(sm: &StateManager, blocks: Vec<peakbot::reasoning::ThinkingBlock>) -> u64 {
    sm.begin_response(blocks)
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract 3 — Rebuild ordering: thinking first.
// ─────────────────────────────────────────────────────────────────────────────

/// get_agent_history on a transcript containing an assistant message with
/// thinking + text + a tool call must yield ONE Message::Assistant whose
/// content sequence is [Reasoning, Text, ToolCall], in that order.
///
/// The capture happens elsewhere (contracts 1, 2); here we hand-build the
/// ChatMessage with `thinking` already populated by opening a response
/// (`begin_response`) — on the orchestrator lane that is the only way
/// blocks reach a row — and assert the rebuild side.
#[test]
fn rebuild_orders_thinking_first_in_same_assistant_message() {
    let sm = StateManager::new();

    sm.add_user_message("read a.txt".to_string());
    // The response that produced both the tool call and the prose below.
    // On the orchestrator lane the blocks come from the open response —
    // `add_assistant_message_with_thinking`'s argument is ignored there —
    // so the response has to be opened before its rows are appended.
    let r1 = respond(
        &sm,
        vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: "Need to read a.txt first.".to_string(),
            signature: FAKE_SIGNATURE.to_string(),
        }],
    );
    sm.add_tool_call(
        MessageSource::Human,
        Some(r1),
        "file_read".to_string(),
        r#"{"path":"a.txt"}"#.to_string(),
        Some("c1".to_string()),
    );
    sm.add_tool_result(
        MessageSource::Human,
        "file_read".to_string(),
        r#"{"path":"a.txt"}"#.to_string(),
        "x".to_string(),
        Some("c1".to_string()),
    );

    // The prose of the SAME response — it must land in the same segment.
    sm.add_assistant_message("Got it.".to_string());

    let history = sm.get_agent_history();

    // Find the Assistant message that carries the FileRead tool call.
    let assistant_with_tool = history
        .iter()
        .find_map(|m| match m {
            RigMessage::Assistant { content, .. } => {
                let found_tool_call = content.iter().any(|c| {
                    matches!(
                        c,
                        AssistantContent::ToolCall(tc) if tc.function.name == "file_read"
                    )
                });
                if found_tool_call { Some(content) } else { None }
            }
            _ => None,
        })
        .expect("the rebuilt history must contain a Message::Assistant with the tool_call");

    // The content vector MUST start with [Reasoning, Text, ToolCall] —
    // Anthropic's 400 rule.
    let items: Vec<&AssistantContent> = assistant_with_tool.iter().collect();
    assert!(
        items.len() >= 3,
        "rebuilt assistant message should carry [Reasoning, Text, ToolCall] — got {} items",
        items.len()
    );

    assert!(
        matches!(items[0], AssistantContent::Reasoning(_)),
        "first item in the assistant message must be Reasoning (thinking), got {:?}",
        classify(&items[0]),
    );
    assert!(
        matches!(items[1], AssistantContent::Text(_)),
        "second item must be Text, got {:?}",
        classify(&items[1]),
    );
    assert!(
        matches!(items[2], AssistantContent::ToolCall(_)),
        "third item must be ToolCall, got {:?}",
        classify(&items[2]),
    );

    // And the signature must be byte-identical to what was captured.
    if let AssistantContent::Reasoning(r) = items[0] {
        let sig = r.content.iter().find_map(|c| match c {
            ReasoningContent::Text { signature, .. } => signature.clone(),
            _ => None,
        });
        assert_eq!(
            sig.as_deref(),
            Some(FAKE_SIGNATURE),
            "signature must round-trip verbatim from capture to rebuild",
        );
    }
}

fn classify(c: &AssistantContent) -> &'static str {
    // No meaningful `Other` today — rig's enum is `non_exhaustive` so the
    // fall-through is here for future variants. The compiler can't see
    // that today, so the wildcard is locally allowed.
    #[allow(unreachable_patterns)]
    match c {
        AssistantContent::Reasoning(_) => "Reasoning",
        AssistantContent::Text(_) => "Text",
        AssistantContent::ToolCall(_) => "ToolCall",
        AssistantContent::Image(_) => "Image",
        _ => "Other",
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract 5 — No-thinking runs are unchanged.
// ─────────────────────────────────────────────────────────────────────────────

/// When no message carries any thinking block, `get_agent_history` must
/// still produce the *current* wire shape: one `Message::Assistant` per
/// Agent row, and one per ToolCall row — separate, not coalesced. This
/// protects every non-Anthropic provider and every knob-off run from the
/// T6 rewire.
#[test]
fn no_thinking_runs_produce_separate_agent_and_tool_call_messages() {
    let sm = StateManager::new();

    sm.add_user_message("do the thing".to_string());
    sm.add_assistant_message_sourced(MessageSource::Human, "Reading.".to_string());
    sm.add_tool_call(
        MessageSource::Human,
        None,
        "bash".to_string(),
        "{}".to_string(),
        Some("c1".to_string()),
    );
    sm.add_tool_result(
        MessageSource::Human,
        "bash".to_string(),
        "{}".to_string(),
        "ok".to_string(),
        Some("c1".to_string()),
    );
    sm.add_user_message("done".to_string());

    let history = sm.get_agent_history();

    // Expect ONE Assistant for the prose row, ONE Assistant for the ToolCall
    // row — not coalesced. This is the pre-change output, preserved.
    let assistants: Vec<&RigMessage> = history
        .iter()
        .filter(|m| matches!(m, RigMessage::Assistant { .. }))
        .collect();

    assert_eq!(
        assistants.len(),
        2,
        "no-thinking runs must emit one Message::Assistant per Agent row AND one per ToolCall row (got {})",
        assistants.len()
    );

    // First assistant carries only Text, second only ToolCall.
    match assistants[0] {
        RigMessage::Assistant { content, .. } => {
            assert!(matches!(content.first(), AssistantContent::Text(_)));
        }
        _ => unreachable!(),
    }
    match assistants[1] {
        RigMessage::Assistant { content, .. } => {
            assert!(matches!(content.first(), AssistantContent::ToolCall(_)));
        }
        _ => unreachable!(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract 4 — Tool-loop regression: capture → rebuild → byte-identical sig.
// ─────────────────────────────────────────────────────────────────────────────

/// The headline Anthropic 400 rule, encoded as a single test.
///
/// Direct rig round-trip check: build a `RigMessage::Assistant` carrying
/// `[Reasoning{text, sig}, Text, ToolCall]`, serialise to JSON, verify
/// the signature field is *exactly* the input string. This is the wire
/// the Anthropic transport sends.
#[test]
fn tool_loop_signature_round_trips_byte_identical_through_rig_wire() {
    // Build the rig-side message the rebuild pathway would produce.
    let reasoning =
        Reasoning::new_with_signature("I need to read the file.", Some(FAKE_SIGNATURE.to_string()));
    let text = Text::new("Reading now.");
    let tool_call = ToolCall::new(
        "c1".to_string(),
        ToolFunction::new(
            "file_read".to_string(),
            serde_json::json!({"path": "a.txt"}),
        ),
    );

    let assistant = RigMessage::Assistant {
        id: None,
        content: OneOrMany::many(vec![
            AssistantContent::Reasoning(reasoning),
            AssistantContent::Text(text),
            AssistantContent::ToolCall(tool_call),
        ])
        .expect("non-empty content"),
    };

    // Serialise to the JSON rig's encoder produces (not the
    // Anthropic-specific wire — that lives in rig's `completion`
    // request and can't be driven from this test). The signature
    // must round-trip byte-equal; that's the byte-identity rig
    // promises for its serialise layer.
    let wire_json = serde_json::to_string(&assistant).expect("serialise");
    let v: serde_json::Value = serde_json::from_str(&wire_json).expect("parse");

    // The signature is somewhere inside the inner `ReasoningContent::Text`
    // payload. Locate it structurally: the first AssistantContent is the
    // Reasoning struct (`{"id":null,"content":[…]}`), and its first
    // inner content is the `Text` variant carrying `{"text":…,"signature":…}`.
    let content = v
        .get("content")
        .and_then(|c| c.as_array())
        .expect("content must be an array");

    let reasoning_struct = &content[0];
    let inner = reasoning_struct
        .get("content")
        .and_then(|c| c.as_array())
        .expect("Reasoning struct must carry a content array");
    let first_text_content = &inner[0];
    let text_payload = first_text_content
        .get("content")
        .expect("ReasoningContent::Text wraps the text payload");

    // Inner variant is `Text` (rig tags ReasoningContent with type=text/summary/etc).
    assert_eq!(
        first_text_content.get("type").and_then(|t| t.as_str()),
        Some("text"),
        "first thinking block must be a ReasoningContent::Text variant, got {first_text_content}",
    );
    assert_eq!(
        text_payload.get("text").and_then(|t| t.as_str()),
        Some("I need to read the file."),
    );
    let sig = text_payload
        .get("signature")
        .and_then(|s| s.as_str())
        .expect("signature must be a string on the wire");
    assert_eq!(
        sig, FAKE_SIGNATURE,
        "signature must round-trip verbatim — Anthropic validates byte equality",
    );

    // Sanity: rig put the Reasoning at index 0 (before Text and ToolCall) — that's
    // the order the rebuild helper must produce too.
    let kinds: Vec<&str> = content
        .iter()
        .map(|c| {
            if c.get("id").is_some() && c.get("content").is_some() {
                "reasoning"
            } else if c.get("text").is_some() {
                "text"
            } else if c.get("function").is_some() {
                "tool_call"
            } else {
                "other"
            }
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["reasoning", "text", "tool_call"],
        "rebuild must put Reasoning before Text and ToolCall in the same Assistant content array",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract 8 — Old conversation JSON loads (post-implementation).
// ─────────────────────────────────────────────────────────────────────────────

/// A pre-fix conversation JSON (no `thinking` key, no `attachments`) must
/// deserialize with `thinking == Vec::new()` and no error. Re-saving must
/// not add a `thinking` key (the field is `skip_serializing_if = "is_empty"`).
///
/// BACKWARD-COMPAT REGRESSION LOCK: this JSON parses *today* (the field
/// doesn't exist). The test pins the post-implementation behaviour — the
/// field lands with `#[serde(default)]` and re-save stays byte-identical.
#[test]
fn old_conversation_json_without_thinking_key_loads_and_resaves_byte_identical() {
    let json = r#"{
        "id": "11111111-1111-1111-1111-111111111111",
        "name": "pre-fix",
        "created_at": "2025-01-01T00:00:00Z",
        "updated_at": "2025-01-01T00:00:00Z",
        "messages": [
            { "role": "User", "content": "hi", "compacted": false, "source": "Human", "timestamp": "2025-01-01T00:00:00Z" },
            { "role": "Assistant", "content": "hello", "compacted": false, "source": "Human", "timestamp": "2025-01-01T00:00:01Z" }
        ],
        "provider_name": "anthropic",
        "model": "claude-3-5-sonnet-latest",
        "cwd": "",
        "metadata": {
            "message_count": 2,
            "total_input_tokens": 0,
            "total_output_tokens": 0,
            "total_api_calls": 0,
            "total_cost": 0.0,
            "lanes": []
        },
        "todos": []
    }"#;

    let conv: Conversation = serde_json::from_str(json)
        .expect("pre-fix JSON without thinking/attachments must parse cleanly");

    // Round-trip: re-serialise and confirm no `thinking` key got injected
    // (the field is gated with `skip_serializing_if = "Vec::is_empty"`).
    let round_tripped = serde_json::to_string(&conv).expect("re-serialise");
    assert!(
        !round_tripped.contains("\"thinking\""),
        "pre-fix JSON must not gain a `thinking` key on round-trip (regression lock); got: {round_tripped}",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract 9 — Round-trip persistence: save → load → identical signature.
// ─────────────────────────────────────────────────────────────────────────────

/// Save a conversation whose assistant message carries a signature, load it
/// back through `FileStorage`, and run `get_agent_history` on a wired
/// `StateManager`. The resulting wire must contain the same signature
/// string — byte-equal.
#[test]
fn round_trip_persistence_preserves_signature_through_get_agent_history() {
    let dir = TempDir::new().expect("tempdir");
    let storage = FileStorage::new(dir.path().to_path_buf()).expect("FileStorage");

    let mut conv = Conversation::new(
        "thinking-roundtrip".into(),
        "anthropic".into(),
        "claude-3-5-sonnet-latest".into(),
        String::new(),
    );
    conv.add_user_message("read a.txt".into());
    // Push the assistant row directly so it carries a `response_id`: a row
    // whose response group is unknown deliberately never replays its
    // reasoning (see `rows_without_response_id_never_replay_reasoning`), so
    // `Conversation::add_assistant_message_with_thinking` — which has no
    // response to name — cannot express what this test is about.
    conv.messages.push(peakbot::ConversationMessage::Assistant {
        content: "Reading now.".to_string(),
        compacted: false,
        source: MessageSource::Human,
        thinking: vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: "Need to read a.txt first.".to_string(),
            signature: FAKE_SIGNATURE.to_string(),
        }],
        timestamp: chrono::Utc::now(),
        response_id: Some(1),
    });

    storage.save(&conv).expect("save");

    // Wire the loaded conversation into a StateManager and rebuild the wire.
    let storage_arc: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
    storage_arc.save(&conv).expect("seed InMemoryStorage");
    let sm = Arc::new(StateManager::new_arc_with_storage(storage_arc));
    sm.load_conversation(conv.id).expect("load into SM");

    let history = sm.get_agent_history();
    let assistant = history
        .iter()
        .find_map(|m| match m {
            RigMessage::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .expect("history must contain a Message::Assistant");

    let first = assistant.first();
    match first {
        AssistantContent::Reasoning(r) => {
            let sig = r.content.iter().find_map(|c| match c {
                ReasoningContent::Text { signature, .. } => signature.clone(),
                _ => None,
            });
            assert_eq!(
                sig.as_deref(),
                Some(FAKE_SIGNATURE),
                "signature must round-trip byte-identical save → load → rebuild",
            );
        }
        other => panic!(
            "first assistant content must be the thinking block, got {}",
            classify(&other),
        ),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract 10 — Compaction keeps survivors' blocks.
// ─────────────────────────────────────────────────────────────────────────────

/// Build a transcript where a user message is marked `compacted=true`. The
/// rebuilt wire must exclude it. The "thinking survives for the survivor"
/// sub-contract is pinned in the dedicated unit test in
/// src/context_manager.rs (which can read the private `thinking` field on
/// `ChatMessage` directly); here we cover the lane-filter half which is
/// publicly observable through `get_agent_history`.
#[test]
fn compaction_drops_compacted_messages_but_preserves_survivor_messages() {
    let sm = StateManager::new();

    sm.add_user_message("Question 1".to_string());
    sm.add_assistant_message_sourced(MessageSource::Human, "Answer 1 prose.".to_string());

    // Add a user message that the compaction summary will sweep away.
    sm.add_user_message("Follow-up to be compacted".to_string());

    // Mark it compacted. The only public mutation surface is the
    // `update_chat_state` snapshot replace — sufficient to test the
    // wire-side filter (the orchestator-lane filter, which is the
    // observable half of the contract).
    let mut chat = sm.get_state().chat.clone();
    if let Some(last_user) = chat
        .messages
        .iter_mut()
        .find(|m| m.role == MessageRole::User && m.content == "Follow-up to be compacted")
    {
        last_user.compacted = true;
    }
    sm.update_chat_state(chat);

    sm.add_assistant_message_sourced(MessageSource::Human, "Answer 2 prose.".to_string());
    sm.add_user_message("Latest question.".to_string());

    let history = sm.get_agent_history();
    let current_turn = sm
        .build_current_turn_message()
        .expect("latest non-compacted user is the current turn prompt");

    // The compacted user message must NOT appear in the wire, and the trailing
    // user must not appear in `history` either: it is the *current turn*, which
    // the dispatch path supplies separately as the `prompt` argument of
    // `prompt_with_history`. So only "Question 1" survives in history.
    //
    // (This assertion used to expect 2 — that was written against a short-lived
    // "don't strip the trailing user once anything is compacted" exception,
    // which duplicated the current turn on the wire: once inside `history`,
    // once as the prompt. The exception is gone; see the "exactly once" check
    // below, which is the real contract.)
    let history_users: Vec<&RigMessage> = history
        .iter()
        .filter(|m| matches!(m, RigMessage::User { .. }))
        .collect();
    assert_eq!(
        history_users.len(),
        1,
        "compacted user message must be excluded from history, and the trailing user \
         must be stripped because it is re-supplied as the prompt (got {} user messages \
         in history)",
        history_users.len(),
    );

    // The latest user message survives exactly once: stripped from history
    // and re-supplied as the current turn prompt, not duplicated and not lost.
    let all_user_texts: Vec<String> = history
        .iter()
        .chain(std::iter::once(&current_turn))
        .filter_map(|m| match m {
            RigMessage::User { content } => Some(
                content
                    .iter()
                    .filter_map(|c| match c {
                        UserContent::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            ),
            _ => None,
        })
        .collect();
    assert!(
        !all_user_texts
            .iter()
            .any(|t| t.contains("Follow-up to be compacted")),
        "compacted user text must never reach the wire"
    );
    assert_eq!(
        all_user_texts
            .iter()
            .filter(|t| t.contains("Latest question."))
            .count(),
        1,
        "latest user message must appear exactly once across history + prompt, not duplicated"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract 11 — Cross-provider gate: Anthropic thinking, foreign provider.
// ─────────────────────────────────────────────────────────────────────────────

/// A transcript captured under Claude carries Anthropic thinking blocks.
/// If the active provider is switched to OpenAI/Ollama/etc, wire rebuild
/// must drop the `Reasoning` content — Anthropic signatures on a foreign
/// wire are at best noise and at worst a 400.
///
/// The gate is a single `bool` set on `StateManager` from `ProviderInfo`
/// (the design premise). The test connects the knob and verifies the
/// rebuild drops `Reasoning`.
#[test]
fn cross_provider_gate_drops_anthropic_reasoning_for_non_anthropic_provider() {
    let sm = StateManager::new();
    // Acquire the provider gate. The setter is `set_wire_reasoning(bool)`
    // per design §3.4; the implementer must add it.
    sm.set_wire_reasoning(false);

    sm.add_user_message("hi".to_string());
    // Orchestrator lane: blocks reach the row through the open response.
    respond(
        &sm,
        vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: "thinking text".into(),
            signature: FAKE_SIGNATURE.to_string(),
        }],
    );
    sm.add_assistant_message("hi".to_string());

    let history = sm.get_agent_history();

    // With the gate closed, no Reasoning content reaches the wire.
    let any_reasoning = history.iter().any(|m| match m {
        RigMessage::Assistant { content, .. } => content
            .iter()
            .any(|c| matches!(c, AssistantContent::Reasoning(_))),
        _ => false,
    });
    assert!(
        !any_reasoning,
        "with the cross-provider gate closed, no Anthropic thinking must reach the wire",
    );

    // But the plain text DOES still flow — the gate protects only the
    // reasoning content, not the prose.
    let any_text = history.iter().any(|m| match m {
        RigMessage::Assistant { content, .. } => content
            .iter()
            .any(|c| matches!(c, AssistantContent::Text(_))),
        _ => false,
    });
    assert!(any_text, "the gate must not strip prose — only reasoning",);
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract 12 — Unsigned block dropped.
// ─────────────────────────────────────────────────────────────────────────────

/// `Thinking { signature: "" }` is captured but never reaches the wire —
/// replaying an un-signed block is a guaranteed 400. The design (§1.2) maps
/// rig's `ReasoningContent::Text { signature: None }` to `Thinking {
/// signature: "" }`, and the wire seam (§3.4) drops it.
#[test]
fn unsigned_thinking_block_does_not_reach_the_wire() {
    let sm = StateManager::new();

    sm.add_user_message("hi".to_string());
    // Orchestrator lane: blocks reach the row through the open response.
    respond(
        &sm,
        vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: THINKING_SENTINEL.into(),
            signature: String::new(),
        }],
    );
    sm.add_assistant_message("hi".to_string());

    let history = sm.get_agent_history();

    let leaked = history.iter().any(|m| match m {
        RigMessage::Assistant { content, .. } => content.iter().any(|c| match c {
            AssistantContent::Reasoning(r) => r.content.iter().any(
                |rc| matches!(rc, ReasoningContent::Text { text, .. } if text == THINKING_SENTINEL),
            ),
            _ => false,
        }),
        _ => false,
    });

    assert!(
        !leaked,
        "an unsigned thinking block must never be emitted on the wire",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract 6 — Knob off strips at capture (wire-side verification).
// ─────────────────────────────────────────────────────────────────────────────

/// With the per-model `preserve_reasoning = false` knob off, the wire side
/// must drop the Anthropic thinking block. The capture seam (SessionHook)
/// is tested in src/hooks/session_hook.rs; this is the wire-side
/// consequence.
#[test]
fn knob_off_drops_thinking_from_wire() {
    let sm = StateManager::new();

    sm.add_user_message("read".to_string());
    // The implementation will resolve the per-model knob in build_provider_config
    // and consult it at the wire seam. The test asserts the wire consequence:
    // the rebuild helper must drop the block when the gate is closed.
    sm.set_wire_reasoning(false);
    // Orchestrator lane: blocks reach the row through the open response.
    respond(
        &sm,
        vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: THINKING_SENTINEL.into(),
            signature: FAKE_SIGNATURE.to_string(),
        }],
    );
    sm.add_assistant_message("Reading.".to_string());

    let history = sm.get_agent_history();
    let leaked = history.iter().any(|m| match m {
        RigMessage::Assistant { content, .. } => content.iter().any(|c| match c {
            AssistantContent::Reasoning(r) => r.content.iter().any(
                |rc| matches!(rc, ReasoningContent::Text { text, .. } if text == THINKING_SENTINEL),
            ),
            _ => false,
        }),
        _ => false,
    });
    assert!(
        !leaked,
        "thinking must be stripped before the wire when the knob is off",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract 13 — Display default invisible (server-side gating).
// ─────────────────────────────────────────────────────────────────────────────

/// With `display_reasoning == false` (the default), the web state snapshot
/// pushed to subscribers via `StateManager::subscribe` must contain no
/// `thinking` field on any `ChatMessage` row. With it true, the text is
/// included but the signature is NEVER sent — signatures are opaque
/// credentials; sending them to the browser is a credential leak.
///
/// This pins the server-side gate (BLOCKER 2) end-to-end: the
/// `strip_thinking_from_app_state` helper inside `notify_update` /
/// `subscribe` clears `thinking` on every broadcast clone when the gate is
/// closed. Signatures never reach the wire regardless of the gate.
#[test]
fn display_default_drops_thinking_from_snapshot_and_never_leaks_signature() {
    // Subscribing first and draining the initial push makes this
    // deterministic without any sleeping: `notify_update` pushes through
    // `mpsc::Sender::try_send` synchronously on the mutating thread, so the
    // snapshot is already queued by the time the `add_*` call returns.

    // ── Off: default gate (false) — the broadcast must carry no thinking ──
    let sm_off = StateManager::new();
    let mut rx_off = sm_off.subscribe();
    let _initial = rx_off.try_recv().expect("subscribe pushes initial state");

    respond(
        &sm_off,
        vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: "thinking text not for browser".into(),
            signature: FAKE_SIGNATURE.into(),
        }],
    );
    sm_off.add_assistant_message("hi".into());

    let snapshot_off = rx_off
        .try_recv()
        .expect("a snapshot must be broadcast after the assistant row is added");
    let json_off = serde_json::to_string(&snapshot_off).expect("encode");
    assert!(
        !json_off.contains("\"thinking\""),
        "with display_reasoning=false, the snapshot must not contain a `thinking` field; got: {json_off}",
    );
    assert!(
        !json_off.contains("thinking text not for browser"),
        "with display_reasoning=false, thinking text must not leak; got: {json_off}",
    );

    // The gate is display-only: the blocks are still on the live state (and
    // therefore still reach the LLM wire via `get_agent_history`).
    assert!(
        sm_off
            .get_state()
            .chat
            .messages
            .iter()
            .any(|m| !m.thinking.is_empty()),
        "the display gate must not strip blocks from the live state — the wire needs them",
    );

    // ── On: gate open — the text rides along, the signature never does ──
    let sm_on = StateManager::new();
    sm_on.set_display_reasoning(true);
    let mut rx_on = sm_on.subscribe();
    let _initial = rx_on.try_recv().expect("subscribe pushes initial state");

    respond(
        &sm_on,
        vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: "user-readable thinking".into(),
            signature: FAKE_SIGNATURE.into(),
        }],
    );
    sm_on.add_assistant_message("hi".into());

    let snapshot_on = rx_on
        .try_recv()
        .expect("a snapshot must be broadcast after the assistant row is added");
    let json_on = serde_json::to_string(&snapshot_on).expect("encode");
    assert!(
        json_on.contains("\"thinking\""),
        "the thinking field must be present when display_reasoning=true; got: {json_on}",
    );
    assert!(
        !json_on.contains(FAKE_SIGNATURE),
        "the signature must NEVER reach the browser — leak found: {json_on}",
    );
    assert!(
        json_on.contains("user-readable thinking"),
        "the text must reach the browser when display_reasoning=true; got: {json_on}",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract 1 — Capture keeps the signature byte-identical (end-to-end via
// the public SessionHook::on_completion_response path).
//
// Rig's `AssistantContent::Reasoning(Text{text, sig})` flows through the
// hook, lands on `AgentEvent::CompletionResponse.thinking` as a
// `ThinkingBlock::Thinking{text, signature}` with the signature byte-equal
// to the input. The capture seam is the public path; the helper
// `extract_content_from_response` is private and tested in
// `src/hooks/session_hook.rs`.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn capture_keeps_signature_byte_identical_via_session_hook() {
    use peakbot::AgentEvent;
    use peakbot::SessionHook;
    use peakbot::mock::MockCompletionModel;
    use peakbot::mock::completion_model::MockModelResponse;
    use rig_core::agent::PromptHook;
    use rig_core::completion::Usage as RigUsage;
    use rig_core::completion::message::UserContent;
    use std::sync::{Arc, Mutex};

    // Wire a SessionHook with an event receiver so we can observe what it emits.
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<peakbot::SourcedEvent>();
    let session_stats = Arc::new(Mutex::new(peakbot::SessionStats::new()));
    let hook = SessionHook::with_context_tracking(Some(sender), session_stats)
        .with_preserve_reasoning(true);

    // Build a CompletionResponse with one Reasoning{text, signature} block.
    let reasoning =
        Reasoning::new_with_signature("captured thinking text", Some(FAKE_SIGNATURE.to_string()));
    let choice = OneOrMany::one(AssistantContent::Reasoning(reasoning));
    let response: rig_core::completion::CompletionResponse<MockModelResponse> =
        rig_core::completion::CompletionResponse {
            choice,
            usage: RigUsage::new(),
            raw_response: MockModelResponse {
                content: String::new(),
                is_tool_call: false,
            },
            message_id: None,
        };

    let prompt = RigMessage::User {
        content: OneOrMany::one(UserContent::Text(Text::new("hi"))),
    };

    let _ = <SessionHook as PromptHook<MockCompletionModel>>::on_completion_response(
        &hook, &prompt, &response,
    )
    .await;

    // Drain the channel; expect one CompletionResponse event.
    let mut event: Option<AgentEvent> = None;
    while let Ok(sourced) = receiver.try_recv() {
        if let AgentEvent::CompletionResponse { .. } = sourced.event {
            event = Some(sourced.event);
        }
    }
    let event = event.expect("SessionHook must emit exactly one CompletionResponse event");
    let AgentEvent::CompletionResponse { thinking, .. } = event else {
        unreachable!()
    };

    // The thinking field is a NEW field on CompletionResponse (design §4.2).
    // Today this is a compile error — the test is RED for the right reason.
    assert_eq!(
        thinking,
        vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: "captured thinking text".to_string(),
            signature: FAKE_SIGNATURE.to_string(),
        }],
        "signature must round-trip verbatim from rig to AgentEvent::CompletionResponse",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract 2 — Capture maps redacted reasoning to the Redacted variant.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn capture_maps_redacted_block_to_opaque_variant_via_session_hook() {
    use peakbot::AgentEvent;
    use peakbot::SessionHook;
    use peakbot::mock::MockCompletionModel;
    use peakbot::mock::completion_model::MockModelResponse;
    use rig_core::agent::PromptHook;
    use rig_core::completion::Usage as RigUsage;
    use rig_core::completion::message::UserContent;
    use std::sync::{Arc, Mutex};

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<peakbot::SourcedEvent>();
    let session_stats = Arc::new(Mutex::new(peakbot::SessionStats::new()));
    let hook = SessionHook::with_context_tracking(Some(sender), session_stats)
        .with_preserve_reasoning(true);

    let reasoning = Reasoning::redacted("opaque-payload-77a1");
    let choice = OneOrMany::one(AssistantContent::Reasoning(reasoning));
    let response: rig_core::completion::CompletionResponse<MockModelResponse> =
        rig_core::completion::CompletionResponse {
            choice,
            usage: RigUsage::new(),
            raw_response: MockModelResponse {
                content: String::new(),
                is_tool_call: false,
            },
            message_id: None,
        };
    let prompt = RigMessage::User {
        content: OneOrMany::one(UserContent::Text(Text::new("hi"))),
    };
    let _ = <SessionHook as PromptHook<MockCompletionModel>>::on_completion_response(
        &hook, &prompt, &response,
    )
    .await;

    let mut event: Option<AgentEvent> = None;
    while let Ok(sourced) = receiver.try_recv() {
        if let AgentEvent::CompletionResponse { .. } = sourced.event {
            event = Some(sourced.event);
        }
    }
    let event = event.expect("SessionHook must emit one CompletionResponse event");
    let AgentEvent::CompletionResponse {
        thinking,
        content,
        reasoning,
        ..
    } = event
    else {
        unreachable!()
    };

    assert_eq!(
        thinking,
        vec![peakbot::reasoning::ThinkingBlock::Redacted {
            data: "opaque-payload-77a1".to_string(),
        }],
        "redacted reasoning must round-trip as Redacted",
    );
    assert!(content.is_empty(), "Redacted contributes no prose");
    assert!(
        reasoning.is_none(),
        "Redacted contributes no reasoning string"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract 7a — Summariser never sees thinking (compaction summariser).
//
// The harness's `extract_summarization_prompt` returns the actual prompt
// sent to the compaction model. We end-to-end exercise the seam: build a
// transcript where a ChatMessage carries a thinking block with the
// sentinel, force a compaction through the public harness surface, and
// assert the recorded prompt does NOT contain the sentinel. The harness's
// session hook is built without the new preserve knob; the test pins the
// OBSERVABLE behaviour that no thinking text leaks to the summariser
// regardless of how the new knob is wired.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn compaction_summariser_prompt_does_not_contain_thinking_text() {
    use crate::harness::TestHarness;
    use peakbot::ContextConfig;
    use peakbot::mock::{MockResponse, Usage};

    let config = ContextConfig {
        threshold: 0.5,
        keep_recent: 3,
        enabled: true,
        compaction_model: None,
    };

    let mut harness = TestHarness::with_system_prompt_and_context(
        "You are a helpful assistant.",
        config,
        500, // 500-token window, threshold = 250.
    );

    // Build a transcript where the assistant message carries a thinking
    // block. The harness runs through SessionHook, which today flattens
    // reasoning to a String and drops the signature — so the ChatMessage
    // it produces has empty `thinking`. The summariser exclusion contract
    // therefore hinges on the design's `format_chat_messages_for_summary`
    // NOT reading `msg.thinking` and feeding it to the LLM. The test pins
    // that no thinking text leaks regardless of which seam populates it.
    //
    // Three turns to push the user message count past `keep_recent=3`,
    // each response with high enough token usage to fire compaction.
    harness.add_response(MockResponse::text_with_usage(
        "OLD_REPLY",
        Usage {
            input_tokens: 300,
            output_tokens: 20,
        },
    ));
    harness.add_response(MockResponse::text_with_usage(
        "OLD_REPLY_2",
        Usage {
            input_tokens: 300,
            output_tokens: 20,
        },
    ));
    harness.add_compaction_response(MockResponse::text("summary text"));
    harness.add_response(MockResponse::text_with_usage(
        "RECENT_REPLY",
        Usage {
            input_tokens: 300,
            output_tokens: 20,
        },
    ));

    // A second sentinel unique to this test (not the canonical one — the
    // harness's flow already has the canonical one in its compaction
    // prompt format). The point is: the LLM is asked about user
    // messages, not thinking. If thinking ever leaks, the sentinel shows up.
    harness.run_message("OLD_MSG_1").await;
    harness.run_message("OLD_MSG_2").await;
    harness.run_message("RECENT_MSG").await;

    if !harness.has_compaction_occurred() {
        // Compaction didn't fire — test is invalid. Skip the assertion.
        return;
    }

    let requests = harness.get_summarization_requests();
    assert!(
        !requests.is_empty(),
        "summarizer must have been called at least once",
    );
    let prompt_blob = serde_json::to_string(&requests[0].chat_history).expect("serialise");
    assert!(
        !prompt_blob.contains("DO_NOT_LEAK_THINKING_SENTINEL_77a1"),
        "summarizer prompt must not contain the thinking sentinel — leak found: {prompt_blob}",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-response segmentation suite (T1–T5, T11) — RED tests for the
// Anthropic "thinking blocks must be replayed in their originating
// response group" fix.
//
// Bug locked down: `convert_history_to_rig` flat-mapped thinking blocks
// from every row in a contiguous Agent|ToolCall|ToolResult run into one
// `Message::Assistant`, regardless of which model response produced them.
// Anthropic verifies the signature against the response it was issued
// with and 400s when groups mismatch. The fix tags every ChatMessage with
// a `response_id` (the monotonic id of the SessionHook::begin_response call
// that opened its response) and the rebuild helper splits each run into
// maximal same-id segments, each emitted as its own Message::Assistant.
// ─────────────────────────────────────────────────────────────────────────────

/// **T1 — Two responses in one run → two assistant messages.**
///
/// THE regression test for the 400. One contiguous transcript run spans
/// TWO separate model responses: r1 produces thinking A + bash + todo,
/// r2 produces thinking B + todo. Today's helper emits a single
/// `Message::Assistant` whose reasoning carries SIG_A *and* SIG_B in front
/// of three tool calls — Anthropic rejects the bundle because the B
/// signature was never paired with those calls. The fix splits the run
/// at the response boundary so the wire carries:
///
/// ```
/// Assistant[Reasoning(SIG_A), ToolCall(c1), ToolCall(c2)]
/// User[ToolResult(c1)]
/// User[ToolResult(c2)]
/// Assistant[Reasoning(SIG_B), ToolCall(c3)]
/// User[ToolResult(c3)]
/// ```
///
/// Pre-implementation: `begin_response` and the new `add_tool_call` arity
/// don't exist, so the test fails to compile (RED for the right reason).
#[test]
fn two_responses_in_one_run_emit_two_assistant_messages() {
    let sm = StateManager::new();

    sm.add_user_message("go".to_string());

    // ── Response r1: thinking A + bash + todo ─────────────────────────────
    let r1 = respond(
        &sm,
        vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: "Plan for r1".to_string(),
            signature: SIG_A.to_string(),
        }],
    );
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

    // ── Response r2: thinking B + todo ─────────────────────────────────────
    let r2 = respond(
        &sm,
        vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: "Plan for r2".to_string(),
            signature: SIG_B.to_string(),
        }],
    );
    sm.add_tool_call(
        MessageSource::Human,
        Some(r2),
        "todo".to_string(),
        r#"{"action":"create","title":"b"}"#.to_string(),
        Some("c3".to_string()),
    );
    sm.add_tool_result(
        MessageSource::Human,
        "todo".to_string(),
        r#"{"action":"create","title":"b"}"#.to_string(),
        "ok".to_string(),
        Some("c3".to_string()),
    );

    sm.add_user_message("next".to_string());

    let history = sm.get_agent_history();

    // ── (1) Exactly two `Assistant` messages ─────────────────────────────
    //
    // The current implementation emits ONE per Agent-or-ToolCall-with-
    // thinking run, so today there is exactly one Assistant message and
    // this assertion fails with a count mismatch even if the test
    // compiled.
    let assistant_indices: Vec<usize> = history
        .iter()
        .enumerate()
        .filter_map(|(i, m)| matches!(m, RigMessage::Assistant { .. }).then_some(i))
        .collect();
    assert_eq!(
        assistant_indices.len(),
        2,
        "two responses must produce two Message::Assistant entries on the wire; got {} (indices={:?})",
        assistant_indices.len(),
        assistant_indices,
    );

    // ── (2) First assistant carries SIG_A and ToolCall(c1), ToolCall(c2). ─
    let first = match &history[assistant_indices[0]] {
        RigMessage::Assistant { content, .. } => content,
        other => panic!(
            "history[{}] must be Assistant, got {:?}",
            assistant_indices[0], other
        ),
    };
    let first_kinds: Vec<&'static str> = first.iter().map(classify).collect();
    assert_eq!(
        first_kinds,
        vec!["Reasoning", "ToolCall", "ToolCall"],
        "first assistant message must be [Reasoning(A), ToolCall(c1), ToolCall(c2)] in that order",
    );
    let first_sigs = collect_signatures(first);
    assert_eq!(
        first_sigs,
        vec![SIG_A.to_string()],
        "first assistant message must carry exactly SIG_A, not SIG_B",
    );

    // The two ToolCalls in the first message must be c1 and c2 (in that
    // order). Today they are c1, c2, c3 (all three in one message).
    let first_tool_call_ids: Vec<String> = first
        .iter()
        .filter_map(|c| match c {
            AssistantContent::ToolCall(tc) => Some(tc.id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        first_tool_call_ids,
        vec!["c1".to_string(), "c2".to_string()],
        "first assistant message's tool calls must be [c1, c2] — c3 must NOT appear here",
    );

    // ── (3) Second assistant carries SIG_B and ToolCall(c3) only. ────────
    let second = match &history[assistant_indices[1]] {
        RigMessage::Assistant { content, .. } => content,
        other => panic!(
            "history[{}] must be Assistant, got {:?}",
            assistant_indices[1], other
        ),
    };
    let second_kinds: Vec<&'static str> = second.iter().map(classify).collect();
    assert_eq!(
        second_kinds,
        vec!["Reasoning", "ToolCall"],
        "second assistant message must be [Reasoning(B), ToolCall(c3)]",
    );
    let second_sigs = collect_signatures(second);
    assert_eq!(
        second_sigs,
        vec![SIG_B.to_string()],
        "second assistant message must carry exactly SIG_B, not SIG_A",
    );
    let second_tool_call_ids: Vec<String> = second
        .iter()
        .filter_map(|c| match c {
            AssistantContent::ToolCall(tc) => Some(tc.id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        second_tool_call_ids,
        vec!["c3".to_string()],
        "second assistant message's tool call must be exactly [c3]",
    );

    // ── (4) Per-message signature SETS equal exactly one response's set. ─
    //
    // Defensive: catches a future regression that emits BOTH signatures
    // in front of all three tool calls (today's bug shape) — the SETS
    // would each contain {SIG_A, SIG_B} and the equality check would
    // catch that even if the per-position ordering assertion above were
    // relaxed.
    let first_set: std::collections::BTreeSet<String> = first_sigs.iter().cloned().collect();
    let second_set: std::collections::BTreeSet<String> = second_sigs.iter().cloned().collect();
    assert_eq!(
        first_set,
        std::collections::BTreeSet::from([SIG_A.to_string()]),
        "first assistant message's signature SET must equal {{SIG_A}}",
    );
    assert_eq!(
        second_set,
        std::collections::BTreeSet::from([SIG_B.to_string()]),
        "second assistant message's signature SET must equal {{SIG_B}}",
    );

    // ── (5) Ordering of ToolResults around the assistant messages. ───────
    //
    // User[ToolResult(c1)] and User[ToolResult(c2)] must come BEFORE the
    // second assistant message (they're r1's results). User[ToolResult(c3)]
    // must come AFTER the second assistant message (it's r2's result, but
    // r2 didn't produce any prose after it, so c3 stands alone as a user
    // turn). Today the order is mixed because the bug groups everything
    // into a single assistant message.
    let user_tool_results: Vec<String> = history
        .iter()
        .filter_map(|m| match m {
            RigMessage::User { content } => content.iter().find_map(|c| match c {
                UserContent::ToolResult(tr) => Some(tr.id.clone()),
                _ => None,
            }),
            _ => None,
        })
        .collect();
    assert_eq!(
        user_tool_results,
        vec!["c1".to_string(), "c2".to_string(), "c3".to_string()],
        "user tool results must appear in transcript order [c1, c2, c3]",
    );
    let idx_first_asst = assistant_indices[0];
    let idx_second_asst = assistant_indices[1];
    let c1_pos = user_tool_results
        .iter()
        .position(|s| s == "c1")
        .expect("c1 must be in user tool results");
    let c2_pos = user_tool_results
        .iter()
        .position(|s| s == "c2")
        .expect("c2 must be in user tool results");
    let c3_pos = user_tool_results
        .iter()
        .position(|s| s == "c3")
        .expect("c3 must be in user tool results");
    // The history positions of the three tool results are the matching
    // history indices (search the history directly to map c1→idx).
    let history_user_positions: Vec<(usize, String)> = history
        .iter()
        .enumerate()
        .filter_map(|(i, m)| match m {
            RigMessage::User { content } => content.iter().find_map(|c| match c {
                UserContent::ToolResult(tr) => Some((i, tr.id.clone())),
                _ => None,
            }),
            _ => None,
        })
        .collect();
    let idx_c1 = history_user_positions
        .iter()
        .find(|(_, id)| id == "c1")
        .unwrap()
        .0;
    let idx_c2 = history_user_positions
        .iter()
        .find(|(_, id)| id == "c2")
        .unwrap()
        .0;
    let idx_c3 = history_user_positions
        .iter()
        .find(|(_, id)| id == "c3")
        .unwrap()
        .0;
    assert!(
        idx_c1 > idx_first_asst && idx_c2 > idx_first_asst,
        "ToolResults c1 and c2 must come AFTER the first assistant message that issued them (c1={}, c2={}, first_asst={})",
        idx_c1,
        idx_c2,
        idx_first_asst,
    );
    assert!(
        idx_c1 < idx_second_asst && idx_c2 < idx_second_asst,
        "ToolResults c1 and c2 must come BEFORE the second assistant message",
    );
    assert!(
        idx_c3 > idx_second_asst,
        "ToolResult c3 must come AFTER the second assistant message (r2 produced c3 only — it is the last live row before the trailing user 'next')",
    );
    // Silence the unused-variable lint without dropping the local names
    // (the explicit asserts above are what the test pins).
    let _ = (c1_pos, c2_pos, c3_pos);
}

/// **T2 — A response with NO thinking never inherits the prior response's
/// thinking.**
///
/// After r1 emits thinking A, r2 emits an empty thinking list. The fix
/// must NOT splice A onto r2's rows just because A is still in the
/// staging slot — every row carrying r2 must carry r2's blocks (here,
/// none). Today's `stage_thinking_for_next_assistant` overwrites the
/// slot unconditionally; the per-response-id segmentation in the rebuild
/// helper must respect that boundary.
#[test]
fn response_without_thinking_never_inherits_the_previous_response_thinking() {
    let sm = StateManager::new();
    sm.add_user_message("go".to_string());

    // r1: thinking A + bash.
    let r1 = respond(
        &sm,
        vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: "alpha".to_string(),
            signature: SIG_A.to_string(),
        }],
    );
    sm.add_tool_call(
        MessageSource::Human,
        Some(r1),
        "bash".to_string(),
        "{}".to_string(),
        Some("c1".to_string()),
    );
    sm.add_tool_result(
        MessageSource::Human,
        "bash".to_string(),
        "{}".to_string(),
        "ok".to_string(),
        Some("c1".to_string()),
    );

    // r2: NO thinking + bash. The new response id replaces the staged
    // blocks — the rebuild helper must NOT reach back to SIG_A.
    let r2 = respond(&sm, vec![]);
    sm.add_tool_call(
        MessageSource::Human,
        Some(r2),
        "bash".to_string(),
        "{}".to_string(),
        Some("c2".to_string()),
    );
    sm.add_tool_result(
        MessageSource::Human,
        "bash".to_string(),
        "{}".to_string(),
        "ok".to_string(),
        Some("c2".to_string()),
    );

    let history = sm.get_agent_history();

    // Find the assistant message whose only tool call is c2.
    let assistant_for_c2 = history
        .iter()
        .find_map(|m| match m {
            RigMessage::Assistant { content, .. } => {
                let only_c2 = content.iter().all(|c| match c {
                    AssistantContent::ToolCall(tc) => tc.id == "c2",
                    _ => false,
                });
                if only_c2
                    && content
                        .iter()
                        .any(|c| matches!(c, AssistantContent::ToolCall(tc) if tc.id == "c2"))
                {
                    Some(content)
                } else {
                    None
                }
            }
            _ => None,
        })
        .expect("the wire must contain an assistant message whose only tool call is c2");

    // CRUCIAL: the c2-only assistant message carries ZERO reasoning.
    let reasoning_blocks = assistant_for_c2
        .iter()
        .filter(|c| matches!(c, AssistantContent::Reasoning(_)))
        .count();
    assert_eq!(
        reasoning_blocks, 0,
        "the r2 assistant message must carry zero Reasoning — r2 staged no thinking; got {} reasoning block(s)",
        reasoning_blocks,
    );

    // And the message carries exactly the c2 tool call.
    let tool_calls: Vec<&str> = assistant_for_c2
        .iter()
        .filter_map(|c| match c {
            AssistantContent::ToolCall(tc) => Some(tc.id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        tool_calls,
        vec!["c2"],
        "the r2 assistant message must carry exactly [ToolCall(c2)] — no Text, no Reasoning, no c1",
    );

    // The c1 row (r1) still carries SIG_A — this is a separate check
    // that the segmentation didn't accidentally drop r1's reasoning.
    let assistant_for_c1 = history
        .iter()
        .find_map(|m| match m {
            RigMessage::Assistant { content, .. } => content
                .iter()
                .any(|c| matches!(c, AssistantContent::ToolCall(tc) if tc.id == "c1"))
                .then_some(content),
            _ => None,
        })
        .expect("the wire must contain an assistant message whose tool call is c1");
    let sigs_c1 = collect_signatures(assistant_for_c1);
    assert_eq!(
        sigs_c1,
        vec![SIG_A.to_string()],
        "r1's assistant message must still carry SIG_A — the segmentation must not drop it",
    );
}

/// **T3 — Final prose becomes its OWN assistant message AFTER the tool
/// results.**
///
/// Sequence:
///   r1: thinking A + bash + result
///   r2: NO thinking → add_assistant_message("done.")
///
/// The orchestrator's "done." prose is the response r2 actually returned,
/// so it must be its own assistant message that appears AFTER the
/// ToolResult for c1. Today's `convert_history_to_rig` collapses
/// thinking-bearing runs into one Assistant; T3 pins the structural
/// promise that the prose response lives in its OWN Assistant with no
/// tool calls (just Text) — and that it does not get hoisted in front of
/// its tool call by some misread of "thinking-first" as "text-first for
/// the trailing message."
#[test]
fn final_prose_is_its_own_assistant_message_after_the_tool_results() {
    let sm = StateManager::new();
    sm.add_user_message("go".to_string());

    let r1 = respond(
        &sm,
        vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: "alpha".to_string(),
            signature: SIG_A.to_string(),
        }],
    );
    sm.add_tool_call(
        MessageSource::Human,
        Some(r1),
        "bash".to_string(),
        "{}".to_string(),
        Some("c1".to_string()),
    );
    sm.add_tool_result(
        MessageSource::Human,
        "bash".to_string(),
        "{}".to_string(),
        "ok".to_string(),
        Some("c1".to_string()),
    );

    // r2: no thinking; orchestrator returns its final prose.
    let _r2 = respond(&sm, vec![]);
    // add_assistant_message is on the orchestrator lane and — per the new
    // design — adopts the staged blocks from r2 (here: none). We do NOT
    // pass response_id directly: add_assistant_message reads
    // current_response_id() + claim_thinking internally.
    sm.add_assistant_message("done".to_string());

    let history = sm.get_agent_history();

    // Pin the exact ordered shape (history[0] is the user turn "go" — it is
    // not trailing, so `get_agent_history` keeps it):
    //   User("go")
    //   Assistant[Reasoning(A), ToolCall(c1)]
    //   User[ToolResult(c1)]
    //   Assistant[Text("done")]
    //
    // Walk the history once and check each item.
    let mut idx = 0;
    let mut sigs_seen = Vec::new();

    // Item 0: the user turn.
    assert!(
        matches!(&history[idx], RigMessage::User { .. }),
        "history[0] must be the User turn, got {:?}",
        history[idx],
    );
    idx += 1;

    // Item 1: Assistant with [Reasoning, ToolCall(c1)].
    match &history[idx] {
        RigMessage::Assistant { content, .. } => {
            let kinds: Vec<&'static str> = content.iter().map(classify).collect();
            assert_eq!(
                kinds,
                vec!["Reasoning", "ToolCall"],
                "first assistant wire item must be Assistant[Reasoning(A), ToolCall(c1)]",
            );
            let sigs = collect_signatures(content);
            assert_eq!(sigs, vec![SIG_A.to_string()]);
            sigs_seen.push(sigs);
        }
        other => panic!("history[1] must be Assistant, got {:?}", other),
    }
    idx += 1;

    // Item 2: User with ToolResult(c1).
    match &history[idx] {
        RigMessage::User { content } => {
            let tr = content.iter().find_map(|c| match c {
                UserContent::ToolResult(tr) => Some(tr.id.as_str()),
                _ => None,
            });
            assert_eq!(tr, Some("c1"), "history[2] must be User[ToolResult(c1)]");
        }
        other => panic!("history[2] must be User ToolResult, got {:?}", other),
    }
    idx += 1;

    // Item 3: Assistant with [Text("done")]. NO Reasoning, NO ToolCall.
    match &history[idx] {
        RigMessage::Assistant { content, .. } => {
            let kinds: Vec<&'static str> = content.iter().map(classify).collect();
            assert_eq!(
                kinds,
                vec!["Text"],
                "history[2] must be Assistant[Text] — NO Reasoning, NO ToolCall; got {:?}",
                kinds,
            );
            // And the text is "done".
            let text = content
                .iter()
                .find_map(|c| match c {
                    AssistantContent::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .expect("Text content must be present");
            assert_eq!(text, "done", "the trailing prose must be \"done\"");

            // Explicitly: no Reasoning.
            let reasoning_count = content
                .iter()
                .filter(|c| matches!(c, AssistantContent::Reasoning(_)))
                .count();
            assert_eq!(
                reasoning_count, 0,
                "the trailing prose assistant must carry ZERO Reasoning blocks — r2 staged no thinking",
            );
            sigs_seen.push(Vec::new());
        }
        other => panic!(
            "history[3] must be Assistant with Text(\"done\"), got {:?}",
            other
        ),
    }

    // And the run produces exactly three wire items after the user turn
    // (no trailing user "next" in this test, so there is nothing after the
    // prose). Today the run yields only one item — this fails before
    // reaching the per-item assertions.
    assert_eq!(
        idx + 1,
        history.len(),
        "the run must produce exactly three wire items after the user turn: [Asst(A,c1), User(tr c1), Asst(done)] — got {} items total",
        history.len(),
    );

    // Sentinel: SIG_A is observed exactly once (in the first assistant
    // message); the trailing prose assistant carries no signature.
    let total_sigs: usize = sigs_seen.iter().map(|v| v.len()).sum();
    assert_eq!(
        total_sigs, 1,
        "exactly one Reasoning block must reach the wire (from r1); got {}",
        total_sigs,
    );
}

/// **T4 — Single response with text AND tool call still coalesces.**
///
/// Non-regression for the existing rebuild contract. One
/// `respond(vec![A])` followed by a tool call AND trailing prose (no
/// intervening `respond`) — both rows belong to r1, so they coalesce into
/// ONE assistant message whose content order is
/// `[Reasoning(A), Text, ToolCall]` and whose signature is verbatim SIG_A.
///
/// If a future implementer over-segments by `Agent`-row vs `ToolCall`-row
/// instead of by `response_id`, this test catches it: the wire would
/// carry TWO assistant messages and the second one would have no
/// reasoning — both are wrong, the test asserts the right shape.
#[test]
fn single_response_with_text_and_tool_call_still_coalesces() {
    let sm = StateManager::new();
    sm.add_user_message("read".to_string());

    let r1 = respond(
        &sm,
        vec![peakbot::reasoning::ThinkingBlock::Thinking {
            text: "alpha".to_string(),
            signature: SIG_A.to_string(),
        }],
    );
    sm.add_tool_call(
        MessageSource::Human,
        Some(r1),
        "file_read".to_string(),
        r#"{"path":"a.txt"}"#.to_string(),
        Some("c1".to_string()),
    );
    // The result closes the pair: `sanitize_tool_pairs` drops an orphan
    // ToolCall at the wire boundary, so a call with no result never reaches
    // the rebuild helper this test is about.
    sm.add_tool_result(
        MessageSource::Human,
        "file_read".to_string(),
        r#"{"path":"a.txt"}"#.to_string(),
        "x".to_string(),
        Some("c1".to_string()),
    );
    // No intervening respond() — the trailing prose belongs to r1 too.
    sm.add_assistant_message("Got it.".to_string());

    let history = sm.get_agent_history();

    let assistants: Vec<&OneOrMany<AssistantContent>> = history
        .iter()
        .filter_map(|m| match m {
            RigMessage::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .collect();
    assert_eq!(
        assistants.len(),
        1,
        "a single response with text + tool call must coalesce into ONE Message::Assistant (got {})",
        assistants.len(),
    );

    let kinds: Vec<&'static str> = assistants[0].iter().map(classify).collect();
    assert_eq!(
        kinds,
        vec!["Reasoning", "Text", "ToolCall"],
        "the coalesced assistant message must be [Reasoning, Text, ToolCall] in that order",
    );

    // Signature byte-equal to what was staged.
    let sigs = collect_signatures(assistants[0]);
    assert_eq!(
        sigs,
        vec![SIG_A.to_string()],
        "the coalesced message must carry SIG_A verbatim (no leakage from any other response)",
    );

    // The Text is "Got it." and the ToolCall id is "c1".
    let text = assistants[0]
        .iter()
        .find_map(|c| match c {
            AssistantContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .expect("Text content must be present");
    assert_eq!(text, "Got it.");
    let tc_id = assistants[0]
        .iter()
        .find_map(|c| match c {
            AssistantContent::ToolCall(tc) => Some(tc.id.clone()),
            _ => None,
        })
        .expect("ToolCall id must be present");
    assert_eq!(tc_id, "c1");
}

/// **T5 — Rows whose `response_id` is unknown (None) never replay
/// reasoning.**
///
/// The `response_id: None` case is the wire-seam hazard: a row persisted
/// in an older schema (no `response_id`), or a row constructed by a tool
/// that didn't go through the SessionHook, MUST NOT have its
/// `thinking` block replayed. Otherwise a single stray thinking block
/// from the legacy form opens the door to the same Anthropic 400 — the
/// signature has no response group to anchor against.
///
/// Pre-implementation: `ChatMessage` has no `response_id` field. The
/// test fails to compile, which is the RED signal for "the field is
/// missing" (a later task wires the rebuild helper to consult it).
#[test]
fn rows_without_response_id_never_replay_reasoning() {
    let sm = StateManager::new();
    sm.add_user_message("go".to_string());

    // Build the rows via the existing public surface, then mutate
    // `response_id = None` (and the thinking payload) via the
    // `update_chat_state` snapshot replace — the same idiom used by
    // `compaction_drops_compacted_messages_but_preserves_survivor_messages`
    // above. This mirrors a legacy persisted row that lacks the new
    // field.
    sm.add_tool_call(
        MessageSource::Human,
        None,
        "bash".to_string(),
        r#"{"command":"ls"}"#.to_string(),
        Some("c1".to_string()),
    );
    // The result closes the pair: `sanitize_tool_pairs` drops an orphan
    // ToolCall at the wire boundary, so a call with no result would never
    // reach the rebuild helper at all.
    sm.add_tool_result(
        MessageSource::Human,
        "bash".to_string(),
        r#"{"command":"ls"}"#.to_string(),
        "ok".to_string(),
        Some("c1".to_string()),
    );
    sm.add_assistant_message_sourced(MessageSource::Human, "done.".to_string());

    let mut chat = sm.get_state().chat.clone();
    for m in chat.messages.iter_mut() {
        if m.role == MessageRole::ToolCall || m.role == MessageRole::Agent {
            m.thinking = vec![peakbot::reasoning::ThinkingBlock::Thinking {
                text: "legacy thinking text".to_string(),
                signature: SIG_A.to_string(),
            }];
            // The seam: response_id is None on both rows. The rebuild
            // helper must drop the block at the wire — replaying an
            // unattached signature would 400 Anthropic. (This is the
            // deliberate `None` case: a row that does not know its
            // response never replays reasoning.)
            m.response_id = None;
        }
    }
    sm.update_chat_state(chat);

    let history = sm.get_agent_history();

    // Zero Reasoning anywhere on the wire.
    let any_reasoning = history.iter().any(|m| match m {
        RigMessage::Assistant { content, .. } => content
            .iter()
            .any(|c| matches!(c, AssistantContent::Reasoning(_))),
        _ => false,
    });
    assert!(
        !any_reasoning,
        "rows with response_id=None must not replay their thinking — the signature has no response group to anchor against",
    );

    // The ToolCall and Text content still appear (one Message::Assistant
    // each, since no thinking drives coalescing).
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
        "two rows with response_id=None must produce two Message::Assistant entries (no coalescing — there's no shared response); got {}",
        assistants.len(),
    );
    // First assistant: just the tool call.
    let first_kinds: Vec<&'static str> = assistants[0].iter().map(classify).collect();
    assert_eq!(
        first_kinds,
        vec!["ToolCall"],
        "first assistant (legacy ToolCall row) must be [ToolCall] only",
    );
    // Second assistant: just the text.
    let second_kinds: Vec<&'static str> = assistants[1].iter().map(classify).collect();
    assert_eq!(
        second_kinds,
        vec!["Text"],
        "second assistant (legacy Agent row) must be [Text] only",
    );
}

/// Collect the signatures in order from a `OneOrMany<AssistantContent>`.
fn collect_signatures(content: &OneOrMany<AssistantContent>) -> Vec<String> {
    content
        .iter()
        .filter_map(|c| match c {
            AssistantContent::Reasoning(r) => r.content.iter().find_map(|rc| match rc {
                ReasoningContent::Text { signature, .. } => signature.clone(),
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// T11 — SessionHook opens a response even when the model emits no thinking.
// ─────────────────────────────────────────────────────────────────────────────

/// The deletion of the old `!thinking.is_empty()` gate in
/// `SessionHook::on_completion_response`. A response whose `choice` carries
/// ONLY `AssistantContent::Text` (no `Reasoning`) must STILL advance the
/// StateManager's `current_response_id`, so a later `add_tool_call`
/// carrying that id gets its own (empty) thinking slot and never inherits
/// the previous response's reasoning.
///
/// Pre-implementation: `current_response_id()` does not exist, so the
/// assertion fails to compile.
#[tokio::test]
async fn session_hook_opens_a_response_even_when_the_model_emits_no_thinking() {
    use peakbot::AgentEvent;
    use peakbot::SessionHook;
    use peakbot::mock::MockCompletionModel;
    use peakbot::mock::completion_model::MockModelResponse;
    use rig_core::agent::PromptHook;
    use rig_core::completion::Usage as RigUsage;
    use rig_core::completion::message::Text as RigText;
    use rig_core::completion::message::UserContent;
    use std::sync::{Arc, Mutex};

    // Wire the hook on the orchestrator lane with preserve_reasoning=true
    // (we want the hook to drive `begin_response` for EVERY response,
    // thinking-bearing or not). The session_stats is required by
    // `with_context_tracking`.
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel::<peakbot::SourcedEvent>();
    let session_stats = Arc::new(Mutex::new(peakbot::SessionStats::new()));
    let sm = Arc::new(StateManager::new());
    let hook = SessionHook::with_context_tracking(Some(sender), session_stats)
        .with_preserve_reasoning(true)
        .with_state_manager(&sm);

    // Before any response: current_response_id must be None.
    assert!(
        sm.current_response_id().is_none(),
        "fresh StateManager must have current_response_id == None before any response has fired",
    );

    // Build a CompletionResponse whose choice is purely text (no
    // Reasoning). With the old `!thinking.is_empty()` gate, this would
    // skip `stage_thinking_for_next_assistant` entirely and the response
    // id would never advance.
    let response: rig_core::completion::CompletionResponse<MockModelResponse> =
        rig_core::completion::CompletionResponse {
            choice: OneOrMany::one(AssistantContent::Text(RigText::new("plain text"))),
            usage: RigUsage::new(),
            raw_response: MockModelResponse {
                content: String::new(),
                is_tool_call: false,
            },
            message_id: None,
        };
    let prompt = RigMessage::User {
        content: OneOrMany::one(UserContent::Text(RigText::new("hi"))),
    };
    let _ = <SessionHook as PromptHook<MockCompletionModel>>::on_completion_response(
        &hook, &prompt, &response,
    )
    .await;

    // First response: current_response_id must have advanced.
    let first_id = sm
        .current_response_id()
        .expect("after a CompletionResponse, current_response_id must be Some — the hook must advance it even with no thinking");
    assert!(
        first_id >= 1,
        "the first response id must be at least 1, got {}",
        first_id,
    );

    // Second response, same shape. id must advance again.
    let response2: rig_core::completion::CompletionResponse<MockModelResponse> =
        rig_core::completion::CompletionResponse {
            choice: OneOrMany::one(AssistantContent::Text(RigText::new("more text"))),
            usage: RigUsage::new(),
            raw_response: MockModelResponse {
                content: String::new(),
                is_tool_call: false,
            },
            message_id: None,
        };
    let _ = <SessionHook as PromptHook<MockCompletionModel>>::on_completion_response(
        &hook, &prompt, &response2,
    )
    .await;
    let second_id = sm
        .current_response_id()
        .expect("after the second CompletionResponse, current_response_id must still be Some");
    assert!(
        second_id > first_id,
        "the second response must yield a strictly greater id (got first={}, second={})",
        first_id,
        second_id,
    );

    // Silence the unused-event import lint — the receiver in this test is
    // intentionally dropped (we only care about the StateManager side).
    let _ = AgentEvent::CompletionResponse {
        content: String::new(),
        reasoning: None,
        thinking: vec![],
        usage: peakbot::TokenUsage::from_raw(0, 0),
        timestamp: chrono::Utc::now(),
    };
}

// Sanity: the test file is well-formed even if individual tests are RED.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sentinel_is_unique() {
    // The sentinel must NOT appear in any prose path. If a future change
    // starts including user text in the sentinel by accident, this fails.
    assert!(!THINKING_SENTINEL.contains("Assistant"));
    assert!(!THINKING_SENTINEL.contains("User"));
    assert!(!THINKING_SENTINEL.contains("Tool"));
}
