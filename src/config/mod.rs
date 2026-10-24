//! Configuration: the 0.24 layout (`providers` / `mcps` / `profiles` /
//! `general`). `Config::parse` (in [`resolve`]) is the only door from YAML to
//! a trusted [`Config`]; this module keeps the leaf types the new layout
//! reuses (provider configs, MCP servers, filters, timeouts, …) plus the
//! config-dir and write-path helpers.

// Allow dead_code for provider-specific config structs - not all providers may be used
// depending on build configuration. The enum variants are accessed via deserialization.
#![allow(dead_code)]

pub mod errors;
pub mod file;
pub mod migrate;
pub mod model_registry;
pub mod named;
pub mod resolve;
pub use errors::{ConfigError, ConfigErrors, ConfigFileKind};
pub use file::{
    ConversationConfig, Env, General, ProfileUi, SearchConfig, VectorDbConfig, WebConfig,
};
pub use migrate::{AutoMigrateError, AutoMigrateOutcome, auto_migrate, auto_migrate_repo};
pub use model_registry::{
    BootSelection, ModelEntry, ModelRegistry, ProviderEntry, RegistryError, ResolvedModel,
};
pub use resolve::{
    AgentInfo, Bound, Catalog, Config, DbPathSpec, Head, LoadError, Member, Origin, ParseCtx,
    ProfileInfo, RepoMode, ResolvedAgent, ResolvedProfile, Selection, Source, Subagent,
    SubagentInfo, VectorSpec, boot_only_diff,
};

use anyhow::Context;
use directories_next::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

/// Provider type enum - identifies which LLM provider to use
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    #[default]
    OpenRouter,
    OpenAI,
    Anthropic,
    LlamaCpp,
    Ollama,
}

impl fmt::Display for ProviderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderType::OpenRouter => write!(f, "openrouter"),
            ProviderType::OpenAI => write!(f, "openai"),
            ProviderType::Anthropic => write!(f, "anthropic"),
            ProviderType::LlamaCpp => write!(f, "llamacpp"),
            ProviderType::Ollama => write!(f, "ollama"),
        }
    }
}

/// Configuration for OpenRouter provider
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OpenRouterConfig {
    /// OpenRouter API key
    #[serde(default)]
    pub api_key: Option<String>,
    /// Model to use (default: anthropic/claude-3.7-sonnet)
    #[serde(default = "default_model")]
    pub model: String,
    /// Maximum tokens for responses
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
    /// Explicit image-support override. `None` (default) → auto-detect from the
    /// model name; `Some(true)`/`Some(false)` force `[img:…]` acceptance on/off.
    #[serde(default)]
    pub vision: Option<bool>,
}

/// Configuration for Ollama provider (local models)
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OllamaConfig {
    /// Base URL (default: http://localhost:11434)
    #[serde(default = "default_ollama_url")]
    pub base_url: String,
    /// Model name (e.g., "llama3", "qwen2.5:14b", "mistral")
    pub model: String,
    /// Temperature setting (optional)
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Explicit image-support override. `None` (default) → auto-detect from the
    /// model name; `Some(true)`/`Some(false)` force `[img:…]` acceptance on/off.
    #[serde(default)]
    pub vision: Option<bool>,
}

/// Configuration for OpenAI provider
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OpenAIConfig {
    /// OpenAI API key
    #[serde(default)]
    pub api_key: Option<String>,
    /// Base URL for the OpenAI API (default: https://api.openai.com/v1)
    /// Can be overridden to use compatible endpoints (e.g., Azure OpenAI, local proxies)
    #[serde(default = "default_openai_url")]
    pub base_url: String,
    /// Model to use (e.g., "gpt-4o", "gpt-4o-mini", "o1-preview")
    pub model: String,
    /// Maximum tokens for responses
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
    /// Explicit image-support override. `None` (default) → auto-detect from the
    /// model name; `Some(true)`/`Some(false)` force `[img:…]` acceptance on/off.
    #[serde(default)]
    pub vision: Option<bool>,
}

/// Anthropic prompt-caching mode. `Auto*` use the top-level breakpoint that
/// advances with the chat (recommended for multi-turn); `Manual` pins system +
/// tool + last user. All cut input-token cost on the stable prefix.
#[derive(Debug, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicCaching {
    /// No caching. Set explicitly to opt out of the default.
    Off,
    /// Manual breakpoints: system prompt + last tool + last message (5m TTL).
    Manual,
    /// Automatic top-level breakpoint, 5-minute TTL. (Default — recommended
    /// for multi-turn Claude usage; cut input-token cost on the stable prefix.)
    #[default]
    Auto,
    /// Automatic top-level breakpoint, 1-hour TTL.
    #[serde(rename = "auto_1h", alias = "auto1h")]
    Auto1h,
}

/// Configuration for Anthropic provider (Claude API, or any Messages-API
/// server — notably llama.cpp's `/v1/messages`). The Messages transport is
/// the only one carrying images in tool results, hence `view_image` is here.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AnthropicConfig {
    /// API key (optional for local servers that don't authenticate).
    #[serde(default)]
    pub api_key: Option<String>,
    /// Base URL for the Anthropic Messages API (default: https://api.anthropic.com).
    /// Point this at e.g. `http://localhost:8080` for a local llama-server.
    #[serde(default = "default_anthropic_url")]
    pub base_url: String,
    /// Model to use (e.g., "claude-3-5-sonnet-latest", or any model served
    /// by a local Anthropic-compatible endpoint).
    pub model: String,
    /// Maximum tokens for responses.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
    /// Prompt-caching mode (default: `auto`). See [`AnthropicCaching`].
    /// Set `prompt_caching: off` to opt out (e.g. for local llama-server
    /// endpoints that may not honor `cache_control`).
    #[serde(default)]
    pub prompt_caching: AnthropicCaching,
    /// Explicit image-support override. `None` → auto-detect (on for this
    /// provider; also gates `view_image` registration). `Some(b)` forces.
    #[serde(default)]
    pub vision: Option<bool>,
    /// Maximum size, in **base64** bytes, of a single image in a tool result
    /// on this endpoint — the number the API's limit actually counts, not
    /// the raw file size. Default 5 MiB (api.anthropic.com's documented
    /// ceiling). Proxies and gateways enforce their own; raise this to
    /// match yours.
    #[serde(default = "default_max_image_base64_bytes")]
    pub max_image_base64_bytes: usize,
    /// Provider-wide override for Anthropic thinking-block capture.
    /// `None` (default) defers to the per-model entry; `Some(b)` forces —
    /// useful when a deployment 400s on thinking blocks and you want to
    /// kill capture for every model on this provider without editing each
    /// model line. Resolved once at agent-build time by
    /// [`crate::providers::resolve_preserve_reasoning`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve_reasoning: Option<bool>,
    /// Provider-wide override for the web transcript's thinking-block
    /// display. Same pattern as `preserve_reasoning`: `None` defers to
    /// the model, `Some(b)` forces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_reasoning: Option<bool>,
}

/// Configuration for LlamaCpp provider (uses OpenAI-compatible completions API)
/// This is compatible with llama.cpp's server mode which provides an OpenAI-compatible API
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LlamaCppConfig {
    /// API key (optional for local llama.cpp instances)
    #[serde(default)]
    pub api_key: Option<String>,
    /// Base URL for the llama.cpp API (default: http://localhost:8080)
    #[serde(default = "default_llamacpp_url")]
    pub base_url: String,
    /// Model to use (e.g., "llama3", "qwen2.5:14b", any model running in llama.cpp)
    pub model: String,
    /// Maximum tokens for responses
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
    /// Extra JSON parameters merged (flattened) into every chat-completions
    /// request body. Useful for proxy-specific flags like `{"no-log": true}`
    /// (LiteLLM) or vendor extensions that aren't in the OpenAI schema.
    #[serde(default)]
    pub extra_params: Option<serde_json::Value>,
    /// Explicit image-support override. `None` (default) → auto-detect from the
    /// model name; `Some(true)`/`Some(false)` force `[img:…]` acceptance on/off.
    #[serde(default)]
    pub vision: Option<bool>,
}

fn default_ollama_url() -> String {
    "http://localhost:11434".to_string()
}

fn default_openai_url() -> String {
    "https://api.openai.com/v1".to_string()
}

fn default_anthropic_url() -> String {
    "https://api.anthropic.com".to_string()
}

/// api.anthropic.com's documented image-size ceiling, measured in base64
/// bytes (the number the API actually counts). Safe-by-default: works on
/// the canonical endpoint out of the box; proxies that allow more raise
/// `max_image_base64_bytes` explicitly.
fn default_max_image_base64_bytes() -> usize {
    5 * 1024 * 1024
}

fn default_llamacpp_url() -> String {
    "http://localhost:8080".to_string()
}

/// Provider configuration - specifies which provider and its specific config
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", content = "config", deny_unknown_fields)]
pub enum ProviderConfig {
    #[serde(rename = "openrouter")]
    OpenRouter(OpenRouterConfig),
    #[serde(rename = "openai")]
    OpenAI(OpenAIConfig),
    #[serde(rename = "anthropic")]
    Anthropic(AnthropicConfig),
    #[serde(rename = "llamacpp")]
    LlamaCpp(LlamaCppConfig),
    #[serde(rename = "ollama")]
    Ollama(OllamaConfig),
}

impl Default for ProviderConfig {
    fn default() -> Self {
        ProviderConfig::OpenRouter(OpenRouterConfig::default())
    }
}

impl Default for OpenRouterConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            model: default_model(),
            max_tokens: default_max_tokens(),
            vision: None,
        }
    }
}

impl Default for OpenAIConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: default_openai_url(),
            model: "gpt-4o".to_string(),
            max_tokens: default_max_tokens(),
            vision: None,
        }
    }
}

impl Default for LlamaCppConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: default_llamacpp_url(),
            model: "llama3".to_string(),
            max_tokens: default_max_tokens(),
            extra_params: None,
            vision: None,
        }
    }
}

/// Upload limits for the web composer and `[img:]` tokens. Boot-only (like
/// `web.*`): the `UploadStore` keeps the boot value, and the reload diff in
/// `reload_session_config` warns on changes.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UploadsConfig {
    /// Per-file ceiling in MB (valid 1..=1024).
    #[serde(default = "default_max_file_mb")]
    pub max_file_mb: u32,
    /// Per-message attachment ceiling (valid 1..=100): uploads + tokens.
    #[serde(default = "default_max_files")]
    pub max_files: u32,
}

impl Default for UploadsConfig {
    fn default() -> Self {
        Self {
            max_file_mb: default_max_file_mb(),
            max_files: default_max_files(),
        }
    }
}

fn default_max_file_mb() -> u32 {
    50
}

fn default_max_files() -> u32 {
    10
}

/// The seven Web tabs, in canonical order — the closed universe a profile's
/// `ui.tabs` filter may name. The old `profile` tab is gone; `settings` gates
/// the Settings gear.
pub const TAB_NAMES: [&str; 7] = [
    "session", "todo", "files", "tasks", "bash", "agents", "settings",
];

/// Outbound HTTP timeouts, applied to every client PeakBot builds (LLM calls,
/// embeddings, MCP auth, web tools). Without these a provider that accepts a
/// request and never answers wedges the turn forever.
///
/// Deliberately NOT a total request timeout: `read_timeout` resets on each
/// successful read, so it bounds *silence* rather than duration.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    /// Give up if TCP+TLS setup takes this long (default: 30 s, 0 = disabled).
    /// A connect either completes in seconds or never will.
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    /// Give up after this many seconds with no bytes read (default: 1800,
    /// 0 = disabled). Completions are non-streaming, so for LLM calls this is
    /// in effect a ceiling on a single generation — raise it if you run models
    /// that legitimately think for longer than 30 minutes.
    #[serde(default = "default_read_timeout_secs")]
    pub read_timeout_secs: u64,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            connect_timeout_secs: default_connect_timeout_secs(),
            read_timeout_secs: default_read_timeout_secs(),
        }
    }
}

fn default_connect_timeout_secs() -> u64 {
    30
}
fn default_read_timeout_secs() -> u64 {
    1800
}

/// Wall-clock ceilings on agent work: how long a single tool call, or a whole
/// delegation to a sub-agent, may run before it is cut short. See `http:` for
/// the per-socket network timeouts, which bound silence rather than duration.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TimeoutsConfig {
    /// Budget for one tool call (default: 1800 = 30 min).
    #[serde(default = "default_tool_secs")]
    pub tool_secs: u64,
    /// Budget for one delegation to a sub-agent (default: 7200 = 2 h). Longer
    /// than `tool_secs` because a delegate runs a full turn loop of its own.
    #[serde(default = "default_delegate_secs")]
    pub delegate_secs: u64,
}

impl Default for TimeoutsConfig {
    fn default() -> Self {
        Self {
            tool_secs: default_tool_secs(),
            delegate_secs: default_delegate_secs(),
        }
    }
}

impl TimeoutsConfig {
    /// Boundary parse: 0 would make every call time out instantly and a
    /// budget beyond a day is a deadline in name only. Returns a user-facing
    /// message on failure.
    pub fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("tool_secs", self.tool_secs),
            ("delegate_secs", self.delegate_secs),
        ] {
            if !(1..=86_400).contains(&value) {
                return Err(format!(
                    "timeouts.{field} must be 1..=86400 seconds (got {value})"
                ));
            }
        }
        Ok(())
    }
}

fn default_tool_secs() -> u64 {
    1800
}
fn default_delegate_secs() -> u64 {
    7200
}

/// MCP transport type (matches Claude/Continue SDK format)
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum McpTransportType {
    /// Standard I/O transport (spawns a local process)
    #[default]
    Stdio,
    /// Server-Sent Events over HTTP
    Sse,
    /// Streamable HTTP transport (recommended for remote servers).
    /// `alias`: the 0.24 layout spells the value `streamable-http` (spec §3.1,
    /// §9.9), matching this type's `Display`; the alias makes the unchanged
    /// struct accept both spellings. Strictly additive — old configs keep
    /// parsing as before.
    #[serde(alias = "streamable-http")]
    StreamableHttp,
}

impl fmt::Display for McpTransportType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            McpTransportType::Stdio => write!(f, "stdio"),
            McpTransportType::Sse => write!(f, "sse"),
            McpTransportType::StreamableHttp => write!(f, "streamable-http"),
        }
    }
}

/// Authentication strategy for a remote MCP server.
///
/// Internally tagged on `type`. The locked config shape is:
///
/// ```yaml
/// auth:
///   type: bearer
///   token: "sk-xxx"
/// ```
///
/// or
///
/// ```yaml
/// auth:
///   type: oauth
/// ```
///
/// "No auth" is encoded by omitting the `auth` field entirely
/// (`Option::None` on `McpServerConfig::auth`). There is no `type: none`
/// variant by design.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum McpAuth {
    /// Static `Authorization: Bearer <token>` header.
    Bearer { token: String },
    /// OAuth 2.1 + PKCE per the MCP authorization spec. Two modes:
    ///
    /// 1. **Dynamic Client Registration** (RFC 7591) — all three fields
    ///    omitted. The server must advertise a `registration_endpoint`.
    ///    Used by Linear-shaped MCP servers.
    /// 2. **Static client credentials** — user pre-registers an OAuth
    ///    client at the server's console (e.g. Google Cloud Console),
    ///    pastes `client_id` + `client_secret` + the required `scopes`.
    ///    Used by Google Workspace MCP servers (Gmail, Drive, Calendar).
    ///
    /// `client_id` without `client_secret` is allowed (public client).
    /// `client_secret` without `client_id` is rejected at [`validate`].
    /// `scopes` defaults to empty (the DCR path on Linear works
    /// scope-less); for static-credentials servers the user must list
    /// the scopes their consent screen was configured for.
    ///
    /// [`validate`]: McpServerConfig::validate
    Oauth {
        #[serde(default)]
        client_id: Option<String>,
        #[serde(default)]
        client_secret: Option<String>,
        #[serde(default)]
        scopes: Vec<String>,
    },
}

/// Resolved auth strategy after merging the legacy `auth_token` field
/// with the new `auth:` block. Internal-only type — produced by
/// [`McpServerConfig::auth_resolved`].
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedAuth {
    Bearer {
        token: String,
    },
    Oauth {
        client_id: Option<String>,
        client_secret: Option<String>,
        scopes: Vec<String>,
    },
}

/// Strip a leading `Bearer ` (case-sensitive — matches the wire spelling)
/// and trim surrounding whitespace. Lets users paste either the raw token
/// or the full `Authorization` header value without producing a doubled
/// `Bearer Bearer xxx` header. Returns the cleaned token.
fn normalize_bearer_token(raw: &str) -> String {
    raw.strip_prefix("Bearer ")
        .unwrap_or(raw)
        .trim()
        .to_string()
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    /// Unique name for this MCP server
    pub name: String,
    /// Transport type: "stdio" (default), "sse", or "streamable-http"
    /// For "stdio": command/args/env are required
    /// For "sse"/"streamable-http": url is required
    #[serde(default)]
    #[serde(rename = "type")]
    pub transport_type: McpTransportType,
    /// Command to run (required for stdio transport)
    pub command: Option<String>,
    /// Arguments for the command (optional)
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Environment variables (optional)
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    /// URL for remote transports (sse, streamable-http)
    pub url: Option<String>,
    /// **Deprecated.** Use `auth: { type: bearer, token: "…" }` instead.
    /// Kept parseable for one release with a deprecation warning at
    /// connect time. Setting both `auth_token` and `auth` is a config
    /// error (see [`McpServerConfig::validate`]). Will be removed in the
    /// release after the next.
    #[serde(default)]
    pub auth_token: Option<String>,
    /// Authentication strategy. See [`McpAuth`].
    #[serde(default)]
    pub auth: Option<McpAuth>,
    /// Custom HTTP headers sent with every request for remote transports.
    /// Header names must be valid HTTP token characters; values must be
    /// visible ASCII. Invalid entries are logged and skipped.
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    /// Enable/disable this server (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl McpServerConfig {
    /// Returns the transport type for this config
    pub fn transport_type(&self) -> McpTransportType {
        self.transport_type.clone()
    }

    /// Validates the configuration based on transport type.
    /// Returns Ok(()) if valid, or Err with an error message.
    pub fn validate(&self) -> Result<(), String> {
        // Cross-validation: legacy `auth_token` and the new `auth:` block
        // are mutually exclusive. Setting both is a contract violation
        // — refuse to start rather than silently pick one.
        if self.auth_token.is_some() && self.auth.is_some() {
            return Err(format!(
                "MCP server '{}': cannot set both `auth_token` and `auth`. \
                 Migrate `auth_token` to `auth: {{ type: bearer, token: \"…\" }}` and remove the legacy field.",
                self.name
            ));
        }

        // OAuth inner-shape sanity check: `client_secret` without
        // `client_id` is nonsensical (the token endpoint authenticates
        // by client_id; the secret is meaningless on its own). Reject
        // it at boot so the user sees the typo immediately, not via a
        // mid-flow OAuth error.
        if let Some(McpAuth::Oauth {
            client_id: None,
            client_secret: Some(_),
            ..
        }) = &self.auth
        {
            return Err(format!(
                "MCP server '{}': `auth.client_secret` is set but `auth.client_id` is missing. \
                 Either provide both (static-credentials path) or remove both (dynamic registration path).",
                self.name
            ));
        }

        match self.transport_type {
            McpTransportType::Stdio => {
                if self.command.is_none() || self.command.as_ref().unwrap().is_empty() {
                    return Err(format!(
                        "MCP server '{}': 'command' is required for stdio transport",
                        self.name
                    ));
                }
            }
            McpTransportType::Sse | McpTransportType::StreamableHttp => {
                if self.url.is_none() || self.url.as_ref().unwrap().is_empty() {
                    return Err(format!(
                        "MCP server '{}': 'url' is required for {} transport",
                        self.name, self.transport_type
                    ));
                }
            }
        }
        Ok(())
    }

    /// Merge the legacy `auth_token` field and the new `auth:` block into
    /// a single resolved strategy. Returns `None` when neither is set.
    ///
    /// Defensive [`Bearer ` prefix-strip][normalize_bearer_token] is applied
    /// to both paths so `Bearer xxx` and `xxx` both produce the same
    /// `Authorization: Bearer xxx` header on the wire. Empty tokens
    /// (after trimming) are treated as `None` — same as before the
    /// refactor.
    ///
    /// Cross-validation in [`Self::validate`] guarantees that this
    /// method's "legacy" branch and the "new" branch are mutually
    /// exclusive at boot. We still tolerate both being set defensively
    /// (the new `auth:` block wins) so a misconfigured user gets the
    /// validation error, not a panic.
    pub fn auth_resolved(&self) -> Option<ResolvedAuth> {
        if let Some(auth) = &self.auth {
            return Some(match auth {
                McpAuth::Bearer { token } => {
                    let cleaned = normalize_bearer_token(token);
                    if cleaned.is_empty() {
                        return None;
                    }
                    ResolvedAuth::Bearer { token: cleaned }
                }
                McpAuth::Oauth {
                    client_id,
                    client_secret,
                    scopes,
                } => ResolvedAuth::Oauth {
                    client_id: client_id.clone(),
                    client_secret: client_secret.clone(),
                    scopes: scopes.clone(),
                },
            });
        }
        if let Some(raw) = self.auth_token.as_deref() {
            let cleaned = normalize_bearer_token(raw);
            if cleaned.is_empty() {
                return None;
            }
            return Some(ResolvedAuth::Bearer { token: cleaned });
        }
        None
    }

    /// Returns user-facing deprecation warnings for this config. Empty
    /// when nothing is deprecated. The caller is expected to log each
    /// entry via `tracing::warn!`; surfacing them as plain strings keeps
    /// this method side-effect-free and trivially unit-testable.
    pub fn deprecation_warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.auth_token.is_some() && self.auth.is_none() {
            out.push(format!(
                "MCP server '{}': `auth_token` is deprecated; use `auth: {{ type: bearer, token: \"…\" }}` instead. The legacy field will be removed in a future release.",
                self.name
            ));
        }
        out
    }
}

/// Embeddings endpoint — any OpenAI-compatible `POST /v1/embeddings` server
/// (OpenAI, llama.cpp, Ollama, LM Studio, TEI, …).
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingsConfig {
    /// Base URL of the embeddings server, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    /// API key. Optional for local servers that don't authenticate.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Embedding model name, e.g. `text-embedding-3-small`.
    pub model: String,
    /// Output dimensionality of the model. Must match the model's real output
    /// and the dimensions of an existing DB at `db_path`.
    pub dimensions: usize,
}

/// Configuration for context compaction
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContextConfig {
    /// Compaction threshold (0.0-1.0), default 0.8
    /// When context usage exceeds this threshold, compaction is triggered
    #[serde(default = "default_threshold")]
    pub threshold: f64,
    /// Keep last N messages always (default: 5)
    #[serde(default = "default_keep_recent")]
    pub keep_recent: usize,
    /// Enable/disable compaction (default: true)
    #[serde(default = "default_context_enabled")]
    pub enabled: bool,
    /// Optional model override for compaction summarization.
    /// When set, uses this model (via the same provider) for summarization.
    /// When None, uses the main model. Either way, the call is tool-free.
    #[serde(default)]
    pub compaction_model: Option<String>,
}

fn default_threshold() -> f64 {
    0.8
}
fn default_keep_recent() -> usize {
    5
}
fn default_context_enabled() -> bool {
    true
}

fn default_true() -> bool {
    true
}

fn default_memory_threshold() -> usize {
    51_200 // 50 KiB
}

fn default_model() -> String {
    "anthropic/claude-3.7-sonnet".to_string()
}

fn default_max_tokens() -> u64 {
    4096
}

/// Configuration for the memory.md feature
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {
    /// Enable the memory.md feature: injects the memory.md instructions into
    /// the system prompt AND auto-compacts an oversized memory.md at
    /// conversation start. `false` disables both (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// File size threshold in bytes to trigger compaction (default: 51200 = 50KB)
    #[serde(default = "default_memory_threshold")]
    pub threshold_bytes: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_bytes: default_memory_threshold(),
        }
    }
}

/// Canonical names of every filterable built-in tool. Used to reject typos in
/// the `tools:` filter at load. Provider-gated tools (`view_image`, `doc_*`)
/// are listed too — naming one that isn't currently registered is a harmless
/// no-op, not an error. `delegate` is not here: it is on iff an agent has
/// subagents, never filterable.
pub const BUILTIN_TOOL_NAMES: &[&str] = &[
    "bash",
    "bash_bg",
    "doc_index",
    "doc_search",
    "fetch_page",
    "fetch_url",
    "file_create",
    "file_insert",
    "file_read",
    "file_str_replace",
    "list_directory",
    "pdf_read",
    "powershell",
    "powershell_bg",
    "think",
    "todo",
    "view_image",
    "web_search",
];

/// One name filter for every allow/deny list (`tools:`, `mcps:`, `skills:`,
/// `ui.tabs:`). `enabled: false` allows nothing; a non-empty
/// `only` is an allowlist; otherwise `disabled` is a blocklist; both lists
/// empty (or absent) allows everything. Setting both lists is a config error.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NameFilter {
    /// Master switch: `false` allows nothing, ignoring the lists.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Blocklist: these names are removed; every other name stays.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Allowlist: only these names stay; every other name is removed.
    #[serde(default)]
    pub only: Vec<String>,
}

// Hand-rolled, NOT derived: a derived `Default` would yield `enabled: false` ("allow nothing"), silently disabling every tool/skill/tab in any `..Default::default()` struct-update literal.
impl Default for NameFilter {
    fn default() -> Self {
        Self {
            enabled: true,
            disabled: Vec::new(),
            only: Vec::new(),
        }
    }
}

impl NameFilter {
    /// Boundary parse: setting both lists is illegal.
    pub fn validate_shape(&self, label: &str) -> Result<(), String> {
        if !self.disabled.is_empty() && !self.only.is_empty() {
            return Err(format!(
                "{label}: set either `disabled` (blocklist) or `only` (allowlist), not both."
            ));
        }
        Ok(())
    }

    /// Boundary parse: every name in `disabled`/`only` must be in `known`
    /// (typo catch).
    pub fn validate_names(&self, label: &str, noun: &str, known: &[&str]) -> Result<(), String> {
        for name in self.disabled.iter().chain(self.only.iter()) {
            if !known.contains(&name.as_str()) {
                return Err(format!(
                    "{label}: unknown {noun} '{name}'. Known {noun}s: {}.",
                    known.join(", ")
                ));
            }
        }
        Ok(())
    }

    /// Whether a name of this value survives the filter. `enabled: false`
    /// short-circuits to none; then allowlist wins, then blocklist. Both
    /// sides are trimmed, matching how config names are normalized.
    pub fn allows(&self, name: &str) -> bool {
        if !self.enabled {
            return false;
        }
        let name = name.trim();
        if !self.only.is_empty() {
            self.only.iter().any(|n| n.trim() == name)
        } else {
            !self.disabled.iter().any(|n| n.trim() == name)
        }
    }
}

/// Configuration for the `fetch_url` tool.
#[derive(Debug, Deserialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct FetchUrlConfig {
    /// Let `fetch_url` send POST requests (default false). Off = the schema
    /// shows GET only. Not a security boundary while `bash` is enabled.
    #[serde(default)]
    pub allow_post: bool,
}

/// Configuration for retry logic with exponential backoff
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (default: 3)
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Initial delay in milliseconds (default: 1000)
    #[serde(default = "default_initial_delay")]
    pub initial_delay_ms: u64,
    /// Maximum delay in milliseconds (default: 30000)
    #[serde(default = "default_max_delay")]
    pub max_delay_ms: u64,
    /// Backoff multiplier (default: 2.0)
    #[serde(default = "default_backoff_factor")]
    pub backoff_factor: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            initial_delay_ms: default_initial_delay(),
            max_delay_ms: default_max_delay(),
            backoff_factor: default_backoff_factor(),
        }
    }
}

fn default_max_retries() -> u32 {
    3
}

fn default_initial_delay() -> u64 {
    1000
}

fn default_max_delay() -> u64 {
    30000
}

fn default_backoff_factor() -> f64 {
    2.0
}

/// Get the platform-specific config directory.
///
/// Public because `peakbot::install::web_token_path` (track I, plan §E.5) needs
/// it to locate `<config_dir>/web-token` next to `config.yaml`.
pub fn get_config_dir() -> Option<std::path::PathBuf> {
    ProjectDirs::from("com", "peakbot", "peakbot").map(|dirs| dirs.config_dir().to_path_buf())
}

/// Get the platform-specific config file path.
/// Platform config file: ~/.config/peakbot/config.yaml (Linux), %APPDATA%\peakbot\peakbot\config\config.yaml (Windows), ~/Library/Application Support/com.peakbot.peakbot/config.yaml (macOS).
/// Returns None if the platform doesn't provide a config directory.
pub fn get_config_file_path() -> Option<std::path::PathBuf> {
    get_config_dir().map(|dir| dir.join("config.yaml"))
}

// ===========================================================================
// Write path.
//
// `save_config_at(dir, yaml)` writes the exact bytes the caller hands it into
// `<dir>/config.yaml`, with a single-slot `<dir>/config.yaml.bak` of the
// previous bytes when one existed, atomic on POSIX (temp + rename), `0600` on
// Unix, and no survivors on success. Callers validate with `Config::parse`
// first and only then write those bytes verbatim — no `Serialize` on
// `Config`, no server-side rendering, no merging. *(principle of least
// astonishment)*
// ===========================================================================

/// What `save_config_at` produced. The handler returns this verbatim so the
/// reviewer can see exactly where the bytes landed and whether a previous
/// file was preserved as a backup.
#[derive(Debug, Clone)]
pub struct SaveOutcome {
    /// Absolute path of the file that now holds the new bytes.
    pub path: PathBuf,
    /// Path of the `.bak` file holding the previous bytes, if there was one.
    /// `None` on a first write.
    pub backup: Option<PathBuf>,
}

/// Write the given YAML bytes to `<dir>/config.yaml` via the locked
/// `backup → temp → 0600 → sync_all → remove → rename` sequence (plan §A-Q4).
///
/// - `create_dir_all` the parent so a first-ever write doesn't need a
///   pre-existing directory.
/// - If `<dir>/config.yaml` exists, copy it to `<dir>/config.yaml.bak`
///   (single slot, overwritten) **before** writing anything.
/// - Write the bytes to `<dir>/config.yaml.tmp`, `0600` on Unix, `sync_all`
///   for crash safety.
/// - Remove the existing `config.yaml` (needed on Windows where rename onto
///   an existing file fails; on POSIX the rename is atomic), then `rename`
///   the tmp into place. The non-atomic window exists only on Windows and
///   only after the backup has already been taken.
///
/// Returns the path of the final file and, if there was a previous file,
/// the path of the single-slot backup. Caller-facing diagnostic; the HTTP
/// handler puts both into the response.
pub fn save_config_at(dir: &std::path::Path, yaml: &str) -> anyhow::Result<SaveOutcome> {
    use std::io::Write;

    std::fs::create_dir_all(dir).with_context(|| format!("create_dir_all {}", dir.display()))?;

    let final_path = dir.join("config.yaml");
    let backup_path = dir.join("config.yaml.bak");
    let tmp_path = dir.join("config.yaml.tmp");

    // 1. Backup BEFORE any write touches the live file, so a crash at any
    //    later step leaves the previous file intact under .bak.
    let backup = if final_path.exists() {
        write_backup(&final_path, &backup_path)
            .with_context(|| format!("backup to {}", backup_path.display()))?;
        Some(backup_path)
    } else {
        None
    };

    // 2. Write the tmp file with mode 0600 on Unix, sync, then swap into
    //    place. If the tmp write fails we leave the live file untouched.
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .with_context(|| format!("create tmp {}", tmp_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            f.set_permissions(perms)
                .with_context(|| format!("chmod 0600 {}", tmp_path.display()))?;
        }
        f.write_all(yaml.as_bytes())
            .with_context(|| format!("write tmp {}", tmp_path.display()))?;
        f.sync_all()
            .with_context(|| format!("sync tmp {}", tmp_path.display()))?;
    }

    // 3. Swap into place. Windows can't rename onto an existing file, so we
    //    remove first there; on POSIX rename is atomic and config.yaml never
    //    goes missing. Either way, the .bak is already in place.
    #[cfg(windows)]
    {
        if final_path.exists() {
            std::fs::remove_file(&final_path)
                .with_context(|| format!("remove old {}", final_path.display()))?;
        }
    }
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename tmp → final at {}", final_path.display()))?;

    Ok(SaveOutcome {
        path: final_path,
        backup,
    })
}

/// Copies `from` to the `.bak` at `to` with mode 0600 on Unix. Not `fs::copy`:
/// that keeps the source mode, and the config holds API keys. An existing
/// `.bak` keeps its old mode on open, hence the explicit `set_permissions`.
fn write_backup(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;

    let bytes = std::fs::read(from)?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(to)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(&bytes)?;
    f.sync_all()
}

/// The web token in effect: `--token` > `$PEAKBOT_WEB_TOKEN` >
/// `general.web.token` > none. Empty / whitespace-only values at every level
/// count as absent (an empty secret is no secret); the winner is trimmed.
pub fn resolve_token(cli: Option<&str>, env: Option<&str>, config: Option<&str>) -> Option<String> {
    [cli, env, config]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|t| !t.is_empty())
        .map(str::to_string)
}

/// Whether to track OpenRouter costs for a turn on `model`: the
/// `general.cost_tracking` switch AND an OpenRouter model — no other provider
/// reports pricing, so the switch is a no-op there.
pub fn cost_tracking_enabled(general_flag: bool, model: &ResolvedModel) -> bool {
    general_flag && model.provider_kind == ProviderType::OpenRouter
}

/// One lock for EVERY test that swaps `XDG_CONFIG_HOME` to point the master
/// config at a tempdir. It is crate-level (not private to `mod tests`)
/// because `Config::load` tests here and the session/reload tests elsewhere
/// drive the same env knob in the same test process — two private locks
/// would not protect the suites from each other.
#[cfg(test)]
pub(crate) static CONFIG_ENV_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_filter_default_allows_everything() {
        let t = NameFilter::default();
        assert!(t.allows("bash"));
        assert!(t.allows("web_search"));
        assert!(t.validate_shape("tools").is_ok());
        assert!(
            t.validate_names("tools", "tool", BUILTIN_TOOL_NAMES)
                .is_ok()
        );
    }

    /// PR3: `powershell_bg` is a SEPARATE known tool name — `tools.disabled`
    /// may name it without a typo error, exactly like `bash_bg` (which must
    /// keep working for Bash hosts).
    #[test]
    fn tools_filter_validate_names_accepts_powershell_bg() {
        let t = NameFilter {
            disabled: vec!["powershell_bg".into()],
            ..Default::default()
        };
        assert!(
            t.validate_names("tools", "tool", BUILTIN_TOOL_NAMES)
                .is_ok(),
            "powershell_bg must be a known built-in tool name (typo catch must not fire)"
        );
    }

    /// PR3 companion pin: adding `powershell_bg` must not have displaced
    /// `bash_bg` from the known universe — Bash hosts still address it.
    #[test]
    fn tools_filter_validate_names_still_accepts_bash_bg() {
        let t = NameFilter {
            disabled: vec!["bash_bg".into()],
            ..Default::default()
        };
        assert!(
            t.validate_names("tools", "tool", BUILTIN_TOOL_NAMES)
                .is_ok(),
            "bash_bg must remain a known built-in tool name"
        );
    }

    #[test]
    fn skill_filter_default_shows_everything() {
        let f = NameFilter::default();
        assert!(f.allows("github"));
        assert!(f.allows("anything"));
        assert!(f.validate_shape("profiles.p.agents.a.skills").is_ok());
        assert!(
            f.validate_names("profiles.p.agents.a.skills", "skill", &["github"])
                .is_ok()
        );
    }

    #[test]
    fn skill_filter_disabled_master_switch_hides_all() {
        let f = NameFilter {
            enabled: false,
            ..Default::default()
        };
        assert!(!f.allows("github"), "enabled=false hides every skill");
    }

    #[test]
    fn skill_filter_only_is_allowlist() {
        let f = NameFilter {
            enabled: true,
            disabled: vec![],
            only: vec!["github".into()],
        };
        assert!(f.allows("github"));
        assert!(!f.allows("helius-solana"));
    }

    #[test]
    fn skill_filter_disabled_is_blocklist() {
        let f = NameFilter {
            enabled: true,
            disabled: vec!["helius-solana".into()],
            only: vec![],
        };
        assert!(!f.allows("helius-solana"));
        assert!(f.allows("github"));
    }

    #[test]
    fn skill_filter_rejects_both_lists() {
        let f = NameFilter {
            enabled: true,
            disabled: vec!["a".into()],
            only: vec!["b".into()],
        };
        assert!(
            f.validate_shape("profiles.p.agents.a.skills").is_err(),
            "both lists is illegal"
        );
    }

    #[test]
    fn skill_filter_rejects_unknown_skill_name() {
        let f = NameFilter {
            enabled: true,
            disabled: vec![],
            only: vec!["ghost".into()],
        };
        assert!(
            f.validate_names("profiles.p.agents.a.skills", "skill", &["github"])
                .is_err(),
            "an unknown skill name is a typo-catch error"
        );
    }

    #[test]
    fn tools_filter_blocklist_removes_named_keeps_rest() {
        let t = NameFilter {
            disabled: vec!["bash_bg".into(), "web_search".into()],
            only: vec![],
            ..Default::default()
        };
        assert!(t.validate_shape("tools").is_ok());
        assert!(
            t.validate_names("tools", "tool", BUILTIN_TOOL_NAMES)
                .is_ok()
        );
        assert!(!t.allows("bash_bg"));
        assert!(!t.allows("web_search"));
        assert!(t.allows("file_read"));
    }

    #[test]
    fn tools_filter_allowlist_keeps_only_named() {
        let t = NameFilter {
            disabled: vec![],
            only: vec!["file_read".into(), "bash".into()],
            ..Default::default()
        };
        assert!(t.validate_shape("tools").is_ok());
        assert!(
            t.validate_names("tools", "tool", BUILTIN_TOOL_NAMES)
                .is_ok()
        );
        assert!(t.allows("file_read"));
        assert!(t.allows("bash"));
        assert!(!t.allows("web_search"));
        assert!(!t.allows("todo"));
    }

    #[test]
    fn tools_filter_both_lists_is_error() {
        let t = NameFilter {
            disabled: vec!["bash".into()],
            only: vec!["file_read".into()],
            ..Default::default()
        };
        assert!(t.validate_shape("tools").is_err());
    }

    #[test]
    fn tools_filter_unknown_tool_is_error() {
        let t = NameFilter {
            disabled: vec!["definitely_not_a_tool".into()],
            only: vec![],
            ..Default::default()
        };
        assert!(
            t.validate_names("tools", "tool", BUILTIN_TOOL_NAMES)
                .is_err()
        );
    }

    /// Setting BOTH the blocklist and the allowlist is illegal (the same
    /// rule for every filter). The error must name both keys.
    #[test]
    fn name_filter_rejects_disabled_and_only_together() {
        let f = NameFilter {
            disabled: vec!["a".into()],
            only: vec!["b".into()],
            ..Default::default()
        };
        let err = f
            .validate_shape("mcps")
            .expect_err("both lists must be rejected");
        assert!(
            err.contains("disabled"),
            "error must name the `disabled` blocklist, got: {err}"
        );
        assert!(
            err.contains("only"),
            "error must name the `only` allowlist, got: {err}"
        );
    }

    /// `validate_names` names the unknown entry and lists the known ones.
    #[test]
    fn name_filter_validate_names_flags_unknown_name_and_lists_known() {
        let gate = NameFilter {
            disabled: vec![],
            only: vec!["ghost".into()],
            ..Default::default()
        };
        let err = gate
            .validate_names("profiles.web.mcps", "server", &["a"])
            .expect_err("an unknown name must be rejected");
        assert!(
            err.contains("ghost"),
            "the error must name the unknown entry, got: {err}"
        );
        assert!(
            err.contains("Known servers: a"),
            "the error must list the known names, got: {err}"
        );
    }

    // ─── MCP OAuth — Slice 1 config-shape pins ──────────────────────────
    //
    // These tests pin the locked YAML shape from `autho.md` and the
    // resolution semantics in `auth_resolved()` / `deprecation_warnings()`.
    // Tracks Gitea #19. Slice 2 will exercise the OAuth flow itself; this
    // slice only proves the config plumbing.

    fn http_config_with(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            transport_type: McpTransportType::StreamableHttp,
            command: None,
            args: None,
            env: None,
            url: Some("https://example.com/mcp".to_string()),
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        }
    }

    #[test]
    fn auth_oauth_round_trips_through_yaml() {
        let yaml = r#"
name: linear
type: streamablehttp
url: https://mcp.linear.app/mcp
auth:
  type: oauth
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).expect("oauth config must parse");
        // DCR shape: all three inner fields default to their empty form.
        assert_eq!(
            config.auth,
            Some(McpAuth::Oauth {
                client_id: None,
                client_secret: None,
                scopes: vec![],
            })
        );
        // No legacy field set on the OAuth path.
        assert!(config.auth_token.is_none());
        // The cross-validator must accept the new shape on its own.
        assert!(config.validate().is_ok());
        assert_eq!(
            config.auth_resolved(),
            Some(ResolvedAuth::Oauth {
                client_id: None,
                client_secret: None,
                scopes: vec![],
            })
        );
        assert!(config.deprecation_warnings().is_empty());
    }

    #[test]
    fn auth_bearer_round_trips_through_yaml() {
        let yaml = r#"
name: bearer-mcp
type: streamablehttp
url: https://example.com/mcp
auth:
  type: bearer
  token: "sk-abc123"
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).expect("bearer config must parse");
        assert_eq!(
            config.auth,
            Some(McpAuth::Bearer {
                token: "sk-abc123".to_string()
            })
        );
        assert!(config.validate().is_ok());
        assert_eq!(
            config.auth_resolved(),
            Some(ResolvedAuth::Bearer {
                token: "sk-abc123".to_string()
            })
        );
        assert!(config.deprecation_warnings().is_empty());
    }

    #[test]
    fn auth_and_auth_token_set_together_is_rejected() {
        // Setting both fields is a contract violation: refuse to start
        // rather than silently pick one. The legacy field has a one-release
        // migration path; "set both" is never a valid migration step.
        let mut config = http_config_with("conflict");
        config.auth_token = Some("legacy".to_string());
        config.auth = Some(McpAuth::Bearer {
            token: "new".to_string(),
        });
        let err = config
            .validate()
            .expect_err("must reject conflicting auth fields");
        assert!(
            err.contains("cannot set both"),
            "validation error must explain the conflict, got: {err}"
        );
    }

    #[test]
    fn bearer_token_with_bearer_prefix_is_stripped() {
        // Users sometimes paste the full `Authorization` header value.
        // The resolver must strip a single leading `Bearer ` so the
        // wire layer doesn't produce `Bearer Bearer xxx`.
        let mut config = http_config_with("prefix-test");
        config.auth = Some(McpAuth::Bearer {
            token: "Bearer xxx".to_string(),
        });
        assert_eq!(
            config.auth_resolved(),
            Some(ResolvedAuth::Bearer {
                token: "xxx".to_string()
            })
        );

        // Same defensive strip on the legacy field.
        let mut legacy = http_config_with("prefix-legacy");
        legacy.auth_token = Some("Bearer xxx".to_string());
        assert_eq!(
            legacy.auth_resolved(),
            Some(ResolvedAuth::Bearer {
                token: "xxx".to_string()
            })
        );
    }

    #[test]
    fn bearer_token_without_prefix_unchanged() {
        // The complement to the strip test: a bare token must survive
        // intact. Whitespace around the value is trimmed (paste-safety).
        let mut config = http_config_with("plain");
        config.auth = Some(McpAuth::Bearer {
            token: "  xxx  ".to_string(),
        });
        assert_eq!(
            config.auth_resolved(),
            Some(ResolvedAuth::Bearer {
                token: "xxx".to_string()
            })
        );

        let mut legacy = http_config_with("plain-legacy");
        legacy.auth_token = Some("xxx".to_string());
        assert_eq!(
            legacy.auth_resolved(),
            Some(ResolvedAuth::Bearer {
                token: "xxx".to_string()
            })
        );
    }

    #[test]
    fn legacy_auth_token_surfaces_deprecation_warning() {
        // The legacy `auth_token` field keeps working for one release,
        // but must announce its retirement loudly. The warning text is
        // exposed as plain strings (side-effect-free) so it's trivially
        // testable; the production caller forwards each entry to
        // `tracing::warn!`.
        let mut config = http_config_with("legacy");
        config.auth_token = Some("sk-old".to_string());

        let warnings = config.deprecation_warnings();
        assert_eq!(warnings.len(), 1, "expected exactly one deprecation entry");
        assert!(
            warnings[0].contains("`auth_token` is deprecated"),
            "warning text must name the deprecated field, got: {}",
            warnings[0]
        );
        assert!(
            warnings[0].contains("legacy"),
            "warning must mention the server name, got: {}",
            warnings[0]
        );

        // The new shape on its own must NOT emit a deprecation warning.
        let mut modern = http_config_with("modern");
        modern.auth = Some(McpAuth::Bearer {
            token: "sk-new".to_string(),
        });
        assert!(modern.deprecation_warnings().is_empty());
    }

    #[test]
    fn auth_block_rejects_unknown_inner_field() {
        // `deny_unknown_fields` must apply inside the `auth:` block too —
        // a typo like `tokn:` should be loud, not silent.
        let yaml = r#"
name: typo
type: streamablehttp
url: https://example.com/mcp
auth:
  type: bearer
  tokn: "sk-typo"
"#;
        let err = serde_yaml::from_str::<McpServerConfig>(yaml)
            .expect_err("unknown field inside auth must reject");
        let msg = err.to_string();
        assert!(
            msg.contains("tokn") || msg.contains("unknown field"),
            "deserialization error must name the unknown field, got: {msg}"
        );
    }

    #[test]
    fn oauth_static_credentials_round_trip_through_yaml() {
        // The Google-Gmail shape: explicit client_id + client_secret +
        // scopes. All three fields must round-trip into `auth_resolved`
        // unchanged so downstream `mcp_auth::authorize` can take the
        // static-credentials path instead of DCR.
        let yaml = r#"
name: gmail
type: streamablehttp
url: https://gmailmcp.googleapis.com/mcp/v1
auth:
  type: oauth
  client_id: "1234567890.apps.googleusercontent.com"
  client_secret: "GOCSPX-secretzzz"
  scopes:
    - https://www.googleapis.com/auth/gmail.readonly
    - https://www.googleapis.com/auth/gmail.compose
"#;
        let config: McpServerConfig =
            serde_yaml::from_str(yaml).expect("static-creds oauth config must parse");
        assert!(config.validate().is_ok());
        let resolved = config.auth_resolved().expect("resolved should be Some");
        match resolved {
            ResolvedAuth::Oauth {
                client_id,
                client_secret,
                scopes,
            } => {
                assert_eq!(
                    client_id.as_deref(),
                    Some("1234567890.apps.googleusercontent.com")
                );
                assert_eq!(client_secret.as_deref(), Some("GOCSPX-secretzzz"));
                assert_eq!(
                    scopes,
                    vec![
                        "https://www.googleapis.com/auth/gmail.readonly".to_string(),
                        "https://www.googleapis.com/auth/gmail.compose".to_string(),
                    ]
                );
            }
            other => panic!("expected ResolvedAuth::Oauth, got {other:?}"),
        }
    }

    #[test]
    fn oauth_public_client_no_secret_is_allowed() {
        // A client_id without a client_secret is a public-client config
        // (RFC 6749 §2.1) — not all OAuth servers require a secret. The
        // validator must accept it; the downstream OAuth flow will pass
        // `None` for the secret to rmcp's `OAuthClientConfig`.
        let yaml = r#"
name: public
type: streamablehttp
url: https://example.com/mcp
auth:
  type: oauth
  client_id: "public-app-id"
  scopes:
    - read
"#;
        let config: McpServerConfig =
            serde_yaml::from_str(yaml).expect("public-client oauth config must parse");
        assert!(config.validate().is_ok());
        let resolved = config.auth_resolved().expect("resolved should be Some");
        assert_eq!(
            resolved,
            ResolvedAuth::Oauth {
                client_id: Some("public-app-id".to_string()),
                client_secret: None,
                scopes: vec!["read".to_string()],
            }
        );
    }

    #[test]
    fn oauth_client_secret_without_client_id_is_rejected() {
        // The mirror of the public-client case: secret-without-id makes
        // no sense (the token endpoint authenticates by client_id). Must
        // fail at boot, not mid-OAuth-flow.
        let yaml = r#"
name: broken
type: streamablehttp
url: https://example.com/mcp
auth:
  type: oauth
  client_secret: "GOCSPX-orphaned-secret"
"#;
        let config: McpServerConfig =
            serde_yaml::from_str(yaml).expect("config must parse before validate runs");
        let err = config
            .validate()
            .expect_err("client_secret without client_id must fail validation");
        assert!(
            err.contains("client_secret") && err.contains("client_id"),
            "validation error must name both fields, got: {err}"
        );
    }

    #[test]
    fn oauth_block_rejects_unknown_inner_field() {
        // Sibling pin to `auth_block_rejects_unknown_inner_field` for
        // the bearer arm — must also fire on the oauth arm. A typo like
        // `scope:` (singular) on a Google-shape config would otherwise
        // silently lose the scopes and fail mid-consent.
        let yaml = r#"
name: typo
type: streamablehttp
url: https://example.com/mcp
auth:
  type: oauth
  client_id: "id"
  client_secret: "secret"
  scope:
    - read
"#;
        let err = serde_yaml::from_str::<McpServerConfig>(yaml)
            .expect_err("unknown field inside auth.oauth must reject");
        let msg = err.to_string();
        assert!(
            msg.contains("scope") || msg.contains("unknown field"),
            "deserialization error must name the unknown field, got: {msg}"
        );
    }

    // ── timeouts block (postmortem fix) ──────────────────────────────────
    //
    // The tool-call wall-clock budgets used to be hard-coded constants. They
    // get a YAML home now so an operator can tune them without recompiling,
    // and so the `validate()` boundary catches the obvious mistakes
    // (zero, >24h) before the tool ever fires. These pins lock down the
    // surface the new block exposes.

    #[test]
    fn timeouts_defaults_are_30min_tool_and_2h_delegate() {
        // Two boundary values, one observer: the defaults are the floor that
        // the boot configuration must produce, so absent any YAML the process
        // starts with a 30-minute tool ceiling and a 2-hour delegation
        // ceiling — anything tighter would surprise users who'd never seen
        // the postmortem.
        let t = TimeoutsConfig::default();
        assert_eq!(
            t.tool_secs, 1_800,
            "default tool_secs is 30 minutes (1800s); pin so a future tweak is deliberate"
        );
        assert_eq!(
            t.delegate_secs, 7_200,
            "default delegate_secs is 2 hours (7200s); pin so a future tweak is deliberate"
        );
    }

    #[test]
    fn timeouts_validate_rejects_zero() {
        // 0 is the documented "disabled" value for HTTP timeouts, but it is
        // NOT acceptable for tool / delegate budgets — a zero budget means
        // `tokio::time::timeout` fires immediately and every tool call
        // would surface as a TIMEOUT. Validate must reject 0 with a message
        // that names the field so an operator staring at the error knows
        // which knob to fix.
        for (field, cfg) in [
            (
                "tool_secs",
                TimeoutsConfig {
                    tool_secs: 0,
                    delegate_secs: 3_600,
                },
            ),
            (
                "delegate_secs",
                TimeoutsConfig {
                    tool_secs: 1_800,
                    delegate_secs: 0,
                },
            ),
        ] {
            let err = cfg.validate().expect_err("zero budget must be rejected");
            let msg = err.to_string();
            assert!(
                msg.contains(field),
                "error must name the offending field ({field}), got: {msg}"
            );
            assert!(
                msg.contains('0') || msg.contains("zero"),
                "error must mention the offending value (0/zero), got: {msg}"
            );
        }
    }

    #[test]
    fn timeouts_validate_rejects_above_24h() {
        // 86400s is the documented upper bound (1 day); 86401 must be
        // rejected to catch an unbounded `u64` slipping into production.
        // Above 24h a tool budget would exceed the session's TTL (10 min
        // default!) and the wall clock becomes a sleep, not a deadline.
        for (field, cfg) in [
            (
                "tool_secs",
                TimeoutsConfig {
                    tool_secs: 86_401,
                    delegate_secs: 3_600,
                },
            ),
            (
                "delegate_secs",
                TimeoutsConfig {
                    tool_secs: 1_800,
                    delegate_secs: 86_401,
                },
            ),
        ] {
            let err = cfg.validate().expect_err("budget > 24h must be rejected");
            assert!(
                err.to_string().contains(field),
                "error must name the offending field ({field}), got: {err}"
            );
        }
    }

    #[test]
    fn timeouts_validate_accepts_the_bounds() {
        // Boundary test: 1 (the smallest legal value) and 86400 (the
        // largest) must BOTH be accepted. Without this the rejection logic
        // could go off-by-one in either direction and silently exclude the
        // legal edge.
        for cfg in [
            TimeoutsConfig {
                tool_secs: 1,
                delegate_secs: 3_600,
            },
            TimeoutsConfig {
                tool_secs: 1_800,
                delegate_secs: 1,
            },
            TimeoutsConfig {
                tool_secs: 86_400,
                delegate_secs: 3_600,
            },
            TimeoutsConfig {
                tool_secs: 1_800,
                delegate_secs: 86_400,
            },
        ] {
            assert!(
                cfg.validate().is_ok(),
                "configs at the boundaries must validate; got error for {cfg:?}"
            );
        }
    }

    #[test]
    fn http_read_timeout_defaults_to_30_minutes() {
        // Pins the new default literally via the default fn, rather than
        // relying on `assert_eq!(cfg.http.read_timeout_secs, 1800)` in the
        // round-trip test: the implementer must change BOTH the function
        // AND the YAML parser default, in the same direction, and this
        // test catches the case where one of the two is forgotten.
        assert_eq!(
            default_read_timeout_secs(),
            1_800,
            "default_read_timeout_secs() must return 1800 (30 min); was 600 pre-postmortem fix"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Reasoning-preservation knobs on `AnthropicConfig` (design §2.2 / §8 — T2).
    //
    // The provider-level `preserve_reasoning: Option<bool>` and
    // `display_reasoning: Option<bool>` override the per-model defaults.
    // `None` = inherit model; `Some(b)` = force. They must parse cleanly
    // and existing configs without the fields must still load.
    // ─────────────────────────────────────────────────────────────────────────

    /// The provider-level `preserve_reasoning` field parses; absent
    /// defaults to `None` (inherit model).
    #[test]
    fn anthropic_config_preserve_reasoning_parses_and_defaults_to_none() {
        let yaml = r#"
model: "claude-3-5-sonnet-latest"
api_key: "sk-test"
preserve_reasoning: false
"#;
        let cfg: AnthropicConfig =
            serde_yaml::from_str(yaml).expect("provider-level preserve_reasoning parses");
        assert_eq!(cfg.preserve_reasoning, Some(false));

        let yaml_default = r#"
model: "claude-3-5-sonnet-latest"
api_key: "sk-test"
"#;
        let cfg_default: AnthropicConfig =
            serde_yaml::from_str(yaml_default).expect("legacy config without field parses");
        assert_eq!(cfg_default.preserve_reasoning, None);
    }

    /// The provider-level `display_reasoning` field parses; absent
    /// defaults to `None` (inherit model).
    #[test]
    fn anthropic_config_display_reasoning_parses_and_defaults_to_none() {
        let yaml = r#"
model: "claude-3-5-sonnet-latest"
api_key: "sk-test"
display_reasoning: true
"#;
        let cfg: AnthropicConfig =
            serde_yaml::from_str(yaml).expect("provider-level display_reasoning parses");
        assert_eq!(cfg.display_reasoning, Some(true));

        let yaml_default = r#"
model: "claude-3-5-sonnet-latest"
api_key: "sk-test"
"#;
        let cfg_default: AnthropicConfig =
            serde_yaml::from_str(yaml_default).expect("legacy config without field parses");
        assert_eq!(cfg_default.display_reasoning, None);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // `max_image_base64_bytes` on `AnthropicConfig` (PR-A §A2, TEST PLAN #17).
    //
    // Measured on the base64 length, not raw bytes — the number the API's
    // image-size limit actually counts. Default 5 MiB (api.anthropic.com's
    // documented ceiling); a typo'd key must be a hard startup error, not a
    // silent default, per `#[serde(deny_unknown_fields)]` on the struct.
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn anthropic_max_image_base64_bytes_defaults_to_5_mib() {
        let yaml_absent = r#"
model: "claude-3-5-sonnet-latest"
api_key: "sk-test"
"#;
        let cfg: AnthropicConfig =
            serde_yaml::from_str(yaml_absent).expect("config without the key must still parse");
        assert_eq!(
            cfg.max_image_base64_bytes,
            5 * 1024 * 1024,
            "absent key must default to 5 MiB"
        );

        let yaml_override = r#"
model: "claude-3-5-sonnet-latest"
api_key: "sk-test"
max_image_base64_bytes: 10485760
"#;
        let cfg_override: AnthropicConfig =
            serde_yaml::from_str(yaml_override).expect("explicit override must parse");
        assert_eq!(
            cfg_override.max_image_base64_bytes,
            10 * 1024 * 1024,
            "an explicit value must override the default"
        );

        let yaml_typo = r#"
model: "claude-3-5-sonnet-latest"
api_key: "sk-test"
max_image_bytes: 10485760
"#;
        let err = serde_yaml::from_str::<AnthropicConfig>(yaml_typo).expect_err(
            "a typo'd key (max_image_bytes) must be a hard error, not silently ignored",
        );
        let msg = err.to_string();
        assert!(
            msg.contains("max_image_bytes") || msg.to_lowercase().contains("unknown field"),
            "error should point at the unrecognized key: {msg}"
        );
    }

    // ── provider configs ─────────────────────────────────────────────────

    #[test]
    fn test_anthropic_prompt_caching_defaults_auto() {
        let cfg: AnthropicConfig =
            serde_yaml::from_str("model: claude-sonnet-4-5\n").expect("must parse");
        assert_eq!(cfg.prompt_caching, AnthropicCaching::Auto);
    }

    #[test]
    fn test_anthropic_prompt_caching_modes_parse() {
        for (s, want) in [
            ("manual", AnthropicCaching::Manual),
            ("auto", AnthropicCaching::Auto),
            ("auto_1h", AnthropicCaching::Auto1h),
            ("off", AnthropicCaching::Off),
        ] {
            let yaml = format!("model: m\nprompt_caching: {s}\n");
            let cfg: AnthropicConfig = serde_yaml::from_str(&yaml).expect("must parse");
            assert_eq!(cfg.prompt_caching, want, "mode {s}");
        }
    }

    // ── http / timeouts / fetch_url ──────────────────────────────────────

    #[test]
    fn http_timeouts_default_and_parse() {
        // Absent block: a config that never mentions http still gets bounded.
        let http: HttpConfig = serde_yaml::from_str("{}").unwrap();
        assert_eq!(http, HttpConfig::default());
        assert_eq!(http.connect_timeout_secs, 30);
        // The postmortem fix raised the read-timeout ceiling from 600 (10 min)
        // to 1800 (30 min) so a model that legitimately thinks for more than ten
        // minutes on a single non-streaming turn doesn't get cut off by the
        // HTTP backstop. Pinned here so a future tweak to the default is
        // deliberate.
        assert_eq!(http.read_timeout_secs, 1800);

        // Partial block: the unspecified knob keeps its default.
        let http: HttpConfig = serde_yaml::from_str("read_timeout_secs: 1200\n").unwrap();
        assert_eq!(http.connect_timeout_secs, 30);
        assert_eq!(http.read_timeout_secs, 1200);

        // 0 is the documented "disabled" escape hatch, not an error.
        let http: HttpConfig = serde_yaml::from_str("read_timeout_secs: 0\n").unwrap();
        assert_eq!(http.read_timeout_secs, 0);
    }

    #[test]
    fn absent_timeouts_block_yields_defaults() {
        // An empty `timeouts:` mapping gets the documented defaults: each
        // field has its own `#[serde(default)]`.
        let t: TimeoutsConfig = serde_yaml::from_str("{}").expect("empty block must parse");
        assert_eq!(t, TimeoutsConfig::default());
    }

    #[test]
    fn timeouts_block_parses_both_fields() {
        let t: TimeoutsConfig = serde_yaml::from_str("tool_secs: 42\ndelegate_secs: 99\n")
            .expect("both fields present must parse");
        assert_eq!(t.tool_secs, 42);
        assert_eq!(t.delegate_secs, 99);
    }

    #[test]
    fn timeouts_partial_block_defaults_the_other_field() {
        // Leaving one key out must default the OTHER key, not leave it zero —
        // an operator who only tunes one knob must not silently disable the
        // other.
        let t: TimeoutsConfig =
            serde_yaml::from_str("tool_secs: 120\n").expect("only-tool_secs block must parse");
        assert_eq!(t.tool_secs, 120, "explicitly set value must be retained");
        assert_eq!(
            t.delegate_secs, 7_200,
            "unset delegate_secs must default to 7200, not zero"
        );

        let t: TimeoutsConfig = serde_yaml::from_str("delegate_secs: 99\n")
            .expect("only-delegate_secs block must parse");
        assert_eq!(t.delegate_secs, 99);
        assert_eq!(
            t.tool_secs, 1_800,
            "unset tool_secs must default to 1800, not zero"
        );
    }

    #[test]
    fn timeouts_rejects_unknown_field() {
        // A typo inside `timeouts:` must be a hard parse error, not silently
        // dropped — silently-dropped knobs caused the original incident.
        let err = serde_yaml::from_str::<TimeoutsConfig>("tool_seccs: 42\n")
            .expect_err("unknown field inside timeouts block must fail to parse");
        let msg = err.to_string();
        assert!(
            msg.contains("tool_seccs"),
            "error must name the offending key, got: {msg}"
        );
    }

    #[test]
    fn fetch_url_absent_defaults_allow_post_false() {
        let cfg: FetchUrlConfig = serde_yaml::from_str("{}").expect("must parse");
        assert_eq!(cfg, FetchUrlConfig::default());
        assert!(!cfg.allow_post);
    }

    #[test]
    fn fetch_url_allow_post_true_parses() {
        let cfg: FetchUrlConfig = serde_yaml::from_str("allow_post: true\n").expect("must parse");
        assert!(cfg.allow_post);
    }

    #[test]
    fn fetch_url_unknown_field_rejected() {
        let err = serde_yaml::from_str::<FetchUrlConfig>("allow_put: true\n")
            .expect_err("deny_unknown_fields must reject allow_put");
        assert!(err.to_string().contains("allow_put"), "got: {err}");
    }

    // ── canonical name lists ─────────────────────────────────────────────

    /// The canonical tab universe: exactly these seven names, in this order.
    #[test]
    fn tab_names_is_the_seven_canonical_tabs_in_order() {
        assert_eq!(
            TAB_NAMES,
            [
                "session", "todo", "files", "tasks", "bash", "agents", "settings"
            ],
            "TAB_NAMES must list the seven web tabs in canonical order"
        );
        let mut seen = std::collections::HashSet::new();
        for name in TAB_NAMES {
            assert!(seen.insert(name), "TAB_NAMES lists '{name}' twice");
        }
    }

    /// `delegate` is on iff an agent has subagents — it must not be offered
    /// as a filterable tool name.
    #[test]
    fn builtin_tool_names_exclude_delegate() {
        assert!(!BUILTIN_TOOL_NAMES.contains(&"delegate"));
    }

    // ── resolve_token / cost_tracking_enabled ────────────────────────────

    #[test]
    fn resolve_token_precedence_is_cli_then_env_then_config() {
        assert_eq!(
            resolve_token(Some("c"), Some("e"), Some("x")),
            Some("c".to_string())
        );
        assert_eq!(
            resolve_token(None, Some("e"), Some("x")),
            Some("e".to_string())
        );
        assert_eq!(resolve_token(None, None, Some("x")), Some("x".to_string()));
        assert_eq!(resolve_token(None, None, None), None);
    }

    #[test]
    fn resolve_token_skips_empty_and_whitespace_and_trims_the_winner() {
        assert_eq!(
            resolve_token(Some(""), Some("e"), Some("x")),
            Some("e".to_string())
        );
        assert_eq!(
            resolve_token(Some("   "), Some("  "), Some(" x\n")),
            Some("x".to_string())
        );
        assert_eq!(resolve_token(Some(""), Some("   "), Some("\n")), None);
    }

    fn model_of(kind: ProviderType) -> ResolvedModel {
        let provider = ProviderEntry {
            name: "p".into(),
            kind,
            api_key: None,
            base_url: None,
            models: vec![ModelEntry {
                name: "m".into(),
                alias: Some("m".into()),
                max_tokens: None,
                temperature: None,
                extra_params: None,
                prompt_caching: None,
                vision: None,
                context_size: Some(1000),
                preserve_reasoning: true,
                display_reasoning: false,
            }],
            preserve_reasoning: None,
            display_reasoning: None,
        };
        let registry = ModelRegistry::build(&[provider]).expect("registry builds");
        registry.resolve("m").expect("alias resolves").clone()
    }

    #[test]
    fn cost_tracking_needs_the_flag_and_an_openrouter_model() {
        let openrouter = model_of(ProviderType::OpenRouter);
        assert!(cost_tracking_enabled(true, &openrouter));
        assert!(!cost_tracking_enabled(false, &openrouter));
        for kind in [
            ProviderType::OpenAI,
            ProviderType::Anthropic,
            ProviderType::LlamaCpp,
            ProviderType::Ollama,
        ] {
            assert!(
                !cost_tracking_enabled(true, &model_of(kind.clone())),
                "{kind} never tracks cost"
            );
        }
    }

    // ── save_config_at (moved from tests/save_master_config_tests.rs) ────

    #[test]
    fn save_config_at_writes_exact_bytes_first_time_without_backup() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "providers: []\n# comment kept verbatim\n";
        let outcome = save_config_at(dir.path(), yaml).expect("first write must succeed");
        assert_eq!(outcome.path, dir.path().join("config.yaml"));
        assert_eq!(std::fs::read_to_string(&outcome.path).unwrap(), yaml);
        assert!(outcome.backup.is_none(), "a first write has no backup");
        assert!(!dir.path().join("config.yaml.bak").exists());
    }

    #[cfg(unix)]
    #[test]
    fn save_config_at_writes_file_and_backup_with_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        save_config_at(dir.path(), "x: 1\n").unwrap();
        let outcome = save_config_at(dir.path(), "x: 2\n").unwrap();
        let backup = outcome.backup.expect("second write has a backup");
        for path in [&outcome.path, &backup] {
            let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{} must be 0600, got {mode:o}", path.display());
        }
    }

    #[test]
    fn save_config_at_backup_is_a_single_slot_of_the_previous_bytes() {
        let dir = tempfile::tempdir().unwrap();
        save_config_at(dir.path(), "x: 1\n").unwrap();
        save_config_at(dir.path(), "x: 2\n").unwrap();
        let outcome = save_config_at(dir.path(), "x: 3\n").unwrap();
        let backup = outcome.backup.expect("backup");
        assert_eq!(backup, dir.path().join("config.yaml.bak"));
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "x: 2\n");
        assert_eq!(std::fs::read_to_string(&outcome.path).unwrap(), "x: 3\n");
    }

    #[test]
    fn save_config_at_leaves_no_tmp_file_and_creates_missing_dirs() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("a").join("b");
        save_config_at(&nested, "x: 1\n").expect("nested write must succeed");
        save_config_at(&nested, "x: 2\n").unwrap();
        assert!(nested.join("config.yaml").exists());
        assert!(!nested.join("config.yaml.tmp").exists(), "no tmp survivor");
    }
}
