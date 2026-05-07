// Allow dead_code for provider-specific config structs - not all providers may be used
// depending on build configuration. The enum variants are accessed via deserialization.
#![allow(dead_code)]

pub mod model_registry;
pub use model_registry::{
    ModelEntry, ModelRegistry, ProviderEntry, RESERVED_UNAVAILABLE_ALIAS, RegistryError,
    ResolvedModel,
};

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
    LlamaCpp,
    Ollama,
}

impl fmt::Display for ProviderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderType::OpenRouter => write!(f, "openrouter"),
            ProviderType::OpenAI => write!(f, "openai"),
            ProviderType::LlamaCpp => write!(f, "llamacpp"),
            ProviderType::Ollama => write!(f, "ollama"),
        }
    }
}

/// Configuration for OpenRouter provider
#[derive(Debug, Deserialize, Clone, PartialEq)]
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
}

/// Configuration for Ollama provider (local models)
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct OllamaConfig {
    /// Base URL (default: http://localhost:11434)
    #[serde(default = "default_ollama_url")]
    pub base_url: String,
    /// Model name (e.g., "llama3", "qwen2.5:14b", "mistral")
    pub model: String,
    /// Temperature setting (optional)
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Number of context tokens (optional, defaults to 2048 for most models)
    #[serde(default)]
    pub num_ctx: Option<usize>,
}

/// Configuration for OpenAI provider
#[derive(Debug, Deserialize, Clone, PartialEq)]
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
}

/// Configuration for LlamaCpp provider (uses OpenAI-compatible completions API)
/// This is compatible with llama.cpp's server mode which provides an OpenAI-compatible API
#[derive(Debug, Deserialize, Clone, PartialEq)]
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
}

fn default_ollama_url() -> String {
    "http://localhost:11434".to_string()
}

fn default_openai_url() -> String {
    "https://api.openai.com/v1".to_string()
}

fn default_llamacpp_url() -> String {
    "http://localhost:8080".to_string()
}

/// Provider configuration - specifies which provider and its specific config
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", content = "config")]
pub enum ProviderConfig {
    #[serde(rename = "openrouter")]
    OpenRouter(OpenRouterConfig),
    #[serde(rename = "openai")]
    OpenAI(OpenAIConfig),
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
        }
    }
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
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
}

/// Multi-agent pipeline configuration
#[derive(Debug, Deserialize, Clone, PartialEq, Default)]
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
            ProviderConfig::LlamaCpp(c) => &c.model,
            ProviderConfig::Ollama(c) => &c.model,
        }
    }

    /// Get the max tokens from the provider config
    pub fn max_tokens(&self) -> u64 {
        match &self.provider {
            ProviderConfig::OpenRouter(c) => c.max_tokens,
            ProviderConfig::OpenAI(c) => c.max_tokens,
            ProviderConfig::LlamaCpp(c) => c.max_tokens,
            ProviderConfig::Ollama(_) => 4096, // Ollama doesn't have this setting in the same way
        }
    }

    /// Get the API key for OpenRouter (if applicable)
    pub fn openrouter_api_key(&self) -> Option<&str> {
        match &self.provider {
            ProviderConfig::OpenRouter(c) => c.api_key.as_deref(),
            ProviderConfig::OpenAI(c) => c.api_key.as_deref(),
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
            let (kind, api_key, base_url, model_name, max_tokens, temperature, num_ctx, extra) =
                describe_legacy(&self.provider);
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
                    num_ctx,
                    extra_params: extra,
                    context_window: None,
                }],
            };
            return ModelRegistry::build(
                std::slice::from_ref(&provider_entry),
                Some(&synthetic_alias),
            );
        }

        ModelRegistry::build(&self.providers, self.default_model.as_deref())
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
    Option<usize>,
    Option<serde_json::Value>,
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
        ),
        ProviderConfig::LlamaCpp(c) => (
            ProviderType::LlamaCpp,
            c.api_key.clone(),
            Some(c.base_url.clone()),
            c.model.clone(),
            c.max_tokens,
            None,
            None,
            c.extra_params.clone(),
        ),
        ProviderConfig::Ollama(c) => (
            ProviderType::Ollama,
            None,
            Some(c.base_url.clone()),
            c.model.clone(),
            // Ollama has no max_tokens — use the default placeholder.
            default_max_tokens(),
            c.temperature,
            c.num_ctx,
            None,
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

#[derive(Debug, Deserialize, Clone, PartialEq)]
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
    /// Bearer token sent in the `Authorization: Bearer <token>` header
    /// for remote transports. Convenience for the most common auth case.
    #[serde(default)]
    pub auth_token: Option<String>,
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
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
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
}

/// Configuration for context compaction
#[derive(Debug, Deserialize, Clone, PartialEq)]
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

/// Configuration for conversation persistence
#[derive(Debug, Deserialize, Clone, PartialEq)]
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
pub struct BashConfig {
    /// Environment variables to set when running bash commands
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
}

/// Configuration for retry logic with exponential backoff
#[derive(Debug, Deserialize, Clone, PartialEq)]
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
            conversation: None,
            bash: BashConfig::default(),
            retry: RetryConfig::default(),
            pipeline: None,
        }
    }
}

/// Get the platform-specific config directory
fn get_config_dir() -> Option<std::path::PathBuf> {
    ProjectDirs::from("com", "peakbot", "peakbot").map(|dirs| dirs.config_dir().to_path_buf())
}

/// Load configuration from YAML file in the platform config directory
/// Load master configuration from `~/.config/peakbot/config.yaml`.
///
/// Returns:
/// - `Ok(None)` if the file doesn't exist (legitimate — env vars may
///   carry the whole config).
/// - `Ok(Some(config))` on a clean parse.
/// - `Err(_)` if the file exists but is unreadable or malformed. We
///   refuse to silently fall back to defaults: a typo'd YAML used to
///   mask itself as "no API key configured" and made the boot path
///   inscrutable. Better to fail loud at the source. *(principle of
///   least astonishment)*
fn load_yaml_config() -> anyhow::Result<Option<Config>> {
    let Some(config_dir) = get_config_dir() else {
        return Ok(None);
    };
    let config_path = config_dir.join("config.yaml");

    tracing::debug!("Looking for config at: {}", config_path.display());

    if !config_path.exists() {
        tracing::debug!("No YAML config found at {}", config_path.display());
        return Ok(None);
    }

    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {e}", config_path.display()))?;
    tracing::debug!("Config file content: {}", content);

    let config: Config = serde_yaml::from_str(&content)
        .map_err(|e| anyhow::anyhow!("Failed to parse {} as YAML: {e}", config_path.display()))?;

    tracing::info!("Loaded YAML config from {}", config_path.display());
    Ok(Some(config))
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
    /// 4. Environment variables (highest priority)
    pub fn load() -> anyhow::Result<Self> {
        // Step 1: Start with defaults
        let mut config = Config::default();

        // Step 2: Load and apply master config if present. A malformed
        // master config is fatal — see `load_yaml_config` for rationale.
        if let Some(master) = load_yaml_config()? {
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

        // Load environment variables and merge (env vars override YAML/defaults)
        // Check for new PROVIDER JSON config first
        if let Ok(provider_json) = std::env::var("PROVIDER")
            && !provider_json.is_empty()
        {
            // Parse JSON provider config
            match serde_json::from_str::<ProviderConfig>(&provider_json) {
                Ok(provider_config) => config.provider = provider_config,
                Err(e) => tracing::warn!("Failed to parse PROVIDER JSON: {}", e),
            }
        } else {
            // Fall back to legacy OpenRouter environment variables
            // This maintains backward compatibility

            // Check if OLLAMA_MODEL is set to switch to Ollama
            if let Ok(model) = std::env::var("OLLAMA_MODEL")
                && !model.is_empty()
            {
                // Using Ollama via legacy env var
                let base_url = std::env::var("OLLAMA_BASE_URL")
                    .unwrap_or_else(|_| "http://localhost:11434".to_string());
                let temperature = std::env::var("OLLAMA_TEMPERATURE")
                    .ok()
                    .and_then(|t| t.parse().ok());
                let num_ctx = std::env::var("OLLAMA_NUM_CTX")
                    .ok()
                    .and_then(|c| c.parse().ok());

                config.provider = ProviderConfig::Ollama(OllamaConfig {
                    base_url,
                    model,
                    temperature,
                    num_ctx,
                });
            } else {
                // OpenRouter config via legacy env vars
                if let Ok(api_key) = std::env::var("OPENROUTER_API_KEY")
                    && !api_key.is_empty()
                    && let ProviderConfig::OpenRouter(ref mut c) = config.provider
                {
                    c.api_key = Some(api_key);
                }

                if let Ok(model) = std::env::var("OPENROUTER_MODEL")
                    && !model.is_empty()
                    && let ProviderConfig::OpenRouter(ref mut c) = config.provider
                {
                    c.model = model;
                }

                if let Ok(tokens) = std::env::var("OPENROUTER_MAX_TOKENS")
                    && let Ok(tokens) = tokens.parse()
                    && let ProviderConfig::OpenRouter(ref mut c) = config.provider
                {
                    c.max_tokens = tokens;
                }
            }
        }

        if let Ok(turns) = std::env::var("AGENT_MAX_TURNS")
            && let Ok(turns) = turns.parse()
        {
            config.agent_max_turns = turns;
        }

        if let Ok(mcp) = std::env::var("MCP_SERVERS")
            && !mcp.is_empty()
        {
            // Parse JSON string into Vec<McpServerConfig>
            match serde_json::from_str::<Vec<McpServerConfig>>(&mcp) {
                Ok(servers) => config.mcp_servers = Some(servers),
                Err(e) => tracing::warn!("Failed to parse MCP_SERVERS JSON: {}", e),
            }
        }

        tracing::debug!("Final config: provider = {:?}", config.provider);

        // SEARXNG_BASE_URL
        if let Ok(url) = std::env::var("SEARXNG_BASE_URL")
            && !url.is_empty()
        {
            let searxng = config.searxng.get_or_insert_with(|| SearXngConfig {
                base_url: String::new(),
                enabled: true,
                timeout_seconds: 30,
                max_results: 10,
            });
            searxng.base_url = url;
        }

        // SEARXNG_ENABLED
        if let Ok(enabled) = std::env::var("SEARXNG_ENABLED")
            && let Ok(enabled) = enabled.parse()
        {
            let searxng = config.searxng.get_or_insert_with(|| SearXngConfig {
                base_url: String::new(),
                enabled: true,
                timeout_seconds: 30,
                max_results: 10,
            });
            searxng.enabled = enabled;
        }

        // SEARXNG_TIMEOUT
        if let Ok(timeout) = std::env::var("SEARXNG_TIMEOUT")
            && let Ok(timeout) = timeout.parse()
            && let Some(searxng) = config.searxng.as_mut()
        {
            searxng.timeout_seconds = timeout;
        }

        // SEARXNG_MAX_RESULTS
        if let Ok(max) = std::env::var("SEARXNG_MAX_RESULTS")
            && let Ok(max) = max.parse()
            && let Some(searxng) = config.searxng.as_mut()
        {
            searxng.max_results = max;
        }

        // COST_TRACKING
        if let Ok(enabled) = std::env::var("COST_TRACKING")
            && let Ok(enabled) = enabled.parse()
        {
            config.cost_tracking = enabled;
        }

        // Context compaction config via environment variables
        // CONTEXT_ENABLED
        if let Ok(enabled) = std::env::var("CONTEXT_ENABLED")
            && let Ok(enabled) = enabled.parse()
        {
            config.context.enabled = enabled;
        }

        // CONTEXT_THRESHOLD
        if let Ok(threshold) = std::env::var("CONTEXT_THRESHOLD")
            && let Ok(threshold) = threshold.parse::<f64>()
            && (0.0..=1.0).contains(&threshold)
        {
            config.context.threshold = threshold;
        }

        // CONTEXT_KEEP_RECENT
        if let Ok(keep_recent) = std::env::var("CONTEXT_KEEP_RECENT")
            && let Ok(keep_recent) = keep_recent.parse()
        {
            config.context.keep_recent = keep_recent;
        }

        // CONTEXT_WINDOW: removed in the per-model registry refactor.
        // The active model's window is now resolved from `ModelEntry.context_window`
        // (or auto-detected from the wire id) — there is no longer a global
        // override here. See `context_manager::auto_detect_context_window`.
        if std::env::var("CONTEXT_WINDOW").is_ok() {
            tracing::warn!(
                "CONTEXT_WINDOW env var is no longer honoured. Set `context_window:` on the active \
                 model under `providers:` instead, or rely on auto-detection from the wire id."
            );
        }

        // Conversation persistence config via environment variables
        // CONVERSATION_AUTO_SAVE
        if let Ok(enabled) = std::env::var("CONVERSATION_AUTO_SAVE")
            && let Ok(enabled) = enabled.parse()
        {
            let conv = config.conversation.get_or_insert(ConversationConfig {
                auto_save: true,
                storage_dir: None,
                max_conversations: 50,
                auto_resume: true,
            });
            conv.auto_save = enabled;
        }

        // CONVERSATION_STORAGE_DIR
        if let Ok(dir) = std::env::var("CONVERSATION_STORAGE_DIR")
            && !dir.is_empty()
        {
            let conv = config.conversation.get_or_insert(ConversationConfig {
                auto_save: true,
                storage_dir: None,
                max_conversations: 50,
                auto_resume: true,
            });
            conv.storage_dir = Some(std::path::PathBuf::from(dir));
        }

        // CONVERSATION_MAX_CONVERSATIONS
        if let Ok(max) = std::env::var("CONVERSATION_MAX_CONVERSATIONS")
            && let Ok(max) = max.parse()
        {
            let conv = config.conversation.get_or_insert(ConversationConfig {
                auto_save: true,
                storage_dir: None,
                max_conversations: 50,
                auto_resume: true,
            });
            conv.max_conversations = max;
        }

        // CONVERSATION_AUTO_RESUME
        if let Ok(enabled) = std::env::var("CONVERSATION_AUTO_RESUME")
            && let Ok(enabled) = enabled.parse()
        {
            let conv = config.conversation.get_or_insert(ConversationConfig {
                auto_save: true,
                storage_dir: None,
                max_conversations: 50,
                auto_resume: true,
            });
            conv.auto_resume = enabled;
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                    num_ctx: None,
                    extra_params: None,
                    context_window: None,
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
                    num_ctx: None,
                    extra_params: None,
                    context_window: None,
                }],
            }],
            default_model: Some("ghost".into()), // intentionally wrong
            ..Config::default()
        };
        let err = config.build_model_registry().unwrap_err();
        assert!(matches!(err, RegistryError::UnknownDefault { .. }));
    }
}
