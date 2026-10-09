//! Shared wire protocol for the duplex NDJSON/WebSocket Views.
//!
//! `StdioUi` (`peakbot --stdio`) and `WebUi` (bare `peakbot`) speak the
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
//! {"type":"send_message","text":"hi","attachments":["<upload-uuid>"]}  // ids optional
//! {"type":"stop"}
//! {"type":"switch_model","alias":"sonnet"}
//! {"type":"switch_cwd","path":"~/proj"}    // maps to UiAction::ChangeCwd
//! {"type":"list_dir","path":"~/proj"}      // browse for the cwd picker
//! {"type":"request_conversations"}
//! {"type":"request_recent_dirs"}
//! {"type":"select_pipeline","name":"web-team"}  // null clears the binding
//! {"type":"kill_session","convo":"aabbcc-…"}
//! {"type":"shutdown"}
//! {"type":"pause"}
//! {"type":"resume"}
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
//! {"type":"recent_dirs","dirs":[...]}
//! {"type":"dir_listing","path":"…","parent":"…","entries":[...],"error":null}
//! {"type":"error","message":"..."}
//! ```

use crate::StateManager;
use crate::config::ModelRegistry;
use crate::ui::app_state::AppState;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

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
        /// Upload ids from `POST /api/uploads`; absent ⇒ none (old clients
        /// and stdio).
        #[serde(default)]
        attachments: Vec<Uuid>,
    },
    Stop,
    /// Request the currently-running (pausable) sub-agent pause at its
    /// next checkpoint. Maps to [`crate::ui::ui_trait::UiAction::PauseSubAgent`].
    Pause,
    /// Resume a paused (or pausing) sub-agent. Maps to
    /// [`crate::ui::ui_trait::UiAction::ResumeSubAgent`].
    Resume,
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
    /// One-shot request for the cwd picker's "Recent" section.
    RequestRecentDirs,
    /// Bind the current conversation to a named pipeline (the Agents panel
    /// selector); `name: null` clears the binding. Maps to
    /// [`crate::ui::ui_trait::UiAction::SelectPipeline`]. The backend enforces
    /// the lock (rejected once the conversation has turns).
    SelectPipeline {
        name: Option<String>,
    },
    /// Bind the current conversation to a config profile (the Profile tab
    /// picker); `name: null` — or an omitted `name` — clears the binding.
    /// Maps to [`crate::ui::ui_trait::UiAction::SelectProfile`]. The backend
    /// enforces the lock and the boot `--profile` pin.
    SelectProfile {
        name: Option<String>,
    },
    /// End an active session for *everyone* attached to it (dropdown "kill").
    KillSession {
        convo: String,
    },
    Shutdown,
}

/// One `models_available` entry — the subset of `ResolvedModel` a model
/// picker needs.
#[derive(Debug, Serialize, Deserialize, Clone)]
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
#[derive(Debug, Serialize, Deserialize)]
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
#[derive(Debug, Serialize, Deserialize)]
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
/// task can push to the shared writer channel. `Deserialize` is derived so
/// tests can round-trip recorded text frames; the live wire is serialize-only.
#[derive(Debug, Serialize, Deserialize)]
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
    /// A full `AppState` snapshot — the same broadcast the TUI sees.
    /// Behind an `Arc` so the coalescing slot and the writer can pass the
    /// snapshot around without deep-copying ~8 MiB; serialises identically
    /// to the previous `Box` (no frontend change).
    State { state: Arc<AppState> },
    /// Reply to `request_conversations` and `kill_session`; empty when no
    /// storage is configured. `error` is set iff a kill was refused.
    ConversationsList {
        items: Vec<ConversationSummaryWire>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Reply to `request_recent_dirs`; empty when no storage is configured.
    RecentDirs { dirs: Vec<String> },
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

/// How many recent conversations define "recent". Bounds *relevance*, not
/// cost — the summaries are already in memory. Without it, a single
/// two-year-old conversation in /tmp would surface as a "recent" directory
/// forever.
const RECENT_DIRS_LOOKBACK: usize = 200;

/// Rows in the picker's Recent section.
const RECENT_DIRS_MAX: usize = 8;

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

/// Distinct, existing working directories of the most recently updated
/// conversations, newest first, excluding the session's current cwd.
/// Empty when no storage is configured — the client hides the section.
pub(crate) fn build_recent_dirs(sm: &StateManager) -> Vec<String> {
    let session_cwd = sm.session_cwd().to_string_lossy().into_owned();
    let mut seen = std::collections::HashSet::new();
    let mut dirs = Vec::new();

    for s in sm
        .list_conversations()
        .unwrap_or_default()
        .into_iter()
        .take(RECENT_DIRS_LOOKBACK)
    {
        if s.cwd.is_empty() || s.cwd == session_cwd {
            continue;
        }
        // Insert every judged path (accepted or rejected) so a dead dir
        // repeated many times across old conversations is stat'ed once.
        if !seen.insert(s.cwd.clone()) {
            continue;
        }
        if !std::path::Path::new(&s.cwd).is_dir() {
            continue;
        }
        dirs.push(s.cwd);
        if dirs.len() >= RECENT_DIRS_MAX {
            break;
        }
    }

    dirs
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

    // ── T7 — send_message attachments (file-uploads design §5.3) ───────────
    //
    // RED: `InboundMessage::SendMessage.attachments` (`#[serde(default)]
    // Vec<Uuid>`) does not exist yet. A malformed UUID must invalidate the
    // whole frame — that is what produces the existing `invalid inbound
    // JSON` error frame on the wire.

    #[test]
    fn send_message_without_attachments_field_defaults_to_empty() {
        // Backward compatible: old clients (and stdio) omit the field.
        let m: InboundMessage =
            serde_json::from_str(r#"{"type":"send_message","text":"x"}"#).unwrap();
        match m {
            InboundMessage::SendMessage { text, attachments } => {
                assert_eq!(text, "x");
                assert!(
                    attachments.is_empty(),
                    "absent attachments must default to []"
                );
            }
            other => panic!("expected SendMessage, got {other:?}"),
        }
    }

    #[test]
    fn send_message_with_attachment_ids_parses() {
        let m: InboundMessage = serde_json::from_str(
            r#"{"type":"send_message","text":"what's in these?","attachments":["5b7c1a2e-0000-4000-8000-000000000001","9e21f4d6-0000-4000-8000-000000000002"]}"#,
        )
        .unwrap();
        match m {
            InboundMessage::SendMessage { text, attachments } => {
                assert_eq!(text, "what's in these?");
                assert_eq!(attachments.len(), 2);
                assert_eq!(
                    attachments[0].to_string(),
                    "5b7c1a2e-0000-4000-8000-000000000001"
                );
                assert_eq!(
                    attachments[1].to_string(),
                    "9e21f4d6-0000-4000-8000-000000000002"
                );
            }
            other => panic!("expected SendMessage, got {other:?}"),
        }
    }

    #[test]
    fn send_message_with_malformed_uuid_is_invalid_frame() {
        assert!(
            serde_json::from_str::<InboundMessage>(
                r#"{"type":"send_message","text":"x","attachments":["not-a-uuid"]}"#
            )
            .is_err(),
            "a malformed attachment UUID must invalidate the frame"
        );
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

    // ── conversations_list kill-refusal error (RED) ────────────────────────
    //
    // The kill refusal rides the existing conversations_list reply: `error`
    // is `Some(…)` iff the kill was refused because the session is still
    // running. When `None`, the key must be omitted entirely
    // (skip_serializing_if) so every non-kill reply keeps its current
    // byte-identical shape. Compile-RED until the `error` field lands.

    #[test]
    fn conversations_list_omits_error_when_none() {
        let m = OutboundMessage::ConversationsList {
            items: vec![],
            error: None,
        };
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(
            json, r#"{"type":"conversations_list","items":[]}"#,
            "a null error must be omitted, not serialised as null"
        );
    }

    #[test]
    fn conversations_list_serializes_error_when_some() {
        let m = OutboundMessage::ConversationsList {
            items: vec![],
            error: Some("Can't kill a running conversation — press Stop first.".to_string()),
        };
        let json = serde_json::to_string(&m).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["type"], "conversations_list");
        assert_eq!(
            parsed["error"], "Can't kill a running conversation — press Stop first.",
            "a refused kill must carry the message on the wire"
        );
    }

    /// `State` now carries `Arc<AppState>`; serde with the `rc` feature
    /// emits the inner value with no wrapper, identical to the previous
    /// `Box<AppState>` — no frontend change. This test guards that invariant.
    #[test]
    fn state_frame_serialises_identically_with_arc_or_box() {
        // We cannot construct both `Box<AppState>` and `Arc<AppState>` values
        // here (the field type is fixed), so we lock the JSON shape against
        // the literal the SPA expects: an object with `type:"state"` and a
        // nested `state:{...}` whose inner object starts with the AppState
        // fields. Drift here means the frontend breaks.
        let m = OutboundMessage::State {
            state: Arc::new(AppState::new()),
        };
        let json = serde_json::to_string(&m).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["type"], "state");
        assert!(
            parsed["state"].is_object(),
            "state must be an object: {json}"
        );
        // AppState serialises with `chat` as its top-level field; drift
        // here means the frontend breaks.
        let state = &parsed["state"];
        assert!(state.get("chat").is_some(), "missing chat: {json}");
    }

    // ── recent_dirs (feature under test — RED until implemented) ─────────
    //
    // These tests are written against the locked design:
    //   - `build_recent_dirs(sm)` walks `sm.list_conversations()` newest-first,
    //     `.take(RECENT_DIRS_LOOKBACK)` (=200), and for each cwd IN ORDER:
    //     skip empty; skip == session_cwd; skip if already in `seen` (seen
    //     records EVERY judged path, accepted or rejected); insert into seen;
    //     skip if !is_dir(); else push; break at RECENT_DIRS_MAX (=8).
    //   - inbound `{"type":"request_recent_dirs"}` / outbound
    //     `{"type":"recent_dirs","dirs":[...]}`.
    //
    // They reference `build_recent_dirs`, `RECENT_DIRS_MAX`,
    // `RECENT_DIRS_LOOKBACK`, `InboundMessage::RequestRecentDirs` and
    // `OutboundMessage::RecentDirs`, none of which exist yet — the crate's
    // test build must fail to compile until the feature lands.

    use crate::conversation::Conversation;
    use crate::storage::{ConversationStorage, FileStorage};
    use chrono::{DateTime, Utc};
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// A StateManager backed by a fresh FileStorage in `storage_dir`, with
    /// `session_cwd` pinned so the session-cwd skip is deterministic.
    fn sm_and_store(
        storage_dir: PathBuf,
        session_cwd: PathBuf,
    ) -> (Arc<StateManager>, Arc<FileStorage>) {
        let store: Arc<FileStorage> = Arc::new(FileStorage::new(storage_dir).unwrap());
        // `Arc::clone(&store)` would let inference pick `Self = Arc<dyn
        // ConversationStorage>` from the annotation and then fail to unify
        // `&store`'s concrete type; an explicit `as` coercion sidesteps that.
        let storage = store.clone() as Arc<dyn ConversationStorage>;
        let sm = StateManager::new_arc_with_storage(storage);
        sm.set_session_cwd(session_cwd);
        (sm, store)
    }

    /// Save a conversation with an explicit `updated_at` (so `list()`'s
    /// newest-first sort is deterministic — never wall-clock dependent) and
    /// the given `cwd`.
    fn save_conv(store: &FileStorage, cwd: &str, updated_at: DateTime<Utc>) {
        let mut conv = Conversation::new(
            "convo".to_string(),
            "test-prov".to_string(),
            "test-model".to_string(),
            cwd.to_string(),
        );
        conv.updated_at = updated_at;
        store.save(&conv).unwrap();
    }

    fn path_str(p: &std::path::Path) -> String {
        p.to_string_lossy().into_owned()
    }

    /// A path that does not exist on any sane machine — the dead-dir fixture.
    const DEAD_DIR: &str = "/nonexistent/peakbot/recent/dirs/dead";

    /// Newest-first: distinct live dirs come back in `list()` order (most
    /// recently updated conversation first).
    #[test]
    fn build_recent_dirs_returns_distinct_dirs_newest_first() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("alpha");
        let b = tmp.path().join("beta");
        let c = tmp.path().join("gamma");
        for d in [&a, &b, &c] {
            std::fs::create_dir(d).unwrap();
        }
        let (sm, store) = sm_and_store(tmp.path().join("storage"), tmp.path().join("session"));

        let now = Utc::now();
        // Newest → oldest: alpha, beta, gamma.
        save_conv(&store, &path_str(&a), now);
        save_conv(&store, &path_str(&b), now - chrono::Duration::seconds(60));
        save_conv(&store, &path_str(&c), now - chrono::Duration::seconds(120));

        let dirs = build_recent_dirs(&sm);
        assert_eq!(dirs, vec![path_str(&a), path_str(&b), path_str(&c)]);
    }

    /// Dedupe: a cwd repeated across conversations appears exactly once, at
    /// its NEWEST occurrence's position (iteration is newest-first, so
    /// first-seen wins).
    #[test]
    fn build_recent_dirs_dedupes_keeping_newest_occurrence() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("alpha");
        let b = tmp.path().join("beta");
        for d in [&a, &b] {
            std::fs::create_dir(d).unwrap();
        }
        let (sm, store) = sm_and_store(tmp.path().join("storage"), tmp.path().join("session"));

        let now = Utc::now();
        // alpha (newest), beta, alpha again (oldest) → [alpha, beta].
        save_conv(&store, &path_str(&a), now);
        save_conv(&store, &path_str(&b), now - chrono::Duration::seconds(60));
        save_conv(&store, &path_str(&a), now - chrono::Duration::seconds(120));

        let dirs = build_recent_dirs(&sm);
        assert_eq!(dirs, vec![path_str(&a), path_str(&b)]);
    }

    /// Pre-cwd conversations persist an empty `cwd`; those summaries are
    /// skipped, live dirs still surface.
    #[test]
    fn build_recent_dirs_skips_empty_cwd() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("alpha");
        let b = tmp.path().join("beta");
        for d in [&a, &b] {
            std::fs::create_dir(d).unwrap();
        }
        let (sm, store) = sm_and_store(tmp.path().join("storage"), tmp.path().join("session"));

        let now = Utc::now();
        // Newest conversation is a pre-cwd file (empty cwd) → skipped.
        save_conv(&store, "", now);
        save_conv(&store, &path_str(&a), now - chrono::Duration::seconds(60));
        save_conv(&store, &path_str(&b), now - chrono::Duration::seconds(120));

        let dirs = build_recent_dirs(&sm);
        assert_eq!(dirs, vec![path_str(&a), path_str(&b)]);
    }

    /// A dir that no longer exists (deleted after the conversation was
    /// saved) must not surface; live dirs still do.
    #[test]
    fn build_recent_dirs_skips_nonexistent_dir() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("alpha");
        std::fs::create_dir(&a).unwrap();
        let (sm, store) = sm_and_store(tmp.path().join("storage"), tmp.path().join("session"));

        let now = Utc::now();
        save_conv(&store, DEAD_DIR, now);
        save_conv(&store, &path_str(&a), now - chrono::Duration::seconds(60));

        let dirs = build_recent_dirs(&sm);
        assert_eq!(dirs, vec![path_str(&a)]);
    }

    /// The session's own cwd is never a "recent dir" — it is where we are.
    #[test]
    fn build_recent_dirs_skips_current_session_cwd() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("alpha");
        let b = tmp.path().join("beta");
        for d in [&a, &b] {
            std::fs::create_dir(d).unwrap();
        }
        // session_cwd == a: a's conversations must be skipped.
        let (sm, store) = sm_and_store(tmp.path().join("storage"), a.clone());

        let now = Utc::now();
        save_conv(&store, &path_str(&a), now);
        save_conv(&store, &path_str(&b), now - chrono::Duration::seconds(60));

        let dirs = build_recent_dirs(&sm);
        assert_eq!(dirs, vec![path_str(&b)]);
    }

    /// More distinct live dirs than RECENT_DIRS_MAX → exactly
    /// RECENT_DIRS_MAX, the newest ones.
    #[test]
    fn build_recent_dirs_caps_at_recent_dirs_max() {
        let tmp = TempDir::new().unwrap();
        let dirs: Vec<std::path::PathBuf> =
            (0..10).map(|i| tmp.path().join(format!("d{i}"))).collect();
        for d in &dirs {
            std::fs::create_dir(d).unwrap();
        }
        let (sm, store) = sm_and_store(tmp.path().join("storage"), tmp.path().join("session"));

        let now = Utc::now();
        // d0 newest … d9 oldest.
        for (i, d) in dirs.iter().enumerate() {
            save_conv(
                &store,
                &path_str(d),
                now - chrono::Duration::seconds(i as i64 * 60),
            );
        }

        let out = build_recent_dirs(&sm);
        assert_eq!(out.len(), RECENT_DIRS_MAX, "cap must hold: {out:?}");
        let expected: Vec<String> = dirs
            .iter()
            .take(RECENT_DIRS_MAX)
            .map(|d| path_str(d))
            .collect();
        assert_eq!(out, expected);
    }

    /// A live dir that only appears beyond the RECENT_DIRS_LOOKBACK window
    /// is excluded: the (lookback+1)th-newest conversation's cwd is never
    /// inspected. The newest `RECENT_DIRS_LOOKBACK` conversations all point
    /// at a dead dir (skipped), the oldest points at a live one.
    #[test]
    fn build_recent_dirs_excludes_dirs_beyond_lookback_window() {
        let tmp = TempDir::new().unwrap();
        let beyond = tmp.path().join("beyond");
        std::fs::create_dir(&beyond).unwrap();
        let (sm, store) = sm_and_store(tmp.path().join("storage"), tmp.path().join("session"));

        let now = Utc::now();
        // Newest RECENT_DIRS_LOOKBACK conversations: all dead-dir cwds.
        for i in 0..RECENT_DIRS_LOOKBACK {
            save_conv(
                &store,
                DEAD_DIR,
                now - chrono::Duration::seconds(i as i64 * 60),
            );
        }
        // The (lookback+1)th-newest: a live dir, outside the window.
        save_conv(
            &store,
            &path_str(&beyond),
            now - chrono::Duration::seconds(RECENT_DIRS_LOOKBACK as i64 * 60),
        );

        assert_eq!(build_recent_dirs(&sm), Vec::<String>::new());
    }

    /// No storage configured → `list_conversations()` is `None` → empty vec
    /// (not an error, not a panic).
    #[test]
    fn build_recent_dirs_returns_empty_without_storage() {
        let sm = StateManager::new();
        assert_eq!(build_recent_dirs(&sm), Vec::<String>::new());
    }

    // ── recent_dirs wire protocol ─────────────────────────────────────────

    #[test]
    fn request_recent_dirs_parses() {
        let m: InboundMessage = serde_json::from_str(r#"{"type":"request_recent_dirs"}"#).unwrap();
        assert!(matches!(m, InboundMessage::RequestRecentDirs));
    }

    #[test]
    fn recent_dirs_serializes_with_tag_and_dirs() {
        let m = OutboundMessage::RecentDirs {
            dirs: vec!["/a".to_string(), "/b".to_string()],
        };
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, r#"{"type":"recent_dirs","dirs":["/a","/b"]}"#);
    }

    // ── pause/resume wire protocol ───────────────────────────────────────

    #[test]
    fn pause_parses() {
        let m: InboundMessage = serde_json::from_str(r#"{"type":"pause"}"#).unwrap();
        assert!(matches!(m, InboundMessage::Pause));
    }

    #[test]
    fn resume_parses() {
        let m: InboundMessage = serde_json::from_str(r#"{"type":"resume"}"#).unwrap();
        assert!(matches!(m, InboundMessage::Resume));
    }

    // ── select_profile wire protocol (Layer D, RED) ─────────────────────────
    //
    // D4: `InboundMessage::SelectProfile { name: Option<String> }` — the
    // profile-axis twin of `SelectPipeline`. Wire shape:
    //   {"type":"select_profile","name":"research"}  // bind
    //   {"type":"select_profile","name":null}         // clear
    //   {"type":"select_profile"}                     // name omitted → None
    //
    // Compile-RED until the variant exists (E0599 on `InboundMessage::SelectProfile`).

    #[test]
    fn select_profile_with_name_parses() {
        let m: InboundMessage =
            serde_json::from_str(r#"{"type":"select_profile","name":"research"}"#).unwrap();
        assert!(
            matches!(m, InboundMessage::SelectProfile { name: Some(n) } if n == "research"),
            "a named select_profile frame must carry the name through"
        );
    }

    #[test]
    fn select_profile_with_null_name_clears() {
        let m: InboundMessage =
            serde_json::from_str(r#"{"type":"select_profile","name":null}"#).unwrap();
        assert!(
            matches!(m, InboundMessage::SelectProfile { name: None }),
            "an explicit null must clear the binding, same as select_pipeline"
        );
    }

    /// `name` omitted entirely must also parse to `None` — a client that
    /// only ever sends `null` on clear and never the bare key must not be
    /// the only supported shape; both must work.
    #[test]
    fn select_profile_with_name_omitted_defaults_to_none() {
        let m: InboundMessage = serde_json::from_str(r#"{"type":"select_profile"}"#)
            .expect("an omitted `name` key must still parse");
        assert!(
            matches!(m, InboundMessage::SelectProfile { name: None }),
            "an omitted name field must default to None"
        );
    }
}
