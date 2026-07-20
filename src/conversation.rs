//! Conversation persistence - data structures for storing conversation history.

use crate::ui::app_state::MessageSource;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Metadata about a conversation (stats, etc.)
///
/// Token + cost fields use `#[serde(default)]` so conversations saved before
/// stats persistence existed (only `message_count`) still deserialize cleanly
/// — they just come back with zeros, which is the right answer for them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMetadata {
    /// Number of messages in the conversation
    #[serde(default)]
    pub message_count: usize,
    /// Input tokens from the last request (mirrors SessionStats — overwritten,
    /// not accumulated; doubles as the live context-size indicator on resume)
    #[serde(default)]
    pub total_input_tokens: u64,
    /// Output tokens from the last request (mirrors SessionStats — overwritten)
    #[serde(default)]
    pub total_output_tokens: u64,
    /// Cumulative number of API calls across the conversation
    #[serde(default)]
    pub total_api_calls: u64,
    /// Cumulative cost in USD across the conversation
    #[serde(default)]
    pub total_cost: f64,
    /// Per-lane stats snapshot (orchestrator + sub-agent roles). `#[serde(default)]`
    /// so pre-pipeline files load with an empty breakdown — the right answer for
    /// them. Mirrors the flat totals' overwrite/accumulate split per lane.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lanes: Vec<LaneMetadata>,
}

/// One persisted per-lane stats bucket. Serializable mirror of the hooks'
/// `LaneStats`, kept in the metadata layer so persistence has no dependency on
/// the non-serde runtime type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneMetadata {
    pub lane: String,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub api_calls: u64,
    #[serde(default)]
    pub cost: f64,
}

impl Default for ConversationMetadata {
    fn default() -> Self {
        Self {
            message_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_api_calls: 0,
            total_cost: 0.0,
            lanes: Vec::new(),
        }
    }
}

/// A message in the conversation history.
///
/// `compacted` marks a message that compaction hid from the LLM in a
/// prior session; persisted so a reload restores the compacted state
/// instead of resurrecting the full history (#59). `serde(default)`
/// keeps pre-compaction files loading as `false`.
///
/// `source` records the producing lane (orchestrator vs a pipeline
/// `SubAgent { role }`, or a `bash_bg` `Background` turn). It mirrors
/// `ChatMessage::source` byte-for-byte: `#[serde(default,
/// skip_serializing_if = "MessageSource::is_human")]` so every
/// pre-lane file loads as `Human` and Human turns stay byte-identical
/// on disk. `Summary` carries no source — a compaction summary is
/// always an orchestrator artefact.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum Message {
    /// User message
    User {
        /// Message content
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        compacted: bool,
        #[serde(default, skip_serializing_if = "MessageSource::is_human")]
        source: MessageSource,
        /// Timestamp when message was sent
        timestamp: DateTime<Utc>,
    },
    /// Assistant (AI) message
    Assistant {
        /// Message content
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        compacted: bool,
        #[serde(default, skip_serializing_if = "MessageSource::is_human")]
        source: MessageSource,
        /// Timestamp when message was generated
        timestamp: DateTime<Utc>,
    },
    /// Tool invocation (captures what the model asked the tool to do)
    ToolCall {
        /// Name of the tool being called
        tool_name: String,
        /// Arguments passed to the tool (JSON string)
        arguments: String,
        /// Tool call ID for correlating calls with results
        #[serde(skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        compacted: bool,
        #[serde(default, skip_serializing_if = "MessageSource::is_human")]
        source: MessageSource,
        /// Timestamp when tool was called
        timestamp: DateTime<Utc>,
    },
    /// Tool execution result
    ToolResult {
        /// Name of the tool that was executed
        tool_name: String,
        /// Arguments that were passed to the tool
        arguments: String,
        /// Tool execution result (or error message)
        result: String,
        /// Tool call ID for correlating calls with results
        #[serde(skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        compacted: bool,
        #[serde(default, skip_serializing_if = "MessageSource::is_human")]
        source: MessageSource,
        /// Timestamp when tool was executed
        timestamp: DateTime<Utc>,
    },
    /// Compaction summary inserted at the boundary; the live stand-in for
    /// the hidden region, never `compacted` itself.
    Summary {
        /// The summary text produced by the compaction model
        content: String,
        /// Timestamp when the summary was generated
        timestamp: DateTime<Utc>,
    },
}

impl Message {
    /// Create a new user message
    pub fn user(content: String) -> Self {
        Message::User {
            content,
            compacted: false,
            source: MessageSource::Human,
            timestamp: Utc::now(),
        }
    }

    /// Create a new assistant message
    pub fn assistant(content: String) -> Self {
        Message::Assistant {
            content,
            compacted: false,
            source: MessageSource::Human,
            timestamp: Utc::now(),
        }
    }

    /// Create a new tool call message
    pub fn tool_call(tool_name: String, arguments: String, call_id: Option<String>) -> Self {
        Message::ToolCall {
            tool_name,
            arguments,
            call_id,
            compacted: false,
            source: MessageSource::Human,
            timestamp: Utc::now(),
        }
    }

    /// Create a new tool result message
    pub fn tool_result(
        tool_name: String,
        arguments: String,
        result: String,
        call_id: Option<String>,
    ) -> Self {
        Message::ToolResult {
            tool_name,
            arguments,
            result,
            call_id,
            compacted: false,
            source: MessageSource::Human,
            timestamp: Utc::now(),
        }
    }

    /// Get the content of the message
    pub fn content(&self) -> &str {
        match self {
            Message::User { content, .. } => content,
            Message::Assistant { content, .. } => content,
            Message::Summary { content, .. } => content,
            Message::ToolResult { result, .. } => result,
            Message::ToolCall { .. } => "",
        }
    }
}

/// A complete conversation with metadata.
///
/// **Stable identity:** `(provider_name, model)` is the persisted
/// re-activation key for `/load`. Aliases are mutable user handles in
/// `config.yaml` and are deliberately NOT stored — the wire identity
/// is what was actually sent to the API and is what survives config
/// renames. See [`crate::config::ModelRegistry::find_by_wire_id`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    /// Unique identifier for this conversation
    pub id: Uuid,
    /// Human-readable name for the conversation (set at creation time)
    pub name: String,
    /// Auto-generated short title for the conversation (set after first response).
    /// Displayed in /conversations listing. `None` until generated.
    #[serde(default)]
    pub title: Option<String>,
    /// When the conversation was created
    pub created_at: DateTime<Utc>,
    /// When the conversation was last updated
    pub updated_at: DateTime<Utc>,
    /// Message history
    pub messages: Vec<Message>,
    /// Provider name (informational handle from the providers list,
    /// e.g. `"openrouter"`, `"patchnotes"`). Together with `model`,
    /// forms the wire-id pair used by `/load` to re-activate the
    /// model. Defaults to empty for pre-v5 files; those then fail
    /// `find_by_wire_id` and surface as `⚠ unavailable`.
    #[serde(default)]
    pub provider_name: String,
    /// Wire id of the model used (e.g. `anthropic/claude-3.7-sonnet`).
    pub model: String,
    /// Working directory this conversation was rooted in. A third axis
    /// of conversation identity alongside `(provider_name, model)`:
    /// `/cd` rewrites it, `/load` re-activates it (best-effort — a path
    /// that no longer exists is skipped with a warning). Defaults to
    /// empty for pre-cwd files, which then simply don't chdir on load.
    #[serde(default)]
    pub cwd: String,
    /// Additional metadata (token count, cost, etc.)
    pub metadata: ConversationMetadata,
    /// Todo list persisted with this conversation
    #[serde(default)]
    pub todos: crate::tools::todo::TodoList,
    /// Whether the multi-agent pipeline was enabled for the session that
    /// created this conversation (config sense: `pipeline.enabled`, a
    /// boot-only fact). Conversation-global, not per-message. Defaults to
    /// `false` for every pre-existing file — old conversations genuinely had
    /// no pipeline.
    #[serde(default)]
    pub pipeline_enabled: bool,
}

impl Conversation {
    /// Create a new conversation with the given name, the active model's
    /// full wire identity, and the directory this conversation is rooted in.
    ///
    /// Both `provider_name` and `model` are required: together they form
    /// the stable key `/load` uses to re-activate the right model. The
    /// alias the user sees is *not* persisted — it's UI sugar that can
    /// be renamed in `config.yaml` without breaking saved conversations.
    ///
    /// `cwd` is the directory tree this conversation is bound to. It is
    /// the caller's responsibility to pass the value the session will
    /// actually operate in (the per-session `session_cwd`, or for `/cd`
    /// the new target). The conversation is persisted 1:1 with this path
    /// — `/load` reapplies it as the session's working tree.
    pub fn new(name: String, provider_name: String, model: String, cwd: String) -> Self {
        let now = Utc::now();
        Conversation {
            id: Uuid::new_v4(),
            name,
            title: None,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            provider_name,
            model,
            cwd,
            metadata: ConversationMetadata::default(),
            todos: crate::tools::todo::TodoList::new(),
            pipeline_enabled: false,
        }
    }

    /// Add a user message to the conversation
    pub fn add_user_message(&mut self, content: String) {
        self.messages.push(Message::user(content));
        self.metadata.message_count = self.messages.len();
        self.updated_at = Utc::now();
    }

    /// Add an assistant message to the conversation
    pub fn add_assistant_message(&mut self, content: String) {
        self.messages.push(Message::assistant(content));
        self.metadata.message_count = self.messages.len();
        self.updated_at = Utc::now();
    }

    /// Add a tool call to the conversation
    pub fn add_tool_call(&mut self, tool_name: String, arguments: String, call_id: Option<String>) {
        self.messages
            .push(Message::tool_call(tool_name, arguments, call_id));
        self.metadata.message_count = self.messages.len();
        self.updated_at = Utc::now();
    }

    /// Add a tool result to the conversation
    pub fn add_tool_result(
        &mut self,
        tool_name: String,
        arguments: String,
        result: String,
        call_id: Option<String>,
    ) {
        self.messages
            .push(Message::tool_result(tool_name, arguments, result, call_id));
        self.metadata.message_count = self.messages.len();
        self.updated_at = Utc::now();
    }

    /// Rename the conversation
    pub fn rename(&mut self, name: String) {
        self.name = name;
        self.updated_at = Utc::now();
    }

    /// Set the auto-generated title (truncated to 80 chars).
    /// Silently ignores titles longer than 80 chars.
    /// Idempotent: calling with a new title when one already exists is a no-op.
    pub fn set_title(&mut self, title: String) {
        if self.title.is_some() {
            // Title already generated — skip
            return;
        }
        if title.len() <= 80 {
            self.title = Some(title);
            self.updated_at = Utc::now();
        }
    }

    /// Whether the title has been generated (short-circuit check before LLM call).
    pub fn has_title(&self) -> bool {
        self.title.is_some()
    }
}

/// Summary of a conversation (for listing)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSummary {
    /// Unique identifier
    pub id: Uuid,
    /// Display name (title if generated, otherwise the creation timestamp name).
    /// Used for rendering in /conversations listing.
    pub name: String,
    /// Auto-generated short title. `None` until first response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Creation timestamp
    pub created_at: DateTime<Utc>,
    /// When the conversation was last updated
    pub updated_at: DateTime<Utc>,
    /// Number of messages
    pub message_count: usize,
    /// Provider name (informational handle, e.g. `"openrouter"`).
    /// Pre-v5 files default to empty; rendered as `(unknown)` in
    /// `/conversations` listings.
    #[serde(default)]
    pub provider_name: String,
    /// Wire id of the model used (e.g. `anthropic/claude-3.7-sonnet`).
    pub model: String,
    /// Whether the pipeline was enabled for the session that created this
    /// conversation. Surfaced to any summary consumer (a picker badge is a
    /// deferred frontend concern). Defaults to `false` for pre-existing files.
    #[serde(default)]
    pub pipeline_enabled: bool,
}

impl From<&Conversation> for ConversationSummary {
    fn from(conv: &Conversation) -> Self {
        ConversationSummary {
            id: conv.id,
            // Show title if generated, otherwise fall back to the creation name
            name: conv.title.clone().unwrap_or_else(|| conv.name.clone()),
            title: conv.title.clone(),
            created_at: conv.created_at,
            updated_at: conv.updated_at,
            message_count: conv.metadata.message_count,
            provider_name: conv.provider_name.clone(),
            model: conv.model.clone(),
            pipeline_enabled: conv.pipeline_enabled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conversation_creation() {
        let conv = Conversation::new(
            "Test".to_string(),
            "openrouter".to_string(),
            "claude-3".to_string(),
            String::new(),
        );
        assert_eq!(conv.name, "Test");
        assert_eq!(conv.provider_name, "openrouter");
        assert_eq!(conv.model, "claude-3");
        assert!(conv.messages.is_empty());
    }

    #[test]
    fn test_add_messages() {
        let mut conv = Conversation::new(
            "Test".to_string(),
            "openrouter".to_string(),
            "claude-3".to_string(),
            String::new(),
        );

        conv.add_user_message("Hello".to_string());
        assert_eq!(conv.messages.len(), 1);

        conv.add_assistant_message("Hi there!".to_string());
        assert_eq!(conv.messages.len(), 2);

        conv.add_tool_result(
            "bash".to_string(),
            r#"{"command": "ls"}"#.to_string(),
            "output".to_string(),
            Some("call_1".to_string()),
        );
        assert_eq!(conv.messages.len(), 3);
        assert_eq!(conv.metadata.message_count, 3);

        // Test adding a tool call
        conv.add_tool_call(
            "file_read".to_string(),
            r#"{"path": "/test/file.txt"}"#.to_string(),
            Some("call_2".to_string()),
        );
        assert_eq!(conv.messages.len(), 4);
    }

    #[test]
    fn test_serialization() {
        let mut conv = Conversation::new(
            "Test".to_string(),
            "openrouter".to_string(),
            "claude-3".to_string(),
            String::new(),
        );
        conv.add_user_message("Hello".to_string());
        conv.add_assistant_message("Hi!".to_string());

        let json = serde_json::to_string(&conv).unwrap();
        let loaded: Conversation = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.id, conv.id);
        assert_eq!(loaded.name, conv.name);
        assert_eq!(loaded.provider_name, "openrouter");
        assert_eq!(loaded.messages.len(), 2);
    }

    #[test]
    fn test_serialization_with_tool_calls() {
        let mut conv = Conversation::new(
            "Test".to_string(),
            "openrouter".to_string(),
            "claude-3".to_string(),
            String::new(),
        );
        conv.add_user_message("List files".to_string());
        conv.add_tool_call(
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            Some("call_1".to_string()),
        );
        conv.add_tool_result(
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            "file1.txt\nfile2.txt".to_string(),
            Some("call_1".to_string()),
        );
        conv.add_assistant_message("Here are the files.".to_string());

        let json = serde_json::to_string_pretty(&conv).unwrap();
        let loaded: Conversation = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.messages.len(), 4);

        // Verify tool call roundtrip
        match &loaded.messages[1] {
            Message::ToolCall {
                tool_name,
                arguments,
                call_id,
                ..
            } => {
                assert_eq!(tool_name, "bash");
                assert_eq!(arguments, r#"{"command":"ls"}"#);
                assert_eq!(call_id.as_deref(), Some("call_1"));
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }

        // Verify tool result roundtrip
        match &loaded.messages[2] {
            Message::ToolResult {
                tool_name,
                arguments,
                result,
                call_id,
                ..
            } => {
                assert_eq!(tool_name, "bash");
                assert_eq!(arguments, r#"{"command":"ls"}"#);
                assert_eq!(result, "file1.txt\nfile2.txt");
                assert_eq!(call_id.as_deref(), Some("call_1"));
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }

    /// Verify backward compatibility: JSON without call_id fields still deserializes
    #[test]
    fn test_backward_compat_no_call_id() {
        let json = r#"{
            "id": "10da8b9d-f242-4786-9c75-c3fbc2530f1f",
            "name": "Test",
            "created_at": "2026-04-14T09:14:07Z",
            "updated_at": "2026-04-14T09:15:41Z",
            "messages": [
                {"role": "ToolCall", "tool_name": "bash", "arguments": "{}", "timestamp": "2026-04-14T09:15:41Z"},
                {"role": "ToolResult", "tool_name": "bash", "arguments": "{}", "result": "ok", "timestamp": "2026-04-14T09:15:41Z"}
            ],
            "model": "test",
            "metadata": {"message_count": 2}
        }"#;

        let conv: Conversation = serde_json::from_str(json).unwrap();
        assert_eq!(conv.messages.len(), 2);

        match &conv.messages[0] {
            Message::ToolCall { call_id, .. } => assert!(call_id.is_none()),
            other => panic!("Expected ToolCall, got {:?}", other),
        }
        match &conv.messages[1] {
            Message::ToolResult { call_id, .. } => assert!(call_id.is_none()),
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }

    // === v5: wire-id (provider_name, model) persistence ===============

    /// Pre-v5 conversation files don't have `provider_name` in their
    /// top-level fields. Loading them must default `provider_name` to
    /// the empty string — `/load` then fails the wire-id lookup with
    /// the canonical `Model 'x/y' not available.` diagnostic rather
    /// than guessing a model.
    #[test]
    fn pre_v5_file_without_provider_name_loads_with_empty_provider() {
        let json = r#"{
            "id": "10da8b9d-f242-4786-9c75-c3fbc2530f1f",
            "name": "Old convo",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "messages": [],
            "model": "anthropic/claude-3.7-sonnet",
            "metadata": {"message_count": 0}
        }"#;
        let conv: Conversation = serde_json::from_str(json).unwrap();
        assert_eq!(conv.provider_name, "");
        assert_eq!(conv.model, "anthropic/claude-3.7-sonnet");
    }

    /// Pre-cwd conversation files have no `cwd` field. Loading them
    /// must default it to the empty string — `/load` then simply skips
    /// the chdir and stays in the current tree rather than failing.
    #[test]
    fn pre_cwd_file_without_cwd_loads_with_empty_cwd() {
        let json = r#"{
            "id": "10da8b9d-f242-4786-9c75-c3fbc2530f1f",
            "name": "Old convo",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "messages": [],
            "provider_name": "openrouter",
            "model": "anthropic/claude-3.7-sonnet",
            "metadata": {"message_count": 0}
        }"#;
        let conv: Conversation = serde_json::from_str(json).unwrap();
        assert_eq!(conv.cwd, "");
    }

    /// `Conversation::new` persists the caller's `cwd` argument
    /// (`cwd` is an explicit constructor parameter, not an implicit
    /// `std::env::current_dir()` read). The persisted value round-trips
    /// through serde without loss, and a value different from the
    /// process cwd is preserved verbatim — proving the constructor does
    /// not silently re-read the process cwd.
    #[test]
    fn new_conversation_persists_cwd() {
        let explicit = "/this/path/was/passed/explicitly/by/the/caller";
        let conv = Conversation::new(
            "Test".into(),
            "openrouter".into(),
            "m".into(),
            explicit.into(),
        );
        assert_eq!(conv.cwd, explicit);

        let json = serde_json::to_string(&conv).unwrap();
        let parsed: Conversation = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.cwd, explicit);
    }

    /// New conversations carry both `provider_name` and `model` —
    /// the wire-id pair `/load` uses for re-activation. Round-trip
    /// through serde without loss.
    #[test]
    fn new_conversation_persists_provider_name_and_model() {
        let conv = Conversation::new(
            "Test".into(),
            "openrouter".into(),
            "anthropic/claude-3.7-sonnet".into(),
            String::new(),
        );
        let json = serde_json::to_string(&conv).unwrap();
        let parsed: Conversation = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.provider_name, "openrouter");
        assert_eq!(parsed.model, "anthropic/claude-3.7-sonnet");
    }

    /// The serialized JSON must NOT contain a `model_alias` field —
    /// aliases are mutable and don't belong on disk. Negative test
    /// guarding against accidental re-introduction.
    #[test]
    fn serialized_metadata_has_no_model_alias_field() {
        let conv = Conversation::new(
            "Test".into(),
            "openrouter".into(),
            "anthropic/claude-3.7-sonnet".into(),
            String::new(),
        );
        let json = serde_json::to_string(&conv).unwrap();
        assert!(
            !json.contains("model_alias"),
            "alias must not be persisted; got: {json}"
        );
    }

    // ── conversation-global pipeline_enabled marker ────────────────────────

    #[test]
    fn pipeline_enabled_roundtrips_and_defaults_false() {
        let mut conv = Conversation::new(
            "Test".into(),
            "openrouter".into(),
            "anthropic/claude-3.7-sonnet".into(),
            String::new(),
        );
        // Fresh conversations default to false.
        assert!(!conv.pipeline_enabled);

        // A pipeline conversation roundtrips true.
        conv.pipeline_enabled = true;
        let json = serde_json::to_string(&conv).unwrap();
        let parsed: Conversation = serde_json::from_str(&json).unwrap();
        assert!(parsed.pipeline_enabled);
        // Summary carries the fact for downstream consumers.
        assert!(ConversationSummary::from(&parsed).pipeline_enabled);

        // A non-pipeline conversation roundtrips false.
        conv.pipeline_enabled = false;
        let json = serde_json::to_string(&conv).unwrap();
        let parsed: Conversation = serde_json::from_str(&json).unwrap();
        assert!(!parsed.pipeline_enabled);
        assert!(!ConversationSummary::from(&parsed).pipeline_enabled);
    }

    // ── per-lane stats metadata ────────────────────────────────────────────

    #[test]
    fn metadata_lanes_roundtrip_and_default_empty() {
        // Fresh metadata has no lanes and omits the field from JSON entirely.
        let empty = ConversationMetadata::default();
        assert!(empty.lanes.is_empty());
        let json = serde_json::to_string(&empty).unwrap();
        assert!(
            !json.contains("lanes"),
            "empty lanes must be skipped from JSON; got: {json}"
        );

        // Populated lanes survive a round-trip.
        let meta = ConversationMetadata {
            lanes: vec![
                LaneMetadata {
                    lane: "orchestrator".into(),
                    input_tokens: 100,
                    output_tokens: 20,
                    api_calls: 3,
                    cost: 0.01,
                },
                LaneMetadata {
                    lane: "reviewer".into(),
                    input_tokens: 500,
                    output_tokens: 40,
                    api_calls: 5,
                    cost: 0.05,
                },
            ],
            ..Default::default()
        };
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: ConversationMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.lanes.len(), 2);
        assert_eq!(parsed.lanes[1].lane, "reviewer");
        assert_eq!(parsed.lanes[1].input_tokens, 500);
        assert_eq!(parsed.lanes[1].api_calls, 5);

        // A JSON with no `lanes` key loads with an empty breakdown.
        let legacy = r#"{"message_count":1,"total_input_tokens":10}"#;
        let parsed: ConversationMetadata = serde_json::from_str(legacy).unwrap();
        assert!(parsed.lanes.is_empty());
    }

    #[test]
    fn pre_existing_file_without_field_defaults_false() {
        // A JSON that has no `pipeline_enabled` key at all; serde default
        // must fill in `false` (least astonishment — old convos had no pipeline).
        let json = r#"{
            "id": "00000000-0000-0000-0000-000000000000",
            "name": "old",
            "created_at": "2020-01-01T00:00:00Z",
            "updated_at": "2020-01-01T00:00:00Z",
            "messages": [],
            "model": "anthropic/claude-3.7-sonnet",
            "metadata": {}
        }"#;
        let parsed: Conversation = serde_json::from_str(json).unwrap();
        assert!(!parsed.pipeline_enabled);
    }

    // === v5: conversation title ====================

    /// Conversation starts with no title.
    #[test]
    fn new_conversation_has_no_title() {
        let conv = Conversation::new(
            "Conversation 2026-05-18".into(),
            "openrouter".into(),
            "claude-3.7-sonnet".into(),
            String::new(),
        );
        assert!(conv.title.is_none());
        assert!(!conv.has_title());
    }

    /// set_title stores the title and updates updated_at.
    #[test]
    fn set_title_stores_title() {
        let mut conv = Conversation::new(
            "Conversation 2026-05-18".into(),
            "openrouter".into(),
            "claude-3.7-sonnet".into(),
            String::new(),
        );
        let before = conv.updated_at;
        conv.set_title("Fix bug in auth".into());
        assert_eq!(conv.title.as_deref(), Some("Fix bug in auth"));
        assert!(conv.has_title());
        assert!(conv.updated_at >= before);
    }

    /// set_title is idempotent — second call is a no-op.
    #[test]
    fn set_title_is_idempotent() {
        let mut conv = Conversation::new(
            "Conversation 2026-05-18".into(),
            "openrouter".into(),
            "claude-3.7-sonnet".into(),
            String::new(),
        );
        conv.set_title("First title".into());
        conv.set_title("Second title".into());
        assert_eq!(conv.title.as_deref(), Some("First title"));
    }

    /// set_title silently ignores titles longer than 80 chars.
    #[test]
    fn set_title_ignores_long_titles() {
        let mut conv = Conversation::new(
            "Conversation 2026-05-18".into(),
            "openrouter".into(),
            "claude-3.7-sonnet".into(),
            String::new(),
        );
        conv.set_title("A".repeat(200));
        assert!(conv.title.is_none());
    }

    /// ConversationSummary.from uses title for name, falls back to conv.name.
    #[test]
    fn conversation_summary_shows_title_when_present() {
        let mut conv = Conversation::new(
            "Conversation 2026-05-18 10:00".into(),
            "openrouter".into(),
            "claude-3.7-sonnet".into(),
            String::new(),
        );
        conv.set_title("Fix sudo bug".into());
        let summary = ConversationSummary::from(&conv);
        assert_eq!(summary.name, "Fix sudo bug");
        assert_eq!(summary.title.as_deref(), Some("Fix sudo bug"));
    }

    /// ConversationSummary.from falls back to conv.name when title is absent.
    #[test]
    fn conversation_summary_falls_back_to_name() {
        let conv = Conversation::new(
            "Conversation 2026-05-18 10:00".into(),
            "openrouter".into(),
            "claude-3.7-sonnet".into(),
            String::new(),
        );
        let summary = ConversationSummary::from(&conv);
        assert_eq!(summary.name, "Conversation 2026-05-18 10:00");
        assert!(summary.title.is_none());
    }

    /// Title round-trips through serde (new conversation → JSON → loaded).
    #[test]
    fn title_roundtrips_through_json() {
        let mut conv = Conversation::new(
            "Test".into(),
            "openrouter".into(),
            "claude-3.7-sonnet".into(),
            String::new(),
        );
        conv.set_title("Rust async patterns".into());
        let json = serde_json::to_string(&conv).unwrap();
        let loaded: Conversation = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.title.as_deref(), Some("Rust async patterns"));
    }

    /// Pre-v5 conversation JSON (no title field) deserializes cleanly.
    #[test]
    fn pre_v5_conversation_without_title_deserializes() {
        let json = r#"{
            "id": "10da8b9d-f242-4786-9c75-c3fbc2530f1f",
            "name": "Old convo",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "messages": [],
            "model": "anthropic/claude-3.7-sonnet",
            "metadata": {"message_count": 0}
        }"#;
        let conv: Conversation = serde_json::from_str(json).unwrap();
        assert!(conv.title.is_none());
        assert_eq!(conv.name, "Old convo");
    }

    // === issue #59: compaction persistence ============================

    /// The `compacted` flag and `Summary` variant round-trip through serde.
    #[test]
    fn compacted_flag_and_summary_roundtrip() {
        let mut conv = Conversation::new("t".into(), "prov".into(), "model".into(), String::new());
        let mut old = Message::user("old".into());
        if let Message::User {
            ref mut compacted, ..
        } = old
        {
            *compacted = true;
        }
        conv.messages.push(old);
        conv.messages.push(Message::Summary {
            content: "summary text".into(),
            timestamp: Utc::now(),
        });
        conv.messages.push(Message::user("recent".into()));

        let json = serde_json::to_string(&conv).unwrap();
        let loaded: Conversation = serde_json::from_str(&json).unwrap();

        assert!(matches!(
            loaded.messages[0],
            Message::User {
                compacted: true,
                ..
            }
        ));
        assert!(matches!(loaded.messages[1], Message::Summary { .. }));
        assert!(matches!(
            loaded.messages[2],
            Message::User {
                compacted: false,
                ..
            }
        ));
    }

    /// Pre-compaction files (no `compacted` field) load as `compacted = false`.
    #[test]
    fn pre_compaction_file_defaults_compacted_to_false() {
        let json = r#"{
            "id": "10da8b9d-f242-4786-9c75-c3fbc2530f1f",
            "name": "Old convo",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "messages": [
                {"role": "User", "content": "hi", "timestamp": "2026-01-01T00:00:00Z"}
            ],
            "model": "m",
            "metadata": {"message_count": 1}
        }"#;
        let conv: Conversation = serde_json::from_str(json).unwrap();
        assert!(matches!(
            conv.messages[0],
            Message::User {
                compacted: false,
                ..
            }
        ));
    }

    /// A non-compacted message must NOT emit a `compacted` key (skip_serializing_if).
    #[test]
    fn uncompacted_message_omits_compacted_key() {
        let conv = {
            let mut c = Conversation::new("t".into(), "prov".into(), "model".into(), String::new());
            c.add_user_message("hi".into());
            c
        };
        let json = serde_json::to_string(&conv).unwrap();
        assert!(
            !json.contains("compacted"),
            "uncompacted messages must not write the flag; got: {json}"
        );
    }

    // ── message source (lane) persistence ──────────────────────────────

    /// A message's `source` lane survives the REAL serialize→deserialize
    /// round-trip: a `SubAgent { role }` assistant turn and a `Background`
    /// user turn come back tagged, not collapsed to `Human`. Exercises the
    /// actual serde path (not a hand-built intermediate), per the
    /// "symmetric persist/restore audited together" rule.
    #[test]
    fn message_source_lane_roundtrips_through_json() {
        let mut conv = Conversation::new("t".into(), "prov".into(), "model".into(), String::new());
        conv.messages.push(Message::Assistant {
            content: "reviewed".into(),
            compacted: false,
            source: MessageSource::SubAgent {
                role: "reviewer".into(),
            },
            timestamp: Utc::now(),
        });
        conv.messages.push(Message::User {
            content: "[bg output]".into(),
            compacted: false,
            source: MessageSource::Background {
                proc_ids: vec![3, 7],
            },
            timestamp: Utc::now(),
        });

        let json = serde_json::to_string(&conv).unwrap();
        let loaded: Conversation = serde_json::from_str(&json).unwrap();

        match &loaded.messages[0] {
            Message::Assistant { source, .. } => assert_eq!(
                source,
                &MessageSource::SubAgent {
                    role: "reviewer".into()
                }
            ),
            other => panic!("expected Assistant, got {other:?}"),
        }
        match &loaded.messages[1] {
            Message::User { source, .. } => {
                assert_eq!(
                    source,
                    &MessageSource::Background {
                        proc_ids: vec![3, 7]
                    }
                )
            }
            other => panic!("expected User, got {other:?}"),
        }
    }

    /// A `Human`-lane message must NOT write a `source`/`kind` key — Human is
    /// the default and is skipped, keeping pre-lane files byte-identical and
    /// avoiding schema churn on the 99% of turns that are orchestrator turns.
    #[test]
    fn human_source_omits_key() {
        let mut conv = Conversation::new("t".into(), "prov".into(), "model".into(), String::new());
        conv.add_user_message("hi".into());
        let json = serde_json::to_string(&conv).unwrap();
        assert!(
            !json.contains("\"kind\""),
            "Human-lane messages must not write a source key; got: {json}"
        );
    }

    /// Pre-lane conversation files (no `source` field) load as `Human` via
    /// `#[serde(default)]` — so all 1807 existing on-disk conversations
    /// deserialize cleanly and render on the orchestrator lane.
    #[test]
    fn pre_lane_file_defaults_source_to_human() {
        let json = r#"{
            "id": "10da8b9d-f242-4786-9c75-c3fbc2530f1f",
            "name": "Old convo",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "messages": [
                {"role": "Assistant", "content": "hi", "timestamp": "2026-01-01T00:00:00Z"}
            ],
            "model": "m",
            "metadata": {"message_count": 1}
        }"#;
        let conv: Conversation = serde_json::from_str(json).unwrap();
        match &conv.messages[0] {
            Message::Assistant { source, .. } => assert_eq!(source, &MessageSource::Human),
            other => panic!("expected Assistant, got {other:?}"),
        }
    }
}
