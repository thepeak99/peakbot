//! Shared wire protocol for the duplex NDJSON/WebSocket Views.
//!
//! `StdioUi` (`peakbot --stdio`) and `WebUi` (`peakbot --web`) speak the
//! *same* protocol — one over stdin/stdout NDJSON, one over WebSocket text
//! frames. The message shapes live here so a change to the format forces
//! both transports to change together (a *necessarily-same* dedup, not a
//! coincidental one). See `webui.md` §3 decision 2.
//!
//! ## Inbound (client → agent)
//!
//! ```json
//! {"type":"attach","convo":"aabbcc-…"}     // first frame; convo may be null
//! {"type":"send_message","text":"hello"}   // slash commands ride this too
//! {"type":"stop"}
//! {"type":"switch_model","alias":"sonnet"}
//! {"type":"request_conversations"}
//! {"type":"kill_session","convo":"aabbcc-…"}
//! {"type":"shutdown"}
//! ```
//!
//! ## Outbound (agent → client)
//!
//! ```json
//! {"type":"ready"}
//! {"type":"attached","convo":"aabbcc-…"}   // the real id the socket bound to
//! {"type":"models_available","active":"sonnet","models":[...]}
//! {"type":"state","state":{...AppState...}}
//! {"type":"conversations_list","items":[...]}
//! {"type":"error","message":"..."}
//! ```

use crate::StateManager;
use crate::config::ModelRegistry;
use crate::ui::app_state::AppState;
use serde::{Deserialize, Serialize};

/// Inbound message types from the client.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum InboundMessage {
    /// First frame of a WebSocket connection: bind to a conversation.
    /// `convo` is the `?convo=` id from the URL (may be `null` → the server
    /// mints a fresh session and reports the real id via `Attached`).
    Attach {
        convo: Option<String>,
    },
    /// `text` may be a slash command (`/new`, `/load <id>`, …) — peakbot
    /// classifies it internally.
    SendMessage {
        text: String,
    },
    Stop,
    SwitchModel {
        alias: String,
    },
    RequestConversations,
    /// End an active session for *everyone* attached to it (dropdown "kill").
    KillSession {
        convo: String,
    },
    Shutdown,
}

/// One `models_available` entry — the subset of `ResolvedModel` a model
/// picker needs.
#[derive(Debug, Serialize, Clone)]
pub struct ModelInfo {
    /// What `/model <alias>` accepts.
    pub alias: String,
    pub provider_name: String,
    /// Model name as the provider knows it.
    pub model_name: String,
    pub context_size: usize,
}

/// Snapshot for the one-shot `models_available` emission. Empty for the
/// legacy single-provider path, where the View skips the emission.
pub fn build_models_snapshot(registry: &ModelRegistry) -> Vec<ModelInfo> {
    registry
        .iter_sorted()
        .into_iter()
        .map(|(_, rm)| ModelInfo {
            alias: rm.alias.clone(),
            provider_name: rm.provider_name.clone(),
            model_name: rm.model_name.clone(),
            context_size: rm.context_size,
        })
        .collect()
}

/// Trimmed subset of `ConversationSummary` for a dropdown picker
/// (backend already sorts newest first).
#[derive(Debug, Serialize)]
pub(crate) struct ConversationSummaryWire {
    /// Fed back as `/load <id>`.
    id: String,
    name: String,
    /// ISO 8601 UTC.
    updated_at: String,
    message_count: usize,
    model: String,
    /// `true` when a live session is currently bound to this conversation
    /// (registry membership). Runtime-only, never persisted.
    active: bool,
}

/// Outbound message envelopes to the client. Owning (no lifetime) so any
/// task can push to the shared writer channel.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum OutboundMessage {
    /// Handshake, sent once before any user input so the client can render
    /// the welcome banner.
    Ready,
    /// Reply to the first `Attach`: the real conversation id this socket is
    /// bound to. The client writes it into the URL (`?convo=…`) so a refresh
    /// or bookmark re-attaches to the same live session.
    Attached { convo: String },
    /// One-shot registry snapshot, emitted right after `ready`. `active`
    /// may be empty in the legacy single-provider path.
    ModelsAvailable {
        active: String,
        models: Vec<ModelInfo>,
    },
    /// A full `AppState` snapshot — the same broadcast the TUI sees. Boxed
    /// to keep the enum slim (clippy `large_enum_variant`).
    State { state: Box<AppState> },
    /// Reply to `request_conversations`; empty when no storage is configured.
    ConversationsList { items: Vec<ConversationSummaryWire> },
    /// A non-fatal protocol or parse error.
    Error { message: String },
}

/// Wire-shaped snapshot of saved conversations. Empty when no storage is
/// configured — the client treats that as "hide the picker". `active_ids`
/// carries the conversation ids with a live session bound (a web-layer
/// concern injected here so this stays the single snapshot builder for both
/// transports; stdio passes an empty set).
pub(crate) fn build_conversations_snapshot(
    sm: &StateManager,
    active_ids: &std::collections::HashSet<uuid::Uuid>,
) -> Vec<ConversationSummaryWire> {
    sm.list_conversations()
        .unwrap_or_default()
        .into_iter()
        .map(|s| ConversationSummaryWire {
            active: active_ids.contains(&s.id),
            id: s.id.to_string(),
            name: s.name,
            updated_at: s.updated_at.to_rfc3339(),
            message_count: s.message_count,
            model: s.model,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_parses_with_and_without_convo() {
        let with: InboundMessage =
            serde_json::from_str(r#"{"type":"attach","convo":"abc-123"}"#).unwrap();
        assert!(matches!(with, InboundMessage::Attach { convo: Some(c) } if c == "abc-123"));

        let without: InboundMessage =
            serde_json::from_str(r#"{"type":"attach","convo":null}"#).unwrap();
        assert!(matches!(without, InboundMessage::Attach { convo: None }));
    }

    #[test]
    fn kill_session_parses() {
        let m: InboundMessage =
            serde_json::from_str(r#"{"type":"kill_session","convo":"xyz"}"#).unwrap();
        assert!(matches!(m, InboundMessage::KillSession { convo } if convo == "xyz"));
    }

    #[test]
    fn attached_serializes_with_tag_and_convo() {
        let m = OutboundMessage::Attached {
            convo: "id-1".to_string(),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, r#"{"type":"attached","convo":"id-1"}"#);
    }

    #[test]
    fn conversation_summary_carries_active_flag() {
        let wire = ConversationSummaryWire {
            id: "i".into(),
            name: "n".into(),
            updated_at: "t".into(),
            message_count: 0,
            model: "m".into(),
            active: true,
        };
        let json = serde_json::to_string(&wire).unwrap();
        assert!(json.contains(r#""active":true"#), "json = {json}");
    }
}
