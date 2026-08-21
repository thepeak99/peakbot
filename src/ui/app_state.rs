//! Application State Definitions
//!
//! This module defines the centralized state that all UIs observe.
//! It mirrors the patterns from ui-example.rs while being compatible
//! with existing PeakBot types (TodoList, SessionStats, etc.).

use crate::TodoStatus;
use crate::image_cache::ImageRef;
use crate::pipeline::PipelineInfo;
use crate::tools::todo::TodoItem as CoreTodoItem;
use crate::tools::view_image;
use crate::ui::ui_trait::TodoItemAction;
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use serde_json;
use std::fmt;

/// Centralized state that all UIs observe
///
/// This is the single source of truth for all UI-renderable state.
/// The StateManager keeps this in sync with the core PeakBot state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppState {
    /// Chat messages
    pub chat: ChatState,

    /// TODO items
    pub todo: TodoState,

    /// Input field state
    pub input: InputState,

    /// Session statistics (tokens, cost, etc.)
    pub stats: SessionState,

    /// Context usage
    pub context: ContextState,

    /// Current conversation info
    pub conversation: Option<ConversationState>,

    /// UI preferences
    pub preferences: UiPreferences,

    /// Whether the agent is currently processing
    #[serde(default)]
    pub is_running: bool,

    /// Whether the agent is currently loading (alias for is_running, kept for compatibility)
    #[serde(default)]
    #[doc(hidden)]
    pub is_loading: bool,

    /// Welcome banner — populated once at startup, never changes
    pub welcome: Option<WelcomeState>,

    /// Whether this state update is the final broadcast after a prompt completed
    #[serde(default)]
    pub is_final: bool,

    /// Agent status message (e.g., "Compacting...", "Stopped")
    pub status_message: Option<String>,

    /// When the current run started. `Some` iff `is_running`.
    ///
    /// Local-only (`Instant` isn't `Serialize`). The TUI reads this to render
    /// a "working" spinner and elapsed timer in the input block title. If a
    /// cross-process UI ever needs this, migrate to `SystemTime` or epoch
    /// millis — do NOT pre-build that bridge.
    #[serde(skip)]
    pub run_started_at: Option<std::time::Instant>,

    /// Signal that the view should quit on its next tick.
    ///
    /// Written by the `/exit` command handler (which runs in the agent
    /// loop, far from the View), read by the View's run loop. Once set,
    /// it never goes back to false — the process is on its way out.
    ///
    /// This intentionally bypasses the Ctrl+C confirmation dialog; the
    /// user asked for an exit, they get an exit.
    #[serde(default)]
    pub exit_requested: bool,

    /// How many user-typed messages are currently waiting in the
    /// agent_loop queue (typed during a busy turn, not yet dequeued).
    ///
    /// Status-bar hint only — the View renders `⏳ N queued` when this
    /// is non-zero so the user knows their typed lines landed in the
    /// queue even though the transcript has not yet shown them.
    /// Incremented by the event loop on send, decremented by the agent
    /// loop on dequeue, zeroed by the event loop on /stop drain. See
    /// `make-flow-great-again.md`.
    #[serde(default)]
    pub pending_input_count: usize,

    /// Live snapshot of running background processes for the TUI status
    /// counter `🛰 N bg`. Populated by `StateManager::update_bg_state`
    /// after each registry mutation. See `bash-background.md`.
    #[serde(default)]
    pub bg: BgState,

    /// Live state of the foreground `bash` tool panel.
    ///
    /// Drives the bottom-strip panel that surfaces the currently-running
    /// (or last-run) `bash` invocation. UI scaffolding only in slice 2
    /// of `make-term-great-again.md`; the producer side (foreground
    /// `bash` PTY wiring) lands in slice 3, so in production this slot
    /// stays at [`BashPanelState::Idle`] until then. Snapshot-shape, not
    /// a handle — mirrors `BgState`'s pattern so AppState stays
    /// `Clone + Serialize + Deserialize`.
    #[serde(default)]
    pub bash_panel: BashPanelState,

    /// View-layer panel-visibility override for the foreground `bash`
    /// panel. **Three-state enum** ([`BashPanelVisibility`]) because
    /// the panel has three meaningful visibility regimes — `Auto`
    /// (follow producer state), `OpenedByUser` (force visible even on
    /// Idle), `ClosedByUser` (force hidden even on Running/Finished).
    /// A two-bool encoding would admit an illegal "both true" state;
    /// the enum makes it unrepresentable.
    ///
    /// Toggled by `Ctrl+B` (see [`StateManager::toggle_bash_panel_visibility`]).
    /// Reset to `Auto` by:
    /// - `StateManager::start_bash_panel` (a new bash starting is implicit
    ///   consent to show its output — the user's prior dismissal was
    ///   about the *previous* output, not future ones);
    /// - `StateManager::reset_bash_panel` (called on `/new`, `/load`,
    ///   `/model` rebuilds — fresh conversations start clean).
    ///
    /// **NOT reset by new user messages.** That was the v1 (PR #67)
    /// contract; it was inverted because typing a follow-up question
    /// shouldn't re-open a panel the user just closed. See the
    /// "Producer→view orthogonality" Zen pass.
    #[serde(default)]
    pub bash_panel_visibility: BashPanelVisibility,

    /// The pipelines (named teams) this install declares, in config order.
    /// Projected once at session build from the boot-built `PipelineSet` —
    /// the Agents panel renders the roster from this, and "no pipelines
    /// configured" is `is_empty()`, never a separate flag.
    #[serde(default)]
    pub pipelines: Vec<PipelineInfo>,

    /// The pipeline this conversation is bound to, or `None` for single-agent
    /// mode. Mirror of `Conversation.pipeline` (the persisted truth); mutable
    /// only before the first turn. Everything downstream — delegate roster,
    /// prompt recipe, orchestrator model, `/model` lock — derives from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_pipeline: Option<String>,
}

impl AppState {
    /// Create a new empty AppState
    pub fn new() -> Self {
        Self::default()
    }
}

/// Chat message state
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatState {
    /// Chat messages
    pub messages: Vec<ChatMessage>,

    /// Whether to auto-scroll to latest message
    pub auto_scroll: bool,

    /// Manual scroll offset (when auto_scroll is disabled)
    pub scroll_offset: usize,
}

impl ChatState {
    /// Create a new empty ChatState
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a message to the chat
    pub fn add_message(&mut self, message: ChatMessage) {
        self.messages.push(message);
        // Auto-scroll when new messages are added
        self.auto_scroll = true;
    }

    /// Clear all messages
    pub fn clear(&mut self) {
        self.messages.clear();
        self.auto_scroll = true;
    }

    /// Get the number of messages
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }
}

/// A single chat message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Role of the message sender
    pub role: MessageRole,

    /// Display content (formatted for UI rendering)
    pub content: String,

    /// Image attachments for user messages.
    ///
    /// Empty for all non-user messages and for text-only user messages.
    /// Skipped from JSON when empty → zero size overhead for existing
    /// conversations. Converted to `rig_core::UserContent::Image` at the wire
    /// boundary in `StateManager::get_agent_history` /
    /// `build_current_turn_message`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::vision::ImageAttachment>,

    /// Images to DISPLAY with this row, by reference. Never model input —
    /// that is `attachments`. Empty for every row but a `view_image` result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<crate::image_cache::ImageRef>,

    /// Timestamp when message was created
    pub timestamp: DateTime<Local>,

    // ── Structured tool data (lossless) ──────────────────────────────
    // These fields preserve the original data from rig so that
    // ChatMessage → Conversation::Message and ChatMessage → rig_core::Message
    // roundtrips are lossless.
    /// Tool name (for ToolCall and ToolResult roles)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,

    /// Raw tool arguments JSON string (for ToolCall and ToolResult roles)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_args: Option<String>,

    /// Tool execution result (for ToolResult role)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<String>,

    /// Tool call ID for correlating calls with results
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,

    /// Whether this message has been compacted (summarized).
    /// Compacted messages are kept for UI display but skipped when
    /// building the rig message array sent to the LLM.
    #[serde(default)]
    pub compacted: bool,

    /// Where this message originated.
    ///
    /// Defaults to [`MessageSource::Human`] for backward compatibility;
    /// pre-v6 conversation files (no `source` field) parse cleanly via
    /// `#[serde(default)]`. Synthetic turns produced by the `bash_bg`
    /// drain seam carry [`MessageSource::Background`] so the renderer
    /// can style them distinctly (🛰 capped, 💬 unlimited) and so the
    /// transcript records which background processes contributed.
    /// See `bash-background.md` open Q6.
    #[serde(default, skip_serializing_if = "MessageSource::is_human")]
    pub source: MessageSource,

    /// Anthropic thinking blocks captured from the response that
    /// produced this message. Replayed verbatim (signature included)
    /// at the wire boundary in `StateManager::get_agent_history`;
    /// empty for every non-Anthropic message and for the on-disk JSON
    /// of pre-existing conversations.
    ///
    /// Wire (web snapshot) shape: the browser only ever sees the text
    /// portion. Signatures and `Redacted` ciphertext are stripped via
    /// the custom serializer below — they are useless for display and
    /// signature data has no business in a DOM or a devtools tab.
    #[serde(
        default,
        serialize_with = "serialize_thinking_for_wire",
        deserialize_with = "deserialize_thinking_for_wire",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub thinking: Vec<crate::reasoning::ThinkingBlock>,

    /// The model response that produced this row.
    ///
    /// Minted once per response by `StateManager::begin_response`; every
    /// `ToolCall` row and the `Agent` row of that response carry the same
    /// value. This is the ONLY thing that makes a response boundary visible
    /// after the fact — it cannot be inferred from row adjacency (every
    /// ToolCall follows a ToolResult).
    ///
    /// `None` means "unknown": a row persisted before this field existed, a
    /// sub-agent row, or a row appended with no response open. Rows with
    /// `None` NEVER contribute `Reasoning` to the wire — replaying a block
    /// whose group is unknown is the 400 this field exists to prevent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<u64>,
}

/// Server-side web-snapshot serializer: drop signatures and
/// `Redacted` payloads. The browser only wants display-shaped text;
/// converting in here (rather than at the channel boundary) means a
/// stray render can never surface signed bytes.
fn serialize_thinking_for_wire<S>(
    blocks: &[crate::reasoning::ThinkingBlock],
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::Serialize;
    #[derive(Serialize)]
    struct ThinkingWireText<'a> {
        text: &'a str,
    }
    let visible: Vec<ThinkingWireText<'_>> = blocks
        .iter()
        .filter_map(|b| match b {
            crate::reasoning::ThinkingBlock::Thinking { text, .. } => {
                Some(ThinkingWireText { text })
            }
            crate::reasoning::ThinkingBlock::Redacted { .. } => None,
        })
        .collect();
    visible.serialize(serializer)
}

/// Wire → StateManager deserializer. The wire carries
/// `Vec<{ text: String }>` (display-shaped only), but the
/// `ChatMessage.thinking` field is the lossless `ThinkingBlock` —
/// restore it with empty signatures on load. Redacted entries from
/// the wire (if a custom client somehow emits them) decode as the
/// lossless variant too; the round-trip stays faithful.
fn deserialize_thinking_for_wire<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<crate::reasoning::ThinkingBlock>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    #[derive(Deserialize)]
    struct ThinkingWireText {
        text: String,
    }
    let wire: Vec<ThinkingWireText> = Vec::deserialize(deserializer)?;
    Ok(wire
        .into_iter()
        .map(|t| crate::reasoning::ThinkingBlock::Thinking {
            text: t.text,
            // Signatures are not transmitted over the web snapshot;
            // round-trip loses them by design (they are credentials-ish
            // and have no display use). The StateManager rebuild path
            // can still see the block — its signature will be empty,
            // so `get_agent_history` drops it at the wire-strip seam
            // (replaying an unsigned block is a guaranteed 400).
            signature: String::new(),
        })
        .collect())
}

impl ChatMessage {
    /// Create a new user message
    pub fn user(content: String) -> Self {
        Self {
            role: MessageRole::User,
            content,
            attachments: Vec::new(),
            images: Vec::new(),
            timestamp: Local::now(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            call_id: None,
            compacted: false,
            thinking: Vec::new(),
            source: MessageSource::Human,
            response_id: None,
        }
    }

    /// Create a user message with image attachments.
    ///
    /// Mirrors [`ChatMessage::user`] but carries images that will be
    /// converted to `rig_core::UserContent::Image` at the wire boundary. Intended
    /// for `[img:…]` inline syntax flowing through `SubmitKind::MultimodalMessage`.
    pub fn user_with_attachments(
        content: String,
        attachments: Vec<crate::vision::ImageAttachment>,
    ) -> Self {
        Self {
            role: MessageRole::User,
            content,
            attachments,
            images: Vec::new(),
            timestamp: Local::now(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            call_id: None,
            compacted: false,
            thinking: Vec::new(),
            source: MessageSource::Human,
            response_id: None,
        }
    }

    /// Create a synthetic user message representing background-process
    /// output. Used by the `bash_bg` drain seam: the agent loop treats
    /// these as ordinary user turns at the wire boundary, but the
    /// renderer and the persisted transcript know the difference via
    /// [`MessageSource::Background`].
    pub fn user_from_background(content: String, proc_ids: Vec<u32>) -> Self {
        Self {
            role: MessageRole::User,
            content,
            attachments: Vec::new(),
            images: Vec::new(),
            timestamp: Local::now(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            call_id: None,
            compacted: false,
            thinking: Vec::new(),
            source: MessageSource::Background { proc_ids },
            response_id: None,
        }
    }

    /// Create a new agent message
    pub fn agent(content: String) -> Self {
        Self {
            role: MessageRole::Agent,
            content,
            attachments: Vec::new(),
            images: Vec::new(),
            timestamp: Local::now(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            call_id: None,
            compacted: false,
            thinking: Vec::new(),
            source: MessageSource::Human,
            response_id: None,
        }
    }

    /// Create a new system message
    pub fn system(content: String) -> Self {
        Self {
            role: MessageRole::System,
            content,
            attachments: Vec::new(),
            images: Vec::new(),
            timestamp: Local::now(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            call_id: None,
            compacted: false,
            thinking: Vec::new(),
            source: MessageSource::Human,
            response_id: None,
        }
    }

    /// Create a compaction summary message
    pub fn summary(content: String) -> Self {
        Self {
            role: MessageRole::Summary,
            content,
            attachments: Vec::new(),
            images: Vec::new(),
            timestamp: Local::now(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            call_id: None,
            compacted: false,
            thinking: Vec::new(),
            source: MessageSource::Human,
            response_id: None,
        }
    }

    /// Create a new tool call message with structured display:
    /// - Shows thought intent first
    /// - Shows key params (2-3 lines max)
    ///
    /// Stores raw tool_name and args for lossless persistence.
    pub fn tool_call(tool_name: &str, args: &str, call_id: Option<String>) -> Self {
        let content = format_tool_call(tool_name, args);
        Self {
            role: MessageRole::ToolCall,
            content,
            attachments: Vec::new(),
            images: Vec::new(),
            timestamp: Local::now(),
            tool_name: Some(tool_name.to_string()),
            tool_args: Some(args.to_string()),
            tool_result: None,
            call_id,
            compacted: false,
            thinking: Vec::new(),
            source: MessageSource::Human,
            response_id: None,
        }
    }

    /// Create a new tool result message with truncation to top 2-3 lines.
    /// Stores raw tool_name, args, result, and call_id for lossless persistence.
    pub fn tool_result(tool_name: &str, args: &str, result: &str, call_id: Option<String>) -> Self {
        let images = image_refs_from_tool_output(tool_name, result);
        let content = format_tool_result(tool_name, result, &images);
        Self {
            role: MessageRole::ToolResult,
            content,
            attachments: Vec::new(),
            images,
            timestamp: Local::now(),
            tool_name: Some(tool_name.to_string()),
            tool_args: Some(args.to_string()),
            tool_result: Some(result.to_string()),
            call_id,
            compacted: false,
            thinking: Vec::new(),
            source: MessageSource::Human,
            response_id: None,
        }
    }

    /// Create a new assistant message (alias for agent)
    pub fn assistant(content: String) -> Self {
        Self::agent(content)
    }

    /// Create a new error message
    pub fn error(content: String) -> Self {
        Self {
            role: MessageRole::System,
            content,
            attachments: Vec::new(),
            images: Vec::new(),
            timestamp: Local::now(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            call_id: None,
            compacted: false,
            thinking: Vec::new(),
            source: MessageSource::Human,
            response_id: None,
        }
    }

    /// Create a message with a fixed timestamp (for testing)
    pub fn with_timestamp(role: MessageRole, content: String, timestamp_str: &str) -> Self {
        use chrono::NaiveDateTime;
        Self {
            role,
            content,
            attachments: Vec::new(),
            images: Vec::new(),
            timestamp: NaiveDateTime::parse_from_str(timestamp_str, "%Y-%m-%d %H:%M:%S")
                .unwrap()
                .and_local_timezone(Local)
                .unwrap(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            call_id: None,
            compacted: false,
            thinking: Vec::new(),
            source: MessageSource::Human,
            response_id: None,
        }
    }

    /// Tag this message with a non-default origin lane. Composes with every
    /// role constructor (e.g. `ChatMessage::agent(t).with_source(SubAgent { role })`),
    /// so sub-agent turns carry their role without duplicating constructors.
    pub fn with_source(mut self, source: MessageSource) -> Self {
        self.source = source;
        self
    }

    /// True iff this message is part of **the orchestrator's live context**:
    /// it survived compaction AND it belongs to the orchestrator lane.
    ///
    /// This is THE definition of "what the orchestrator model sees". Every seam
    /// that counts, summarises, or serialises the orchestrator's context filters
    /// on this and nothing else. A sub-agent's internal turns live in the
    /// transcript (display + persistence) but are never the orchestrator's
    /// context — its own context died with the delegation.
    pub fn is_orchestrator_context(&self) -> bool {
        !self.compacted && self.source.is_orchestrator_lane()
    }

    /// Drop the raw wire payload of a tool-result row that is no longer in
    /// the orchestrator's live context (W1). Pure; idempotent; re-derives
    /// `content`; a no-op for every row that carries no binary payload. The
    /// row's `images` are untouched — that is how the picture survives (W2).
    // No production caller yet — the three sites that enforce W1 (elide on
    // append, on compaction, and on load) are T6/T7; only the unit tests
    // below exercise this today. Drop this allow when T6 wires up the first
    // production caller (same situation `image_ref_from_output` was in
    // before T4). `#[expect]` is not usable here: under `cfg(test)` the
    // tests below *do* call this, so the expectation goes unfulfilled and
    // fails `--all-targets`.
    #[allow(dead_code)]
    pub(crate) fn elide_binary_payload(&mut self) {
        const NOTICE_PREFIX: &str = "[image not retained in the transcript: ";

        if self.tool_name.as_deref() != Some(view_image::NAME) {
            return;
        }
        let Some(result) = self.tool_result.as_ref() else {
            return;
        };
        if result.starts_with(NOTICE_PREFIX) {
            return; // already elided — idempotent fixpoint
        }

        // `display_name` is model-influenced and unbounded — `view_image`
        // falls back to the whole `args.path` when the path has no file
        // name — so cap it. Without this the "notice" for a pathological
        // path is itself kilobytes, which defeats the point of eliding.
        let display_name = truncate_str(
            self.images
                .first()
                .map(|r| r.display_name.as_str())
                .unwrap_or("image"),
            80,
        );
        let dropped_kb = result.len() / 1024;
        self.tool_result = Some(format!(
            "{NOTICE_PREFIX}{display_name} — {dropped_kb} KB of base64 dropped after the \
             turn. Call view_image again to load it.]"
        ));
        self.content = format_tool_result(view_image::NAME, "", &self.images);
    }
}

/// Format tool call with structured output: thought intent first, then params
pub(crate) fn format_tool_call(tool_name: &str, args: &str) -> String {
    // Special-case the think tool: show "Thinking..." instead of verbose reasoning
    if tool_name == "think" {
        return "🤔 Thinking...".to_string();
    }

    // Try to parse JSON args to extract thought and key params
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(args) {
        let mut lines = Vec::new();

        // Line 1: Thought intent (always first if present, never truncated)
        if let Some(thought) = parsed.get("thought").and_then(|v| v.as_str())
            && !thought.is_empty()
        {
            lines.push(format!("💭 {}", thought));
        }

        // Line 2: Tool name with key params
        let mut params = Vec::new();
        for (key, value) in parsed.as_object().unwrap_or(&serde_json::Map::new()) {
            if key == "thought" {
                continue; // Already shown
            }
            let value_str = match value {
                serde_json::Value::String(s) => truncate_str(s, 60),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Null => "null".to_string(),
                _ => truncate_str(&value.to_string(), 40),
            };
            params.push(format!("{}={}", key, value_str));
        }

        let params_str = params.join(", ");
        lines.push(format!("🔧 {}({})", tool_name, params_str));

        // Limit to 2-3 lines total
        if lines.len() > 3 {
            lines.truncate(3);
        }

        lines.join("\n")
    } else {
        // Fallback: just show tool name with raw args
        format!("🔧 {}({})", tool_name, truncate_str(args, 150))
    }
}

/// Format tool result with truncation to top 2-3 lines
pub(crate) fn format_tool_result(tool_name: &str, result: &str, images: &[ImageRef]) -> String {
    // Special handling per tool type
    match tool_name {
        "bash" => format_bash_result(result),
        "file_read" => format_file_read_result(result),
        "list_directory" => format_list_directory_result(result),
        "web_search" => format_search_result(result),
        view_image::NAME => format_view_image_result(images),
        _ => format_generic_result(result),
    }
}

/// A `view_image` row renders as exactly one line — the raw base64 payload
/// never belongs in the transcript, before OR after elision.
fn format_view_image_result(images: &[ImageRef]) -> String {
    match images.first() {
        Some(r) => format!("🖼 {}", r.display_name),
        None => "🖼 image".to_string(),
    }
}

/// `view_image` output carries at most one ref; every other tool's result
/// is never parsed as one, no matter how it happens to be shaped.
fn image_refs_from_tool_output(tool_name: &str, result: &str) -> Vec<ImageRef> {
    if tool_name != view_image::NAME {
        return Vec::new();
    }
    view_image::image_ref_from_output(result)
        .into_iter()
        .collect()
}

pub(crate) fn truncate_str(s: &str, max_len: usize) -> String {
    let s_len = s.chars().count();

    if s_len <= max_len {
        s.to_string()
    } else if max_len < 3 {
        // Not enough room for "...", just truncate to what fits
        s.chars().take(max_len).collect()
    } else {
        s.chars().take(max_len - 3).collect::<String>() + "..."
    }
}

/// Truncate a single line to max chars, adding "..." if truncated
pub(crate) fn truncate_line(s: &str, max_len: usize) -> String {
    let s_len = s.chars().count();

    if s_len <= max_len {
        s.to_string()
    } else if max_len < 3 {
        // Not enough room for "...", just truncate to what fits
        s.chars().take(max_len).collect()
    } else {
        s.chars().take(max_len - 3).collect::<String>() + "..."
    }
}

/// Truncate each line to max chars, then truncate to top N lines
fn truncate_lines(s: &str, max_lines: usize, max_chars: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let total = lines.len();

    // First truncate each line
    let truncated: Vec<String> = lines.iter().map(|l| truncate_line(l, max_chars)).collect();

    if total <= max_lines {
        truncated.join("\n")
    } else {
        let preview: Vec<&str> = truncated
            .iter()
            .take(max_lines)
            .map(|s| s.as_str())
            .collect();
        format!(
            "{}\n... [{} lines truncated]",
            preview.join("\n"),
            total - max_lines
        )
    }
}

/// Truncate result to top N lines
fn truncate_to_lines(s: &str, max_lines: usize) -> String {
    truncate_lines(s, max_lines, 60)
}

fn format_bash_result(result: &str) -> String {
    // Parse bash output format: "Exit code: X\nSTDOUT:\n...\nSTDERR:\n..."
    let lines: Vec<&str> = result.lines().collect();

    // Extract exit code
    let exit_code = lines
        .iter()
        .find(|l| l.starts_with("Exit code:"))
        .map(|l| l.split_whitespace().last().unwrap_or("0"))
        .unwrap_or("0");

    // Find stdout/stderr sections
    let mut stdout_lines = Vec::new();
    let mut stderr_lines = Vec::new();
    let mut in_stdout = false;
    let mut in_stderr = false;

    for line in &lines {
        if line.starts_with("STDOUT:") {
            in_stdout = true;
            in_stderr = false;
        } else if line.starts_with("STDERR:") {
            in_stdout = false;
            in_stderr = true;
        } else if line.starts_with("Exit code:") || line.starts_with("Full output saved") {
            in_stdout = false;
            in_stderr = false;
        } else if in_stdout {
            stdout_lines.push(*line);
        } else if in_stderr {
            stderr_lines.push(*line);
        }
    }

    let exit_icon = if exit_code == "0" { "✅" } else { "❌" };
    let mut output = format!("{} Exit {}", exit_icon, exit_code);

    // Show first 2-3 lines of stdout (truncated to 60 chars each)
    let stdout_preview: Vec<String> = stdout_lines
        .iter()
        .take(2)
        .map(|l| truncate_line(l, 60))
        .collect();
    if !stdout_preview.is_empty() {
        output.push_str(&format!(" | {}", stdout_preview.join(" | ")));
    }

    // Show stderr if present (1 line max)
    if !stderr_lines.is_empty() {
        output.push_str(&format!(" | ⚠️ {}", truncate_str(stderr_lines[0], 50)));
    }

    // Add truncation notice if there was more
    let total_lines = stdout_lines.len() + stderr_lines.len();
    if total_lines > 3 {
        output.push_str(&format!(" ... [{} more lines]", total_lines - 3));
    }

    output
}

fn format_file_read_result(result: &str) -> String {
    // Parse: "     1\tcontent\n     2\tcontent2\n..."
    let lines: Vec<&str> = result.lines().collect();

    // Extract total lines from last line format or count
    let total_lines = lines.len();

    // Truncate lines and show first 3 only
    let truncated: Vec<String> = lines.iter().take(3).map(|l| truncate_line(l, 60)).collect();
    let preview_str = truncated.join("\n");

    let mut output = format!("📄 {} lines\n{}", total_lines, preview_str);

    if lines.len() > 3 {
        output.push_str(&format!("\n... [{} more lines]", lines.len() - 3));
    }

    output
}

fn format_list_directory_result(result: &str) -> String {
    let lines: Vec<&str> = result.lines().collect();
    let total = lines.len();

    // Truncate each entry and show first 3
    let preview: Vec<String> = lines.iter().take(3).map(|l| truncate_line(l, 60)).collect();
    let preview_str = preview.join(", ");

    let mut output = format!("📁 {} entries\n{}", total, preview_str);

    if lines.len() > 3 {
        output.push_str(&format!("\n... [{} more]", lines.len() - 3));
    }

    output
}

fn format_search_result(result: &str) -> String {
    // Truncate each line and show first 3 results
    let lines: Vec<&str> = result.lines().collect();
    let preview: Vec<String> = lines.iter().take(3).map(|l| truncate_line(l, 60)).collect();
    let preview_str = preview.join("\n");

    let mut output = preview_str;

    if lines.len() > 3 {
        output.push_str(&format!("\n... [{} more results]", lines.len() - 3));
    }

    output
}

fn format_generic_result(result: &str) -> String {
    // Generic truncation to 2-3 lines
    truncate_to_lines(result, 3)
}

/// The lane label for every non-sub-agent turn. Single source of truth for
/// the string `SessionStats` buckets and gates on.
pub const ORCHESTRATOR_LANE: &str = "orchestrator";

/// Origin of a chat message.
///
/// Distinguishes background-process-driven synthetic turns from
/// human-typed input so the renderer can style them and the persisted
/// transcript records the source. See `bash-background.md` open Q6.
///
/// **Deserialization back-compat:** accepts BOTH the legacy string form
/// (`"source": "Human"`, `"source": "SubAgent"`, `"source": "Background"`)
/// that pre-v6 conversation files use AND the current tagged form
/// (`"source": {"kind": "human"}`). The legacy form is a bare string;
/// every variant decodes to the carrier with empty/default fields. The
/// serialization side stays tagged — only legacy *loads* are tolerated.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageSource {
    /// Typed by the human OR a normal assistant/tool flow.
    #[default]
    Human,
    /// Synthetic user turn produced by draining background process output.
    /// Carries the ids of contributing processes.
    Background {
        /// Ids of processes that contributed to this synthetic turn.
        proc_ids: Vec<u32>,
    },
    /// A turn produced by a sub-agent running inside a `delegate` tool call.
    /// Carries the role name so the renderer can label/colour it and so
    /// `get_agent_history` can filter it out of the orchestrator's wire
    /// context (see `is_orchestrator_lane`).
    SubAgent {
        /// The pipeline role that produced this turn.
        role: String,
    },
}

impl<'de> serde::Deserialize<'de> for MessageSource {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        // Tagged shape — the only form the current code emits.
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum Tagged {
            Human,
            Background { proc_ids: Vec<u32> },
            SubAgent { role: String },
        }

        // Accept either a string (legacy pre-v6 form) or a tagged map.
        // String maps to a Human / Background{vec![]} / SubAgent{""} —
        // enough to round-trip every pre-v6 file we have on disk, where
        // the carrier data was not persisted.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Shape {
            Legacy(String),
            Tagged(Tagged),
        }

        let shape = Shape::deserialize(deserializer)?;
        match shape {
            Shape::Legacy(s) => match s.as_str() {
                "Human" | "human" => Ok(MessageSource::Human),
                "Background" | "background" => Ok(MessageSource::Background {
                    proc_ids: Vec::new(),
                }),
                "SubAgent" | "sub_agent" | "subagent" => Ok(MessageSource::SubAgent {
                    role: String::new(),
                }),
                other => Err(D::Error::custom(format!(
                    "unknown legacy MessageSource variant: {other}"
                ))),
            },
            Shape::Tagged(Tagged::Human) => Ok(MessageSource::Human),
            Shape::Tagged(Tagged::Background { proc_ids }) => {
                Ok(MessageSource::Background { proc_ids })
            }
            Shape::Tagged(Tagged::SubAgent { role }) => Ok(MessageSource::SubAgent { role }),
        }
    }
}

impl MessageSource {
    /// Borrow-cheap predicate for `skip_serializing_if`.
    pub fn is_human(&self) -> bool {
        matches!(self, MessageSource::Human)
    }

    /// True for every lane whose turns belong in the orchestrator's wire
    /// context — i.e. everything *except* a sub-agent's internal turns.
    /// `Background` counts as orchestrator input (it's real conversation
    /// the orchestrator must see); only `SubAgent` is isolated.
    ///
    /// This is the load-bearing isolation predicate: `get_agent_history`
    /// filters on it so a sub-agent's turns never leak into the
    /// orchestrator model's context.
    pub fn is_orchestrator_lane(&self) -> bool {
        !matches!(self, MessageSource::SubAgent { .. })
    }

    /// Stable label for per-lane stats aggregation. Every
    /// orchestrator-lane turn (Human/Background) buckets under
    /// `"orchestrator"`; a sub-agent buckets under its role name. This is the
    /// key `SessionStats` groups tokens/cost by, and the label the `/stats`
    /// breakdown prints.
    pub fn lane_label(&self) -> &str {
        match self {
            MessageSource::SubAgent { role } => role,
            _ => ORCHESTRATOR_LANE,
        }
    }
}

/// Role of a message sender
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// User message
    User,

    /// Agent (AI) message
    Agent,

    /// System message
    System,

    /// Tool invocation
    ToolCall,

    /// Tool execution result
    ToolResult,

    /// Compaction summary (injected by the compactor)
    Summary,
}

impl fmt::Display for MessageRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageRole::User => write!(f, "User"),
            MessageRole::Agent => write!(f, "Agent"),
            MessageRole::System => write!(f, "System"),
            MessageRole::ToolCall => write!(f, "Tool Call"),
            MessageRole::ToolResult => write!(f, "Tool Result"),
            MessageRole::Summary => write!(f, "Summary"),
        }
    }
}

/// TODO panel state
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TodoState {
    /// Whether the TODO panel is visible
    pub visible: bool,

    /// TODO items
    pub items: Vec<TodoItem>,
}

impl TodoState {
    /// Create a new empty TodoState
    pub fn new() -> Self {
        Self::default()
    }

    /// Toggle visibility
    pub fn toggle_visibility(&mut self) {
        self.visible = !self.visible;
    }

    /// Add a TODO item
    pub fn add_item(&mut self, text: String) -> TodoItem {
        let item = TodoItem::new(text);
        self.items.push(item.clone());
        item
    }

    /// Update a TODO item by index
    pub fn update_item(&mut self, index: usize, action: TodoItemAction) -> Option<TodoItem> {
        if let Some(item) = self.items.get_mut(index) {
            match action {
                TodoItemAction::ToggleComplete => {
                    item.completed = !item.completed;
                    if item.completed {
                        item.status = TodoStatus::Completed;
                    } else {
                        item.status = TodoStatus::Pending;
                    }
                }
                TodoItemAction::UpdateStatus(status) => {
                    item.status = status.clone();
                    item.completed = matches!(status, TodoStatus::Completed);
                }
                TodoItemAction::Delete => {
                    return Some(self.items.remove(index));
                }
            }
            Some(item.clone())
        } else {
            None
        }
    }

    /// Remove a TODO item by index
    pub fn remove_item(&mut self, index: usize) -> Option<TodoItem> {
        Some(self.items.remove(index))
    }

    /// Clear completed items
    pub fn clear_completed(&mut self) -> usize {
        let initial = self.items.len();
        self.items.retain(|item| !item.completed);
        initial - self.items.len()
    }

    /// Get count of items by status
    pub fn count_by_status(&self) -> (usize, usize, usize, usize) {
        let mut pending = 0;
        let mut in_progress = 0;
        let mut completed = 0;
        let mut cancelled = 0;

        for item in &self.items {
            match item.status {
                TodoStatus::Pending => pending += 1,
                TodoStatus::InProgress => in_progress += 1,
                TodoStatus::Completed => completed += 1,
                TodoStatus::Cancelled => cancelled += 1,
            }
        }

        (pending, in_progress, completed, cancelled)
    }
}

/// A single TODO item (UI representation)
///
/// This is a simplified version of the core TodoItem that's optimized
/// for UI rendering. It's kept in sync with the core TodoItem by the
/// StateManager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    /// Unique ID (from core TodoItem)
    pub id: usize,

    /// Task description
    pub text: String,

    /// Whether the item is completed
    pub completed: bool,

    /// Status of the item
    pub status: TodoStatus,

    /// Whether this item is currently selected
    #[serde(default)]
    pub selected: bool,
}

impl TodoItem {
    /// Create a new TODO item
    pub fn new(text: String) -> Self {
        Self {
            id: 0, // Will be set by StateManager when syncing
            text,
            completed: false,
            status: TodoStatus::Pending,
            selected: false,
        }
    }
}

/// Convert from core TodoItem to UI TodoItem
impl From<&CoreTodoItem> for TodoItem {
    fn from(item: &CoreTodoItem) -> Self {
        Self {
            id: item.id,
            text: item.task.clone(),
            completed: matches!(item.status, TodoStatus::Completed),
            status: item.status.clone(),
            selected: false,
        }
    }
}

/// Input field state
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InputState {
    /// Current input buffer
    pub buffer: String,

    /// Cursor position in the buffer
    #[serde(default)]
    pub cursor_pos: usize,

    /// Whether the input is in command mode (after typing /)
    #[serde(default)]
    pub in_command_mode: bool,

    /// Number of wrapped lines in the input (for dynamic height)
    #[serde(default)]
    pub wrapped_lines: usize,
}

impl InputState {
    /// Create a new empty InputState
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear the input buffer
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor_pos = 0;
        self.in_command_mode = false;
        self.wrapped_lines = 0;
    }

    /// Check if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Get the current cursor position
    pub fn cursor(&self) -> usize {
        self.cursor_pos
    }

    /// Set the cursor position
    pub fn set_cursor(&mut self, pos: usize) {
        self.cursor_pos = pos.min(self.buffer.len());
    }

    /// Set the wrapped line count
    pub fn set_wrapped_lines(&mut self, lines: usize) {
        self.wrapped_lines = lines;
    }
}

/// One lane's slice of the session stats, serialized to the web wire so the
/// Session panel can break stats down per agent (and scope to one). Mirrors
/// [`crate::hooks::SessionStats`]'s `LaneStats`: every field accumulates over
/// the session.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LaneStat {
    /// Lane label — `"orchestrator"` or a sub-agent role name.
    pub lane: String,
    /// Input tokens summed across this lane's requests.
    pub input_tokens: u64,
    /// Output tokens summed across this lane's requests.
    pub output_tokens: u64,
    /// API calls made on this lane.
    pub api_calls: u64,
    /// Model alias this lane runs on (orchestrator: the active alias; a role:
    /// its member entry in the selected pipeline). Empty when unknown — old
    /// wire snapshots and lanes with no configured role.
    #[serde(default)]
    pub model: String,
    /// Accumulated cost (USD) on this lane.
    pub cost: f64,
}

/// Session statistics state
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionState {
    /// Total input tokens
    pub total_input_tokens: u64,

    /// Total output tokens
    pub total_output_tokens: u64,

    /// Total API calls
    pub total_api_calls: u64,

    /// Total cost in USD
    pub total_cost: f64,

    /// Per-lane breakdown (orchestrator + each sub-agent role), sorted with
    /// orchestrator first. Empty until the first lane-attributed request. The
    /// flat totals above stay the authoritative grand total — this is a scoped
    /// view for the Agents panel, never the source of the totals.
    #[serde(default)]
    pub lanes: Vec<LaneStat>,

    /// Current model name (wire id).
    pub model: String,

    /// Current provider name (informational handle from the providers
    /// list, e.g. `"openrouter"`, `"patchnotes"`). Together with
    /// `model`, forms the wire-id pair persisted on conversations and
    /// used by `/load` to re-activate the model. Empty in the rare
    /// boot path where the legacy `provider:` block hasn't been
    /// promoted yet.
    #[serde(default)]
    pub provider_name: String,

    /// Current model alias (the user-facing handle from the
    /// [`crate::config::ModelRegistry`]). May be empty in tests or in
    /// the rare boot path where the legacy `provider:` block hasn't
    /// been promoted yet — never trust this is non-empty without
    /// checking. Production boot stamps this from the resolved model.
    /// **Display only — not persisted on disk.** Conversations carry
    /// `provider_name + model` for re-activation.
    #[serde(default)]
    pub model_alias: String,
}

impl SessionState {
    /// Create a new empty SessionState
    pub fn new() -> Self {
        Self::default()
    }

    /// Get total tokens
    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }

    /// Format cost as a string
    pub fn format_cost(&self) -> String {
        format!("{:.4}", self.total_cost)
    }

    /// Format tokens as a string with K/M suffix
    pub fn format_tokens(&self, tokens: u64) -> String {
        if tokens >= 1_000_000 {
            format!("{:.1}M", tokens as f64 / 1_000_000.0)
        } else if tokens >= 1_000 {
            format!("{:.1}k", tokens as f64 / 1_000.0)
        } else {
            format!("{}", tokens)
        }
    }
}

/// Context usage state
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextState {
    /// Current context usage (tokens)
    pub current_usage: u64,

    /// Context window size (tokens)
    pub window_size: u64,

    /// Whether compaction is enabled
    #[serde(default)]
    pub compaction_enabled: bool,

    /// Compaction threshold (0.0 - 1.0)
    #[serde(default = "default_threshold")]
    pub compaction_threshold: f64,
}

impl ContextState {
    /// Create a new ContextState
    pub fn new() -> Self {
        Self::default()
    }

    /// Get usage as a percentage
    pub fn usage_percentage(&self) -> f64 {
        if self.window_size == 0 {
            return 0.0;
        }
        (self.current_usage as f64 / self.window_size as f64) * 100.0
    }

    /// Check if compaction should be triggered
    pub fn should_compact(&self) -> bool {
        self.compaction_enabled && self.usage_percentage() >= (self.compaction_threshold * 100.0)
    }
}

fn default_threshold() -> f64 {
    0.8 // 80%
}

/// Conversation state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationState {
    /// Conversation ID
    pub id: String,

    /// Conversation name
    pub name: String,

    /// Model used
    pub model: String,

    /// Message count
    pub message_count: usize,

    /// Last updated timestamp
    pub updated_at: DateTime<Local>,
}

impl ConversationState {
    /// Create a new ConversationState
    pub fn new(id: String, name: String, model: String) -> Self {
        Self {
            id,
            name,
            model,
            message_count: 0,
            updated_at: Local::now(),
        }
    }
}

/// Live snapshot of background processes (the `bash_bg` registry mirrored
/// for the TUI). Updated by `StateManager::update_bg_state` after each
/// registry mutation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BgState {
    /// Number of *running* background processes. Drives the `🛰 N bg`
    /// status-bar counter; rendered only when non-zero.
    pub running_count: usize,

    /// Up to the 5 most recent entries for an optional `/bg` listing.
    /// Kept tight — the canonical view is the `bash_bg list` tool call.
    pub recent_summaries: Vec<BgSummary>,
}

/// One row in `BgState`, mirroring `bg_processes::BgListEntry` at the
/// AppState boundary so the UI never has to depend on the bg module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BgSummary {
    pub id: u32,
    pub command: String,
    pub label: Option<String>,
    /// `"running"` | `"exited"`. Kept stringly-typed at the UI boundary
    /// to avoid pulling chrono/registry types into AppState.
    pub status: String,
    pub exit_code: Option<i32>,
}

/// Snapshot of the foreground `bash` tool panel.
///
/// Mirrors the design in `make-term-great-again.md`:
/// - **Idle** — no `bash` invocation has happened yet (or the state has
///   been cleared by `/new` / `/load`). Panel is hidden — zero rows.
/// - **Running** — a `bash` tool call is in flight. Panel shows the
///   command, pid, elapsed time, and a 5-line scrolling tail of the
///   live output buffer, plus a `stdin» _` row.
/// - **Finished** — the previous `bash` tool call has exited. Panel
///   shows the command, exit code, total duration, and the final 5
///   lines of output. Stays visible until the next `bash` call or until
///   the conversation is cleared.
///
/// The enum makes illegal states (e.g. `running` with an `exit_code`)
/// unrepresentable — the lifecycle is compiler-enforced.
///
/// `tail` is **already trimmed to ≤ 5 lines** at the snapshot boundary
/// by `StateManager`. The renderer pads with blanks if fewer; it never
/// truncates here. The full ring buffer lives in the `pty_runner`
/// `LineBuffer` (slice 3 wiring); this snapshot is just what the panel
/// needs to draw — same shape as `BgState` / `BgSummary`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BashPanelState {
    /// No foreground `bash` activity. Panel hidden.
    #[default]
    Idle,
    /// A `bash` invocation is currently running.
    Running {
        /// The shell command being executed (verbatim — what the model
        /// passed to the `bash` tool). Used for the panel header.
        command: String,
        /// Child process id. Surfaced in the header so the user can
        /// `kill -9 N` from another terminal if needed.
        pid: u32,
        /// Wall-clock start of the invocation. The renderer computes
        /// `elapsed = now - started_at` at draw time.
        started_at: DateTime<Local>,
        /// Last ≤ 5 lines of the live output buffer, ANSI-stripped.
        /// Cardinal rule: same bytes the eventual tool result will
        /// carry — see "one buffer, two views" in
        /// `make-term-great-again.md`.
        tail: Vec<String>,
    },
    /// The previous `bash` invocation has exited. Persists until the
    /// next call or conversation reset.
    Finished {
        /// Same `command` string that drove [`Self::Running`].
        command: String,
        /// Process exit code (0 = success → `✓` glyph, anything else →
        /// `✗` glyph).
        exit_code: i32,
        /// Total wall-clock duration in whole seconds. Stored as a scalar
        /// rather than `(started_at, finished_at)` because the renderer
        /// only needs the formatted duration (`ran 00:42`) — keeping the
        /// snapshot lean (`BgSummary`-style discipline).
        duration_secs: u64,
        /// Final ≤ 5 lines of the output buffer, ANSI-stripped.
        tail: Vec<String>,
    },
}

impl BashPanelState {
    /// Predicate for `skip_serializing_if` and layout decisions —
    /// `Idle` ⇒ the panel takes zero rows in the layout.
    pub fn is_idle(&self) -> bool {
        matches!(self, BashPanelState::Idle)
    }

    /// True iff the panel is currently `Running` (a live PTY child is
    /// attached). Used by the REPL to gate the `Ctrl+S` stdin-focus
    /// keybind — focusing an empty stdin row would just confuse the
    /// user.
    pub fn is_running(&self) -> bool {
        matches!(self, BashPanelState::Running { .. })
    }
}

/// View-layer override for [`BashPanelState`] visibility.
///
/// Encodes the three meaningful regimes of the foreground bash panel's
/// visibility as one enum field on [`AppState`]. The natural reach —
/// "is_hidden: bool" or two booleans — either loses a state or admits
/// an illegal one. This enum picks the type-system-friendly middle.
///
/// **Effective visibility** (the rule the renderer consumes):
///
/// | Visibility       | Idle  | Running | Finished |
/// |------------------|-------|---------|----------|
/// | `Auto`           | false | true    | true     |
/// | `OpenedByUser`   | true  | true    | true     |
/// | `ClosedByUser`   | false | false   | false    |
///
/// Variant names describe **how we got into this state**, not what
/// the renderer should do. `OpenedByUser` aged better than `ForceShow`
/// when this enum was reviewed — a future rendering-rule tweak
/// doesn't invalidate the gesture history.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum BashPanelVisibility {
    /// No user override. Renderer follows producer state: visible
    /// when Running/Finished, hidden when Idle. This is the fresh
    /// default and the state every reset path returns to.
    #[default]
    Auto,
    /// User pressed `Ctrl+B` while the panel was hidden — force
    /// visible. Meaningful primarily on Idle (renders an empty
    /// "no bash output yet" frame). On Running/Finished it's a
    /// no-op equivalent to `Auto`.
    OpenedByUser,
    /// User pressed `Ctrl+B` while the panel was visible — force
    /// hidden. Survives new user messages (typing a follow-up does
    /// NOT re-open). Cleared only by another `Ctrl+B`, by a new
    /// bash invocation (the producer reset), or by a conversation
    /// reset (`/new`, `/load`, `/model`).
    ClosedByUser,
}

impl BashPanelVisibility {
    /// Derive effective visibility given the current producer state.
    ///
    /// Single source of truth for the renderer-vs-state contract.
    /// Used by [`crate::ui::repl::bash_panel::effective_panel_height`]
    /// and by [`crate::state::StateManager::toggle_bash_panel_visibility`]
    /// (to decide which gesture variant to flip into).
    pub fn is_visible(self, state: &BashPanelState) -> bool {
        match self {
            BashPanelVisibility::Auto => !state.is_idle(),
            BashPanelVisibility::OpenedByUser => true,
            BashPanelVisibility::ClosedByUser => false,
        }
    }
}

/// UI preferences
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UiPreferences {
    /// Theme (light/dark)
    #[serde(default = "default_theme")]
    pub theme: String,

    /// Font size
    #[serde(default = "default_font_size")]
    pub font_size: u8,

    /// Show timestamps
    #[serde(default)]
    pub show_timestamps: bool,

    /// Wrap long lines
    #[serde(default = "default_true")]
    pub wrap_lines: bool,
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_font_size() -> u8 {
    12
}

fn default_true() -> bool {
    true
}

/// Welcome banner state — populated once at startup
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WelcomeState {
    pub provider_name: String,
    pub model: String,
    pub max_tokens: usize,
    pub builtin_tools_count: usize,
    pub mcp_tools_count: usize,
    pub skills_count: usize,
    pub searxng_enabled: bool,
    pub searxng_url: Option<String>,
    pub cost_tracking_enabled: bool,
    pub compaction_enabled: bool,
    pub compaction_threshold: f64,
    pub compaction_keep_recent: usize,
    pub conversation_persistence_enabled: bool,
    pub cwd: std::path::PathBuf,
    /// PeakBot version this binary was built from (matches the TUI
    /// welcome banner). `#[serde(default)]` keeps older wire snapshots
    /// (pre-v0.14) parsing cleanly — the field reads back as `""` until
    /// the next boot populates it.
    #[serde(default)]
    pub peakbot_version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Phase 1: sub-agent lane plumbing ────────────────────────────────

    #[test]
    fn human_and_background_are_orchestrator_lane() {
        assert!(MessageSource::Human.is_orchestrator_lane());
        assert!(
            MessageSource::Background {
                proc_ids: vec![1, 2]
            }
            .is_orchestrator_lane()
        );
    }

    #[test]
    fn sub_agent_is_not_orchestrator_lane() {
        let s = MessageSource::SubAgent {
            role: "researcher".to_string(),
        };
        assert!(!s.is_orchestrator_lane());
        assert!(!s.is_human());
    }

    /// `is_orchestrator_context` is the single definition of "what the
    /// orchestrator model sees": lane AND compaction state, never one alone.
    #[test]
    fn is_orchestrator_context_requires_lane_and_live() {
        let human = ChatMessage::user("hi".into());
        assert!(human.is_orchestrator_context());

        let background = ChatMessage::user_from_background("out".into(), vec![1]);
        assert!(background.is_orchestrator_context());

        let mut compacted = ChatMessage::user("old".into());
        compacted.compacted = true;
        assert!(!compacted.is_orchestrator_context());

        let sub = MessageSource::SubAgent {
            role: "junior".to_string(),
        };
        let sub_live = ChatMessage::agent("internal".into()).with_source(sub.clone());
        assert!(!sub_live.is_orchestrator_context());

        let mut sub_compacted = ChatMessage::agent("internal".into()).with_source(sub);
        sub_compacted.compacted = true;
        assert!(!sub_compacted.is_orchestrator_context());
    }

    #[test]
    fn lane_label_uses_the_shared_constant() {
        assert_eq!(MessageSource::Human.lane_label(), ORCHESTRATOR_LANE);
        assert_eq!(
            MessageSource::Background { proc_ids: vec![1] }.lane_label(),
            ORCHESTRATOR_LANE
        );
        assert_eq!(
            MessageSource::SubAgent {
                role: "junior".to_string()
            }
            .lane_label(),
            "junior"
        );
    }

    #[test]
    fn with_source_tags_any_role() {
        let role = "reviewer".to_string();
        let want = MessageSource::SubAgent { role: role.clone() };

        let agent = ChatMessage::agent("hi".into()).with_source(want.clone());
        let call = ChatMessage::tool_call("bash", "{}", None).with_source(want.clone());
        let result = ChatMessage::tool_result("bash", "{}", "ok", None).with_source(want.clone());

        assert_eq!(agent.source, want);
        assert_eq!(call.source, want);
        assert_eq!(result.source, want);
    }

    #[test]
    fn sub_agent_source_serde_roundtrip() {
        let s = MessageSource::SubAgent {
            role: "researcher".to_string(),
        };
        let json = serde_json::to_string(&s).unwrap();
        // snake_case tag so the web wire reads `m.source?.kind === "sub_agent"`.
        assert_eq!(json, r#"{"kind":"sub_agent","role":"researcher"}"#);
        let back: MessageSource = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
    // ── Stage 1.2: AppState wire-shape back-compat ──────────────────────
    //
    // The plan removes `pipeline_available` and `subagents_enabled`
    // from AppState and adds `pipelines: Vec<PipelineInfo>` and
    // `selected_pipeline: Option<String>` (plan §3 "AppState — DELETED:
    // pipeline_available, subagents_enabled (both derived)").
    //
    // Two properties must hold:
    // 1. A snapshot emitted by the OLD code (fields present) still
    //    parses on the new code. The fields must be `#[serde(default)]`
    //    on the new code path, OR serde-tolerant in some other way.
    // 2. The new fields round-trip 1:1 through JSON.

    /// A snapshot emitted by today's `AppState` (pre-Stage 1.2) carries
    /// `pipeline_available: true` and `subagents_enabled: true`. When
    /// the new code parses it, those fields must be silently absorbed
    /// (the new code's `selected_pipeline` derives from the catalogue,
    /// not from the deleted booleans). Without `#[serde(default)]` on
    /// the new fields this test would fail with "missing field
    /// selected_pipeline"; without absorb-tolerance for the old fields
    /// it would fail with "unknown field pipeline_available".
    #[test]
    fn old_app_state_snapshot_with_legacy_booleans_parses() {
        // Hand-craft a JSON snapshot in the shape the pre-Stage 1.2
        // code emits. Empty chat / stats / context / conversation —
        // we only care about the field-level serde behaviour, not the
        // content.
        let old_snapshot = r#"{
            "chat": {"messages": [], "auto_scroll": true, "scroll_offset": 0},
            "todo": {"items": [], "visible": false},
            "input": {"buffer": "", "cursor": 0, "history": []},
            "stats": {"model": "", "model_alias": "", "provider_name": "", "total_input_tokens": 0, "total_output_tokens": 0, "total_api_calls": 0, "total_cost": 0.0, "lanes": []},
            "context": {"current_usage": 0, "window_size": 0, "compaction_enabled": false, "compaction_threshold": 0.0, "last_input_tokens": 0, "compaction_keep_recent": 0},
            "conversation": null,
            "preferences": {"theme": "auto", "tool_render_mode": "collapsed"},
            "is_running": false,
            "is_loading": false,
            "is_final": false,
            "status_message": null,
            "exit_requested": false,
            "pending_input_count": 0,
            "bg": {"running_count": 0, "recent_summaries": []},
            "bash_panel": {"kind": "idle"},
            "bash_panel_visibility": "Auto",
            "pipeline_available": true,
            "subagents_enabled": false
        }"#;

        let parsed: AppState =
            serde_json::from_str(old_snapshot).expect("old snapshot must parse on new code");
        // The legacy booleans were absorbed without error — the
        // new code derives `selected_pipeline` from `pipelines` (the
        // catalogue), not from those booleans. So:
        assert_eq!(
            parsed.selected_pipeline, None,
            "old snapshot has no catalogue → selected_pipeline derives to None"
        );
        assert!(
            parsed.pipelines.is_empty(),
            "old snapshot has no catalogue → pipelines is empty"
        );
    }

    /// The new fields round-trip 1:1. `pipelines: Vec<PipelineInfo>`
    /// uses `#[serde(default)]` for forward compat (a new code
    /// snapshot read by an older client sees an empty vec). The
    /// `selected_pipeline: Option<String>` uses
    /// `skip_serializing_if = "Option::is_none"` so a snapshot with
    /// no selection stays byte-identical to the pre-Stage-1.2 form
    /// for that field.
    #[test]
    fn new_app_state_fields_round_trip() {
        let mut state = AppState::new();
        // Stamp a non-empty selection + catalogue.
        state.selected_pipeline = Some("web-team".into());
        state.pipelines = vec![PipelineInfo {
            name: "web-team".into(),
            orchestrator_model: "sonnet".into(),
            members: vec![("reviewer".to_string(), "flash".to_string())],
        }];

        let json = serde_json::to_string(&state).expect("serializes");
        assert!(
            json.contains("\"selected_pipeline\":\"web-team\""),
            "selected_pipeline must serialize as a top-level field; got: {json}"
        );
        assert!(
            json.contains("\"pipelines\""),
            "pipelines must serialize as a top-level field; got: {json}"
        );

        let parsed: AppState = serde_json::from_str(&json).expect("round-trips back");
        assert_eq!(parsed.selected_pipeline, Some("web-team".into()));
        assert_eq!(parsed.pipelines.len(), 1);
        assert_eq!(parsed.pipelines[0].name, "web-team");
        assert_eq!(parsed.pipelines[0].orchestrator_model, "sonnet");
        assert_eq!(
            parsed.pipelines[0].members,
            vec![("reviewer".to_string(), "flash".to_string())]
        );
    }

    /// The deleted legacy booleans (`pipeline_available`,
    /// `subagents_enabled`) MUST NOT appear in the new snapshot when
    /// serialized by the new code. Pin: a fresh `AppState` serializes
    /// to a JSON document that does NOT contain either of those keys.
    /// If a refactor keeps the fields around as dead state, this test
    /// flags it (they should be gone, per plan §3).
    #[test]
    fn new_app_state_does_not_serialize_deleted_booleans() {
        let state = AppState::new();
        let json = serde_json::to_string(&state).expect("serializes");
        assert!(
            !json.contains("pipeline_available"),
            "the deleted `pipeline_available` field must NOT appear in the new snapshot; got: {json}"
        );
        assert!(
            !json.contains("subagents_enabled"),
            "the deleted `subagents_enabled` field must NOT appear in the new snapshot; got: {json}"
        );
    }

    #[test]
    fn test_truncate_str_normal() {
        let s = "hello world"; // 11 characters
        // max_len = 10: take (10-3)=7 chars = "hello w", add "..." = "hello w..." (10 chars)
        assert_eq!(truncate_str(s, 10), "hello w...");
        // max_len = 8: take (8-3)=5 chars = "hello", add "..." = "hello..." (8 chars)
        assert_eq!(truncate_str(s, 8), "hello...");
        // max_len = 5: take (5-3)=2 chars = "he", add "..." = "he..." (5 chars)
        assert_eq!(truncate_str(s, 5), "he...");
    }

    #[test]
    fn test_truncate_str_no_truncation_needed() {
        let s = "hi";
        assert_eq!(truncate_str(s, 10), "hi");
        assert_eq!(truncate_str(s, 2), "hi");
    }

    #[test]
    fn test_truncate_str_exact_length() {
        let s = "hello";
        assert_eq!(truncate_str(s, 5), "hello");
    }

    #[test]
    fn test_truncate_str_bug_max_len_less_than_3() {
        // When max_len < 3, the output should NOT exceed max_len
        let s = "hello world";

        // max_len = 2 should return at most 2 characters
        let result = truncate_str(s, 2);
        assert!(
            result.chars().count() <= 2,
            "truncate_str(s, 2) returned '{}' with {} chars, expected <= 2",
            result,
            result.chars().count()
        );

        // max_len = 1 should return at most 1 character
        let result = truncate_str(s, 1);
        assert!(
            result.chars().count() <= 1,
            "truncate_str(s, 1) returned '{}' with {} chars, expected <= 1",
            result,
            result.chars().count()
        );

        // max_len = 0 should return empty string
        let result = truncate_str(s, 0);
        assert!(
            result.chars().count() == 0,
            "truncate_str(s, 0) returned '{}' with {} chars, expected 0",
            result,
            result.chars().count()
        );
    }

    #[test]
    fn test_truncate_str_edge_cases() {
        // max_len = 3 should return at most 3 characters (no room for "...")
        let s = "hello world";
        let result = truncate_str(s, 3);
        assert!(
            result.chars().count() <= 3,
            "truncate_str(s, 3) returned '{}' with {} chars, expected <= 3",
            result,
            result.chars().count()
        );
    }

    #[test]
    fn test_truncate_line_bug_max_len_less_than_3() {
        // Same bug should exist in truncate_line
        let s = "hello world";

        let result = truncate_line(s, 2);
        assert!(
            result.chars().count() <= 2,
            "truncate_line(s, 2) returned '{}' with {} chars, expected <= 2",
            result,
            result.chars().count()
        );
    }

    // ══════════════════════════════════════════════════════════════════
    // T4: `ChatMessage.images`, ctor extraction, `format_tool_result`'s
    // `view_image` arm, and `elide_binary_payload`.
    //
    // Written BEFORE the implementation exists — every test below is
    // expected to fail to COMPILE today (missing field `images`, missing
    // fn `elide_binary_payload`, missing 3rd param on `format_tool_result`,
    // and `crate::tools::view_image` being private — the last one is a
    // real gap T4 must also close; see the delivery report).
    //
    // Isolation: no filesystem. `ImageRef`s are fabricated directly and
    // `view_image` tool output is built as a JSON string by hand (via
    // `ImageRef`'s own `Serialize` impl, so the wire shape can never
    // drift from `src/image_cache.rs`). `image_cache::spill` is never
    // called.
    // ══════════════════════════════════════════════════════════════════

    use crate::image_cache::ImageRef;
    use crate::tools::view_image;

    /// A well-formed (but never-spilled) `ImageRef`. The id satisfies
    /// `image_cache::path_for`'s grammar (`^[0-9a-f]{64}\.(png|jpg|jpeg|gif|webp)$`)
    /// without ever touching `image_cache::spill`, so no real file backs it.
    fn make_ref(display_name: &str) -> ImageRef {
        ImageRef {
            id: format!("{}.png", "ab".repeat(32)), // 64 hex chars + ext
            display_name: display_name.to_string(),
        }
    }

    /// Builds the exact wire JSON `ViewImageTool::call` produces, using
    /// `ImageRef`'s real `Serialize` impl so the shape can't drift from
    /// `src/image_cache.rs`. `image_ref` is omitted (not `null`) when `None`,
    /// mirroring `ViewImageOutput`'s `skip_serializing_if`.
    fn view_image_json(image_ref: Option<&ImageRef>, data: &str) -> String {
        let mut obj = serde_json::json!({
            "type": "image",
            "data": data,
            "mimeType": "image/png",
        });
        if let Some(r) = image_ref {
            obj["image_ref"] = serde_json::to_value(r).expect("ImageRef serializes");
        }
        obj.to_string()
    }

    /// A representative `bash` tool result, used as the regression-guard
    /// baseline throughout this section.
    fn bash_sample_result() -> &'static str {
        "Exit code: 0\nSTDOUT:\nhello world\nSTDERR:\n"
    }

    /// Today's (pre-T4) exact `content` for `ChatMessage::tool_result("bash",
    /// ..., bash_sample_result(), ...)`. Captured from the current
    /// `format_bash_result` before any T4 code exists — this is the
    /// regression-guard baseline for criterion 2.
    const BASH_CONTENT_BASELINE: &str = "✅ Exit 0 | hello world";

    /// Today's (pre-T4) exact serialization of a representative
    /// `ChatMessage::tool_result("bash", ...)` row with a fixed timestamp,
    /// captured from the current struct shape (no `images` field exists
    /// yet). This is the regression-guard baseline for criterion 8, and
    /// doubles as "old JSON with no images key" for criterion 10.
    ///
    /// The `timestamp` value is a `{ts}` hole rather than a literal:
    /// `DateTime<Local>` serializes with the *machine's* UTC offset, so a
    /// hardcoded offset would pin this baseline to a single timezone and
    /// fail everywhere else (a `TZ=UTC` CI runner included). Everything the
    /// baseline actually guards — the field set, the field order, and which
    /// keys are skipped when empty — stays literal.
    const BASH_MESSAGE_JSON_TEMPLATE: &str = r#"{"role":"toolresult","content":"✅ Exit 0 | hello world","timestamp":{ts},"tool_name":"bash","tool_args":"{\"command\":\"echo hi\"}","tool_result":"Exit code: 0\nSTDOUT:\nhello world\nSTDERR:\n","call_id":"call_1","compacted":false}"#;

    /// `BASH_MESSAGE_JSON_TEMPLATE` with the timestamp rendered in the
    /// machine's local offset — i.e. the bytes pre-T4 code would have
    /// written on *this* host.
    fn bash_message_json_baseline() -> String {
        let ts = serde_json::to_string(&fixed_local_ts()).expect("timestamp serializes");
        BASH_MESSAGE_JSON_TEMPLATE.replace("{ts}", &ts)
    }

    /// The fixed local timestamp baked into the baseline above.
    fn fixed_local_ts() -> DateTime<Local> {
        use chrono::NaiveDateTime;
        NaiveDateTime::parse_from_str("2024-01-01 00:00:00", "%Y-%m-%d %H:%M:%S")
            .unwrap()
            .and_local_timezone(Local)
            .unwrap()
    }

    /// Rebuilds, via the real `ChatMessage::tool_result` constructor, the
    /// exact message that produced `bash_message_json_baseline()`.
    fn bash_baseline_message() -> ChatMessage {
        let mut msg = ChatMessage::tool_result(
            "bash",
            r#"{"command":"echo hi"}"#,
            bash_sample_result(),
            Some("call_1".to_string()),
        );
        msg.timestamp = fixed_local_ts();
        msg
    }

    // ── format_tool_result: pure function-level tests ───────────────────
    // These call `format_tool_result(tool_name, result, images)` directly
    // (the 3-arg signature T4 introduces) so a failure here is
    // attributable to the formatting arm itself, independent of the
    // constructor's extraction step tested further below.

    /// Criterion 1 (function-level half): a `view_image` row with exactly
    /// one ref renders as exactly one line, `🖼 <display_name>`.
    #[test]
    fn format_tool_result_view_image_with_ref_returns_single_emoji_line() {
        let r = make_ref("shot.png");
        let content = format_tool_result(view_image::NAME, "irrelevant raw json", &[r]);
        assert_eq!(content, "🖼 shot.png");
        assert_eq!(content.lines().count(), 1, "must be exactly one line");
    }

    /// Item 3 of the spec ("If there is no ref, fall back to something
    /// sensible — decide and state what you assert"): DECISION — with no
    /// ref, the row still renders as one `🖼`-prefixed line using the
    /// generic label `"image"` (there is no display name to show; we do
    /// NOT fall back to the old three-line base64 dump, since that is
    /// exactly the regression this arm exists to fix).
    #[test]
    fn format_tool_result_view_image_with_no_refs_uses_generic_fallback_label() {
        let content = format_tool_result(view_image::NAME, "irrelevant raw json", &[]);
        assert_eq!(
            content, "🖼 image",
            "DECISION: no-ref fallback is the generic label \"image\" behind the same 🖼 prefix"
        );
        assert_eq!(content.lines().count(), 1, "must still be exactly one line");
    }

    /// The arm must derive its content from the `images` parameter, never
    /// by re-parsing `result` itself — that parse happens once, in the
    /// constructor's extraction step. A garbage `result` string must not
    /// change the outcome, and must not panic.
    #[test]
    fn format_tool_result_view_image_ignores_raw_result_string_uses_images_param() {
        let r = make_ref("shot.png");
        let content =
            format_tool_result(view_image::NAME, "{completely bogus, not json at all", &[r]);
        assert_eq!(content, "🖼 shot.png");
    }

    #[test]
    fn format_tool_result_view_image_unicode_display_name_line() {
        let name =
            "\u{30b9}\u{30af}\u{30ea}\u{30fc}\u{30f3}\u{30b7}\u{30e7}\u{30c3}\u{30c8} 001.png";
        let r = make_ref(name);
        let content = format_tool_result(view_image::NAME, "irrelevant", &[r]);
        assert_eq!(content, format!("🖼 {name}"));
    }

    /// Boundary: an empty display name is a degenerate but legal
    /// `ImageRef`. Pin the exact rendering rather than leaving it to
    /// implementer whim.
    #[test]
    fn format_tool_result_view_image_empty_display_name_line() {
        let r = make_ref("");
        let content = format_tool_result(view_image::NAME, "irrelevant", &[r]);
        assert_eq!(content, "🖼 ");
    }

    /// Regression guard at the function level: adding the 3rd `images`
    /// parameter must not change `bash`'s existing formatting.
    #[test]
    fn format_tool_result_bash_arm_unchanged_by_new_signature() {
        let content = format_tool_result("bash", bash_sample_result(), &[]);
        assert_eq!(content, BASH_CONTENT_BASELINE);
    }

    /// Smoke test: the other existing arms (`file_read`, `list_directory`,
    /// `web_search`, and the generic fallback) must keep working — not
    /// panic, not silently swallow the `images` parameter into their own
    /// output — after the signature grows a 3rd parameter.
    #[test]
    fn format_tool_result_other_arms_do_not_panic_with_empty_images_slice() {
        let _ = format_tool_result("file_read", "     1\thello\n", &[]);
        let _ = format_tool_result("list_directory", "total 0\n", &[]);
        let _ = format_tool_result("web_search", "[]", &[]);
        let _ = format_tool_result("some_unknown_tool", "anything at all", &[]);
    }

    // ── ChatMessage::tool_result: constructor-level tests ────────────────

    /// Criterion 1 (end-to-end): the constructor extracts the ref from a
    /// real `view_image` output JSON and derives `content` from it.
    #[test]
    fn tool_result_view_image_extracts_single_image_ref_and_emoji_content() {
        let r = make_ref("shot.png");
        let json = view_image_json(Some(&r), "AAAA");
        let msg = ChatMessage::tool_result(view_image::NAME, "{}", &json, None);

        assert_eq!(msg.images.len(), 1);
        assert_eq!(msg.images[0], r);
        assert_eq!(msg.content, "🖼 shot.png");
    }

    /// Criterion 2: `bash` extracts no images, and `content` is UNCHANGED
    /// from today's behaviour (the captured baseline).
    #[test]
    fn tool_result_bash_has_no_images_and_content_matches_today_baseline() {
        let msg = ChatMessage::tool_result(
            "bash",
            r#"{"command":"echo hi"}"#,
            bash_sample_result(),
            Some("call_1".to_string()),
        );
        assert!(msg.images.is_empty());
        assert_eq!(msg.content, BASH_CONTENT_BASELINE);
    }

    /// The extraction guard is `tool_name == view_image::NAME` — a `bash`
    /// row whose result happens to be a byte-identical `view_image`-shaped
    /// JSON object (carrying a real `image_ref`) must still extract NO
    /// images, because the tool name says it isn't a `view_image` result.
    #[test]
    fn tool_result_ignores_image_ref_shaped_json_for_non_view_image_tool_name() {
        let r = make_ref("shot.png");
        let json = view_image_json(Some(&r), "AAAA");
        let msg = ChatMessage::tool_result("bash", "{}", &json, None);
        assert!(
            msg.images.is_empty(),
            "a non-view_image tool_name must never extract an image, no matter the result shape"
        );
    }

    /// A `view_image` output with the `image_ref` field entirely absent
    /// (e.g. a spill failure) must not panic, must yield no images, and
    /// must still render as a single sensible line (this pins the same
    /// fallback decided above, reached through the real constructor).
    #[test]
    fn tool_result_view_image_with_missing_ref_field_has_empty_images_and_fallback_content() {
        let json = view_image_json(None, "AAAA");
        let msg = ChatMessage::tool_result(view_image::NAME, "{}", &json, None);
        assert!(msg.images.is_empty());
        assert_eq!(msg.content, "🖼 image");
    }

    /// A `view_image` "result" that isn't valid JSON at all (corrupt data,
    /// truncated persistence, a hand-edited transcript) must not panic and
    /// must degrade to zero images plus the same generic fallback.
    #[test]
    fn tool_result_view_image_malformed_json_result_has_empty_images_and_no_panic() {
        let msg = ChatMessage::tool_result(view_image::NAME, "{}", "not json at all {{{", None);
        assert!(msg.images.is_empty());
        assert_eq!(msg.content, "🖼 image");
    }

    // ── ChatMessage::elide_binary_payload ────────────────────────────────

    /// Criterion 3: a ~2 MB `view_image` row shrinks to a short stored
    /// notice, its `images` are byte-for-byte unchanged, and `content`
    /// stays the one-line emoji rendering.
    #[test]
    fn elide_binary_payload_shrinks_large_view_image_row_under_512_bytes_and_preserves_ref_and_content()
     {
        let r = make_ref("shot.png");
        let huge_data = "A".repeat(2_000_000);
        let json = view_image_json(Some(&r), &huge_data);
        let mut msg = ChatMessage::tool_result(view_image::NAME, "{}", &json, None);

        // Sanity: the row really did start out large.
        assert!(
            msg.tool_result.as_ref().unwrap().len() > 1_000_000,
            "test setup sanity: stored tool_result should start out multi-MB"
        );
        assert_eq!(msg.images, vec![r.clone()]);
        assert_eq!(msg.content, "🖼 shot.png");

        msg.elide_binary_payload();

        assert!(
            msg.tool_result.as_ref().is_some_and(|s| s.len() < 512),
            "tool_result must shrink to < 512 bytes after elision, got {:?} bytes",
            msg.tool_result.as_ref().map(|s| s.len())
        );
        assert_eq!(
            msg.images,
            vec![r],
            "images must survive elision untouched (W2)"
        );
        assert_eq!(
            msg.content, "🖼 shot.png",
            "content must still be the one-line emoji rendering after elision"
        );
    }

    /// Criterion 4: eliding a `bash` row is a complete no-op — byte-equal
    /// serialization before and after.
    #[test]
    fn elide_binary_payload_leaves_bash_row_byte_equal() {
        let mut msg = bash_baseline_message();
        let before = serde_json::to_string(&msg).expect("serialize before");
        msg.elide_binary_payload();
        let after = serde_json::to_string(&msg).expect("serialize after");
        assert_eq!(
            before, after,
            "eliding a non-view_image row must be a pure no-op"
        );
    }

    /// Criterion 5: a row with `tool_result: None` (e.g. a `ToolCall` row,
    /// even one tagged with the `view_image` tool name) must be untouched
    /// and must not panic.
    #[test]
    fn elide_binary_payload_on_none_tool_result_is_noop_and_does_not_panic() {
        let mut msg = ChatMessage::tool_call(view_image::NAME, "{}", None);
        assert!(msg.tool_result.is_none(), "test setup sanity");
        let before = serde_json::to_string(&msg).expect("serialize before");
        msg.elide_binary_payload();
        let after = serde_json::to_string(&msg).expect("serialize after");
        assert_eq!(before, after, "a row with no tool_result must be untouched");
    }

    /// Criterion 6: eliding twice is byte-equal to eliding once — no
    /// growth, no double-wrapping of the notice.
    ///
    /// This is `f(f(x)) == f(x)` applied to a *single* instance: elide once,
    /// snapshot the serialization, elide the same instance again, and
    /// compare. Using one instance (rather than constructing two separate
    /// `ChatMessage`s) means there is exactly one `timestamp` value in play
    /// by construction — the test can't flake on `Local::now()` skew
    /// between two independent stamps.
    #[test]
    fn elide_binary_payload_is_a_byte_equal_fixpoint_when_applied_twice() {
        let r = make_ref("shot.png");
        let huge_data = "A".repeat(2_000_000);
        let json = view_image_json(Some(&r), &huge_data);

        let mut msg = ChatMessage::tool_result(view_image::NAME, "{}", &json, None);
        msg.elide_binary_payload();
        let once = serde_json::to_string(&msg).expect("serialize after first elision");

        msg.elide_binary_payload();
        let twice = serde_json::to_string(&msg).expect("serialize after second elision");

        assert_eq!(
            once, twice,
            "eliding twice must be byte-equal to eliding once"
        );
    }

    /// Criterion 7 + the "< 512 bytes" load-bearing requirement together:
    /// the notice mentions the display name and stays well under 512
    /// bytes.
    #[test]
    fn elide_binary_payload_notice_mentions_display_name_and_is_short() {
        let r = make_ref("shot.png");
        let huge_data = "A".repeat(2_000_000);
        let json = view_image_json(Some(&r), &huge_data);
        let mut msg = ChatMessage::tool_result(view_image::NAME, "{}", &json, None);

        msg.elide_binary_payload();

        let notice = msg
            .tool_result
            .expect("tool_result must remain Some after elision");
        assert!(
            notice.contains("shot.png"),
            "elision notice must mention the display name; got: {notice}"
        );
        assert!(
            notice.len() < 512,
            "elision notice must be < 512 bytes, was {} bytes",
            notice.len()
        );
    }

    /// The "< 512 bytes" bound must hold for a display name the model
    /// controls: `view_image` falls back to the entire `args.path` when the
    /// path has no file name, and multi-byte characters make a
    /// character-count cap alone insufficient. A 4 KB name of 4-byte
    /// characters is the worst realistic case.
    #[test]
    fn elide_binary_payload_notice_stays_short_for_a_pathological_display_name() {
        let r = ImageRef {
            id: format!("{}.png", "ab".repeat(32)),
            display_name: "\u{1f600}".repeat(1_000), // 4 KB of 4-byte chars
        };
        let json = view_image_json(Some(&r), &"A".repeat(50_000));
        let mut msg = ChatMessage::tool_result(view_image::NAME, "{}", &json, None);

        msg.elide_binary_payload();

        let notice = msg
            .tool_result
            .expect("tool_result must remain Some after elision");
        assert!(
            notice.len() < 512,
            "notice must stay bounded regardless of display name length, was {} bytes",
            notice.len()
        );
        assert_eq!(
            msg.images,
            vec![r],
            "truncation is display-only — the stored ref keeps the full name"
        );
    }

    /// Criterion 11: elision must be pure — no filesystem, no spill-cache
    /// consultation. Proven by fabricating a ref whose id is well-formed
    /// (passes `image_cache::path_for`'s grammar) but backed by NO real
    /// spilled file (never went through `image_cache::spill`). Elision
    /// must still succeed and preserve the ref unchanged.
    #[test]
    fn elide_binary_payload_never_touches_filesystem_for_ref_with_no_spilled_file() {
        // Grammar-valid (64 hex chars + known ext) but content-address of
        // nothing this test ever wrote to the spill cache.
        let bogus_id = format!("{}.png", "0".repeat(64));
        let r = ImageRef {
            id: bogus_id.clone(),
            display_name: "phantom.png".to_string(),
        };
        let data = "A".repeat(50_000);
        let json = view_image_json(Some(&r), &data);
        let mut msg = ChatMessage::tool_result(view_image::NAME, "{}", &json, None);

        msg.elide_binary_payload();

        assert_eq!(
            msg.images,
            vec![r],
            "elision must preserve a ref whose backing file does not exist on disk"
        );
        assert!(msg.tool_result.as_ref().is_some_and(|s| s.len() < 512));
        assert_eq!(msg.content, "🖼 phantom.png");
    }

    /// Boundary: zero-length base64 `data`. Must elide cleanly (no
    /// division-by-zero-style edge bugs in whatever size math the notice
    /// computes) and must not panic.
    #[test]
    fn elide_binary_payload_on_zero_length_data_view_image_row_is_noop_safe() {
        let r = make_ref("empty.png");
        let json = view_image_json(Some(&r), "");
        let mut msg = ChatMessage::tool_result(view_image::NAME, "{}", &json, None);

        msg.elide_binary_payload();

        assert_eq!(msg.images, vec![r]);
        assert!(msg.tool_result.as_ref().is_some_and(|s| s.len() < 512));
    }

    // ── Serde compatibility ───────────────────────────────────────────────

    /// Criterion 8: with `images` empty, serialization is BYTE-IDENTICAL
    /// to today's pre-T4 output — no `"images":[]` key must appear.
    #[test]
    fn chat_message_with_empty_images_serializes_byte_identical_to_pre_t4_baseline() {
        let msg = bash_baseline_message();
        assert!(msg.images.is_empty(), "test setup sanity");
        let json = serde_json::to_string(&msg).expect("serialize");
        assert_eq!(json, bash_message_json_baseline());
        assert!(
            !json.contains("images"),
            "the images key must be entirely absent when empty; got: {json}"
        );
    }

    /// Criterion 9: a message with a non-empty `images` round-trips
    /// through serialize → deserialize, preserving every ref.
    #[test]
    fn chat_message_with_images_round_trips_through_serde() {
        let r = make_ref("shot.png");
        let json_payload = view_image_json(Some(&r), "AAAA");
        let msg = ChatMessage::tool_result(view_image::NAME, "{}", &json_payload, None);
        assert_eq!(msg.images, vec![r.clone()], "test setup sanity");

        let wire = serde_json::to_string(&msg).expect("serialize");
        let back: ChatMessage = serde_json::from_str(&wire).expect("deserialize");

        assert_eq!(back.images, vec![r]);
        assert_eq!(back.content, msg.content);
    }

    /// Criterion 10: an OLD `ChatMessage` JSON blob — no `images` key at
    /// all, exactly what pre-T4 code persisted — must still deserialize,
    /// yielding `images == []` via `#[serde(default)]`.
    #[test]
    fn deserializing_pre_t4_json_without_images_key_yields_empty_images() {
        let baseline = bash_message_json_baseline();
        assert!(
            !baseline.contains("images"),
            "sanity: baseline fixture must not itself contain an images key"
        );
        let parsed: ChatMessage =
            serde_json::from_str(&baseline).expect("old JSON must still parse");
        assert!(parsed.images.is_empty());
    }
}
