//! Boundary-only sanitization of tool-call/result pairs in chat history.
//!
//! The rig wire layer assumes that every [`MessageRole::ToolCall`] is
//! immediately followed by a [`MessageRole::ToolResult`] with the same
//! `call_id`. Three paths can break that:
//!
//! 1. **Loading a conversation from disk** ([`StateManager::sync_from_conversation`])
//!    — a file may have been truncated mid-write, edited by hand, or
//!    produced by an older version with a different invariant.
//! 2. **Context compaction** ([`StateManager::replace_chat_messages`])
//!    — summarisation can drop a `ToolResult` while keeping its `ToolCall`,
//!    or vice-versa.
//! 3. **Concurrent appends at runtime** — `add_tool_call` / `add_tool_result`
//!    are not one atomic unit, and another task (the `bash_bg` drain seam
//!    calling `add_user_message_from_background`) can append a User message
//!    into the gap between them. The `RwLock` orders individual pushes, not
//!    pairs. Hence [`StateManager::get_agent_history`] sanitizes too: it is
//!    the last point before the wire, where an unpaired `ToolCall` turns
//!    into a 400 that wedges the conversation for good.
//!
//! [`sanitize_tool_pairs`] is the single repair function applied at all
//! three points; orphans are dropped silently. See `opus-proposal.md` for
//! the design rationale and the explicit decision to **not** log or report
//! drops — sanitization is a boundary repair, not a fault.
//!
//! [`StateManager::sync_from_conversation`]: crate::state::StateManager
//! [`StateManager::replace_chat_messages`]: crate::state::StateManager::replace_chat_messages
//! [`StateManager::get_agent_history`]: crate::state::StateManager::get_agent_history

use crate::ui::app_state::{ChatMessage, MessageRole};
use std::borrow::Borrow;

/// Drop tool-call/result messages that violate the
/// `ToolCall(id) → ToolResult(id)` adjacent-pair invariant.
///
/// After this function returns, every [`MessageRole::ToolCall`] in the
/// result is immediately followed by a [`MessageRole::ToolResult`] with
/// the same `call_id` (or both `None`, for pre-v4 messages without IDs).
/// All other tool messages are dropped **silently**. Non-tool messages
/// pass through unchanged in order.
///
/// The function is pure and stateless. It is generic over anything that
/// borrows a [`ChatMessage`] so the wire-boundary caller can sanitize a
/// `Vec<&ChatMessage>` without cloning the transcript on every request.
pub fn sanitize_tool_pairs<T: Borrow<ChatMessage>>(messages: Vec<T>) -> Vec<T> {
    let mut out = Vec::with_capacity(messages.len());
    let mut iter = messages.into_iter().peekable();
    while let Some(msg) = iter.next() {
        match msg.borrow().role {
            MessageRole::ToolCall => {
                let call_id = &msg.borrow().call_id;
                let paired = iter.peek().is_some_and(|next| {
                    let next = next.borrow();
                    next.role == MessageRole::ToolResult && &next.call_id == call_id
                });
                if paired {
                    out.push(msg);
                    out.extend(iter.next());
                }
                // Otherwise an orphan ToolCall: dropped.
            }
            // Any matching ToolResult would have been consumed by the
            // ToolCall arm above. Reaching here means orphan.
            MessageRole::ToolResult => {}
            _ => out.push(msg),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let out = sanitize_tool_pairs(Vec::<ChatMessage>::new());
        assert!(out.is_empty());
    }

    #[test]
    fn canonical_pair_passes_through() {
        let input = vec![
            ChatMessage::user("hi".into()),
            ChatMessage::agent("thinking".into()),
            tc(Some("x")),
            tr(Some("x")),
            ChatMessage::agent("done".into()),
        ];
        let out = sanitize_tool_pairs(input);
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
    }

    #[test]
    fn orphan_call_dropped() {
        let input = vec![
            ChatMessage::user("hi".into()),
            tc(Some("x")),
            ChatMessage::agent("done".into()),
        ];
        let out = sanitize_tool_pairs(input);
        assert_eq!(roles(&out), vec![MessageRole::User, MessageRole::Agent]);
    }

    #[test]
    fn orphan_result_dropped() {
        let input = vec![
            ChatMessage::user("hi".into()),
            tr(Some("x")),
            ChatMessage::agent("done".into()),
        ];
        let out = sanitize_tool_pairs(input);
        assert_eq!(roles(&out), vec![MessageRole::User, MessageRole::Agent]);
    }

    #[test]
    fn consecutive_calls_first_dropped() {
        let input = vec![
            ChatMessage::user("hi".into()),
            tc(Some("x")),
            tc(Some("y")),
            tr(Some("y")),
            ChatMessage::agent("done".into()),
        ];
        let out = sanitize_tool_pairs(input);
        // First ToolCall is orphaned (next is another ToolCall, not a Result).
        // Second pair survives intact.
        assert_eq!(
            roles(&out),
            vec![
                MessageRole::User,
                MessageRole::ToolCall,
                MessageRole::ToolResult,
                MessageRole::Agent,
            ]
        );
        assert_eq!(out[1].call_id.as_deref(), Some("y"));
    }

    #[test]
    fn consecutive_results_both_dropped() {
        let input = vec![
            ChatMessage::user("hi".into()),
            tr(Some("x")),
            tr(Some("y")),
            ChatMessage::agent("done".into()),
        ];
        let out = sanitize_tool_pairs(input);
        assert_eq!(roles(&out), vec![MessageRole::User, MessageRole::Agent]);
    }

    #[test]
    fn mismatched_call_id_both_dropped() {
        let input = vec![tc(Some("x")), tr(Some("y"))];
        let out = sanitize_tool_pairs(input);
        assert!(out.is_empty());
    }

    #[test]
    fn legacy_none_ids_pair_correctly() {
        let input = vec![tc(None), tr(None)];
        let out = sanitize_tool_pairs(input);
        assert_eq!(
            roles(&out),
            vec![MessageRole::ToolCall, MessageRole::ToolResult]
        );
    }
}
