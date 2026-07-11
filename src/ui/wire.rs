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
//! {"type":"switch_cwd","path":"~/proj"}    // maps to UiAction::ChangeCwd
//! {"type":"list_dir","path":"~/proj"}      // browse for the cwd picker
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
//! {"type":"dir_listing","path":"…","parent":"…","entries":[...],"error":null}
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
    /// Commit a new working directory (the cwd picker's "Use this
    /// directory"). Maps to [`crate::ui::ui_trait::UiAction::ChangeCwd`] —
    /// same reset-and-rebuild seam as `/cd`. The backend re-resolves and
    /// validates `path`; the client never canonicalises.
    SwitchCwd {
        path: String,
    },
    /// Browse a directory for the cwd picker. Answered with a one-shot
    /// [`OutboundMessage::DirListing`] — a transient request/response, not
    /// session state (see that variant's note).
    ListDir {
        path: String,
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

/// One entry in a [`OutboundMessage::DirListing`]. Directories drive
/// navigation; files are shown greyed (this is a *folder* picker).
#[derive(Debug, Serialize)]
pub(crate) struct DirEntryWire {
    pub name: String,
    pub is_dir: bool,
}

/// Build a `dir_listing` reply for `path`. Resolves via the shared `/cd`
/// resolver (expands `~`, canonicalises, validates it's a directory), then
/// reads the entries. Any failure is folded into the returned frame's
/// `error` (empty `entries`), so the picker renders it inline rather than
/// dropping the frame. Directories sort first, then case-insensitive by
/// name. Hidden entries (leading `.`) are omitted — the picker is for
/// choosing a project cwd, not a file manager.
pub(crate) fn build_dir_listing(path: &str) -> OutboundMessage {
    let resolved = match crate::ui::repl::repl_impl::resolve_cd_path(path) {
        Ok(p) => p,
        Err(e) => {
            return OutboundMessage::DirListing {
                path: path.to_string(),
                parent: None,
                entries: Vec::new(),
                error: Some(e),
            };
        }
    };
    let dir = std::path::Path::new(&resolved);
    let parent = dir.parent().map(|p| p.to_string_lossy().into_owned());

    let mut entries: Vec<DirEntryWire> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    return None;
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                Some(DirEntryWire { name, is_dir })
            })
            .collect(),
        Err(e) => {
            return OutboundMessage::DirListing {
                path: resolved,
                parent,
                entries: Vec::new(),
                error: Some(format!("cannot read directory: {e}")),
            };
        }
    };
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    OutboundMessage::DirListing {
        path: resolved,
        parent,
        entries,
        error: None,
    }
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
    /// One-shot answer to `list_dir` — a transient request/response for the
    /// cwd picker's directory browser. Deliberately **not** part of
    /// `AppState`: browsing is ephemeral UI state, not session state, so it
    /// rides a side-channel rather than the state broadcast. `error` folds
    /// the failure inline (no such dir, permission denied) so the modal
    /// renders it in place; when `Some`, `entries` is empty. `parent` is
    /// `None` at the filesystem root.
    DirListing {
        path: String,
        parent: Option<String>,
        entries: Vec<DirEntryWire>,
        error: Option<String>,
    },
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
    fn switch_cwd_and_list_dir_parse() {
        let sc: InboundMessage =
            serde_json::from_str(r#"{"type":"switch_cwd","path":"~/p"}"#).unwrap();
        assert!(matches!(sc, InboundMessage::SwitchCwd { path } if path == "~/p"));

        let ld: InboundMessage =
            serde_json::from_str(r#"{"type":"list_dir","path":"/tmp"}"#).unwrap();
        assert!(matches!(ld, InboundMessage::ListDir { path } if path == "/tmp"));
    }

    #[test]
    fn build_dir_listing_lists_subdirs_and_folds_error() {
        let tmp = std::env::temp_dir().join(format!("pb_dl_{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("sub_b")).unwrap();
        std::fs::create_dir_all(tmp.join("sub_a")).unwrap();
        std::fs::write(tmp.join("a_file.txt"), b"x").unwrap();
        std::fs::create_dir_all(tmp.join(".hidden")).unwrap();

        let ok = build_dir_listing(tmp.to_str().unwrap());
        match ok {
            OutboundMessage::DirListing {
                entries,
                error,
                parent,
                ..
            } => {
                assert!(error.is_none());
                assert!(parent.is_some(), "a temp subdir always has a parent");
                let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
                assert!(!names.contains(&".hidden"), "hidden entries omitted");
                // Dirs sort first, then case-insensitive name.
                assert_eq!(names, vec!["sub_a", "sub_b", "a_file.txt"]);
            }
            _ => panic!("expected DirListing"),
        }

        // A non-existent path folds the error inline (frame still returned).
        let bad = build_dir_listing("/no/such/dir/anywhere/xyz");
        match bad {
            OutboundMessage::DirListing { error, entries, .. } => {
                assert!(error.is_some());
                assert!(entries.is_empty());
            }
            _ => panic!("expected DirListing"),
        }

        std::fs::remove_dir_all(&tmp).ok();
    }

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
