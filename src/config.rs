#![allow(dead_code)]

use directories_next::ProjectDirs;
use serde::Deserialize;
use std::collections::HashMap;

/// Provider type enum - identifies which LLM provider to use
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    #[default]
    OpenRouter,
    Ollama,
}

/// Configuration for OpenRouter provider
#[derive(Debug, Deserialize, Clone)]
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
#[derive(Debug, Deserialize, Clone)]
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

fn default_ollama_url() -> String {
    "http://localhost:11434".to_string()
}

/// Provider configuration - specifies which provider and its specific config
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", content = "config")]
pub enum ProviderConfig {
    #[serde(rename = "openrouter")]
    OpenRouter(OpenRouterConfig),
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

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// LLM Provider configuration (OpenRouter or Ollama)
    #[serde(default)]
    pub provider: ProviderConfig,

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

    // === DEPRECATED: Legacy config fields for backward compatibility ===
    /// Legacy: OpenRouter API key (use provider.config.api_key instead)
    #[serde(default)]
    pub openrouter_api_key: Option<String>,
    /// Legacy: OpenRouter model (use provider.config.model instead)
    #[serde(default)]
    pub openrouter_model: Option<String>,
    /// Legacy: Max tokens (use provider.config.max_tokens instead)
    #[serde(default)]
    pub openrouter_max_tokens: Option<u64>,
}

impl Config {
    /// Get the model name from the provider config
    pub fn model(&self) -> &str {
        match &self.provider {
            ProviderConfig::OpenRouter(c) => &c.model,
            ProviderConfig::Ollama(c) => &c.model,
        }
    }

    /// Get the max tokens from the provider config
    pub fn max_tokens(&self) -> u64 {
        match &self.provider {
            ProviderConfig::OpenRouter(c) => c.max_tokens,
            ProviderConfig::Ollama(_) => 4096, // Ollama doesn't have this setting in the same way
        }
    }

    /// Get the API key for OpenRouter (if applicable)
    pub fn openrouter_api_key(&self) -> Option<&str> {
        match &self.provider {
            ProviderConfig::OpenRouter(c) => c.api_key.as_deref(),
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
}

#[derive(Debug, Deserialize, Clone)]
pub struct McpServerConfig {
    /// Unique name for this MCP server
    pub name: String,
    /// Command to run
    pub command: String,
    /// Arguments for the command
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Environment variables
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize, Clone)]
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
#[derive(Debug, Deserialize, Clone)]
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
    /// Model context window size (0 or None = auto-detect from API)
    /// Common values: 128k, 200k, etc.
    #[serde(default)]
    pub context_window: Option<usize>,
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

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: ProviderConfig::default(),
            agent_max_turns: default_max_turns(),
            mcp_servers: None,
            searxng: None,
            cost_tracking: default_cost_tracking(),
            context: ContextConfig {
                threshold: default_threshold(),
                keep_recent: default_keep_recent(),
                enabled: default_context_enabled(),
                context_window: None,
            },
            // Legacy fields default to None
            openrouter_api_key: None,
            openrouter_model: None,
            openrouter_max_tokens: None,
        }
    }
}

/// Get the platform-specific config directory
fn get_config_dir() -> Option<std::path::PathBuf> {
    ProjectDirs::from("com", "peakbot", "peakbot").map(|dirs| dirs.config_dir().to_path_buf())
}

/// Load configuration from YAML file in the platform config directory
fn load_yaml_config() -> Option<Config> {
    let config_dir = get_config_dir()?;
    let config_path = config_dir.join("config.yaml");

    tracing::debug!("Looking for config at: {}", config_path.display());

    if !config_path.exists() {
        tracing::debug!("No YAML config found at {}", config_path.display());
        return None;
    }

    let content = std::fs::read_to_string(&config_path).ok()?;
    tracing::debug!("Config file content: {}", content);

    let config: Config = serde_yaml::from_str(&content).ok()?;

    tracing::info!("Loaded YAML config from {}", config_path.display());
    Some(config)
}

impl Config {
    /// Load configuration from YAML file first, then environment variables.
    /// Environment variables take precedence over YAML config.
    pub fn load() -> anyhow::Result<Self> {
        // Start with YAML config if present, otherwise use defaults
        let mut config = load_yaml_config().unwrap_or_default();

        tracing::debug!("Config after YAML load: provider = {:?}", config.provider);

        // === Backward Compatibility: Migrate legacy YAML config to new provider format ===
        // If provider is still default (OpenRouter with defaults) but legacy fields exist,
        // migrate them to the new provider format
        if let ProviderConfig::OpenRouter(ref mut provider_cfg) = config.provider {
            // Check for legacy OpenRouter config fields in YAML
            if let Some(ref api_key) = config.openrouter_api_key {
                if provider_cfg.api_key.is_none() && !api_key.is_empty() {
                    provider_cfg.api_key = Some(api_key.clone());
                }
            }
            if let Some(ref model) = config.openrouter_model {
                if !model.is_empty() {
                    provider_cfg.model = model.clone();
                }
            }
            if let Some(tokens) = config.openrouter_max_tokens {
                provider_cfg.max_tokens = tokens;
            }
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
                {
                    if let ProviderConfig::OpenRouter(ref mut c) = config.provider {
                        c.api_key = Some(api_key);
                    }
                }

                if let Ok(model) = std::env::var("OPENROUTER_MODEL")
                    && !model.is_empty()
                {
                    if let ProviderConfig::OpenRouter(ref mut c) = config.provider {
                        c.model = model;
                    }
                }

                if let Ok(tokens) = std::env::var("OPENROUTER_MAX_TOKENS")
                    && let Ok(tokens) = tokens.parse()
                {
                    if let ProviderConfig::OpenRouter(ref mut c) = config.provider {
                        c.max_tokens = tokens;
                    }
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
        if let Ok(url) = std::env::var("SEARXNG_BASE_URL") {
            if !url.is_empty() {
                let searxng = config.searxng.get_or_insert_with(|| SearXngConfig {
                    base_url: String::new(),
                    enabled: true,
                    timeout_seconds: 30,
                    max_results: 10,
                });
                searxng.base_url = url;
            }
        }

        // SEARXNG_ENABLED
        if let Ok(enabled) = std::env::var("SEARXNG_ENABLED") {
            if let Ok(enabled) = enabled.parse() {
                let searxng = config.searxng.get_or_insert_with(|| SearXngConfig {
                    base_url: String::new(),
                    enabled: true,
                    timeout_seconds: 30,
                    max_results: 10,
                });
                searxng.enabled = enabled;
            }
        }

        // SEARXNG_TIMEOUT
        if let Ok(timeout) = std::env::var("SEARXNG_TIMEOUT") {
            if let Ok(timeout) = timeout.parse() {
                if let Some(searxng) = config.searxng.as_mut() {
                    searxng.timeout_seconds = timeout;
                }
            }
        }

        // SEARXNG_MAX_RESULTS
        if let Ok(max) = std::env::var("SEARXNG_MAX_RESULTS") {
            if let Ok(max) = max.parse() {
                if let Some(searxng) = config.searxng.as_mut() {
                    searxng.max_results = max;
                }
            }
        }

        // COST_TRACKING
        if let Ok(enabled) = std::env::var("COST_TRACKING") {
            if let Ok(enabled) = enabled.parse() {
                config.cost_tracking = enabled;
            }
        }

        // Context compaction config via environment variables
        // CONTEXT_ENABLED
        if let Ok(enabled) = std::env::var("CONTEXT_ENABLED") {
            if let Ok(enabled) = enabled.parse() {
                config.context.enabled = enabled;
            }
        }

        // CONTEXT_THRESHOLD
        if let Ok(threshold) = std::env::var("CONTEXT_THRESHOLD") {
            if let Ok(threshold) = threshold.parse::<f64>() {
                if threshold >= 0.0 && threshold <= 1.0 {
                    config.context.threshold = threshold;
                }
            }
        }

        // CONTEXT_KEEP_RECENT
        if let Ok(keep_recent) = std::env::var("CONTEXT_KEEP_RECENT") {
            if let Ok(keep_recent) = keep_recent.parse() {
                config.context.keep_recent = keep_recent;
            }
        }

        // CONTEXT_WINDOW
        if let Ok(window) = std::env::var("CONTEXT_WINDOW") {
            if let Ok(window) = window.parse() {
                config.context.context_window = Some(window);
            }
        }

        Ok(config)
    }
}
