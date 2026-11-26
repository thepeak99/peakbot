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
//! {"type":"select","profile":"dev","agent":"team"}  // maps to UiAction::Select
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
//! {"type":"heartbeat"}                      // every 20 s, from the server that owns the session
//! ```

use crate::StateManager;
use crate::config::ModelRegistry;
use crate::ui::app_state::AppState;
use crate::ui::delta::AttachCursor;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

/// Wire protocol version, sent in `ready`. Bump it on any incompatible wire change.
pub const PROTOCOL_VERSION: u32 = 1;

/// Lenient cursor decoder: a malformed `since` must never fail the frame that
/// carries it. `None` means "no usable cursor" (fall back to a full send).
fn deserialize_cursor<'de, D>(deserializer: D) -> Result<Option<AttachCursor>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value
        .as_ref()
        .and_then(crate::ui::delta::parse_cursor_value))
}

/// Inbound message types from the client.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum InboundMessage {
    /// First frame of a WebSocket connection: bind to a conversation.
    /// `convo` is the `?convo=` id from the URL (may be `null` → the server
    /// mints a fresh session and reports the real id via `Attached`).
    Attach {
        convo: Option<String>,
        /// Bind a fresh conversation to this profile (ignored once it has turns).
        #[serde(default)]
        profile: Option<String>,
        /// With `profile`: the agent to start on; absent = the profile's first.
        #[serde(default)]
        agent: Option<String>,
        /// Set only by a relay; forbids chaining remote profiles.
        #[serde(default)]
        relayed: bool,
        /// Cached-prefix cursor. Absent/malformed ⇒ the server sends full
        /// frames; a malformed value never fails the attach.
        #[serde(default, deserialize_with = "deserialize_cursor")]
        since: Option<AttachCursor>,
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
    /// Select the `(profile, agent)` the conversation should run under —
    /// the Agents panel picker (replaces `select_pipeline` /
    /// `select_profile`). Maps to
    /// [`crate::ui::ui_trait::UiAction::Select`]. The backend enforces the
    /// lock (rejected once the conversation has turns) and the boot
    /// `--profile` pin.
    Select {
        profile: String,
        agent: String,
        /// The requested orchestrator model alias; absent = the agent's
        /// configured model.
        #[serde(default)]
        model: Option<String>,
        /// Subagent → model alias overrides (overridden subagents only).
        #[serde(default)]
        subagent_models: std::collections::BTreeMap<String, String>,
    },
    /// End an active session for *everyone* attached to it (dropdown "kill").
    KillSession {
        convo: String,
        /// Routing hint for a relay; the local path ignores it.
        #[serde(default)]
        profile: Option<String>,
    },
    Shutdown,
    /// Move this socket's delta base (the client's periodic rebase). `rebase`
    /// is accepted as an alias. Validation matches `attach`; a malformed or
    /// invalid cursor falls back to full frames for the rest of the socket.
    #[serde(alias = "rebase")]
    SyncAnchor {
        #[serde(default, deserialize_with = "deserialize_cursor")]
        since: Option<AttachCursor>,
    },
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
    /// The server-composed absolute path of this entry (listing path +
    /// name, joined with the platform separator). The client sends this
    /// back verbatim as the next `list_dir` payload — it never composes
    /// paths itself (a client-side `/` concat is garbage on Windows, where
    /// verbatim-style paths don't accept `/` as a separator: os error 123).
    pub full_path: String,
}

/// Build a `dir_listing` reply for `path`. Resolves via the shared `/cd`
/// resolver (expands `~`, canonicalises, validates it's a directory), then
/// reads the entries. Any failure is folded into the returned frame's
/// `error` (empty `entries`), so the picker renders it inline rather than
/// dropping the frame. Directories sort first, then case-insensitive by
/// name. Hidden entries (leading `.`) are omitted — the picker is for
/// choosing a project cwd, not a file manager.
pub(crate) fn build_dir_listing(path: &str) -> OutboundMessage {
    let resolved = match crate::utils::resolve_cd_path(path) {
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
                let full_path = dir.join(&name).to_string_lossy().into_owned();
                Some(DirEntryWire {
                    name,
                    is_dir,
                    full_path,
                })
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
/// (backend already sorts newest first). Fields are `pub(crate)` so the
/// T7 list merge can read `profile` to tag kept rows and rewrite it to L.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ConversationSummaryWire {
    /// Fed back as `/load <id>`.
    pub(crate) id: String,
    pub(crate) name: String,
    /// ISO 8601 UTC.
    pub(crate) updated_at: String,
    pub(crate) message_count: usize,
    pub(crate) model: String,
    /// `true` when a live session is currently bound to this conversation
    /// (registry membership). Runtime-only, never persisted.
    pub(crate) active: bool,
    /// `true` when that live session has an agent turn in flight. Always
    /// serialised (never omitted) so clients can rely on a bool; drives the
    /// spinner on background desktop tabs. Runtime-only, never persisted.
    pub(crate) running: bool,
    /// The config profile this conversation is bound to (design T1, §6.1).
    /// `#[serde(default)]`: absent on old servers and on legacy
    /// profile-less conversations ⇒ `None`.
    #[serde(default)]
    pub(crate) profile: Option<String>,
    /// The stable origin of this row: `None` for local, the remote URL for
    /// direct connections, `cloud://<DeviceId>` for cloud connections. Used
    /// by the SPA's shared conversation cache for per-origin snapshotting
    /// and retention. Omitted when `None` (old server / local).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) machine: Option<String>,
}

/// One remote server's reachability in a `conversations_list` (mobile
/// redesign). `id` is the display identifier: origin URL for direct remotes,
/// device slug for cloud peers. `stable_id` (optional) is the cache key
/// that never changes — the origin URL for direct, `cloud://<DeviceId>` for
/// cloud — used by the SPA's shared conversation cache for retention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct MachineStatusWire {
    pub(crate) id: String,
    pub(crate) online: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) stable_id: Option<String>,
}

/// Outbound message envelopes to the client. Owning (no lifetime) so any
/// task can push to the shared writer channel. `Deserialize` is derived so
/// tests can round-trip recorded text frames; the live wire is serialize-only.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum OutboundMessage {
    /// Handshake, sent once before any user input so the client can render
    /// the welcome banner.
    /// `v` is [`PROTOCOL_VERSION`]; a build that predates it omits `v` (reads as 0).
    Ready {
        #[serde(default)]
        v: u32,
    },
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
    ///
    /// `messages_from: Some(n)` marks a delta frame: `state.chat.messages`
    /// holds only the authoritative suffix starting at index `n`. `n` is the
    /// length of the prefix the client must already have; every such frame is
    /// absolute against its socket anchor, so coalescing is safe. Omitted on
    /// full frames (old clients and non-delta sockets see the old shape).
    State {
        state: Arc<AppState>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        messages_from: Option<usize>,
    },
    /// Reply to `request_conversations` and `kill_session`; empty when no
    /// storage is configured. `error` is set iff a kill was refused.
    /// `machines` lists the remote servers (this device is implicit) —
    /// omitted when empty so a no-remote frame stays byte-identical.
    ConversationsList {
        items: Vec<ConversationSummaryWire>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        machines: Vec<MachineStatusWire>,
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
    /// Liveness beat: lets the client tell a dead upstream from a quiet one.
    /// Carries no fields, so a client that predates it ignores it as an unknown type.
    Heartbeat,
    /// An unpinned web attach on a config with no local profile and no home
    /// remote: there is nothing to attach to, so no session is minted. The
    /// socket closes right after. Fieldless, so older clients ignore it.
    NoLocalProfile,
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
/// transports; stdio passes an empty set). `running_ids` is the subset of
/// those whose agent is mid-turn (same injection rule).
pub(crate) fn build_conversations_snapshot(
    sm: &StateManager,
    active_ids: &std::collections::HashSet<uuid::Uuid>,
    running_ids: &std::collections::HashSet<uuid::Uuid>,
) -> Vec<ConversationSummaryWire> {
    summaries_to_wire(
        sm.list_conversations().unwrap_or_default(),
        active_ids,
        running_ids,
    )
}

/// Storage rows → wire rows with the registry's active/running flags. Split
/// out so the HTTP list can run without a session's `StateManager`.
pub(crate) fn summaries_to_wire(
    rows: Vec<crate::storage::ConversationSummary>,
    active_ids: &std::collections::HashSet<uuid::Uuid>,
    running_ids: &std::collections::HashSet<uuid::Uuid>,
) -> Vec<ConversationSummaryWire> {
    rows.into_iter()
        .map(|s| ConversationSummaryWire {
            active: active_ids.contains(&s.id),
            running: running_ids.contains(&s.id),
            id: s.id.to_string(),
            name: s.name,
            updated_at: s.updated_at.to_rfc3339(),
            message_count: s.message_count,
            model: s.model,
            profile: s.profile.clone(),
            machine: None,
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

    // ── win-verbatim-paths: server-composed child paths ─────────────
    //
    // The cwd picker must never compose child paths client-side: it used to
    // send `${browsed}/${name}` — a `/`-joined path that is garbage on
    // Windows (`\\?\C:\Users\Docker/peakbot-test` → os error 123). The
    // server composes every entry's full path and the client sends it back
    // verbatim in `list_dir`.

    #[test]
    fn build_dir_listing_entries_carry_server_composed_full_paths() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("sub_a")).unwrap();
        std::fs::create_dir_all(tmp.path().join("sub_b")).unwrap();
        std::fs::write(tmp.path().join("a_file.txt"), b"x").unwrap();

        let listing = build_dir_listing(tmp.path().to_str().unwrap());
        match listing {
            OutboundMessage::DirListing {
                path,
                entries,
                error,
                ..
            } => {
                assert!(error.is_none(), "listing must not error: {error:?}");
                assert!(!entries.is_empty(), "fixture entries must surface");
                for e in &entries {
                    // The server composes: resolved listing path + entry name,
                    // joined with the platform's separator — never a client-
                    // side `/` concat.
                    let expected = std::path::Path::new(&path)
                        .join(&e.name)
                        .to_string_lossy()
                        .into_owned();
                    assert_eq!(
                        e.full_path, expected,
                        "full_path of `{}` must be the server-composed join of `{path}`",
                        e.name
                    );
                    // The composed path of a *directory* entry must round-trip
                    // through the shared `/cd` resolver — i.e. be usable as the
                    // next `list_dir` payload (this is what breaks today on
                    // Windows). File entries are deliberately skipped: the
                    // `/cd` resolver rejects non-directories by design, and the
                    // picker only ever sends `list_dir` for dir rows.
                    if e.is_dir {
                        assert!(
                            crate::utils::resolve_cd_path(&e.full_path).is_ok(),
                            "resolve_cd_path must accept the server-composed full_path: {expected}"
                        );
                    }
                    // And the field must ride the wire so the SPA can read it.
                    let json = serde_json::to_string(e).unwrap();
                    assert!(
                        json.contains(&format!(r#""full_path":"{expected}""#)),
                        "full_path must serialize onto the wire: {json}"
                    );
                }
            }
            _ => panic!("expected DirListing"),
        }
    }

    #[test]
    fn attach_parses_with_and_without_convo() {
        let with: InboundMessage =
            serde_json::from_str(r#"{"type":"attach","convo":"abc-123"}"#).unwrap();
        assert!(matches!(with, InboundMessage::Attach { convo: Some(c), .. } if c == "abc-123"));

        let without: InboundMessage =
            serde_json::from_str(r#"{"type":"attach","convo":null}"#).unwrap();
        assert!(matches!(
            without,
            InboundMessage::Attach { convo: None, .. }
        ));
    }

    #[test]
    fn kill_session_parses() {
        let m: InboundMessage =
            serde_json::from_str(r#"{"type":"kill_session","convo":"xyz"}"#).unwrap();
        assert!(matches!(m, InboundMessage::KillSession { convo, .. } if convo == "xyz"));
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
            // issue #373: the background-tab spinner flag (additive).
            running: false,
            profile: None,
            machine: None,
        };
        let json = serde_json::to_string(&wire).unwrap();
        assert!(json.contains(r#""active":true"#), "json = {json}");
    }

    // ── running flag (issue #373: spinners on background tabs) ──────────
    //
    // Contract (design §5 backend): `ConversationSummaryWire` gains
    // `running: bool` — `true` iff a live session on this conversation is
    // mid-turn. Plain bool: always serialised (no skip), so an old client
    // ignores it and a new client against an old server sees absence
    // (`running ?? false`). Compile-RED until the field lands.

    #[test]
    fn conversation_summary_carries_running_flag() {
        let wire = ConversationSummaryWire {
            id: "i".into(),
            name: "n".into(),
            updated_at: "t".into(),
            message_count: 0,
            model: "m".into(),
            active: true,
            running: true,
            profile: None,
            machine: None,
        };
        let json = serde_json::to_string(&wire).unwrap();
        assert!(json.contains(r#""running":true"#), "json = {json}");

        let idle = ConversationSummaryWire {
            running: false,
            ..wire
        };
        let json = serde_json::to_string(&idle).unwrap();
        assert!(
            json.contains(r#""running":false"#),
            "an idle summary must serialise running:false, not omit the key: {json}"
        );
    }

    /// `build_conversations_snapshot` marks the convos in `running_ids`
    /// (the web call sites pass `&registry.running_ids()`; stdio passes an
    /// empty set). Compile-RED until the third parameter lands.
    #[test]
    fn conversations_snapshot_marks_running_ids() {
        let tmp = TempDir::new().unwrap();
        let (sm, store) = sm_and_store(tmp.path().join("storage"), tmp.path().join("session"));

        let now = Utc::now();
        // Two saved convos (empty cwd is fine for this test).
        save_conv(&store, "", now);
        save_conv(&store, "", now - chrono::Duration::seconds(60));

        let list = sm.list_conversations().expect("list");
        assert_eq!(list.len(), 2);
        let running_id = list[0].id;
        let idle_id = list[1].id;

        let active = [running_id, idle_id]
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let running = [running_id]
            .into_iter()
            .collect::<std::collections::HashSet<_>>();

        let items = build_conversations_snapshot(&sm, &active, &running);
        assert_eq!(items.len(), 2);
        let by_id = |id: &str| {
            items
                .iter()
                .find(|i| i.id == id)
                .unwrap_or_else(|| panic!("missing {id}"))
        };
        assert!(
            by_id(&running_id.to_string()).running,
            "a mid-turn session's convo must carry running:true"
        );
        assert!(
            !by_id(&idle_id.to_string()).running,
            "an idle session's convo must carry running:false"
        );
    }

    /// The stdio transport passes empty sets (no live-session concept):
    /// every summary is inactive and not running.
    #[test]
    fn conversations_snapshot_is_all_false_with_empty_id_sets() {
        let tmp = TempDir::new().unwrap();
        let (sm, store) = sm_and_store(tmp.path().join("storage"), tmp.path().join("session"));
        save_conv(&store, "", Utc::now());

        let empty: std::collections::HashSet<uuid::Uuid> = std::collections::HashSet::new();
        let items = build_conversations_snapshot(&sm, &empty, &empty);
        assert!(
            items.iter().all(|i| !i.active && !i.running),
            "empty id sets must yield inactive, non-running summaries: {items:?}"
        );
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
            machines: vec![],
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
            machines: vec![],
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

    /// R4: remote machine statuses ride the frame as `machines:[{id,online}]`.
    #[test]
    fn conversations_list_serializes_machines() {
        let m = OutboundMessage::ConversationsList {
            items: vec![],
            machines: vec![
                MachineStatusWire {
                    id: "https://10.0.0.5:7823".to_string(),
                    online: false,
                    stable_id: None,
                },
                MachineStatusWire {
                    id: "https://home-server.local:7823".to_string(),
                    online: true,
                    stable_id: None,
                },
            ],
            error: None,
        };
        let v: serde_json::Value = serde_json::to_value(&m).unwrap();
        assert_eq!(v["type"], "conversations_list");
        assert_eq!(
            v["machines"],
            serde_json::json!([
                {"id": "https://10.0.0.5:7823", "online": false},
                {"id": "https://home-server.local:7823", "online": true},
            ])
        );
    }

    /// R5: no remotes ⇒ the key is omitted and the frame is byte-identical to
    /// today's (the stdio transport and every single-machine install).
    #[test]
    fn conversations_list_omits_machines_when_empty() {
        let m = OutboundMessage::ConversationsList {
            items: vec![],
            machines: Vec::new(),
            error: None,
        };
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, r#"{"type":"conversations_list","items":[]}"#);
        assert!(!json.contains("machines"), "json = {json}");
    }

    /// R6: a frame from an older server (no `machines`) deserialises to `[]`.
    #[test]
    fn conversations_list_without_machines_deserializes_to_empty() {
        let parsed: OutboundMessage =
            serde_json::from_str(r#"{"type":"conversations_list","items":[]}"#)
                .expect("old frame must parse");
        match parsed {
            OutboundMessage::ConversationsList {
                items,
                machines,
                error,
            } => {
                assert!(items.is_empty());
                assert!(machines.is_empty(), "absent machines ⇒ empty: {machines:?}");
                assert!(error.is_none());
            }
            other => panic!("expected ConversationsList, got {other:?}"),
        }
    }

    /// R4 (round-trip): the status shape survives serde in both directions —
    /// the relay deserialises a remote's frame.
    #[test]
    fn conversations_list_machines_round_trip() {
        let json = r#"{"type":"conversations_list","items":[],"machines":[{"id":"https://h:7823","online":true}]}"#;
        let parsed: OutboundMessage = serde_json::from_str(json).expect("parses");
        let OutboundMessage::ConversationsList { machines, .. } = &parsed else {
            panic!("expected ConversationsList");
        };
        assert_eq!(
            machines,
            &vec![MachineStatusWire {
                id: "https://h:7823".to_string(),
                online: true,
                stable_id: None,
            }]
        );
        assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
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
            messages_from: None,
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

    /// T7 (selection-models plan §9.1/23): the extended select frame carries
    /// the orchestrator model alias and the subagent override map.
    #[test]
    fn inbound_select_with_models_parses() {
        let m: InboundMessage = serde_json::from_str(
            r#"{"type":"select","profile":"dev","agent":"team","model":"opus","subagent_models":{"tester":"haiku"}}"#,
        )
        .expect("the extended select frame must parse");
        match &m {
            InboundMessage::Select {
                profile,
                agent,
                model,
                subagent_models,
            } => {
                assert_eq!(profile, "dev");
                assert_eq!(agent, "team");
                assert_eq!(model.as_deref(), Some("opus"), "the model leg parses");
                assert_eq!(
                    subagent_models.get("tester").map(String::as_str),
                    Some("haiku"),
                    "the override map parses"
                );
            }
            other => panic!("expected InboundMessage::Select, got {other:?}"),
        }
    }

    // ── select wire protocol (0.24 cutover) ───────────────────────────────
    //
    // `{"type":"select","profile":…,"agent":…}` — the single selection
    // frame that replaces `select_pipeline` / `select_profile`. Wire
    // shape per spec §8.7; maps to `UiAction::Select(Selection)`.

    /// T10: the inbound `select` frame round-trips — both legs of the
    /// selection survive the parse, and the frame is shaped exactly as
    /// the SPA's `select(profile, agent)` sends it.
    #[test]
    fn inbound_select_roundtrip() {
        let m: InboundMessage =
            serde_json::from_str(r#"{"type":"select","profile":"dev","agent":"team"}"#)
                .expect("a well-formed select frame must parse");
        match &m {
            InboundMessage::Select {
                profile,
                agent,
                model,
                subagent_models,
            } => {
                assert_eq!(profile, "dev", "the profile leg must carry through");
                assert_eq!(agent, "team", "the agent leg must carry through");
                assert!(model.is_none(), "the 2-key frame has no model leg");
                assert!(
                    subagent_models.is_empty(),
                    "the 2-key frame has no overrides"
                );
            }
            other => panic!("expected InboundMessage::Select, got {other:?}"),
        }

        // The frame the SPA emits is the ONLY accepted shape: both keys
        // required (no `name` legacy field, no null-clearing — a selection
        // is always a concrete pair).
        assert!(
            serde_json::from_str::<InboundMessage>(r#"{"type":"select"}"#).is_err(),
            "a select frame without both legs is malformed"
        );
        assert!(
            serde_json::from_str::<InboundMessage>(
                r#"{"type":"select_pipeline","name":"web-team"}"#
            )
            .is_err(),
            "the removed select_pipeline frame must no longer parse"
        );
        assert!(
            serde_json::from_str::<InboundMessage>(
                r#"{"type":"select_profile","name":"research"}"#
            )
            .is_err(),
            "the removed select_profile frame must no longer parse"
        );
    }

    // ── tab colour / remote profiles (design T1, §6.1) ──────────────────
    //
    // `ConversationSummaryWire` gains `#[serde(default)] profile:
    // Option<String>` — the profile the conversation is bound to. Absent on
    // old servers and for legacy profile-less conversations ⇒ None.
    // Compile-RED until the field lands.

    fn summary_with_profile(profile: Option<&str>) -> ConversationSummaryWire {
        ConversationSummaryWire {
            id: "i".into(),
            name: "n".into(),
            updated_at: "t".into(),
            message_count: 0,
            model: "m".into(),
            active: false,
            running: false,
            profile: profile.map(str::to_string),
            machine: None,
        }
    }

    #[test]
    fn conversation_summary_wire_round_trips_profile() {
        let wire = summary_with_profile(Some("home-dev"));
        let json = serde_json::to_string(&wire).expect("summary serialises");
        assert!(
            json.contains(r#""profile":"home-dev""#),
            "the row must carry its profile: {json}"
        );
        let back: ConversationSummaryWire =
            serde_json::from_str(&json).expect("summary deserialises");
        assert_eq!(back.profile.as_deref(), Some("home-dev"));
    }

    /// An old payload (no `profile` key at all) still deserialises — the
    /// field is `#[serde(default)]`, absent ⇒ None (legacy reading).
    #[test]
    fn conversation_summary_wire_without_profile_deserialises_lenient() {
        let old = r#"{"id":"i","name":"n","updated_at":"t","message_count":0,"model":"m","active":false,"running":false}"#;
        let wire: ConversationSummaryWire =
            serde_json::from_str(old).expect("an old summary without `profile` must parse");
        assert_eq!(wire.profile, None, "absent profile reads as None");
    }

    /// The SPA mirror is `string | null`: an explicit JSON null also reads
    /// as None.
    #[test]
    fn conversation_summary_wire_profile_null_reads_as_none() {
        let json = r#"{"id":"i","name":"n","updated_at":"t","message_count":0,"model":"m","active":false,"running":false,"profile":null}"#;
        let wire: ConversationSummaryWire =
            serde_json::from_str(json).expect("a null profile must parse");
        assert_eq!(wire.profile, None);
    }

    /// `request_conversations` rows carry `profile` from
    /// `Conversation.profile` (a conversation without one stays None).
    #[test]
    fn conversations_snapshot_carries_profile() {
        let tmp = TempDir::new().unwrap();
        let (sm, store) = sm_and_store(tmp.path().join("storage"), tmp.path().join("session"));

        let now = Utc::now();
        let mut profiled = Conversation::new(
            "profiled".to_string(),
            "test-prov".to_string(),
            "test-model".to_string(),
            String::new(),
        );
        profiled.updated_at = now;
        profiled.profile = Some("home-dev".to_string());
        store.save(&profiled).unwrap();
        save_conv(&store, "", now - chrono::Duration::seconds(60));

        let empty: std::collections::HashSet<uuid::Uuid> = std::collections::HashSet::new();
        let items = build_conversations_snapshot(&sm, &empty, &empty);
        assert_eq!(items.len(), 2);
        let by_id = |id: &str| {
            items
                .iter()
                .find(|i| i.id == id)
                .unwrap_or_else(|| panic!("missing {id}"))
        };
        assert_eq!(
            by_id(&profiled.id.to_string()).profile.as_deref(),
            Some("home-dev"),
            "a bound conversation must carry its profile on the row"
        );
        assert_eq!(
            items
                .iter()
                .find(|i| i.id != profiled.id.to_string())
                .unwrap()
                .profile,
            None,
            "a profile-less conversation must carry None, not a guess"
        );
    }

    // ── remote profiles T4: wire deltas (design §6.1, §9 T4) ────────────
    //
    // All additive and serde-lenient: old payloads (no `profile` / `agent` /
    // `relayed` / `v`) keep parsing with their legacy reading.
    // `PROTOCOL_VERSION` is the single source of truth — one const here,
    // exact match on the relay (locked decision Q-A, 2026-10-31).

    /// Locked decision: `PROTOCOL_VERSION` is defined once in
    // `src/ui/wire.rs` and is 1 (bumped on ANY incompatible wire change).
    #[test]
    fn protocol_version_is_one() {
        assert_eq!(PROTOCOL_VERSION, 1, "the locked protocol version is 1");
    }

    /// T1 (wire): the server→client liveness beat is exactly
    /// `{"type":"heartbeat"}` — no fields, so an old client's unknown-type
    /// branch ignores it and the SPA's watchdog arms on `type` alone.
    #[test]
    fn heartbeat_serialises_to_the_locked_shape() {
        let json =
            serde_json::to_string(&OutboundMessage::Heartbeat).expect("heartbeat serialises");
        assert_eq!(
            json, r#"{"type":"heartbeat"}"#,
            "the heartbeat frame must be exactly {{\"type\":\"heartbeat\"}} with no other fields"
        );
    }

    /// T1 (wire): the beat round-trips — the relay's parse gate must accept
    /// it, so a remote's beat can be forwarded to the local client through
    /// the relay's catch-all.
    #[test]
    fn heartbeat_deserialises_to_the_unit_variant() {
        let m: OutboundMessage =
            serde_json::from_str(r#"{"type":"heartbeat"}"#).expect("the locked frame must parse");
        assert!(
            matches!(m, OutboundMessage::Heartbeat),
            "the frame must round-trip to the Heartbeat variant, got {m:?}"
        );
    }

    /// Additive-change pin: the beat is a new variant, not a new protocol —
    /// the relay's exact-match gate keeps accepting same-version remotes.
    #[test]
    fn heartbeat_keeps_protocol_version_one() {
        assert_eq!(
            PROTOCOL_VERSION, 1,
            "the heartbeat must not bump the protocol version"
        );
    }

    /// Additive-change pin: tool_args projection is a VALUE-level change
    /// (we omit redundant fields), not a schema change (no new types, no
    /// new field names). The relay's version gate must not see this.
    #[test]
    fn tool_args_projection_keeps_protocol_version_one() {
        assert_eq!(
            PROTOCOL_VERSION, 1,
            "tool_args projection is value-level, not a schema change — version must stay at 1"
        );
    }

    #[test]
    fn attach_parses_with_profile_agent_and_relayed() {
        let m: InboundMessage = serde_json::from_str(
            r#"{"type":"attach","convo":null,"profile":"home-dev","agent":"team","relayed":true}"#,
        )
        .expect("the extended attach frame must parse");
        match m {
            InboundMessage::Attach {
                convo,
                profile,
                agent,
                relayed,
                ..
            } => {
                assert!(convo.is_none(), "a fresh attach carries convo:null");
                assert_eq!(profile.as_deref(), Some("home-dev"));
                assert_eq!(agent.as_deref(), Some("team"));
                assert!(relayed, "the relayed marker must parse");
            }
            other => panic!("expected Attach, got {other:?}"),
        }
    }

    /// Old SPA payloads (pre-remote-profiles) omit all three new fields —
    /// they must still parse with the legacy reading: no profile, no agent,
    /// not relayed.
    #[test]
    fn attach_old_payload_without_new_fields_still_parses() {
        let m: InboundMessage = serde_json::from_str(r#"{"type":"attach","convo":null}"#)
            .expect("an old attach frame must keep parsing");
        match m {
            InboundMessage::Attach {
                profile,
                agent,
                relayed,
                ..
            } => {
                assert!(profile.is_none(), "absent profile reads as None");
                assert!(agent.is_none(), "absent agent reads as None");
                assert!(
                    !relayed,
                    "absent relayed reads as false (a browser is never a relay)"
                );
            }
            other => panic!("expected Attach, got {other:?}"),
        }
    }

    /// `relayed` is a `#[serde(default)]` bool: explicit `false` must be
    /// indistinguishable from absent — a normal attach.
    #[test]
    fn attach_relayed_false_explicit_is_a_normal_attach() {
        let m: InboundMessage =
            serde_json::from_str(r#"{"type":"attach","convo":null,"relayed":false}"#)
                .expect("an explicit relayed:false must parse");
        assert!(
            matches!(m, InboundMessage::Attach { relayed: false, .. }),
            "explicit false must read as a normal attach"
        );
    }

    #[test]
    fn kill_session_parses_with_optional_profile() {
        let m: InboundMessage =
            serde_json::from_str(r#"{"type":"kill_session","convo":"abc","profile":"home-dev"}"#)
                .expect("the extended kill_session frame must parse");
        match m {
            InboundMessage::KillSession { convo, profile } => {
                assert_eq!(convo, "abc");
                assert_eq!(profile.as_deref(), Some("home-dev"));
            }
            other => panic!("expected KillSession, got {other:?}"),
        }

        // Old frame: no profile key (the local path ignores it anyway).
        let old: InboundMessage = serde_json::from_str(r#"{"type":"kill_session","convo":"abc"}"#)
            .expect("an old kill_session frame must keep parsing");
        assert!(
            matches!(old, InboundMessage::KillSession { profile: None, .. }),
            "absent profile reads as None"
        );
    }

    /// JSON shape lock: `{"type":"ready","v":1}` — the relay's exact-match
    /// check (T13) compares `v` against `PROTOCOL_VERSION`.
    #[test]
    fn ready_serializes_with_protocol_version() {
        let json = serde_json::to_string(&OutboundMessage::Ready {
            v: PROTOCOL_VERSION,
        })
        .expect("ready serialises");
        assert_eq!(
            json, r#"{"type":"ready","v":1}"#,
            "the ready frame must be exactly {{\"type\":\"ready\",\"v\":1}}"
        );
    }

    /// A `ready` from an older build (no `v`) deserialises to `v: 0` —
    /// the exact-match rule then refuses it as E-VERSION. (The relay-side
    /// refusal is T6; this locks the deserialise half here.)
    #[test]
    fn ready_without_v_deserialises_to_zero() {
        let m: OutboundMessage =
            serde_json::from_str(r#"{"type":"ready"}"#).expect("an old ready frame must parse");
        match m {
            OutboundMessage::Ready { v } => {
                assert_eq!(v, 0, "a missing v must read as 0, never 1")
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn ready_round_trips_the_version() {
        let m: OutboundMessage =
            serde_json::from_str(r#"{"type":"ready","v":7}"#).expect("ready with v parses");
        match m {
            OutboundMessage::Ready { v } => assert_eq!(v, 7, "v must round-trip"),
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    // ─── delta sync: cursor parsing, suffix frames, rebase (Stage 3) ────────
    //
    // Written against the interface the revised cache/delta design specifies,
    // not an existing implementation. Compile-RED until `src/ui/delta.rs`
    // lands and `Attach { since }`, `SyncAnchor`, and `State { messages_from }`
    // are added to the wire variants.

    use crate::state::state_manager::StateManager;
    use crate::ui::delta::{
        AttachCursor, REBASE_EVERY, should_rebase, slice_from_conversation,
        validate_cursor_for_conversation,
    };
    // `Conversation`, `Arc`, and `AppState` are already in scope via `super::*`
    // / the module-level `use crate::conversation::Conversation;` above.

    fn test_conv_with_messages(count: usize) -> Conversation {
        let mut conv = Conversation::new(
            "test".to_string(),
            "openrouter".to_string(),
            "anthropic/claude-3.7-sonnet".to_string(),
            "/tmp".to_string(),
        );
        for i in 0..count {
            conv.add_user_message(format!("msg {i}"));
        }
        conv
    }

    fn raw_ts_for_conversation(conv: &Conversation, index: usize) -> String {
        let messages = crate::ui::app_state::chat_messages_from_conversation(conv);
        let json = serde_json::to_value(&messages[index]).expect("serde chat message");
        json["timestamp"]
            .as_str()
            .expect("timestamp is a string in JSON")
            .to_string()
    }

    #[test]
    fn attach_parses_valid_since_cursor() {
        let json = r#"{"type":"attach","convo":"c","since":{"n":2,"timestamp":"2026-01-01T00:00:00Z","epoch":7}}"#;
        let m: InboundMessage = serde_json::from_str(json).expect("parses valid cursor");
        match m {
            InboundMessage::Attach { since, .. } => {
                let c = since.expect("since present for valid cursor");
                assert_eq!(c.n, 2);
                assert_eq!(c.timestamp, "2026-01-01T00:00:00Z");
                assert_eq!(c.epoch, 7);
            }
            other => panic!("expected Attach, got {other:?}"),
        }
    }

    #[test]
    fn attach_without_since_field_is_none() {
        let m: InboundMessage =
            serde_json::from_str(r#"{"type":"attach","convo":"c"}"#).expect("parses");
        match m {
            InboundMessage::Attach { since, .. } => assert!(since.is_none()),
            other => panic!("expected Attach, got {other:?}"),
        }
    }

    #[test]
    fn attach_with_null_since_is_none() {
        let m: InboundMessage =
            serde_json::from_str(r#"{"type":"attach","convo":"c","since":null}"#).expect("parses");
        match m {
            InboundMessage::Attach { since, .. } => assert!(since.is_none()),
            other => panic!("expected Attach, got {other:?}"),
        }
    }

    #[test]
    fn malformed_since_does_not_fail_the_attach_frame() {
        let cases = [
            r#"{"type":"attach","convo":"c","since":{"n":0}}"#,
            r#"{"type":"attach","convo":"c","since":{"n":"nope","timestamp":"x","epoch":1}}"#,
            r#"{"type":"attach","convo":"c","since":{"n":1.5,"timestamp":"x","epoch":1}}"#,
            r#"{"type":"attach","convo":"c","since":{"timestamp":"x","epoch":1}}"#, // bare timestamp
            r#"{"type":"attach","convo":"c","since":"string"}"#,
            r#"{"type":"attach","convo":"c","since":{"n":1,"timestamp":"x","epoch":"oops"}}"#,
            r#"{"type":"attach","convo":"c","since":{"n":-1,"timestamp":"x","epoch":1}}"#,
            // Beyond 2^53-1 a JS client cannot round-trip the epoch exactly.
            r#"{"type":"attach","convo":"c","since":{"n":1,"timestamp":"x","epoch":9007199254740992}}"#,
        ];
        for json in cases {
            let m: InboundMessage = serde_json::from_str(json).unwrap_or_else(|e| {
                panic!("malformed cursor must not fail the attach frame: {e} ({json})")
            });
            match m {
                InboundMessage::Attach { since, .. } => {
                    assert!(
                        since.is_none(),
                        "malformed cursor {json} should yield since=None"
                    );
                }
                other => panic!("expected Attach, got {other:?} ({json})"),
            }
        }
    }

    #[test]
    fn sync_anchor_parses_with_valid_cursor() {
        let m: InboundMessage = serde_json::from_str(
            r#"{"type":"sync_anchor","since":{"n":12,"timestamp":"t","epoch":7}}"#,
        )
        .expect("parses");
        match m {
            InboundMessage::SyncAnchor { since: Some(c) } => {
                assert_eq!(c.n, 12);
                assert_eq!(c.epoch, 7);
            }
            other => panic!("expected SyncAnchor, got {other:?}"),
        }
    }

    #[test]
    fn rebase_alias_parses_as_sync_anchor() {
        let m: InboundMessage =
            serde_json::from_str(r#"{"type":"rebase","since":{"n":12,"timestamp":"t","epoch":7}}"#)
                .expect("parses");
        match m {
            InboundMessage::SyncAnchor { since: Some(c) } => {
                assert_eq!(c.n, 12);
            }
            other => panic!("expected SyncAnchor for rebase alias, got {other:?}"),
        }
    }

    #[test]
    fn malformed_sync_anchor_is_lenient_and_does_not_fail_parse() {
        let cases = [
            r#"{"type":"sync_anchor"}"#,
            r#"{"type":"rebase"}"#,
            r#"{"type":"sync_anchor","since":{"n":0}}"#,
            r#"{"type":"rebase","since":"not-an-object"}"#,
        ];
        for json in cases {
            let m: InboundMessage = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("malformed rebase must not fail parse: {e} ({json})"));
            match m {
                InboundMessage::SyncAnchor { since: None } => {} // lenient
                other => panic!("expected lenient SyncAnchor, got {other:?} ({json})"),
            }
        }
    }

    #[test]
    fn suffix_state_frame_carries_messages_from() {
        let state = Arc::new(AppState::new());
        let m = OutboundMessage::State {
            state,
            messages_from: Some(3),
        };
        let v = serde_json::to_value(&m).expect("serializes");
        assert_eq!(v["messages_from"], 3);
        assert_eq!(v["type"], "state");
    }

    #[test]
    fn full_state_frame_omits_messages_from() {
        let state = Arc::new(AppState::new());
        let m = OutboundMessage::State {
            state,
            messages_from: None,
        };
        let json = serde_json::to_string(&m).expect("serializes");
        assert!(
            !json.contains("messages_from"),
            "full frame must not carry messages_from: {json}"
        );
    }

    #[test]
    fn state_frame_without_messages_from_field_deserializes_as_none() {
        let m: OutboundMessage =
            serde_json::from_str(r#"{"type":"state","state":{}}"#).expect("parses");
        match m {
            OutboundMessage::State { messages_from, .. } => {
                assert!(
                    messages_from.is_none(),
                    "missing field must deserialize as None"
                );
            }
            other => panic!("expected State, got {other:?}"),
        }
    }

    #[test]
    fn delta_stage_does_not_bump_protocol_version() {
        assert_eq!(
            PROTOCOL_VERSION, 1,
            "Stage 3 must not bump PROTOCOL_VERSION"
        );
    }

    // ─── cursor validation against authoritative messages ────────────────────

    #[test]
    fn validate_accepts_exact_prefix_end() {
        let conv = test_conv_with_messages(3);
        let raw = raw_ts_for_conversation(&conv, 2);
        let cursor = AttachCursor {
            n: 3,
            timestamp: raw,
            epoch: conv.history_epoch,
        };
        assert!(
            validate_cursor_for_conversation(&cursor, &conv).is_some(),
            "valid cursor must be accepted"
        );
    }

    #[test]
    fn validate_rejects_n_zero() {
        let conv = test_conv_with_messages(3);
        let raw = raw_ts_for_conversation(&conv, 0);
        let cursor = AttachCursor {
            n: 0,
            timestamp: raw,
            epoch: conv.history_epoch,
        };
        assert!(validate_cursor_for_conversation(&cursor, &conv).is_none());
    }

    #[test]
    fn validate_rejects_n_greater_than_len() {
        let conv = test_conv_with_messages(3);
        let raw = raw_ts_for_conversation(&conv, 2);
        let cursor = AttachCursor {
            n: 4,
            timestamp: raw,
            epoch: conv.history_epoch,
        };
        assert!(validate_cursor_for_conversation(&cursor, &conv).is_none());
    }

    #[test]
    fn validate_rejects_epoch_mismatch() {
        let conv = test_conv_with_messages(3);
        let raw = raw_ts_for_conversation(&conv, 2);
        let cursor = AttachCursor {
            n: 3,
            timestamp: raw,
            epoch: conv.history_epoch + 1,
        };
        assert!(validate_cursor_for_conversation(&cursor, &conv).is_none());
    }

    #[test]
    fn validate_rejects_timestamp_mismatch_byte_exact() {
        let conv = test_conv_with_messages(3);
        let raw = raw_ts_for_conversation(&conv, 2);
        let wrong = format!("{raw}0"); // alter one character
        let cursor = AttachCursor {
            n: 3,
            timestamp: wrong,
            epoch: conv.history_epoch,
        };
        assert!(validate_cursor_for_conversation(&cursor, &conv).is_none());
    }

    #[test]
    fn same_timestamp_two_messages_disambiguated_by_n() {
        let conv = test_conv_with_messages(2);
        // Force both messages to share the same raw timestamp via JSON.
        let ts = raw_ts_for_conversation(&conv, 0);
        let json = serde_json::to_value(&conv).unwrap();
        let mut obj = json.as_object().expect("obj").clone();
        obj["messages"][1]["timestamp"] = serde_json::Value::String(ts.clone());
        let same_ts_json = serde_json::to_string(&obj).unwrap();
        let same_conv: Conversation = serde_json::from_str(&same_ts_json).unwrap();

        let c1 = AttachCursor {
            n: 1,
            timestamp: ts.clone(),
            epoch: same_conv.history_epoch,
        };
        let c2 = AttachCursor {
            n: 2,
            timestamp: ts.clone(),
            epoch: same_conv.history_epoch,
        };
        assert!(
            validate_cursor_for_conversation(&c1, &same_conv).is_some(),
            "n=1 must be valid"
        );
        assert!(
            validate_cursor_for_conversation(&c2, &same_conv).is_some(),
            "n=2 must be valid"
        );

        let c0 = AttachCursor { n: 0, ..c1.clone() };
        let c3 = AttachCursor { n: 3, ..c1 };
        assert!(
            validate_cursor_for_conversation(&c0, &same_conv).is_none(),
            "n=0 invalid"
        );
        assert!(
            validate_cursor_for_conversation(&c3, &same_conv).is_none(),
            "n=3 invalid"
        );
    }

    #[test]
    fn slice_from_returns_authoritative_tail() {
        let conv = test_conv_with_messages(4);
        let suffix = slice_from_conversation(&conv, 2);
        assert_eq!(suffix.len(), 2);
        assert_eq!(suffix[0].content, "msg 2");
        assert_eq!(suffix[1].content, "msg 3");
    }

    #[test]
    fn slice_at_end_yields_empty_tail() {
        let conv = test_conv_with_messages(3);
        let suffix = slice_from_conversation(&conv, 3);
        assert!(suffix.is_empty());
    }

    #[test]
    fn slice_at_beyond_len_yields_empty() {
        let conv = test_conv_with_messages(3);
        let suffix = slice_from_conversation(&conv, 5);
        assert!(suffix.is_empty());
    }

    // ─── rebase cadence ──────────────────────────────────────────────────────

    #[test]
    fn rebase_every_constant_is_10() {
        assert_eq!(REBASE_EVERY, 10);
    }

    #[test]
    fn should_rebase_fires_at_multiples_of_every() {
        assert!(!should_rebase(9, 10));
        assert!(should_rebase(10, 10));
        assert!(!should_rebase(11, 10));
        assert!(should_rebase(20, 10));
        assert!(!should_rebase(0, 10));
    }

    #[test]
    fn should_rebase_is_injectable_for_tests() {
        assert!(!should_rebase(2, 3));
        assert!(should_rebase(3, 3));
        assert!(!should_rebase(4, 3));
    }

    // ─── durable lineage (history_epoch) ─────────────────────────────────────

    #[test]
    fn new_conversation_gets_fresh_nonzero_epoch() {
        let c = Conversation::new("a".into(), "p".into(), "m".into(), "/tmp".into());
        assert_ne!(
            c.history_epoch, 0,
            "fresh conversation must have a non-zero epoch"
        );
    }

    #[test]
    fn new_conversation_epoch_survives_json_number_precision() {
        let c = Conversation::new("a".into(), "p".into(), "m".into(), "/tmp".into());
        assert!(
            c.history_epoch < (1u64 << 53),
            "the epoch crosses the wire as a JS number; a full u64 would be rounded"
        );
    }

    #[test]
    fn two_new_conversations_have_different_epochs() {
        let a = Conversation::new("a".into(), "p".into(), "m".into(), "/tmp".into());
        let b = Conversation::new("b".into(), "p".into(), "m".into(), "/tmp".into());
        assert_ne!(
            a.history_epoch, b.history_epoch,
            "distinct conversations must have distinct epochs"
        );
    }

    #[test]
    fn legacy_conversation_without_history_epoch_loads_as_zero() {
        let c = Conversation::new("x".into(), "p".into(), "m".into(), "/tmp".into());
        // Serialize, remove the epoch field, and deserialize
        let mut v = serde_json::to_value(&c).expect("serialize");
        v.as_object_mut().expect("obj").remove("history_epoch");
        let back: Conversation = serde_json::from_value(v).expect("deserializes legacy JSON");
        assert_eq!(
            back.history_epoch, 0,
            "legacy conversation without the field must load as epoch 0"
        );
    }

    #[test]
    fn save_and_load_preserves_history_epoch() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::storage::FileStorage::new(tmp.path().join("storage")).unwrap();
        let mut c = Conversation::new("e".into(), "p".into(), "m".into(), "/tmp".into());
        c.add_user_message("hi".into());
        let epoch = c.history_epoch;
        store.save(&c).expect("save");
        let loaded = store.load(c.id).expect("load");
        assert_eq!(
            loaded.history_epoch, epoch,
            "save/load must preserve history_epoch"
        );
    }

    #[test]
    fn cold_restart_permits_delta_attach() {
        // Save a conversation, drop the session, "cold restart" (new FileStorage),
        // load the same conversation, and prove a cursor built pre-restart is
        // still valid against the loaded copy.
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::storage::FileStorage::new(tmp.path().join("storage")).unwrap();
        let mut c = Conversation::new("cold".into(), "p".into(), "m".into(), "/tmp".into());
        c.add_user_message("first".into());
        c.add_user_message("second".into());
        c.add_user_message("third".into());
        let pre_restart_epoch = c.history_epoch;
        let pre_restart_raw = raw_ts_for_conversation(&c, 2);
        let convo_id = c.id;
        store.save(&c).expect("save");

        // "Cold restart" — drop the original store and conversation
        drop(c);
        drop(store);

        let store2 = crate::storage::FileStorage::new(tmp.path().join("storage")).unwrap();
        let loaded = store2.load(convo_id).expect("load after restart");
        assert_eq!(
            loaded.history_epoch, pre_restart_epoch,
            "epoch must survive cold restart"
        );

        let post_restart_raw = raw_ts_for_conversation(&loaded, 2);
        assert_eq!(
            post_restart_raw, pre_restart_raw,
            "raw timestamp must survive cold restart"
        );

        let cursor = AttachCursor {
            n: 3,
            timestamp: pre_restart_raw,
            epoch: pre_restart_epoch,
        };
        assert!(
            validate_cursor_for_conversation(&cursor, &loaded).is_some(),
            "a cursor built before restart must validate against the loaded conversation"
        );
    }

    #[test]
    fn app_state_exposes_current_epoch() {
        let sm = StateManager::new_arc();
        sm.create_conversation("title".into(), "p".into(), "m".into(), "/tmp".into());
        let state = sm.get_state();
        assert_ne!(
            state.history_epoch, 0,
            "AppState must expose the current conversation's epoch"
        );
    }

    #[test]
    fn append_does_not_change_history_epoch() {
        let sm = StateManager::new_arc();
        sm.create_conversation("title".into(), "p".into(), "m".into(), "/tmp".into());
        let epoch_before = sm.get_current_conversation().unwrap().history_epoch;
        sm.add_user_message("hello".into());
        sm.add_assistant_message("hi there".into());
        let epoch_after = sm.get_current_conversation().unwrap().history_epoch;
        assert_eq!(
            epoch_before, epoch_after,
            "normal append must not change history_epoch"
        );
    }

    #[test]
    fn set_running_does_not_change_history_epoch() {
        let sm = StateManager::new_arc();
        sm.create_conversation("title".into(), "p".into(), "m".into(), "/tmp".into());
        let epoch_before = sm.get_current_conversation().unwrap().history_epoch;
        sm.set_running(true);
        sm.set_running(false);
        let epoch_after = sm.get_current_conversation().unwrap().history_epoch;
        assert_eq!(
            epoch_before, epoch_after,
            "set_running must not change history_epoch"
        );
    }

    #[test]
    fn set_status_does_not_change_history_epoch() {
        let sm = StateManager::new_arc();
        sm.create_conversation("title".into(), "p".into(), "m".into(), "/tmp".into());
        let epoch_before = sm.get_current_conversation().unwrap().history_epoch;
        sm.set_status(Some("working".to_string()));
        sm.set_status(None);
        let epoch_after = sm.get_current_conversation().unwrap().history_epoch;
        assert_eq!(
            epoch_before, epoch_after,
            "set_status must not change history_epoch"
        );
    }

    #[test]
    fn rename_does_not_change_history_epoch() {
        let mut c = Conversation::new("title".into(), "p".into(), "m".into(), "/tmp".into());
        let epoch_before = c.history_epoch;
        c.rename("new title".into());
        assert_eq!(
            c.history_epoch, epoch_before,
            "rename must not change history_epoch"
        );
    }

    #[test]
    fn set_title_does_not_change_history_epoch() {
        let mut c = Conversation::new("title".into(), "p".into(), "m".into(), "/tmp".into());
        let epoch_before = c.history_epoch;
        c.set_provisional_title("prov".into());
        c.set_definitive_title("final".into());
        assert_eq!(
            c.history_epoch, epoch_before,
            "title updates must not change history_epoch"
        );
    }

    #[test]
    fn clear_chat_changes_history_epoch() {
        let sm = StateManager::new_arc();
        sm.create_conversation("title".into(), "p".into(), "m".into(), "/tmp".into());
        sm.add_user_message("a".into());
        let epoch_before = sm.get_current_conversation().unwrap().history_epoch;
        sm.clear_chat();
        let epoch_after = sm.get_current_conversation().unwrap().history_epoch;
        assert_ne!(
            epoch_before, epoch_after,
            "clear_chat must change history_epoch"
        );
    }

    #[test]
    fn load_conversation_updates_app_state_epoch() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::storage::FileStorage::new(tmp.path().join("storage")).unwrap();
        let c = Conversation::new("load-test".into(), "p".into(), "m".into(), "/tmp".into());
        store.save(&c).expect("save");
        let storage: std::sync::Arc<dyn crate::storage::ConversationStorage> =
            std::sync::Arc::new(store);
        let sm = StateManager::new_arc_with_storage(storage);
        sm.create_conversation("other".into(), "p".into(), "m".into(), "/tmp".into());
        let other_epoch = sm.get_current_conversation().unwrap().history_epoch;
        sm.load_conversation(c.id).expect("load");
        let state = sm.get_state();
        assert_eq!(
            state.history_epoch, c.history_epoch,
            "AppState must expose loaded conversation's epoch"
        );
        assert_ne!(
            state.history_epoch, other_epoch,
            "epoch must change after loading a different conversation"
        );
    }
}
