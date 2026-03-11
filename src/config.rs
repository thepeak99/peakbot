#![allow(dead_code)]

use directories_next::ProjectDirs;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// OpenRouter API key
    #[serde(default)]
    pub openrouter_api_key: Option<String>,
    /// Model to use (default: anthropic/claude-3.7-sonnet)
    #[serde(default = "default_model")]
    pub openrouter_model: String,
    /// Maximum tokens for responses
    #[serde(default = "default_max_tokens")]
    pub openrouter_max_tokens: u64,
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
            openrouter_api_key: None,
            openrouter_model: default_model(),
            openrouter_max_tokens: default_max_tokens(),
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

        tracing::debug!("Config after YAML load: {:?}", config.openrouter_api_key);

        // Load environment variables and merge (env vars override YAML/defaults)
        // Use a simple approach: try to get each env var manually to avoid issues
        if let Ok(api_key) = std::env::var("OPENROUTER_API_KEY")
            && !api_key.is_empty()
        {
            config.openrouter_api_key = Some(api_key);
        }

        if let Ok(model) = std::env::var("OPENROUTER_MODEL")
            && !model.is_empty()
        {
            config.openrouter_model = model;
        }

        if let Ok(tokens) = std::env::var("OPENROUTER_MAX_TOKENS")
            && let Ok(tokens) = tokens.parse()
        {
            config.openrouter_max_tokens = tokens;
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

        tracing::debug!("Final config: {:?}", config.openrouter_api_key);

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

    /// Check if SearXNG is configured and enabled
    pub fn searxng_enabled(&self) -> bool {
        self.searxng
            .as_ref()
            .map(|c| c.enabled && !c.base_url.is_empty())
            .unwrap_or(false)
    }

    /// Get the SearXNG base URL
    pub fn searxng_base_url(&self) -> Option<String> {
        self.searxng.as_ref().map(|c| c.base_url.clone())
    }
}
