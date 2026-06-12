// Allow dead_code for provider-specific config structs - not all providers may be used
// depending on build configuration. The enum variants are accessed via deserialization.
#![allow(dead_code)]

pub mod model_registry;
pub use model_registry::{ModelEntry, ModelRegistry, ProviderEntry, RegistryError, ResolvedModel};

use directories_next::ProjectDirs;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;

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

/// Anthropic prompt-caching mode. Injects ephemeral `cache_control`
/// breakpoints to cut input-token cost on the stable prefix of each request.
///
/// `Manual` mirrors LiteLLM's `system/0` + `user/-1` injection points
/// (rig also marks the tool-schema block, within Anthropic's 4-breakpoint
/// budget). `Auto*` use Anthropic's top-level breakpoint that advances as the
/// conversation grows — recommended for multi-turn.
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

/// Configuration for Anthropic provider (Claude API, or any server that
/// speaks the Anthropic Messages API — notably llama.cpp's `/v1/messages`).
///
/// Unlike the OpenAI-completions and Responses APIs, the Anthropic Messages
/// API carries images inside `tool_result` blocks. That is the whole reason
/// this provider exists as a first-class option: pointing `base_url` at a
/// local llama-server lets the `view_image` tool feed pixels back to a local
/// multimodal model.
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
    /// Explicit image-support override. `None` (default) → auto-detect (the
    /// Anthropic transport carries images, so this is on by default). On this
    /// provider the flag also gates the `view_image` tool: `Some(true)` shows it
    /// and allows `[img:…]`; `Some(false)` hides the tool and refuses `[img:…]`.
    #[serde(default)]
    pub vision: Option<bool>,
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

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// LLM Provider configuration (legacy single-provider shape).
    /// Used as the *fallback* when no `providers:` list is declared.
    /// Always non-empty thanks to the `default_provider_config()` synth
    /// path; `providers:` (when present) takes precedence at boot.
    #[serde(default)]
    pub provider: ProviderConfig,

    /// Multi-model providers list. When non-empty, this is the source
    /// of truth — `provider:` is ignored. Each entry owns its
    /// `models:` list.
    #[serde(default)]
    pub providers: Vec<ProviderEntry>,

    /// The alias to boot with. Required iff `providers:` is non-empty.
    #[serde(default)]
    pub default_model: Option<String>,

    /// DEPRECATED: Use provider.config.model instead
    /// Maximum tool turns per message
    #[serde(default = "default_max_turns")]
    pub agent_max_turns: usize,
    /// MCP servers configuration (YAML array)
    #[serde(default)]
    pub mcp_servers: Option<Vec<McpServerConfig>>,
    /// SearXNG search configuration
    #[serde(default)]
    pub searxng: Option<SearXngConfig>,
    /// Enable token cost tracking (default: true)
    #[serde(default = "default_cost_tracking")]
    pub cost_tracking: bool,
    /// Context compaction configuration (enabled by default when not specified)
    #[serde(default)]
    pub context: ContextConfig,
    /// Memory.md automatic compaction configuration
    #[serde(default)]
    pub memory: MemoryConfig,
    /// Conversation persistence configuration
    #[serde(default)]
    pub conversation: Option<ConversationConfig>,
    /// Bash tool configuration (env vars, etc.)
    #[serde(default)]
    pub bash: BashConfig,
    /// Retry configuration for API errors
    #[serde(default)]
    pub retry: RetryConfig,
    /// Multi-agent pipeline configuration
    #[serde(default)]
    pub pipeline: Option<PipelineConfig>,
    /// Vector DB configuration (doc_index / doc_search tools).
    /// When absent, both tools are skipped entirely (not registered).
    #[serde(default)]
    pub vector_db: Option<VectorDbConfig>,
}

/// Multi-agent pipeline configuration
#[derive(Debug, Deserialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct PipelineConfig {
    /// Whether multi-agent pipelines are enabled (default: false)
    #[serde(default)]
    pub enabled: bool,

    /// Sub-agent definitions keyed by agent name
    #[serde(default)]
    pub agents: HashMap<String, AgentDefinition>,
}

/// Definition of a sub-agent that can be delegated to
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinition {
    /// Agent type (must match a provider type)
    #[serde(rename = "type")]
    pub agent_type: ProviderType,

    /// Model to use for this agent
    /// If not specified, uses the default model for the provider type
    #[serde(default)]
    pub model: Option<String>,

    /// System prompt / preamble for this agent
    pub prompt: String,

    /// Optional: max tokens override (uses provider default if not specified)
    #[serde(default)]
    pub max_tokens: Option<u64>,

    /// Optional: temperature override (uses provider default if not specified)
    #[serde(default)]
    pub temperature: Option<f32>,

    /// Optional: API key (uses main provider key if not specified)
    #[serde(default)]
    pub api_key: Option<String>,

    /// Optional: base URL override
    #[serde(default)]
    pub base_url: Option<String>,
}

impl PipelineConfig {
    /// Check if the pipeline has any agents defined
    pub fn has_agents(&self) -> bool {
        !self.agents.is_empty()
    }

    /// Get the names of all defined agents
    pub fn agent_names(&self) -> Vec<&str> {
        self.agents.keys().map(|s| s.as_str()).collect()
    }

    /// Get an agent definition by name
    pub fn get_agent(&self, name: &str) -> Option<&AgentDefinition> {
        self.agents.get(name)
    }
}

impl Config {
    /// Get the model name from the provider config
    pub fn model(&self) -> &str {
        match &self.provider {
            ProviderConfig::OpenRouter(c) => &c.model,
            ProviderConfig::OpenAI(c) => &c.model,
            ProviderConfig::Anthropic(c) => &c.model,
            ProviderConfig::LlamaCpp(c) => &c.model,
            ProviderConfig::Ollama(c) => &c.model,
        }
    }

    /// Get the max tokens from the provider config
    pub fn max_tokens(&self) -> u64 {
        match &self.provider {
            ProviderConfig::OpenRouter(c) => c.max_tokens,
            ProviderConfig::OpenAI(c) => c.max_tokens,
            ProviderConfig::Anthropic(c) => c.max_tokens,
            ProviderConfig::LlamaCpp(c) => c.max_tokens,
            ProviderConfig::Ollama(_) => 4096, // Ollama doesn't have this setting in the same way
        }
    }

    /// Get the API key for OpenRouter (if applicable)
    pub fn openrouter_api_key(&self) -> Option<&str> {
        match &self.provider {
            ProviderConfig::OpenRouter(c) => c.api_key.as_deref(),
            ProviderConfig::OpenAI(c) => c.api_key.as_deref(),
            ProviderConfig::Anthropic(c) => c.api_key.as_deref(),
            ProviderConfig::LlamaCpp(c) => c.api_key.as_deref(),
            ProviderConfig::Ollama(_) => None,
        }
    }

    /// Check if cost tracking is supported (only for OpenRouter)
    pub fn supports_pricing(&self) -> bool {
        matches!(self.provider, ProviderConfig::OpenRouter(_))
    }

    /// Get the provider name as a string
    pub fn provider_name(&self) -> &str {
        match &self.provider {
            ProviderConfig::OpenRouter(_) => "openrouter",
            ProviderConfig::OpenAI(_) => "openai",
            ProviderConfig::Anthropic(_) => "anthropic",
            ProviderConfig::LlamaCpp(_) => "llamacpp",
            ProviderConfig::Ollama(_) => "ollama",
        }
    }

    /// Get the SearXNG base URL
    pub fn searxng_base_url(&self) -> Option<String> {
        self.searxng.as_ref().map(|c| c.base_url.clone())
    }

    /// Check if SearXNG is configured and enabled
    pub fn searxng_enabled(&self) -> bool {
        self.searxng
            .as_ref()
            .map(|c| c.enabled && !c.base_url.is_empty())
            .unwrap_or(false)
    }

    /// Check if conversation persistence is enabled
    pub fn conversation_enabled(&self) -> bool {
        self.conversation
            .as_ref()
            .map(|c| c.auto_save)
            .unwrap_or(true)
    }

    /// Get the conversation storage directory
    pub fn conversation_storage_dir(&self) -> std::path::PathBuf {
        self.conversation
            .as_ref()
            .and_then(|c| c.storage_dir.clone())
            .unwrap_or_else(|| {
                dirs::data_local_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join("peakbot")
                    .join("conversations")
            })
    }

    /// Get max conversations setting
    pub fn conversation_max(&self) -> usize {
        self.conversation
            .as_ref()
            .map(|c| c.max_conversations)
            .unwrap_or(50)
    }

    /// Check if auto-resume is enabled
    pub fn conversation_auto_resume(&self) -> bool {
        self.conversation
            .as_ref()
            .map(|c| c.auto_resume)
            .unwrap_or(true)
    }

    /// Get the bash tool environment variables
    pub fn bash_env(&self) -> Option<&HashMap<String, String>> {
        self.bash.env.as_ref()
    }

    /// Get the retry configuration
    pub fn retry(&self) -> &RetryConfig {
        &self.retry
    }

    /// Get pipeline configuration if present
    pub fn pipeline(&self) -> Option<&PipelineConfig> {
        self.pipeline.as_ref()
    }

    /// Check if multi-agent pipelines are enabled
    pub fn pipeline_enabled(&self) -> bool {
        self.pipeline
            .as_ref()
            .map(|p| p.enabled && !p.agents.is_empty())
            .unwrap_or(false)
    }

    /// Build a [`ModelRegistry`] from the loaded config.
    ///
    /// Two paths:
    ///
    /// 1. **Multi-model** (`providers:` non-empty): validates the
    ///    declared providers + models, returns the populated registry.
    ///    Returns an error if validation fails.
    ///
    /// 2. **Legacy single-provider** (`providers:` empty): synthesises
    ///    a one-entry registry whose alias is `default`. Internal
    ///    contract: the legacy `provider:` block is wrapped without
    ///    user-visible behaviour change. The synthesised alias is
    ///    `default` and the default_model is `default`.
    ///
    /// *(necessarily same — they really are the same operation)*
    pub fn build_model_registry(&self) -> Result<ModelRegistry, RegistryError> {
        if self.providers.is_empty() {
            // Legacy synthesis path. We construct a single-entry list
            // whose ProviderEntry copies the legacy `provider:` shape.
            // The wire id is read off the active legacy variant.
            let (
                kind,
                api_key,
                base_url,
                model_name,
                max_tokens,
                temperature,
                extra,
                caching,
                vision,
            ) = describe_legacy(&self.provider);
            let synthetic_alias = "default".to_string();
            let provider_entry = ProviderEntry {
                name: kind.to_string(),
                kind: kind.clone(),
                api_key,
                base_url,
                models: vec![ModelEntry {
                    name: model_name,
                    alias: Some(synthetic_alias.clone()),
                    max_tokens: Some(max_tokens),
                    temperature,
                    extra_params: extra,
                    prompt_caching: caching,
                    vision,
                    context_size: None,
                }],
            };
            return ModelRegistry::build(
                std::slice::from_ref(&provider_entry),
                Some(&synthetic_alias),
            );
        }

        ModelRegistry::build(&self.providers, self.default_model.as_deref())
    }

    /// Resolve the boot provider config from the registry and mirror it
    /// into `self.provider`, returning a clone of the resolved value for
    /// use with `create_provider`.
    ///
    /// In multi-model mode the legacy `self.provider` field defaults to
    /// `OpenRouterConfig { api_key: None, … }` — leftover scaffolding
    /// that downstream consumers like `AgentRunner::new`'s Layer 1
    /// compaction-model construction still read. Without this mirror,
    /// boot fails with a misleading "OpenRouter API key not configured"
    /// even when the real credentials are sitting in the registry. The
    /// `/model` switch path already maintains this invariant at
    /// `lib.rs:1082`; boot must match.
    ///
    /// When no registry default alias exists (pure legacy single-
    /// provider config), `self.provider` is already authoritative — this
    /// is a no-op that returns its clone.
    ///
    /// *(simplicity is the key — one field tracks the active provider)*
    pub fn resolve_and_mirror_boot_provider(&mut self, registry: &ModelRegistry) -> ProviderConfig {
        let resolved = match registry.default_alias() {
            Some(alias) => registry
                .resolve(alias)
                .expect("default_alias is guaranteed to resolve by ModelRegistry::build")
                .provider_config
                .clone(),
            None => self.provider.clone(),
        };
        self.provider = resolved.clone();
        resolved
    }
}

/// Describe the legacy `provider:` block in a tuple shape that the
/// synth path can splat into a `(ProviderEntry, ModelEntry)` pair.
/// Tuple type is private to this module — small, local, single use.
#[allow(clippy::type_complexity)]
fn describe_legacy(
    p: &ProviderConfig,
) -> (
    ProviderType,
    Option<String>,
    Option<String>,
    String,
    u64,
    Option<f32>,
    Option<serde_json::Value>,
    Option<AnthropicCaching>,
    Option<bool>,
) {
    match p {
        ProviderConfig::OpenRouter(c) => (
            ProviderType::OpenRouter,
            c.api_key.clone(),
            None,
            c.model.clone(),
            c.max_tokens,
            None,
            None,
            None,
            c.vision,
        ),
        ProviderConfig::OpenAI(c) => (
            ProviderType::OpenAI,
            c.api_key.clone(),
            Some(c.base_url.clone()),
            c.model.clone(),
            c.max_tokens,
            None,
            None,
            None,
            c.vision,
        ),
        ProviderConfig::Anthropic(c) => (
            ProviderType::Anthropic,
            c.api_key.clone(),
            Some(c.base_url.clone()),
            c.model.clone(),
            c.max_tokens,
            None,
            None,
            Some(c.prompt_caching.clone()),
            c.vision,
        ),
        ProviderConfig::LlamaCpp(c) => (
            ProviderType::LlamaCpp,
            c.api_key.clone(),
            Some(c.base_url.clone()),
            c.model.clone(),
            c.max_tokens,
            None,
            c.extra_params.clone(),
            None,
            c.vision,
        ),
        ProviderConfig::Ollama(c) => (
            ProviderType::Ollama,
            None,
            Some(c.base_url.clone()),
            c.model.clone(),
            // Ollama has no max_tokens — use the default placeholder.
            default_max_tokens(),
            c.temperature,
            None,
            None,
            c.vision,
        ),
    }
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
    /// Streamable HTTP transport (recommended for remote servers)
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

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SearXngConfig {
    /// Base URL of the SearXNG instance (e.g., "https://searx.example.com")
    pub base_url: String,
    /// Enable/disable search (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Request timeout in seconds (default: 30)
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    /// Default maximum number of results to return (default: 10)
    #[serde(default = "default_max_results")]
    pub max_results: u32,
    /// Optional bearer token sent as `Authorization: Bearer <token>`.
    /// Use for SearXNG instances behind an auth proxy. Stored in plaintext
    /// in config.yaml — do not commit the file to a public repo.
    #[serde(default)]
    pub bearer_token: Option<String>,
}

/// Vector DB configuration for the `doc_index` / `doc_search` tools.
///
/// When this block is present and `enabled`, a single shared `VectorStore`
/// is opened at startup and injected into both tools. When absent (or
/// `enabled: false`), neither tool is registered — there is no silent no-op.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VectorDbConfig {
    /// Enable/disable the vector tools (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Path to the redb-backed vector store. Per-repo by default.
    /// ⚠ On reopen, ruvector rebuilds from disk and uses the STORED config
    /// (dimensions, metric) — the dimensions below are only used to CREATE a
    /// new DB. A model whose dims differ from an existing DB is rejected.
    #[serde(default = "default_vector_db_path")]
    pub db_path: String,
    /// Embeddings endpoint configuration (independent of the chat provider).
    pub embeddings: EmbeddingsConfig,
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

fn default_vector_db_path() -> String {
    "./.peakbot/vectors.db".to_string()
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
fn default_timeout() -> u64 {
    30
}
fn default_max_results() -> u32 {
    10
}

fn default_model() -> String {
    "anthropic/claude-3.7-sonnet".to_string()
}

fn default_max_tokens() -> u64 {
    4096
}

fn default_max_turns() -> usize {
    50
}

fn default_cost_tracking() -> bool {
    true
}

/// Configuration for memory.md automatic compaction
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {
    /// Enable automatic memory compaction at conversation start (default: true)
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

/// Configuration for conversation persistence
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConversationConfig {
    /// Enable auto-save (default: true)
    #[serde(default = "default_true")]
    pub auto_save: bool,
    /// Storage directory (default: platform data dir)
    #[serde(default)]
    pub storage_dir: Option<std::path::PathBuf>,
    /// Maximum conversations to keep (default: 50, 0 = unlimited)
    #[serde(default = "default_max_conversations")]
    pub max_conversations: usize,
    /// Auto-load last conversation on startup (default: true)
    #[serde(default = "default_true")]
    pub auto_resume: bool,
}

/// Configuration for the bash tool
#[derive(Debug, Deserialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct BashConfig {
    /// Environment variables to set when running bash commands
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
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

fn default_max_conversations() -> usize {
    50
}
impl Config {
    /// Merge another config into this one (top-level key override).
    /// Fields present in `other` replace the corresponding fields in `self`.
    /// Fields absent in `other` (or left as defaults) are left unchanged.
    ///
    /// This implements shallow merge - if a top-level key is specified
    /// in `other`, the entire corresponding field in `self` is replaced.
    pub fn merge_with(&mut self, other: Config) {
        // Only override if other has non-default provider
        if other.provider != ProviderConfig::default() {
            self.provider = other.provider;
        }

        // agent_max_turns - only override if explicitly set to non-default
        if other.agent_max_turns != default_max_turns() {
            self.agent_max_turns = other.agent_max_turns;
        }

        // mcp_servers - only override if explicitly set
        if other.mcp_servers.is_some() {
            self.mcp_servers = other.mcp_servers;
        }

        // searxng - override if set
        if other.searxng.is_some() {
            self.searxng = other.searxng;
        }

        // cost_tracking - only override if explicitly set to non-default
        if other.cost_tracking != default_cost_tracking() {
            self.cost_tracking = other.cost_tracking;
        }

        // context - always override if other has non-default
        if other.context != ContextConfig::default() {
            self.context = other.context;
        }

        // memory - always override if other has non-default
        if other.memory != MemoryConfig::default() {
            self.memory = other.memory;
        }

        // conversation - override if set
        if other.conversation.is_some() {
            self.conversation = other.conversation;
        }

        // bash - only override if explicitly set
        if other.bash != BashConfig::default() {
            self.bash = other.bash;
        }

        // retry - always override if other has non-default
        if other.retry != RetryConfig::default() {
            self.retry = other.retry;
        }

        // pipeline - override if set
        if other.pipeline.is_some() {
            self.pipeline = other.pipeline;
        }

        // providers list - override if non-empty (per-repo overrides
        // the master providers list wholesale)
        if !other.providers.is_empty() {
            self.providers = other.providers;
        }

        // default_model - override if explicitly set (per-repo can pin
        // a different boot model on top of the master's providers list)
        if other.default_model.is_some() {
            self.default_model = other.default_model;
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: ProviderConfig::default(),
            providers: Vec::new(),
            default_model: None,
            agent_max_turns: default_max_turns(),
            mcp_servers: None,
            searxng: None,
            cost_tracking: default_cost_tracking(),
            context: ContextConfig {
                threshold: default_threshold(),
                keep_recent: default_keep_recent(),
                enabled: default_context_enabled(),
                compaction_model: None,
            },
            memory: MemoryConfig {
                enabled: true,
                threshold_bytes: default_memory_threshold(),
            },
            conversation: None,
            bash: BashConfig::default(),
            retry: RetryConfig::default(),
            pipeline: None,
            vector_db: None,
        }
    }
}

/// Get the platform-specific config directory
fn get_config_dir() -> Option<std::path::PathBuf> {
    ProjectDirs::from("com", "peakbot", "peakbot").map(|dirs| dirs.config_dir().to_path_buf())
}

/// Get the platform-specific config file path.
/// Returns the full path to config.yaml (e.g., ~/.config/peakbot/peakbot/config.yaml).
/// Returns None if the platform doesn't provide a config directory.
pub fn get_config_file_path() -> Option<std::path::PathBuf> {
    get_config_dir().map(|dir| dir.join("config.yaml"))
}

/// Result of loading configuration, including metadata about what was loaded.
pub struct LoadedConfig {
    /// The loaded configuration
    pub config: Config,
    /// Whether the master config file was found (true if file existed and was parsed)
    pub config_file_found: bool,
    /// The path where the master config was found (None if not found)
    pub config_file_path: Option<std::path::PathBuf>,
}

/// Load configuration from YAML file in the platform config directory.
///
/// Returns:
/// - `Ok((None, None))` if the config directory doesn't exist on this platform
/// - `Ok((None, Some(path)))` if the config file doesn't exist (path = where it should be)
/// - `Ok((Some(config), Some(path)))` on a clean parse
/// - `Err(_)` if the file exists but is unreadable or malformed. We
///   refuse to silently fall back to defaults: a typo'd YAML used to
///   mask itself as "no API key configured" and made the boot path
///   inscrutable. Better to fail loud at the source. *(principle of
///   least astonishment)*
fn load_yaml_config() -> anyhow::Result<(Option<Config>, Option<std::path::PathBuf>)> {
    let Some(config_dir) = get_config_dir() else {
        return Ok((None, None));
    };
    let config_path = config_dir.join("config.yaml");

    tracing::debug!("Looking for config at: {}", config_path.display());

    if !config_path.exists() {
        tracing::debug!("No YAML config found at {}", config_path.display());
        return Ok((None, Some(config_path)));
    }

    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {e}", config_path.display()))?;
    tracing::debug!("Config file content: {}", content);

    let config: Config = serde_yaml::from_str(&content)
        .map_err(|e| anyhow::anyhow!("Failed to parse {} as YAML: {e}", config_path.display()))?;

    tracing::info!("Loaded YAML config from {}", config_path.display());
    Ok((Some(config), Some(config_path)))
}

/// Load per-repository configuration from `.peakbot/config.yaml` in the current working directory.
/// If the file exists and is valid, returns the parsed config.
/// If the file doesn't exist, returns None silently.
/// If the file is malformed, logs a warning and returns None.
fn load_per_repo_config() -> Option<Config> {
    // Look for .peakbot/config.yaml in the current working directory
    let per_repo_path = std::env::current_dir().ok()?.join(".peakbot/config.yaml");

    if !per_repo_path.exists() {
        tracing::debug!("No per-repo config found at {}", per_repo_path.display());
        return None;
    }

    let content = match std::fs::read_to_string(&per_repo_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to read .peakbot/config.yaml: {}. Ignoring.", e);
            return None;
        }
    };

    match serde_yaml::from_str::<Config>(&content) {
        Ok(config) => {
            tracing::info!("Loaded per-repo config from {}", per_repo_path.display());
            Some(config)
        }
        Err(e) => {
            tracing::warn!("Failed to parse .peakbot/config.yaml: {}. Ignoring.", e);
            None
        }
    }
}

impl Config {
    /// Load configuration from multiple sources in priority order:
    /// 1. Defaults (lowest priority)
    /// 2. Master config (~/.config/peakbot/peakbot/config.yaml)
    /// 3. Per-repo config (.peakbot/config.yaml in cwd) - top-level key override
    ///
    /// Returns `LoadedConfig` with the loaded config and metadata about what was found.
    pub fn load() -> anyhow::Result<LoadedConfig> {
        // Step 1: Start with defaults
        let mut config = Config::default();

        // Step 2: Load and apply master config if present. A malformed
        // master config is fatal — see `load_yaml_config` for rationale.
        let (master, config_file_path) = load_yaml_config()?;
        let config_file_found = master.is_some();
        if let Some(master) = master {
            config = master;
            tracing::debug!("Config after master load: provider = {:?}", config.provider);
        }

        // Step 3: Load and merge per-repo config if present (top-level key override)
        if let Some(repo_config) = load_per_repo_config() {
            config.merge_with(repo_config);
            tracing::debug!(
                "Config after per-repo merge: provider = {:?}",
                config.provider
            );
        }

        Ok(LoadedConfig {
            config,
            config_file_found,
            config_file_path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anthropic_prompt_caching_defaults_auto() {
        let yaml = r#"
provider:
  type: anthropic
  config:
    model: claude-sonnet-4-5
"#;
        let cfg: Config = serde_yaml::from_str(yaml).expect("must parse");
        match cfg.provider {
            ProviderConfig::Anthropic(c) => {
                assert_eq!(c.prompt_caching, AnthropicCaching::Auto);
            }
            _ => panic!("expected Anthropic provider"),
        }
    }

    #[test]
    fn test_anthropic_prompt_caching_modes_parse() {
        for (s, want) in [
            ("manual", AnthropicCaching::Manual),
            ("auto", AnthropicCaching::Auto),
            ("auto_1h", AnthropicCaching::Auto1h),
            ("off", AnthropicCaching::Off),
        ] {
            let yaml = format!(
                "provider:\n  type: anthropic\n  config:\n    model: m\n    prompt_caching: {s}\n"
            );
            let cfg: Config = serde_yaml::from_str(&yaml).expect("must parse");
            match cfg.provider {
                ProviderConfig::Anthropic(c) => assert_eq!(c.prompt_caching, want, "mode {s}"),
                _ => panic!("expected Anthropic provider"),
            }
        }
    }

    #[test]
    fn test_legacy_anthropic_caching_survives_registry() {
        // The legacy `provider:` block routes through describe_legacy →
        // ModelEntry → resolve. Caching must not be dropped along the way.
        let yaml = r#"
provider:
  type: anthropic
  config:
    model: claude-sonnet-4-5
    prompt_caching: manual
"#;
        let cfg: Config = serde_yaml::from_str(yaml).expect("must parse");
        let registry = cfg.build_model_registry().expect("registry builds");
        let resolved = registry.resolve("default").expect("default alias resolves");
        match &resolved.provider_config {
            ProviderConfig::Anthropic(c) => {
                assert_eq!(c.prompt_caching, AnthropicCaching::Manual);
            }
            _ => panic!("expected Anthropic provider"),
        }
    }

    #[test]
    fn test_merge_with_partial_provider_override() {
        // Master config with full OpenRouter settings
        let mut master = Config::default();
        if let ProviderConfig::OpenRouter(ref mut c) = master.provider {
            c.api_key = Some("master-key".to_string());
            c.model = "anthropic/claude-3.7-sonnet".to_string();
            c.max_tokens = 4096;
        }

        // Per-repo config specifying a different model
        // NOTE: With shallow merge, the entire provider object is replaced
        let repo_config = Config {
            provider: ProviderConfig::OpenRouter(OpenRouterConfig {
                api_key: Some("repo-key".to_string()),
                model: "google/gemini-2.0-flash-001".to_string(),
                max_tokens: 8192,
                vision: None,
            }),
            ..Config::default()
        };

        // Merge
        master.merge_with(repo_config);

        // With shallow merge, entire provider is replaced
        if let ProviderConfig::OpenRouter(c) = master.provider {
            assert_eq!(c.model, "google/gemini-2.0-flash-001");
            assert_eq!(c.api_key, Some("repo-key".to_string()));
            assert_eq!(c.max_tokens, 8192);
        } else {
            panic!("Expected OpenRouter provider");
        }
    }

    #[test]
    fn test_merge_preserves_master_when_repo_doesnt_override() {
        // Master has provider configured
        let mut master = Config::default();
        if let ProviderConfig::OpenRouter(ref mut c) = master.provider {
            c.api_key = Some("master-key".to_string());
            c.model = "anthropic/claude-3.7-sonnet".to_string();
        }

        // Repo only specifies cost_tracking, not provider
        let repo_config = Config {
            cost_tracking: false,
            ..Config::default()
        };

        master.merge_with(repo_config);

        // Provider should be preserved from master
        if let ProviderConfig::OpenRouter(c) = master.provider {
            assert_eq!(c.model, "anthropic/claude-3.7-sonnet");
            assert_eq!(c.api_key, Some("master-key".to_string()));
        }
        assert!(!master.cost_tracking);
    }

    #[test]
    fn test_merge_mcp_servers_replacement() {
        let mut master = Config {
            mcp_servers: Some(vec![McpServerConfig {
                name: "master-server".to_string(),
                transport_type: McpTransportType::Stdio,
                command: Some("npx".to_string()),
                args: None,
                env: None,
                url: None,
                auth_token: None,
                auth: None,
                headers: None,
                enabled: true,
            }]),
            ..Config::default()
        };

        let repo_config = Config {
            mcp_servers: Some(vec![McpServerConfig {
                name: "repo-server".to_string(),
                transport_type: McpTransportType::Stdio,
                command: Some("npx".to_string()),
                args: None,
                env: None,
                url: None,
                auth_token: None,
                auth: None,
                headers: None,
                enabled: true,
            }]),
            ..Config::default()
        };

        master.merge_with(repo_config);

        // Should be completely replaced by repo's servers
        assert!(master.mcp_servers.is_some());
        let servers = master.mcp_servers.unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "repo-server");
    }

    #[test]
    fn test_merge_context_config() {
        let master = Config::default();

        // Repo overrides context settings
        let repo_config = Config {
            context: ContextConfig {
                threshold: 0.5,
                keep_recent: 10,
                enabled: true,
                compaction_model: None,
            },
            ..Config::default()
        };

        let mut merged = master;
        merged.merge_with(repo_config);

        assert_eq!(merged.context.threshold, 0.5);
        assert_eq!(merged.context.keep_recent, 10);
    }

    // === build_model_registry: locked-plan v4 §"Legacy synthesis" ====

    #[test]
    fn build_model_registry_legacy_provider_synthesises_default_alias() {
        // Default config has the legacy `provider:` block (OpenRouter +
        // default model) and an empty `providers:` list.
        let config = Config::default();
        let reg = config
            .build_model_registry()
            .expect("legacy synth should always succeed for default config");

        assert_eq!(reg.default_alias(), Some("default"));
        assert!(reg.contains("default"));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn build_model_registry_uses_providers_list_when_present() {
        let config = Config {
            providers: vec![ProviderEntry {
                name: "openrouter".into(),
                kind: ProviderType::OpenRouter,
                api_key: Some("sk".into()),
                base_url: None,
                models: vec![ModelEntry {
                    name: "anthropic/claude-3.7-sonnet".into(),
                    alias: Some("sonnet".into()),
                    max_tokens: None,
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    vision: None,
                    context_size: None,
                }],
            }],
            default_model: Some("sonnet".into()),
            ..Config::default()
        };
        let reg = config.build_model_registry().expect("should build");
        assert_eq!(reg.default_alias(), Some("sonnet"));
        assert!(reg.contains("sonnet"));
        // Legacy `provider:` block is *not* synthesised when providers
        // list is non-empty.
        assert!(!reg.contains("default"));
    }

    #[test]
    fn build_model_registry_propagates_validation_errors() {
        let config = Config {
            providers: vec![ProviderEntry {
                name: "openrouter".into(),
                kind: ProviderType::OpenRouter,
                api_key: None,
                base_url: None,
                models: vec![ModelEntry {
                    name: "anthropic/claude-3.7-sonnet".into(),
                    alias: Some("sonnet".into()),
                    max_tokens: None,
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    vision: None,
                    context_size: None,
                }],
            }],
            default_model: Some("ghost".into()), // intentionally wrong
            ..Config::default()
        };
        let err = config.build_model_registry().unwrap_err();
        assert!(matches!(err, RegistryError::UnknownDefault { .. }));
    }

    // ────────────────────────────────────────────────────────────────────
    // Boot-provider mirror pin (regression for the "compaction model
    // construction failure under multi-model config" bug).
    //
    // Background: in multi-model mode (`providers:` + `default_model:`)
    // the legacy `config.provider` field defaults to
    // `OpenRouterConfig { api_key: None, … }` — leftover scaffolding.
    // `AgentRunner::new` reads `config.provider` for Layer 1 boot-time
    // compaction-model construction. Without explicit mirroring, that
    // read sees the stale default and bails with "OpenRouter API key
    // not configured" *even when* the registry's resolved default model
    // has a real api_key.
    //
    // `resolve_and_mirror_boot_provider` is the single source of truth
    // for "config.provider == active provider after boot". This pin
    // locks the invariant for the regression case.
    // ────────────────────────────────────────────────────────────────────

    #[test]
    fn boot_provider_mirror_propagates_credentials_from_registry() {
        // Pre-fix shape: legacy `provider:` block is at its default
        // (api_key: None), but the `providers:` list has the real key.
        let mut config = Config {
            providers: vec![ProviderEntry {
                name: "openrouter".into(),
                kind: ProviderType::OpenRouter,
                api_key: Some("sk-or-real-key".into()),
                base_url: None,
                models: vec![ModelEntry {
                    name: "anthropic/claude-3.7-sonnet".into(),
                    alias: Some("sonnet".into()),
                    max_tokens: None,
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    vision: None,
                    context_size: None,
                }],
            }],
            default_model: Some("sonnet".into()),
            ..Config::default()
        };

        // Precondition: the stale default `config.provider` has no key.
        match &config.provider {
            ProviderConfig::OpenRouter(c) => {
                assert!(
                    c.api_key.is_none(),
                    "test precondition: stale default config.provider has no api_key"
                );
            }
            other => panic!("expected default OpenRouter, got {other:?}"),
        }

        let registry = config.build_model_registry().expect("registry builds");
        let resolved = config.resolve_and_mirror_boot_provider(&registry);

        // Invariant 1: the returned resolved config carries the real key.
        match &resolved {
            ProviderConfig::OpenRouter(c) => {
                assert_eq!(
                    c.api_key.as_deref(),
                    Some("sk-or-real-key"),
                    "resolved provider must carry registry credentials"
                );
            }
            other => panic!("expected OpenRouter, got {other:?}"),
        }

        // Invariant 2 (load-bearing): `config.provider` was mirrored —
        // this is the field `AgentRunner::new` reads for compaction.
        match &config.provider {
            ProviderConfig::OpenRouter(c) => {
                assert_eq!(
                    c.api_key.as_deref(),
                    Some("sk-or-real-key"),
                    "config.provider must mirror resolved credentials \
                     so AgentRunner::new's Layer 1 compaction-model \
                     construction sees the real api_key"
                );
            }
            other => panic!("expected OpenRouter, got {other:?}"),
        }
    }

    #[test]
    fn boot_provider_mirror_legacy_config_is_noop() {
        // Pure legacy single-provider config: no `providers:` block,
        // `config.provider` is already authoritative. The mirror should
        // not corrupt or empty it.
        let mut config = Config {
            provider: ProviderConfig::OpenRouter(OpenRouterConfig {
                api_key: Some("sk-legacy".into()),
                model: "anthropic/claude-3.5-sonnet".into(),
                max_tokens: 4096,
                vision: None,
            }),
            ..Config::default()
        };
        let registry = config.build_model_registry().expect("legacy synth builds");
        let resolved = config.resolve_and_mirror_boot_provider(&registry);

        match &resolved {
            ProviderConfig::OpenRouter(c) => {
                assert_eq!(c.api_key.as_deref(), Some("sk-legacy"));
                assert_eq!(c.model, "anthropic/claude-3.5-sonnet");
            }
            other => panic!("expected OpenRouter, got {other:?}"),
        }
        // `config.provider` is unchanged in shape and credentials.
        match &config.provider {
            ProviderConfig::OpenRouter(c) => {
                assert_eq!(c.api_key.as_deref(), Some("sk-legacy"));
                assert_eq!(c.model, "anthropic/claude-3.5-sonnet");
            }
            other => panic!("expected OpenRouter, got {other:?}"),
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // deny_unknown_fields pins
    //
    // Pre-fix, an unknown field at *any* level of the config silently
    // parsed and got dropped on the floor — most painfully `max_tokens`
    // placed at the provider level instead of per-model, which fell back
    // to the 4096 default with no diagnostic. These pins lock in that
    // unknown fields are now hard parse errors, naming the offending key.
    //
    // The pins exercise three load-bearing layers: the per-provider
    // `LlamaCppConfig` (legacy block), the multi-model `ProviderEntry`
    // (where the user actually hit it), and the top-level `Config` (so
    // typos like `cost_traking:` scream too).
    // ────────────────────────────────────────────────────────────────────

    #[test]
    fn unknown_field_on_provider_entry_is_rejected() {
        // The actual bug: `max_tokens:` placed at the provider level
        // (where it doesn't belong) used to be silently dropped, and
        // every model under it fell back to the 4096 default.
        let yaml = r#"
providers:
  - name: local
    type: llamacpp
    base_url: http://localhost:8080
    max_tokens: 8192   # WRONG LOCATION — belongs under the model entry
    models:
      - name: my-model
        alias: local
default_model: local
"#;
        let err = serde_yaml::from_str::<Config>(yaml)
            .expect_err("unknown field at provider level must fail to parse");
        let msg = err.to_string();
        assert!(
            msg.contains("max_tokens"),
            "error must name the offending key, got: {msg}"
        );
    }

    #[test]
    fn unknown_field_on_model_entry_is_rejected() {
        let yaml = r#"
providers:
  - name: local
    type: llamacpp
    base_url: http://localhost:8080
    models:
      - name: my-model
        alias: local
        max_tokenz: 8192   # typo
default_model: local
"#;
        let err = serde_yaml::from_str::<Config>(yaml)
            .expect_err("unknown field on model entry must fail to parse");
        let msg = err.to_string();
        assert!(
            msg.contains("max_tokenz"),
            "error must name the offending key, got: {msg}"
        );
    }

    #[test]
    fn unknown_field_at_top_level_is_rejected() {
        let yaml = r#"
cost_traking: false   # typo of cost_tracking
"#;
        let err = serde_yaml::from_str::<Config>(yaml)
            .expect_err("unknown top-level key must fail to parse");
        let msg = err.to_string();
        assert!(
            msg.contains("cost_traking"),
            "error must name the offending key, got: {msg}"
        );
    }

    #[test]
    fn well_formed_multimodel_config_still_parses() {
        // No-regression: locking down unknown fields must not break the
        // happy path. A complete-looking multi-model config must still
        // round-trip cleanly through serde.
        let yaml = r#"
providers:
  - name: local
    type: llamacpp
    base_url: http://localhost:8080
    api_key: optional
    models:
      - name: my-model
        alias: local
        max_tokens: 8192
        temperature: 0.4
default_model: local
cost_tracking: false
"#;
        let cfg: Config = serde_yaml::from_str(yaml).expect("well-formed config must parse");
        assert_eq!(cfg.default_model.as_deref(), Some("local"));
        assert_eq!(cfg.providers.len(), 1);
        assert_eq!(cfg.providers[0].models.len(), 1);
        assert_eq!(cfg.providers[0].models[0].max_tokens, Some(8192));
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
}
