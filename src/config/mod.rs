// Allow dead_code for provider-specific config structs - not all providers may be used
// depending on build configuration. The enum variants are accessed via deserialization.
#![allow(dead_code)]
// The stage 1.1 test pin at line 3432 deliberately uses
// `assert_eq!(agents_md, true)` for symmetry with the surrounding
// literal comparisons; clippy's `bool_assert_comparison` lint wants
// `assert!` instead. Allow here so the RED→GREEN transition is a pure
// impl change with no test edits.
#![allow(clippy::bool_assert_comparison)]

pub mod model_registry;
pub use model_registry::{ModelEntry, ModelRegistry, ProviderEntry, RegistryError, ResolvedModel};

use anyhow::Context;
use directories_next::ProjectDirs;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
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

    /// Replaces the built-in persona (`src/system_prompt_persona.txt`) at the
    /// head of the agentless system prompt. Absent or whitespace-only =
    /// built-in. Not used in orchestrator mode (see
    /// `pipeline.orchestrator_prompt`) and never applied to sub-agents (a
    /// role's `prompt:` is its whole persona).
    #[serde(default)]
    pub persona: Option<String>,

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

    /// Named, ordered list of pipelines (plan §3.3, stage 1.1). The
    /// orchestrator of every entry lives next to the sub-agents inside
    /// `orchestrator:` — a team owns its full cast. Declaration order is
    /// UI order.
    #[serde(default)]
    pub pipelines: Vec<PipelineDef>,
    /// Vector DB configuration (doc_index / doc_search tools).
    /// When absent, both tools are skipped entirely (not registered).
    #[serde(default)]
    pub vector_db: Option<VectorDbConfig>,

    /// Web UI (the default `peakbot` mode) settings — sticky-session expiry.
    #[serde(default)]
    pub web: WebConfig,

    /// Built-in tool filter (blocklist `disabled:` XOR allowlist `only:`).
    /// Absent block = every tool available.
    #[serde(default)]
    pub tools: ToolsConfig,

    /// Outbound HTTP timeouts. Boot-only — read once when the process starts.
    #[serde(default)]
    pub http: HttpConfig,

    /// Wall-clock budgets for tool calls and delegation. Applied per call, so
    /// a reload takes effect on the next tool.
    #[serde(default)]
    pub timeouts: TimeoutsConfig,
}

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

/// Web UI settings. Only the sticky-session reaper is configurable; all
/// fields default, so an absent `web:` block is fine.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    /// Kill a session this many seconds after it becomes fully idle — no
    /// sockets attached, the agent not processing a turn, and no live
    /// `bash_bg` children under it (#158). Any of those three makes the
    /// session live and resets the clock. (Default: 600 = 10 min.)
    #[serde(default = "default_session_ttl_secs")]
    pub session_ttl_secs: u64,
    /// How often the reaper scans for expired sessions (default: 60 s).
    #[serde(default = "default_reaper_tick_secs")]
    pub reaper_tick_secs: u64,
    /// Serve the web UI over HTTPS using PeakBot's built-in CA (default: false).
    /// Install the CA on your device once to get a trusted padlock; a fresh
    /// leaf is minted each boot. The `--tls` flag overrides this.
    #[serde(default)]
    pub tls: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            session_ttl_secs: default_session_ttl_secs(),
            reaper_tick_secs: default_reaper_tick_secs(),
            tls: false,
        }
    }
}

fn default_session_ttl_secs() -> u64 {
    600
}
fn default_reaper_tick_secs() -> u64 {
    60
}

/// Multi-agent pipeline configuration
#[derive(Debug, Deserialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct PipelineConfig {
    /// Whether multi-agent pipelines are enabled (default: false)
    #[serde(default)]
    pub enabled: bool,

    /// Extra framing appended to the **orchestrator's** system prompt when
    /// sub-agents are active. The orchestrator already loses the crusader
    /// persona in this mode; this is where you tell it how to lead the team.
    /// Absent = no addendum.
    #[serde(default)]
    pub orchestrator_prompt: Option<String>,

    /// Sub-agent definitions keyed by agent name. `Members` is the
    /// newtype with duplicate-key detection (D4, plan §3.3) — legacy
    /// `pipeline:` blocks get that check for free.
    #[serde(default)]
    pub agents: Members,
}

/// Definition of a sub-agent role the orchestrator can delegate to.
///
/// A role names a `model:` **alias** from the top-level `providers:` list
/// (the same aliases `/model` uses) — resolved by [`crate::pipeline::SubAgentRegistry`],
/// which is why there are no provider/credential fields here. Model, key,
/// and base URL all come from the resolved alias.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinition {
    /// Alias of the model this role runs on (from `providers:`). When
    /// omitted, the registry falls back to `default_model`.
    #[serde(default)]
    pub model: Option<String>,

    /// System prompt / preamble for this role.
    pub prompt: String,

    /// Extra environment variables merged into this sub-agent's bash tool
    /// env (mirrors top-level `bash.env`). Consumed when the sub-agent is
    /// built.
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,

    /// Which skills this role's sub-agent sees in its prompt. Default: all.
    #[serde(default)]
    pub skills: SkillFilter,

    /// Inject the repo's `agents.md` into this role's preamble. Default: off
    /// (sub-agents get a lean, task-scoped preamble unless a role opts in).
    #[serde(default)]
    pub agents_md: bool,
}

/// Sub-agent role map with duplicate-key detection (D4, plan §3.3).
///
/// `serde_yaml` silently last-wins on duplicate `HashMap` keys, so a plain
/// `HashMap<String, AgentDefinition>` would let a config like
///
/// ```yaml
/// agents:
///   reviewer:
///     prompt: first
///   reviewer:
///     prompt: second
/// ```
///
/// boot and discard the first entry without a peep. `Members` is a thin
/// newtype with a manual `Deserialize` that walks the YAML mapping and
/// rejects the second key with a clear error (`duplicate member
/// 'reviewer'`). The map is exposed via `Deref<Target = HashMap<…>>` so
/// existing call sites that just want to read roles (e.g.
/// `SubAgentRegistry::from_members`, tests) need no ceremony.
///
/// The inner field is `pub(crate)` so test fixtures across the crate
/// can build a `Members` literal (e.g. `Members(HashMap::from([...]))`)
/// without going through the `From` impl. Production code outside this
/// module should construct via `serde_yaml` or the `From` impl.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Members(pub(crate) HashMap<String, AgentDefinition>);

impl Deref for Members {
    type Target = HashMap<String, AgentDefinition>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<HashMap<String, AgentDefinition>> for Members {
    fn from(map: HashMap<String, AgentDefinition>) -> Self {
        Self(map)
    }
}

impl<'de> Deserialize<'de> for Members {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MembersVisitor;

        impl<'de> serde::de::Visitor<'de> for MembersVisitor {
            type Value = Members;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a map of member name to sub-agent definition")
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut access: M,
            ) -> Result<Self::Value, M::Error> {
                let mut out: HashMap<String, AgentDefinition> =
                    HashMap::with_capacity(access.size_hint().unwrap_or(0));
                while let Some((key, value)) = access.next_entry::<String, AgentDefinition>()? {
                    if out.contains_key(&key) {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate member '{key}'"
                        )));
                    }
                    out.insert(key, value);
                }
                Ok(Members(out))
            }
        }

        deserializer.deserialize_map(MembersVisitor)
    }
}

/// Orchestrator configuration nested inside a `PipelineDef` (plan §3.3,
/// amendment 1). All three fields default to `None`:
///
/// * `model: None` falls back to `default_model` at build time,
/// * `prompt: None` means "no addendum to the orchestrator recipe",
/// * `persona: None` keeps the global persona; when set it REPLACES the
///   global persona in the orchestrator's system-prompt recipe for this
///   pipeline only (mirrors how `config.persona()` works today).
#[derive(Debug, Deserialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct OrchestratorDef {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub persona: Option<String>,
}

/// One pipeline entry from the new `pipelines:` list (plan §3.3).
///
/// `orchestrator:` is REQUIRED so "pipeline with no orchestrator" is a
/// parse error, not a runtime rule. `agents:` is required and non-empty
/// (validated later by `PipelineSet::build`).
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PipelineDef {
    pub name: String,
    pub orchestrator: OrchestratorDef,
    pub agents: Members,
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

    /// Get the wall-clock tool/delegation budgets
    pub fn timeouts(&self) -> &TimeoutsConfig {
        &self.timeouts
    }

    /// Get pipeline configuration if present
    pub fn pipeline(&self) -> Option<&PipelineConfig> {
        self.pipeline.as_ref()
    }

    /// The configured persona, trimmed. `None` for absent or whitespace-only
    /// values — same representation as "use the built-in" (plan §A-Q7).
    pub fn persona(&self) -> Option<&str> {
        self.persona
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
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
                preserve_reasoning,
                display_reasoning,
            ) = describe_legacy(&self.provider);
            let synthetic_alias = "default".to_string();
            let provider_entry = ProviderEntry {
                name: kind.to_string(),
                kind: kind.clone(),
                api_key,
                base_url,
                preserve_reasoning,
                display_reasoning,
                models: vec![ModelEntry {
                    name: model_name,
                    alias: Some(synthetic_alias.clone()),
                    max_tokens: Some(max_tokens),
                    temperature,
                    extra_params: extra,
                    prompt_caching: caching,
                    vision,
                    context_size: None,
                    preserve_reasoning: preserve_reasoning.unwrap_or(true),
                    display_reasoning: display_reasoning.unwrap_or(false),
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
    /// *(simplicity is the key — one field tracks the active provider)*
    pub fn resolve_and_mirror_boot_provider(&mut self, registry: &ModelRegistry) -> ProviderConfig {
        let resolved = registry
            .resolve(registry.default_alias())
            .expect("default_alias is guaranteed to resolve by ModelRegistry::build")
            .provider_config
            .clone();
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
    Option<bool>,
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
            c.vision,
            None,
            None,
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
            c.preserve_reasoning,
            c.display_reasoning,
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
            None,
            None,
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
            None,
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

/// Canonical names of every built-in tool. Used to reject typos in the
/// `tools:` filter at load. Provider-gated tools (`view_image`, `doc_*`,
/// `delegate`) are listed too — naming one that isn't currently registered
/// is a harmless no-op, not an error.
pub const BUILTIN_TOOL_NAMES: &[&str] = &[
    "bash",
    "bash_bg",
    "delegate",
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
    "think",
    "todo",
    "view_image",
    "web_search",
];

/// Built-in tool filter. `disabled` is a blocklist (those are removed, the
/// rest stay); `only` is an allowlist (only those stay). The two are mutually
/// exclusive — setting both is a config error. Both empty = every tool stays.
#[derive(Debug, Deserialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    /// Blocklist: these tools are removed; every other tool stays.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Allowlist: only these tools stay; every other tool is removed.
    #[serde(default)]
    pub only: Vec<String>,
}

impl ToolsConfig {
    /// Boundary parse: reject the illegal `disabled`+`only` combination and
    /// any name that isn't a real tool (typo catch). Returns a user-facing
    /// message on failure.
    pub fn validate(&self) -> Result<(), String> {
        if !self.disabled.is_empty() && !self.only.is_empty() {
            return Err(
                "tools: set either `disabled` (blocklist) or `only` (allowlist), not both."
                    .to_string(),
            );
        }
        for name in self.disabled.iter().chain(self.only.iter()) {
            if !BUILTIN_TOOL_NAMES.contains(&name.as_str()) {
                return Err(format!(
                    "tools: unknown tool '{name}'. Known tools: {}.",
                    BUILTIN_TOOL_NAMES.join(", ")
                ));
            }
        }
        Ok(())
    }

    /// Whether a built-in tool of this name should be registered. Allowlist
    /// wins when present, then blocklist; empty filter allows everything.
    pub fn allows(&self, name: &str) -> bool {
        if !self.only.is_empty() {
            self.only.iter().any(|n| n == name)
        } else {
            !self.disabled.iter().any(|n| n == name)
        }
    }
}

/// Per-role skill filter (which skills a sub-agent sees in its prompt).
///
/// Mirrors [`ToolsConfig`] — `only` (allowlist) XOR `disabled` (blocklist),
/// both empty means "all skills" — plus an `enabled` short-circuit so a
/// focused role can be given no skills at all. Skill names are validated
/// against the discovered skill set in a second boundary pass (after skill
/// discovery), not here, since the names aren't known at config-parse time.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SkillFilter {
    /// Master switch: `false` gives this role no skills, ignoring the lists.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Blocklist: these skills are hidden; every other skill stays.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Allowlist: only these skills stay; every other skill is hidden.
    #[serde(default)]
    pub only: Vec<String>,
}

impl Default for SkillFilter {
    fn default() -> Self {
        Self {
            enabled: true,
            disabled: Vec::new(),
            only: Vec::new(),
        }
    }
}

impl SkillFilter {
    /// Boundary parse: reject the illegal `disabled`+`only` combination and
    /// any name not in `known` (typo catch). Called after skill discovery.
    pub fn validate(&self, role: &str, known: &[String]) -> Result<(), String> {
        if !self.disabled.is_empty() && !self.only.is_empty() {
            return Err(format!(
                "pipeline.agents.{role}.skills: set either `disabled` (blocklist) or \
                 `only` (allowlist), not both."
            ));
        }
        for name in self.disabled.iter().chain(self.only.iter()) {
            if !known.iter().any(|k| k == name) {
                return Err(format!(
                    "pipeline.agents.{role}.skills: unknown skill '{name}'. Known skills: {}.",
                    known.join(", ")
                ));
            }
        }
        Ok(())
    }

    /// Whether a skill of this name is shown to the role. `enabled: false`
    /// short-circuits to none; then allowlist wins, then blocklist.
    pub fn shows(&self, name: &str) -> bool {
        if !self.enabled {
            return false;
        }
        if !self.only.is_empty() {
            self.only.iter().any(|n| n == name)
        } else {
            !self.disabled.iter().any(|n| n == name)
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

        // pipelines list - override if non-empty (per-repo overrides
        // the master pipelines list wholesale; mirrors the `providers:`
        // rule above).
        if !other.pipelines.is_empty() {
            self.pipelines = other.pipelines;
        }

        // tools - override if other has a non-default filter
        if other.tools != ToolsConfig::default() {
            self.tools = other.tools;
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

        // persona - override if set (per-repo can replace the master persona).
        // Whitespace-only values are kept here; `Config::persona()` collapses
        // them to None at read time so "absent" stays a single representation.
        if other.persona.is_some() {
            self.persona = other.persona;
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: ProviderConfig::default(),
            providers: Vec::new(),
            default_model: None,
            persona: None,
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
            pipelines: Vec::new(),
            vector_db: None,
            web: WebConfig::default(),
            tools: ToolsConfig::default(),
            http: HttpConfig::default(),
            timeouts: TimeoutsConfig::default(),
        }
    }
}

/// Get the platform-specific config directory.
///
/// Public because `peakbot::install::web_token_path` (track I, plan §E.5) needs
/// it to locate `<config_dir>/web-token` next to `config.yaml`.
pub fn get_config_dir() -> Option<std::path::PathBuf> {
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

/// Load per-repository configuration from `<dir>/.peakbot/config.yaml`.
/// If the file exists and is valid, returns the parsed config.
/// If the file doesn't exist, returns None silently.
/// If the file is malformed, logs a warning and returns None.
///
/// `dir` is always supplied by the caller: post-boot the process cwd is
/// NOT the session cwd (nothing ever calls `set_current_dir`), so reading
/// `std::env::current_dir()` here would silently return the wrong repo's
/// config after `/cd`. (Invariant I-5 — the base directory for per-repo
/// config is an argument, not ambient state.)
fn load_per_repo_config(dir: &std::path::Path) -> Option<Config> {
    let per_repo_path = dir.join(".peakbot/config.yaml");

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
        // A malformed master config is fatal — see `load_yaml_config`.
        let (master, config_file_path) = load_yaml_config()?;
        let config_file_found = master.is_some();
        // The ONE legitimate process-cwd read: at boot it really is the
        // session cwd. Every post-boot path passes the dir explicitly.
        let per_repo = std::env::current_dir()
            .ok()
            .and_then(|d| load_per_repo_config(&d));
        let config = Self::merge_sources(master, per_repo);

        config.tools.validate().map_err(anyhow::Error::msg)?;
        config.timeouts.validate().map_err(anyhow::Error::msg)?;

        Ok(LoadedConfig {
            config,
            config_file_found,
            config_file_path,
        })
    }

    /// Re-read master + `<cwd>/.peakbot/config.yaml` for a live session.
    /// Replaces `reload()` — `cwd` must be the directory the session WILL
    /// be in after the calling verb completes (target for `/cd`, current
    /// `session_cwd` for the other verbs). Same precedence as
    /// [`Config::load`]; a malformed-master error maps to `Err(reason)`
    /// so the caller can keep the previous config and warn instead of
    /// crashing.
    pub fn reload_for(cwd: &std::path::Path) -> Result<Config, String> {
        let master = load_yaml_config().map_err(|e| e.to_string())?.0;
        Ok(Self::merge_sources(master, load_per_repo_config(cwd)))
    }

    /// Pure merge of the two config sources over defaults — the
    /// testable core shared by [`Config::load`] and [`Config::reload_for`].
    /// Master (if present) replaces defaults; per-repo (if present) is
    /// merged on top with top-level key override.
    fn merge_sources(master: Option<Config>, per_repo: Option<Config>) -> Config {
        let mut config = master.unwrap_or_default();
        if let Some(repo_config) = per_repo {
            config.merge_with(repo_config);
        }
        config
    }

    /// Adopt every field from `fresh` **except** `provider`, which is
    /// owned by the resolve step and must never be overwritten by a
    /// reload. This is the single point that enforces that invariant.
    pub fn adopt_reloaded(&mut self, fresh: Config) {
        let saved_provider = self.provider.clone();
        *self = fresh;
        self.provider = saved_provider;
    }
}

// ===========================================================================
// Track S — write path (plan §A-Q4, task S1).
//
// `save_config_at(dir, yaml)` is the pure, test-friendly core: it writes the
// exact bytes the caller hands it into `<dir>/config.yaml`, with a single-slot
// `<dir>/config.yaml.bak` of the previous bytes when one existed, atomic on
// POSIX (temp + rename), `0600` on Unix, and no survivors on success.
//
// `save_master_config(yaml)` is the production seam: it resolves the platform
// config path from `get_config_file_path()` and delegates to `save_config_at`
// on the parent directory. This is the only entry the HTTP /setup handler
// uses — the wizard posts the reviewed YAML, the server parses + validates it,
// and only on success writes those bytes verbatim. No `Serialize` on `Config`,
// no server-side rendering, no merging. *(principle of least astonishment)*
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
        std::fs::copy(&final_path, &backup_path)
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
    //    remove first there. On POSIX the remove is a no-op and rename is
    //    atomic. Either way, the .bak is already in place.
    if final_path.exists() {
        std::fs::remove_file(&final_path)
            .with_context(|| format!("remove old {}", final_path.display()))?;
    }
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename tmp → final at {}", final_path.display()))?;

    Ok(SaveOutcome {
        path: final_path,
        backup,
    })
}

/// Write the given YAML bytes to the platform master config file
/// (`get_config_file_path()`). On success returns the outcome for the
/// handler; on failure the caller surfaces the error to the wizard.
pub fn save_master_config(yaml: &str) -> anyhow::Result<SaveOutcome> {
    let path = get_config_file_path()
        .ok_or_else(|| anyhow::anyhow!("this platform has no config directory"))?;
    let parent = path.parent().ok_or_else(|| {
        anyhow::anyhow!("config path has no parent directory: {}", path.display())
    })?;
    save_config_at(parent, yaml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_filter_default_allows_everything() {
        let t = ToolsConfig::default();
        assert!(t.allows("bash"));
        assert!(t.allows("web_search"));
        assert!(t.validate().is_ok());
    }

    #[test]
    fn skill_filter_default_shows_everything() {
        let f = SkillFilter::default();
        assert!(f.shows("github"));
        assert!(f.shows("anything"));
        assert!(f.validate("role", &["github".into()]).is_ok());
    }

    #[test]
    fn skill_filter_disabled_master_switch_hides_all() {
        let f = SkillFilter {
            enabled: false,
            ..Default::default()
        };
        assert!(!f.shows("github"), "enabled=false hides every skill");
    }

    #[test]
    fn skill_filter_only_is_allowlist() {
        let f = SkillFilter {
            enabled: true,
            disabled: vec![],
            only: vec!["github".into()],
        };
        assert!(f.shows("github"));
        assert!(!f.shows("helius-solana"));
    }

    #[test]
    fn skill_filter_disabled_is_blocklist() {
        let f = SkillFilter {
            enabled: true,
            disabled: vec!["helius-solana".into()],
            only: vec![],
        };
        assert!(!f.shows("helius-solana"));
        assert!(f.shows("github"));
    }

    #[test]
    fn skill_filter_rejects_both_lists() {
        let f = SkillFilter {
            enabled: true,
            disabled: vec!["a".into()],
            only: vec!["b".into()],
        };
        let known = vec!["a".into(), "b".into()];
        assert!(f.validate("role", &known).is_err(), "both lists is illegal");
    }

    #[test]
    fn skill_filter_rejects_unknown_skill_name() {
        let f = SkillFilter {
            enabled: true,
            disabled: vec![],
            only: vec!["ghost".into()],
        };
        assert!(
            f.validate("role", &["github".into()]).is_err(),
            "an unknown skill name is a typo-catch error"
        );
    }

    #[test]
    fn tools_filter_blocklist_removes_named_keeps_rest() {
        let t = ToolsConfig {
            disabled: vec!["bash_bg".into(), "web_search".into()],
            only: vec![],
        };
        assert!(t.validate().is_ok());
        assert!(!t.allows("bash_bg"));
        assert!(!t.allows("web_search"));
        assert!(t.allows("file_read"));
    }

    #[test]
    fn tools_filter_allowlist_keeps_only_named() {
        let t = ToolsConfig {
            disabled: vec![],
            only: vec!["file_read".into(), "bash".into()],
        };
        assert!(t.validate().is_ok());
        assert!(t.allows("file_read"));
        assert!(t.allows("bash"));
        assert!(!t.allows("web_search"));
        assert!(!t.allows("todo"));
    }

    #[test]
    fn tools_filter_both_lists_is_error() {
        let t = ToolsConfig {
            disabled: vec!["bash".into()],
            only: vec!["file_read".into()],
        };
        assert!(t.validate().is_err());
    }

    #[test]
    fn tools_filter_unknown_tool_is_error() {
        let t = ToolsConfig {
            disabled: vec!["definitely_not_a_tool".into()],
            only: vec![],
        };
        assert!(t.validate().is_err());
    }

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

        assert_eq!(reg.default_alias(), "default");
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
                preserve_reasoning: None,
                display_reasoning: None,
                models: vec![ModelEntry {
                    name: "anthropic/claude-3.7-sonnet".into(),
                    alias: Some("sonnet".into()),
                    max_tokens: None,
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    vision: None,
                    context_size: None,
                    preserve_reasoning: true,
                    display_reasoning: false,
                }],
            }],
            default_model: Some("sonnet".into()),
            ..Config::default()
        };
        let reg = config.build_model_registry().expect("should build");
        assert_eq!(reg.default_alias(), "sonnet");
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
                preserve_reasoning: None,
                display_reasoning: None,
                models: vec![ModelEntry {
                    name: "anthropic/claude-3.7-sonnet".into(),
                    alias: Some("sonnet".into()),
                    max_tokens: None,
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    vision: None,
                    context_size: None,
                    preserve_reasoning: true,
                    display_reasoning: false,
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
                preserve_reasoning: None,
                display_reasoning: None,
                models: vec![ModelEntry {
                    name: "anthropic/claude-3.7-sonnet".into(),
                    alias: Some("sonnet".into()),
                    max_tokens: None,
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    vision: None,
                    context_size: None,
                    preserve_reasoning: true,
                    display_reasoning: false,
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
    fn http_timeouts_default_and_parse() {
        // Absent block: a config that never mentions http still gets bounded.
        let cfg: Config = serde_yaml::from_str("cost_tracking: true").unwrap();
        assert_eq!(cfg.http.connect_timeout_secs, 30);
        // The postmortem fix raised the read-timeout ceiling from 600 (10 min)
        // to 1800 (30 min) so a model that legitimately thinks for more than ten
        // minutes on a single non-streaming turn doesn't get cut off by the
        // HTTP backstop. Pinned here so a future tweak to the default is
        // deliberate.
        assert_eq!(cfg.http.read_timeout_secs, 1800);

        // Partial block: the unspecified knob keeps its default.
        let cfg: Config = serde_yaml::from_str("http:\n  read_timeout_secs: 1200\n").unwrap();
        assert_eq!(cfg.http.connect_timeout_secs, 30);
        assert_eq!(cfg.http.read_timeout_secs, 1200);

        // 0 is the documented "disabled" escape hatch, not an error.
        let cfg: Config = serde_yaml::from_str("http:\n  read_timeout_secs: 0\n").unwrap();
        assert_eq!(cfg.http.read_timeout_secs, 0);
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

    #[test]
    fn merge_sources_no_master_uses_defaults_plus_repo() {
        // Missing master file (None) → defaults, then per-repo merged on top.
        let repo = Config {
            agent_max_turns: 99,
            ..Config::default()
        };
        let merged = Config::merge_sources(None, Some(repo));
        assert_eq!(merged.agent_max_turns, 99);
    }

    #[test]
    fn merge_sources_repo_overrides_master() {
        // Per-repo top-level keys win over master (precedence contract).
        let master = Config {
            agent_max_turns: 10,
            ..Config::default()
        };
        let repo = Config {
            agent_max_turns: 42,
            ..Config::default()
        };
        let merged = Config::merge_sources(Some(master), Some(repo));
        assert_eq!(merged.agent_max_turns, 42);
    }

    #[test]
    fn merge_sources_master_only_when_no_repo() {
        // Master present, no per-repo → master survives unchanged.
        let master = Config {
            agent_max_turns: 7,
            ..Config::default()
        };
        let merged = Config::merge_sources(Some(master), None);
        assert_eq!(merged.agent_max_turns, 7);
    }

    #[test]
    fn adopt_reloaded_preserves_provider_adopts_ancillary() {
        // `provider` is owned by the resolve step: a reload must never
        // clobber it, but must adopt every ancillary field.
        let mut live = Config {
            agent_max_turns: 3,
            ..Config::default()
        };
        if let ProviderConfig::OpenRouter(ref mut c) = live.provider {
            c.model = "resolve-owned-model".to_string();
        }

        let fresh = Config {
            agent_max_turns: 77,
            ..Config::default() // default (different) provider
        };

        live.adopt_reloaded(fresh);

        // Provider is the resolve-owned one, NOT the fresh default.
        assert_eq!(live.model(), "resolve-owned-model");
        // Ancillary fields came from fresh.
        assert_eq!(live.agent_max_turns, 77);
    }

    // ── Phase 2: pipeline role shape (alias ref + env) ───────────────────

    /// The v2 role shape is `{ model?, prompt, env? }`. `model` names an
    /// alias from `providers:` (resolved later by the registry); `env` is
    /// optional and mirrors `bash.env`.
    #[test]
    fn pipeline_role_parses_alias_prompt_and_env() {
        let yaml = r#"
enabled: true
agents:
  researcher:
    model: flash
    prompt: "You research codebases."
  reviewer:
    prompt: "You review diffs."
    env:
      REVIEW_STRICT: "1"
"#;
        let cfg: PipelineConfig = serde_yaml::from_str(yaml).expect("v2 role shape must parse");
        assert!(cfg.enabled);

        let researcher = cfg.agents.get("researcher").expect("researcher present");
        assert_eq!(researcher.model.as_deref(), Some("flash"));
        assert_eq!(researcher.prompt, "You research codebases.");
        assert!(researcher.env.is_none());

        let reviewer = cfg.agents.get("reviewer").expect("reviewer present");
        // model omitted → None (registry falls back to default_model).
        assert!(reviewer.model.is_none());
        assert_eq!(
            reviewer
                .env
                .as_ref()
                .and_then(|e| e.get("REVIEW_STRICT"))
                .map(String::as_str),
            Some("1"),
        );
    }

    /// The old role shape carried provider fields (`type`, `api_key`,
    /// `base_url`). Those are gone in v2; `deny_unknown_fields` makes a
    /// stale config a hard, honest error rather than a silent no-op.
    #[test]
    fn pipeline_role_rejects_legacy_provider_fields() {
        let yaml = r#"
enabled: true
agents:
  researcher:
    type: openrouter
    prompt: "legacy shape"
"#;
        let err = serde_yaml::from_str::<PipelineConfig>(yaml)
            .expect_err("legacy `type:` field must be rejected");
        assert!(
            err.to_string().contains("type") || err.to_string().contains("unknown field"),
            "error should name the offending field, got: {err}"
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
    fn config_default_carries_timeouts_defaults() {
        // The manual `impl Default for Config` must wire the `timeouts` field
        // to `TimeoutsConfig::default()` — easy to forget on an edit, and a
        // mismatched default would leave every boot pinned to a weird hand-
        // rolled `TimeBudget` for every tool. Single equality assertion, no
        // field-by-field guesswork.
        assert_eq!(
            Config::default().timeouts,
            TimeoutsConfig::default(),
            "Config::default().timeouts must equal TimeoutsConfig::default() (manual Default impl)"
        );
    }

    #[test]
    fn absent_timeouts_block_yields_defaults() {
        // Mirrors the pattern set by `http_timeouts_default_and_parse`: a
        // config that never mentions `timeouts:` still parses cleanly and
        // gets the documented defaults. Without `#[serde(default)]` on the
        // field, an omitted block would be a hard error.
        let cfg: Config = serde_yaml::from_str("cost_tracking: true\n")
            .expect("absent timeouts block must still parse");
        assert_eq!(cfg.timeouts, TimeoutsConfig::default());
    }

    #[test]
    fn timeouts_block_parses_both_fields() {
        // Round-trip: explicit YAML produces the same struct a hand-built
        // literal would. The auto-generated serde defaults are easy to read
        // for a single field; the two-field interaction is the part that
        // genuinely needs pinning.
        let yaml = "timeouts:\n  tool_secs: 42\n  delegate_secs: 99\n";
        let cfg: Config = serde_yaml::from_str(yaml).expect("both fields present must parse");
        assert_eq!(cfg.timeouts.tool_secs, 42);
        assert_eq!(cfg.timeouts.delegate_secs, 99);
    }

    #[test]
    fn timeouts_partial_block_defaults_the_other_field() {
        // Per-field `#[serde(default)]`: leaving one key out must default
        // the OTHER key, not leave it zero. Catches a regression where the
        // implementer forgets one of the two defaults and an operator who
        // only tunes one knob silently disables the other.
        let cfg: Config = serde_yaml::from_str("timeouts:\n  tool_secs: 120\n")
            .expect("only-tool_secs block must parse");
        assert_eq!(
            cfg.timeouts.tool_secs, 120,
            "explicitly set value must be retained"
        );
        assert_eq!(
            cfg.timeouts.delegate_secs, 7_200,
            "unset delegate_secs must default to 7200, not zero"
        );

        let cfg: Config = serde_yaml::from_str("timeouts:\n  delegate_secs: 99\n")
            .expect("only-delegate_secs block must parse");
        assert_eq!(cfg.timeouts.delegate_secs, 99);
        assert_eq!(
            cfg.timeouts.tool_secs, 1_800,
            "unset tool_secs must default to 1800, not zero"
        );
    }

    #[test]
    fn timeouts_rejects_unknown_field() {
        // Sibling pin to the existing `deny_unknown_fields` pins in this
        // module: an unknown key inside `timeouts:` (e.g. a typo of
        // `tool_secs`) must be a hard parse error, not silently dropped.
        // The original incident was caused by silently-dropped knobs — the
        // same shape inside the new block would re-create it.
        let yaml = "timeouts:\n  tool_seccs: 42\n";
        let err = serde_yaml::from_str::<Config>(yaml)
            .expect_err("unknown field inside timeouts block must fail to parse");
        let msg = err.to_string();
        assert!(
            msg.contains("tool_seccs"),
            "error must name the offending key, got: {msg}"
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

    // --- P1: persona config key (plan §A-Q7) ----------------------------------
    //
    // These tests are RED-by-design. They compile against the existing
    // `serde_yaml::from_str::<Config>` seam and assert that a `persona:` key
    // is accepted. Today (pre-impl) `Config` has `#[serde(deny_unknown_fields)]`
    // and no `persona:` field, so `serde_yaml::from_str` returns Err and the
    // assertion fires. When P1 lands — adding `pub persona: Option<String>`
    // with `#[serde(default)]` — these tests turn GREEN.
    //
    // The exact-string round-trip is locked here because the rule "leading
    // whitespace in a persona must survive" is what the explicit |2- indent
    // indicator exists for. Without it the YAML emitter cannot safely emit
    // free-form multi-line persona text (§A-Q7).
    //
    // *Do not* call `cfg.persona()` here — that accessor is part of P1 and
    // is asserted separately in `tests/persona_round_trip_tests.rs`.

    /// A YAML with no `persona:` key parses fine today (the field is absent
    /// in pre-impl and `#[serde(default)]` in post-impl). This is the
    /// "absence = built-in" baseline the wizard relies on.
    #[test]
    fn p1_persona_absent_in_yaml_parses_as_default_config() {
        let cfg: Config = serde_yaml::from_str("").expect("empty config must parse");
        // Pre-impl: no `persona` field — this is a smoke test that the
        // empty document parses. Post-impl: `persona` is `None` by default.
        let _ = cfg;
    }

    /// A bare `persona: |2-` with a normal first line (no leading space) must
    /// round-trip to the exact emitted string. The explicit `2` indicator
    /// is the load-bearing detail (plan §A-Q7) — without it, a persona whose
    /// first line starts with a space silently corrupts every following line.
    #[test]
    fn p1_persona_block_scalar_with_explicit_indent_indicator_parses() {
        let yaml = "\
persona: |2-
  You are a coding agent working in the user's local filesystem.

  State what you are about to do in one line, do it, then report what changed.
";
        let result: Result<Config, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_ok(),
            "persona: |2- block must be accepted by Config; got: {:?}",
            result.err()
        );
    }

    /// The `|2-` indicator exists precisely so a persona whose **first**
    /// line starts with a space survives verbatim — without it, YAML infers
    /// the indent from the first non-empty line and silently strips the
    /// leading space (and the spaces on every following line).
    #[test]
    fn p1_persona_first_line_starting_with_space_round_trips() {
        let yaml = "persona: |2-\n  indented-first-line\n  second\n";
        let cfg: Config =
            serde_yaml::from_str(yaml).expect("persona: with leading-space content must parse");
        // Pre-impl: this assertion is dead-code-friendly because the field
        // doesn't exist; once P1 lands, the accessor returns the trimmed
        // first line. We *do not* assert the exact string here — that lives
        // in tests/persona_round_trip_tests.rs, which targets the planned
        // public API and stays compile-fail until P1 lands.
        let _ = cfg;
    }

    /// A `|2-` block containing a blank line in the middle must keep it —
    /// blank lines are content (a paragraph break in the persona prose),
    /// not trailing whitespace.
    #[test]
    fn p1_persona_with_blank_line_in_middle_parses() {
        let yaml = "persona: |2-\n  first paragraph\n\n  second paragraph\n";
        let cfg: Config = serde_yaml::from_str(yaml).expect("persona: with blank line must parse");
        let _ = cfg;
    }

    /// `deny_unknown_fields` is the security argument for the whole write
    /// path (§A-Q4 — the only thing standing between a hostile browser and
    /// the on-disk config). When P1 lands it must keep `deny_unknown_fields`
    /// AND accept `persona:` — these are two separate guarantees and the
    /// test below locks both.
    #[test]
    fn p1_deny_unknown_fields_still_rejects_real_unknown_keys() {
        let yaml = "persona: |2-\n  x\nthis_is_definitely_not_a_real_key: 1\n";
        let result: Result<Config, _> = serde_yaml::from_str(yaml);
        // Whether or not `persona:` is accepted, `this_is_definitely_not_a_real_key`
        // MUST still be rejected. This pins the security argument independently
        // of the persona field's existence.
        assert!(
            result.is_err(),
            "deny_unknown_fields must still reject unknown keys after P1 lands"
        );
    }

    // ============================================================================
    // Stage 1.1 — Multi-pipeline config layer (plan §3.3 / §7).
    //
    // RED by design: PipelineDef, OrchestratorDef, Members, Config.pipelines
    // do not exist yet. These tests pin the serde surface and the manual-Default
    // / merge_with behaviour the builder must land in commit 1/3 of the
    // multi-pipeline PR. Compile errors are the expected initial state.
    //
    // Naming convention mirrors the existing pins in this module
    // (e.g. `config_default_carries_timeouts_defaults`,
    // `p1_deny_unknown_fields_still_rejects_real_unknown_keys`).
    // ============================================================================

    /// The minimum viable `providers:` block for a Config that parses a
    /// `pipelines:` block. Two aliases (`sonnet`, `flash`) and a default
    /// (`flash`) — matches the registry fixture in `src/pipeline/registry.rs`
    /// so the same YAML shape can be reused by the PipelineSet tests below.
    const STAGE11_PROVIDERS_YAML: &str = "\
providers:
  - name: openrouter
    type: openrouter
    api_key: sk-or-test
    models:
      - name: anthropic/claude-3.7-sonnet
        alias: sonnet
      - name: google/gemini-2.0-flash-001
        alias: flash
default_model: flash
";

    #[test]
    fn stage11_two_pipeline_yaml_parses_in_declaration_order() {
        // The plan pins that `pipelines:` is an ordered Vec (declaration
        // order = UI order — §3 "ordered; declaration order is UI order").
        // Parse two pipelines where the second one has no orchestrator
        // body and confirm:
        //   1. .len() == 2 in declaration order
        //   2. every member field on AgentDefinition survives (model,
        //      prompt, skills, agents_md, env) — proves Members Derefs
        //      to the existing AgentDefinition struct unchanged
        //   3. all three orchestrator fields parse: model, prompt,
        //      AND the amendment-1 persona
        let yaml = format!(
            "{STAGE11_PROVIDERS_YAML}\
pipelines:
  - name: web-team
    orchestrator:
      model: sonnet
      prompt: \"You lead the web team. Delegate UI work.\"
      persona: \"You are a focused orchestrator.\"
    agents:
      reviewer:
        model: flash
        prompt: \"You review diffs.\"
        skills:
          only: [github]
        agents_md: true
        env:
          REVIEW_STRICT: \"1\"
      tester:
        prompt: \"You write failing tests first.\"
  - name: research-crew
    orchestrator: {{}}
    agents:
      reviewer:
        prompt: \"You critique sources.\"
"
        );

        let cfg: Config =
            serde_yaml::from_str(&yaml).expect("two-pipeline YAML must parse under the new shape");

        assert_eq!(
            cfg.pipelines.len(),
            2,
            "pipelines: Vec preserves declaration order; got: {:?}",
            cfg.pipelines.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        assert_eq!(cfg.pipelines[0].name, "web-team");
        assert_eq!(cfg.pipelines[1].name, "research-crew");

        // --- orchestrator fields (amendment 1: model + prompt + persona) ---
        let orch0 = &cfg.pipelines[0].orchestrator;
        assert_eq!(orch0.model.as_deref(), Some("sonnet"));
        assert_eq!(
            orch0.prompt.as_deref(),
            Some("You lead the web team. Delegate UI work.")
        );
        assert_eq!(
            orch0.persona.as_deref(),
            Some("You are a focused orchestrator.")
        );

        // --- agents: Deref<HashMap<String, AgentDefinition>> keeps the
        // existing field shape end-to-end (skills, env, agents_md) ---
        let members0 = &cfg.pipelines[0].agents;
        let reviewer = members0
            .get("reviewer")
            .expect("reviewer role survives parse");
        assert_eq!(reviewer.model.as_deref(), Some("flash"));
        assert_eq!(reviewer.prompt, "You review diffs.");
        assert_eq!(reviewer.agents_md, true);
        assert_eq!(
            reviewer.skills.only,
            vec!["github".to_string()],
            "skills.only: must parse through Member's Deref<HashMap<…, AgentDefinition>>"
        );
        assert_eq!(
            reviewer
                .env
                .as_ref()
                .and_then(|e| e.get("REVIEW_STRICT"))
                .map(String::as_str),
            Some("1"),
            "env: per-role vars must round-trip"
        );

        // tester has no `model:` — registry's `default_model` is the fallback.
        let tester = members0.get("tester").expect("tester role survives parse");
        assert!(tester.model.is_none());
        assert_eq!(tester.prompt, "You write failing tests first.");

        // Second pipeline: `orchestrator: {}` — see the dedicated test below.
        assert!(cfg.pipelines[1].orchestrator.model.is_none());
        assert!(cfg.pipelines[1].orchestrator.prompt.is_none());
        assert!(cfg.pipelines[1].orchestrator.persona.is_none());
    }

    #[test]
    fn stage11_missing_orchestrator_key_is_a_serde_error_naming_the_field() {
        // D1 / amendment 1: `orchestrator:` is REQUIRED on each pipeline entry,
        // so "no orchestrator" is a *parse* error, not a runtime validation
        // rule. This is the whole reason OrchestratorDef is its own struct
        // rather than a reserved member key.
        //
        // The error MUST name `orchestrator` so an operator staring at the
        // boot log knows which field they forgot — `serde` with `deny_unknown_fields`
        // does not help here, `missing field` is the load-bearing keyword.
        let yaml = "\
providers:
  - name: openrouter
    type: openrouter
    api_key: sk-or-test
    models:
      - name: m
        alias: m
default_model: m

pipelines:
  - name: web-team
    agents:
      reviewer:
        prompt: x
";
        let err = serde_yaml::from_str::<Config>(yaml)
            .expect_err("missing orchestrator: key must fail to parse");
        let msg = err.to_string();
        assert!(
            msg.contains("orchestrator"),
            "error must name the missing `orchestrator` field; got: {msg}"
        );
    }

    #[test]
    fn stage11_typo_in_orchestrator_key_is_rejected_by_deny_unknown_fields() {
        // `#[serde(deny_unknown_fields)]` on PipelineDef means a typo at the
        // entry level (`orchestartor:` instead of `orchestrator:`) is rejected
        // at parse, not silently ignored. The error must name the typo so the
        // user sees it. Sibling pin to `timeouts_rejects_unknown_field` and
        // `p1_deny_unknown_fields_still_rejects_real_unknown_keys`.
        let yaml = "\
providers:
  - name: openrouter
    type: openrouter
    api_key: sk-or-test
    models:
      - name: m
        alias: m
default_model: m

pipelines:
  - name: web-team
    orchestartor: {}
    agents:
      reviewer:
        prompt: x
";
        let err = serde_yaml::from_str::<Config>(yaml)
            .expect_err("typo `orchestartor:` must fail deny_unknown_fields");
        let msg = err.to_string();
        assert!(
            msg.contains("orchestartor"),
            "error must name the offending (typo) key `orchestartor`; got: {msg}"
        );
    }

    #[test]
    fn stage11_typo_in_orchestrator_block_is_rejected_by_deny_unknown_fields() {
        // Same denial, one level deeper: a typo *inside* the `orchestrator:`
        // block must also be rejected (the block itself carries
        // `deny_unknown_fields` per plan §3.3). The classic mistake is
        // `modell:` instead of `model:` — silent-default would mean the
        // orchestrator boots on `default_model` instead of the alias the
        // user thought they picked.
        let yaml = "\
providers:
  - name: openrouter
    type: openrouter
    api_key: sk-or-test
    models:
      - name: m
        alias: m
default_model: m

pipelines:
  - name: web-team
    orchestrator:
      modell: sonnet
    agents:
      reviewer:
        prompt: x
";
        let err = serde_yaml::from_str::<Config>(yaml)
            .expect_err("typo `modell:` inside orchestrator: must fail deny_unknown_fields");
        let msg = err.to_string();
        assert!(
            msg.contains("modell"),
            "error must name the offending inner key `modell`; got: {msg}"
        );
    }

    #[test]
    fn stage11_empty_orchestrator_block_parses_with_all_none_fields() {
        // Per plan §2 the `orchestrator: {}` shorthand means "inherit
        // default_model, no prompt addendum, no persona override". All three
        // fields must be `None` after parse — this pins that the orchestrator
        // struct is built with `#[serde(default)]` on every field, not that
        // the field is missing entirely (which would itself be a parse error
        // — covered by `stage11_missing_orchestrator_key_is_a_serde_error_naming_the_field`).
        let yaml = "\
providers:
  - name: openrouter
    type: openrouter
    api_key: sk-or-test
    models:
      - name: m
        alias: m
default_model: m

pipelines:
  - name: research-crew
    orchestrator: {}
    agents:
      reviewer:
        prompt: x
";
        let cfg: Config = serde_yaml::from_str(yaml).expect("orchestrator: {} must parse");
        let orch = &cfg.pipelines[0].orchestrator;
        assert!(
            orch.model.is_none(),
            "orchestrator.model must default to None when omitted"
        );
        assert!(
            orch.prompt.is_none(),
            "orchestrator.prompt must default to None when omitted"
        );
        assert!(
            orch.persona.is_none(),
            "orchestrator.persona must default to None when omitted (amendment 1)"
        );
    }

    #[test]
    fn stage11_duplicate_member_key_under_agents_is_rejected() {
        // D4: serde_yaml silently last-wins duplicate HashMap keys, so
        // `Members` is a newtype with a manual Deserialize that rejects
        // duplicates. This is the load-bearing reason Members is its own
        // type — a plain HashMap would let this YAML boot and silently
        // discard the first `reviewer:` entry. The error must name the
        // duplicate key so the user knows which member to rename.
        //
        // Plan table: "Duplicate member | serde parse error:
        // `pipelines[0].agents: duplicate member 'reviewer'.`"
        let yaml = "\
providers:
  - name: openrouter
    type: openrouter
    api_key: sk-or-test
    models:
      - name: m
        alias: m
default_model: m

pipelines:
  - name: web-team
    orchestrator: {}
    agents:
      reviewer:
        prompt: first
      reviewer:
        prompt: second
";
        let err = serde_yaml::from_str::<Config>(yaml)
            .expect_err("duplicate member key must fail at parse, not silently last-win");
        let msg = err.to_string();
        assert!(
            msg.contains("duplicate") && msg.contains("reviewer"),
            "error must call out duplicate and the offending key 'reviewer'; got: {msg}"
        );
    }

    #[test]
    fn stage11_pipelines_field_round_trips_through_members_deref() {
        // `Members` is documented as a Deref<Target = HashMap<…>> so callers
        // can iterate / index without ceremony. Pin the Deref surface
        // explicitly so a future impl choice (e.g. wrapping in Vec to
        // preserve declaration order on the *member* side too) doesn't
        // silently break callers that read members like a map.
        let yaml = format!(
            "{STAGE11_PROVIDERS_YAML}\
pipelines:
  - name: web-team
    orchestrator: {{}}
    agents:
      reviewer:
        prompt: review
      tester:
        prompt: test
"
        );
        let cfg: Config = serde_yaml::from_str(&yaml).expect("parses");
        let members = &cfg.pipelines[0].agents;
        // HashMap-style accessors via Deref:
        assert!(members.contains_key("reviewer"));
        assert!(members.contains_key("tester"));
        assert_eq!(members.len(), 2);
        assert_eq!(
            members.get("reviewer").map(|d| d.prompt.as_str()),
            Some("review")
        );
    }

    #[test]
    fn stage11_manual_default_pipelines_is_empty() {
        // Sibling pin to `config_default_carries_timeouts_defaults` (line
        // ~2932). The manual `impl Default for Config` is a known trap —
        // forgetting to wire `pipelines: Vec::new()` would default it via
        // `Vec::default()` accidentally (which is empty too, but only by
        // coincidence — and only if the field even exists). One assertion
        // is enough; the post-impl code must explicitly initialize it.
        assert!(
            Config::default().pipelines.is_empty(),
            "Config::default().pipelines must be empty (manual Default impl pin)"
        );
    }

    #[test]
    fn stage11_merge_with_per_repo_pipelines_overrides_master() {
        // Mirrors the `providers:` override rule: per-repo non-empty list
        // replaces master wholesale. Master had one pipeline ("alpha"),
        // per-repo has a different one ("beta") — the merged config must
        // show only "beta".
        let yaml_alpha = format!(
            "{STAGE11_PROVIDERS_YAML}\
pipelines:
  - name: alpha
    orchestrator: {{}}
    agents:
      r:
        prompt: p
"
        );
        let yaml_beta = format!(
            "{STAGE11_PROVIDERS_YAML}\
pipelines:
  - name: beta
    orchestrator: {{}}
    agents:
      r:
        prompt: p
"
        );
        let mut master: Config = serde_yaml::from_str(&yaml_alpha).expect("master parses");
        let repo: Config = serde_yaml::from_str(&yaml_beta).expect("repo parses");

        master.merge_with(repo);

        assert_eq!(master.pipelines.len(), 1, "non-empty repo must override");
        assert_eq!(master.pipelines[0].name, "beta");
    }

    #[test]
    fn stage11_merge_with_empty_per_repo_keeps_master_pipelines() {
        // Mirror: an absent / empty `pipelines:` in the per-repo config
        // must NOT clobber the master's pipelines. (Same shape as
        // `test_merge_preserves_master_when_repo_doesnt_override` for the
        // `provider` field — pin it for the new field so a future
        // refactor doesn't accidentally turn the empty case into an
        // override.)
        let yaml_alpha = format!(
            "{STAGE11_PROVIDERS_YAML}\
pipelines:
  - name: alpha
    orchestrator: {{}}
    agents:
      r:
        prompt: p
"
        );
        let mut master: Config = serde_yaml::from_str(&yaml_alpha).expect("master parses");
        // Per-repo with no `pipelines:` block — .pipelines defaults to [].
        let repo: Config =
            serde_yaml::from_str(STAGE11_PROVIDERS_YAML).expect("empty pipelines repo parses");

        assert!(repo.pipelines.is_empty(), "fixture: repo has no pipelines");
        master.merge_with(repo);

        assert_eq!(
            master.pipelines.len(),
            1,
            "empty per-repo pipelines must preserve master pipelines"
        );
        assert_eq!(master.pipelines[0].name, "alpha");
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

    // =========================================================================
    // Reload-safe `pipelines:` (ticket pipelines-reload.md §8, tests 1–5).
    //
    // The headline change is `load_per_repo_config(dir: &Path)` — the
    // process cwd is no longer read here, so `/cd` into a repo with its
    // own `.peakbot/config.yaml` finally reads THAT repo's config instead
    // of the boot tree's. Test #1 is the regression pin for the
    // load-bearing bug; tests #2–#5 lock the merge semantics against
    // the new signature.
    // =========================================================================

    /// `load_per_repo_config` must read from the directory it is handed,
    /// not from `std::env::current_dir()`. Two tempdirs, only A has a
    /// `.peakbot/config.yaml` with `pipelines:` — the assertion is that
    /// `load_per_repo_config(A)` is `Some` with that pipeline and
    /// `load_per_repo_config(B)` is `None`, **without the test ever
    /// touching the process cwd** (which `agents.md` is documented to
    /// leak into prompts).
    ///
    /// This is the regression pin for the "after /cd the reload re-reads
    /// the OLD repo" bug — see the §1 SUMMARY in the ticket.
    #[test]
    fn per_repo_config_is_read_from_the_given_dir_not_the_process_cwd() {
        let tmp_a = tempfile::tempdir().expect("tmpdir A");
        let tmp_b = tempfile::tempdir().expect("tmpdir B");

        // Only A has a per-repo file with a `pipelines:` block.
        let peakbot_a = tmp_a.path().join(".peakbot");
        std::fs::create_dir_all(&peakbot_a).expect("mkdir .peakbot in A");
        std::fs::write(
            peakbot_a.join("config.yaml"),
            "pipelines:\n  - name: alpha\n    orchestrator: {}\n    agents:\n      r:\n        prompt: p\n",
        )
        .expect("write A");

        // Sanity: the process cwd is somewhere else. This matters because
        // the OLD implementation (`std::env::current_dir()`) would have
        // returned None when run from the repo root — the bug shipped
        // BECAUSE tests didn't assert the cwd path. Recording the cwd
        // here documents the precondition; the test will still pass on
        // any cwd because the assertion is on A vs. B.
        let cwd_before = std::env::current_dir().expect("current_dir");

        let from_a = super::load_per_repo_config(tmp_a.path());
        let from_b = super::load_per_repo_config(tmp_b.path());

        // cwd was never touched — the test would still pass if the impl
        // moved it, but recording it catches the more obvious cheating
        // path of "set cwd to A first".
        assert_eq!(
            std::env::current_dir().expect("current_dir after"),
            cwd_before,
            "the test itself must not mutate the process cwd (else the \
             regression pin is meaningless — it'd pass on the OLD impl \
             too by setting cwd = A first)"
        );

        let cfg_a = from_a.expect("A has a .peakbot/config.yaml and must be parsed");
        assert_eq!(
            cfg_a.pipelines.len(),
            1,
            "A's per-repo config must carry its pipelines: block"
        );
        assert_eq!(cfg_a.pipelines[0].name, "alpha");
        assert!(
            from_b.is_none(),
            "B has no .peakbot/config.yaml; must silently return None"
        );
    }

    /// Wholesale-override rule for `pipelines:` — per-repo's non-empty
    /// list replaces the master's. Same shape as the existing
    /// `stage11_merge_with_per_repo_pipelines_overrides_master`, but
    /// driven through `merge_sources(master, per_repo)` directly (the
    /// private seam the new code calls), so a future refactor of
    /// `merge_with` doesn't break the reload path while leaving the
    /// direct-merge test green.
    #[test]
    fn per_repo_pipelines_override_master_wholesale_via_merge_sources() {
        let master = Config {
            pipelines: vec![PipelineDef {
                name: "alpha".into(),
                orchestrator: OrchestratorDef::default(),
                agents: Members::default(),
            }],
            ..Config::default()
        };
        let per_repo = Config {
            pipelines: vec![PipelineDef {
                name: "beta".into(),
                orchestrator: OrchestratorDef::default(),
                agents: Members::default(),
            }],
            ..Config::default()
        };
        let merged = Config::merge_sources(Some(master), Some(per_repo));
        let names: Vec<&str> = merged.pipelines.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["beta"],
            "non-empty per-repo pipelines must replace the master list \
             wholesale (mirrors the providers: rule at src/config/mod.rs:1646-1650)"
        );
    }

    /// An absent / empty `pipelines:` in the per-repo config must NOT
    /// clobber the master's pipelines. Pins the "non-empty overrides"
    /// half of `merge_with` for `pipelines:` (`:1634-1639`).
    ///
    /// This is the case that hits `/cd` from a repo with no
    /// `pipelines:` into a master that declares one — the master teams
    /// must survive.
    #[test]
    fn empty_per_repo_pipelines_keep_master_pipelines_via_merge_sources() {
        let master = Config {
            pipelines: vec![PipelineDef {
                name: "alpha".into(),
                orchestrator: OrchestratorDef::default(),
                agents: Members::default(),
            }],
            ..Config::default()
        };
        // Per-repo with `pipelines: []` (explicit empty) — the "absent"
        // case is also pinned here because `Config::default().pipelines`
        // is empty, so the same assertion covers both.
        let per_repo = Config::default();
        assert!(
            per_repo.pipelines.is_empty(),
            "fixture: per-repo has no pipelines"
        );
        let merged = Config::merge_sources(Some(master), Some(per_repo));
        assert_eq!(
            merged.pipelines.len(),
            1,
            "empty per-repo pipelines must preserve the master list"
        );
        assert_eq!(merged.pipelines[0].name, "alpha");
    }

    /// Malformed YAML at `<dir>/.peakbot/config.yaml` must NOT panic or
    /// return `Err` — `load_per_repo_config` is intentionally infallible
    /// (`Option<Config>`), so a typo'd per-repo config degrades to
    /// "no per-repo overlay" with a `tracing::warn!` instead of crashing
    /// the session. Pre-existing behaviour (`:1788-1791`); the test pins
    /// it against the new `dir: &Path` signature.
    #[test]
    fn malformed_per_repo_config_is_ignored() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let peakbot = tmp.path().join(".peakbot");
        std::fs::create_dir_all(&peakbot).expect("mkdir .peakbot");
        std::fs::write(
            peakbot.join("config.yaml"),
            "this: is: not: valid: yaml: [unterminated",
        )
        .expect("write malformed");

        let result = super::load_per_repo_config(tmp.path());
        assert!(
            result.is_none(),
            "malformed per-repo YAML must degrade to None (warn-and-ignore), \
             not crash the caller; got: {result:?}"
        );
    }

    /// No `<dir>/.peakbot/config.yaml` at all — silent `None`. The
    /// `None`-on-missing contract is the difference between "this repo
    /// has its own config" and "no per-repo overlay, fall through to
    /// master"; without it, every `load_per_repo_config` would have to
    /// log a "no config" debug line on the common path.
    #[test]
    fn missing_per_repo_config_is_silent() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        assert!(
            super::load_per_repo_config(tmp.path()).is_none(),
            "no .peakbot/config.yaml in the dir must silently return None"
        );
    }
}
