//! Delta-sync primitives for the shared conversation cache.
//!
//! The client caches an authoritative prefix and sends
//! `since { n, timestamp, epoch }` on attach. `n` is the prefix length, the
//! timestamp is the exact raw wire string of message `n-1` (never parsed —
//! identical instants can serialise differently), and `epoch` is the durable
//! lineage token. Everything here validates that claim against the projected
//! history the server actually sends; trust nothing downstream.

#[cfg(test)]
use crate::conversation::Conversation;
use crate::ui::app_state::ChatMessage;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How many newly applied authoritative rows move the client's anchor.
/// Mirrored by `web/src/deltaSync.ts` (`ANCHOR_EVERY`) — the client owns the
/// cadence; the server only validates the resulting `sync_anchor`.
#[cfg(test)]
pub(crate) const REBASE_EVERY: usize = 10;

/// A cursor as it travels on the wire. `n` is a prefix length (>= 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachCursor {
    pub n: usize,
    pub timestamp: String,
    pub epoch: u64,
}

/// Parse a cursor leniently: any malformed shape is `None`, never an error.
/// This is what lets a bad `since` ride an `attach` without failing the frame.
pub(crate) fn parse_cursor_value(value: &Value) -> Option<AttachCursor> {
    let object = value.as_object()?;
    let n = usize::try_from(object.get("n")?.as_u64()?).ok()?;
    if n == 0 {
        return None;
    }
    let timestamp = object.get("timestamp")?.as_str()?.to_string();
    let epoch = object.get("epoch")?.as_u64()?;
    if epoch > crate::conversation::MAX_JS_SAFE_EPOCH {
        return None;
    }
    Some(AttachCursor {
        n,
        timestamp,
        epoch,
    })
}

/// Accept `cursor` iff it is exactly a prefix of `messages` and belongs to
/// `epoch`. Returns the prefix length so callers cannot accidentally use a
/// partially validated cursor.
pub(crate) fn validate_cursor(
    cursor: &AttachCursor,
    messages: &[ChatMessage],
    epoch: u64,
) -> Option<usize> {
    if cursor.n == 0 || cursor.n > messages.len() || cursor.epoch != epoch {
        return None;
    }
    let timestamp = serde_json::to_value(messages[cursor.n - 1].timestamp).ok()?;
    if timestamp.as_str()? != cursor.timestamp {
        return None;
    }
    Some(cursor.n)
}

/// The authoritative suffix from `n`; beyond the end is an empty tail (never
/// a panic), because the caller has already established that `n` is a prefix.
pub(crate) fn slice_from_messages(messages: &[ChatMessage], n: usize) -> Vec<ChatMessage> {
    messages
        .get(n..)
        .map(<[ChatMessage]>::to_vec)
        .unwrap_or_default()
}

/// True on every non-zero multiple of `every` newly applied rows.
#[cfg(test)]
pub(crate) fn should_rebase(applied: usize, every: usize) -> bool {
    every > 0 && applied > 0 && applied.is_multiple_of(every)
}

/// Test seam for the Conversation-shaped lineage contract. Production
/// validates against the projected `AppState` rows, which is exactly what
/// this projects first.
#[cfg(test)]
pub(crate) fn validate_cursor_for_conversation(
    cursor: &AttachCursor,
    conv: &Conversation,
) -> Option<usize> {
    let messages = crate::ui::app_state::chat_messages_from_conversation(conv);
    validate_cursor(cursor, &messages, conv.history_epoch)
}

/// Test seam: the projected authoritative suffix of a conversation.
#[cfg(test)]
pub(crate) fn slice_from_conversation(conv: &Conversation, n: usize) -> Vec<ChatMessage> {
    let messages = crate::ui::app_state::chat_messages_from_conversation(conv);
    slice_from_messages(&messages, n)
}
