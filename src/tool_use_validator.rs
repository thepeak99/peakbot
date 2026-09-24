//! Boundary-only sanitization of tool-call/result pairs in chat history.
//!
//! The rig wire layer 400s on a [`MessageRole::ToolCall`] not immediately
//! followed by its [`MessageRole::ToolResult`], and that wedges the
//! conversation for good. `add_tool_call` / `add_tool_result` are not one
//! atomic unit, so a concurrent append (the `bash_bg` drain seam) or a
//! transcript truncated mid-write can split a pair.
//!
//! [`sanitize_tool_pairs`] therefore runs at exactly one place:
//! [`StateManager::get_agent_history`] — the last stop before the wire, and
//! the only projection already filtered to the orchestrator lane. On the
//! full multi-lane transcript adjacency does not mean "paired": a `delegate`
//! pair is legitimately split by the sub-agent's turns, so sanitizing there
//! deletes real history (#271). An orphan call is **answered** with
//! [`UNRECORDED_RESULT`] — a silent strip would lie to the model about a
//! call whose side effects may have happened; a stray result (no matching
//! call) is dropped, because a result without its call cannot go on the wire.
//!
//! [`StateManager::get_agent_history`]: crate::state::StateManager::get_agent_history

use crate::ui::app_state::{ChatMessage, MessageRole};

/// The answer given to a `ToolCall` whose result was never recorded (the
/// turn was cancelled, or the transcript was truncated mid-write).
/// Deliberately conservative: the call's side effects may have happened, so
/// the model must check before redoing.
pub const UNRECORDED_RESULT: &str = "INTERRUPTED: no result was recorded for this call (turn was cancelled). Its side effects may have happened; check before redoing.";

/// Build the synthetic `ToolResult` that answers an interrupted
/// `ToolCall`. The single constructor for that row — the wire-boundary
/// validator and the Stop/teardown seams
/// (`StateManager::close_interrupted_tool_call`) both go through it. Copies
/// the call's identity and `source`, so the answer always sits on the same
/// lane as the call.
pub fn interrupted_result(call: &ChatMessage, text: &str) -> ChatMessage {
    ChatMessage::tool_result(
        call.tool_name.as_deref().unwrap_or_default(),
        call.tool_args.as_deref().unwrap_or_default(),
        text,
        call.call_id.clone(),
    )
    .with_source(call.source.clone())
}

/// Enforce the `ToolCall(id) → ToolResult(id)` adjacent-pair invariant.
///
/// After this function returns, every [`MessageRole::ToolCall`] in the
/// result is immediately followed by a [`MessageRole::ToolResult`] with
/// the same `call_id` (or both `None`, for pre-v4 messages without IDs).
/// An orphan call is answered with [`UNRECORDED_RESULT`]; a stray result
/// (no matching call) is dropped. Non-tool messages pass through unchanged
/// in order.
///
/// The function is pure and stateless: it never writes synthesized rows
/// back to the transcript, and it returns owned rows so it can synthesize.
pub fn sanitize_tool_pairs(messages: Vec<&ChatMessage>) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(messages.len());
    let mut iter = messages.into_iter().peekable();
    while let Some(msg) = iter.next() {
        match msg.role {
            MessageRole::ToolCall => {
                let paired = iter.peek().is_some_and(|next| {
                    next.role == MessageRole::ToolResult && next.call_id == msg.call_id
                });
                if paired {
                    out.push(msg.clone());
                    out.extend(iter.next().cloned());
                } else {
                    // Orphan call: answer it, never delete it — a silent
                    // strip would hide a call whose side effects may have
                    // happened.
                    out.push(msg.clone());
                    out.push(interrupted_result(msg, UNRECORDED_RESULT));
                }
            }
            // Any matching ToolResult would have been consumed by the
            // ToolCall arm above. Reaching here means orphan.
            MessageRole::ToolResult => {}
            _ => out.push(msg.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app_state::MessageSource;

    fn tc(id: Option<&str>) -> ChatMessage {
        ChatMessage::tool_call("bash", "{}", id.map(String::from))
    }

    fn tr(id: Option<&str>) -> ChatMessage {
        ChatMessage::tool_result("bash", "{}", "ok", id.map(String::from))
    }

    fn roles(msgs: &[ChatMessage]) -> Vec<MessageRole> {
        msgs.iter().map(|m| m.role).collect()
    }

    #[test]
    fn empty_input_is_clean() {
        let input: Vec<ChatMessage> = Vec::new();
        let out = sanitize_tool_pairs(input.iter().collect());
        assert!(out.is_empty());
    }

    #[test]
    fn canonical_pair_passes_through() {
        let input = [
            ChatMessage::user("hi".into()),
            ChatMessage::agent("thinking".into()),
            tc(Some("x")),
            tr(Some("x")),
            ChatMessage::agent("done".into()),
        ];
        let out = sanitize_tool_pairs(input.iter().collect());
        assert_eq!(
            roles(&out),
            vec![
                MessageRole::User,
                MessageRole::Agent,
                MessageRole::ToolCall,
                MessageRole::ToolResult,
                MessageRole::Agent,
            ]
        );
        // A paired call/result is untouched — the real result survives verbatim.
        assert_eq!(out[3].tool_result.as_deref(), Some("ok"));
    }

    #[test]
    fn orphan_call_gets_interrupted_result() {
        let input = [
            ChatMessage::user("hi".into()),
            tc(Some("x")),
            ChatMessage::agent("done".into()),
        ];
        let out = sanitize_tool_pairs(input.iter().collect());
        // The orphan is answered, not deleted: the call stays, and the
        // synthesized result carries its identity (I3).
        assert_eq!(
            roles(&out),
            vec![
                MessageRole::User,
                MessageRole::ToolCall,
                MessageRole::ToolResult,
                MessageRole::Agent,
            ]
        );
        assert_eq!(out[2].call_id.as_deref(), Some("x"));
        assert_eq!(out[2].tool_name.as_deref(), Some("bash"));
        assert_eq!(out[2].tool_result.as_deref(), Some(UNRECORDED_RESULT));
        assert_eq!(out[2].source, out[1].source);
    }

    #[test]
    fn synthesized_result_inherits_background_source() {
        let input = [
            ChatMessage::user("hi".into()),
            tc(Some("x")).with_source(MessageSource::Background { proc_ids: vec![1] }),
            ChatMessage::agent("done".into()),
        ];
        let out = sanitize_tool_pairs(input.iter().collect());
        assert_eq!(
            roles(&out),
            vec![
                MessageRole::User,
                MessageRole::ToolCall,
                MessageRole::ToolResult,
                MessageRole::Agent,
            ]
        );
        assert_eq!(
            out[2].source,
            MessageSource::Background { proc_ids: vec![1] },
            "the synthesized result must copy its call's source (I2)"
        );
    }

    #[test]
    fn orphan_result_dropped() {
        let input = [
            ChatMessage::user("hi".into()),
            tr(Some("x")),
            ChatMessage::agent("done".into()),
        ];
        let out = sanitize_tool_pairs(input.iter().collect());
        assert_eq!(roles(&out), vec![MessageRole::User, MessageRole::Agent]);
    }

    #[test]
    fn consecutive_calls_first_gets_interrupted_result() {
        let input = [
            ChatMessage::user("hi".into()),
            tc(Some("x")),
            tc(Some("y")),
            tr(Some("y")),
            ChatMessage::agent("done".into()),
        ];
        let out = sanitize_tool_pairs(input.iter().collect());
        // First ToolCall is orphaned (next is another ToolCall, not a Result):
        // it is answered, not deleted. Second pair survives intact.
        assert_eq!(
            roles(&out),
            vec![
                MessageRole::User,
                MessageRole::ToolCall,
                MessageRole::ToolResult,
                MessageRole::ToolCall,
                MessageRole::ToolResult,
                MessageRole::Agent,
            ]
        );
        assert_eq!(out[1].call_id.as_deref(), Some("x"));
        assert_eq!(out[2].call_id.as_deref(), Some("x"));
        assert_eq!(out[2].tool_result.as_deref(), Some(UNRECORDED_RESULT));
        assert_eq!(out[3].call_id.as_deref(), Some("y"));
        assert_eq!(out[4].tool_result.as_deref(), Some("ok"));
    }

    #[test]
    fn consecutive_results_both_dropped() {
        let input = [
            ChatMessage::user("hi".into()),
            tr(Some("x")),
            tr(Some("y")),
            ChatMessage::agent("done".into()),
        ];
        let out = sanitize_tool_pairs(input.iter().collect());
        assert_eq!(roles(&out), vec![MessageRole::User, MessageRole::Agent]);
    }

    #[test]
    fn mismatched_call_id_closes_call_and_drops_stray_result() {
        let input = [tc(Some("x")), tr(Some("y"))];
        let out = sanitize_tool_pairs(input.iter().collect());
        // The call is answered with the unrecorded marker; the stray result
        // (no matching call) is still dropped.
        assert_eq!(
            roles(&out),
            vec![MessageRole::ToolCall, MessageRole::ToolResult]
        );
        assert_eq!(out[0].call_id.as_deref(), Some("x"));
        assert_eq!(out[1].call_id.as_deref(), Some("x"));
        assert_eq!(out[1].tool_result.as_deref(), Some(UNRECORDED_RESULT));
    }

    #[test]
    fn legacy_none_ids_pair_correctly() {
        let input = [tc(None), tr(None)];
        let out = sanitize_tool_pairs(input.iter().collect());
        assert_eq!(
            roles(&out),
            vec![MessageRole::ToolCall, MessageRole::ToolResult]
        );
    }

    #[test]
    fn legacy_none_id_orphan_gets_none_id_result() {
        let input = [tc(None)];
        let out = sanitize_tool_pairs(input.iter().collect());
        assert_eq!(
            roles(&out),
            vec![MessageRole::ToolCall, MessageRole::ToolResult]
        );
        assert_eq!(out[1].call_id, None);
        assert_eq!(out[1].tool_result.as_deref(), Some(UNRECORDED_RESULT));
    }
}
