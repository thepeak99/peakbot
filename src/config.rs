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
    pub openrouter_max_tokens: u32,
    /// Maximum tool turns per message
    #[serde(default = "default_max_turns")]
    pub agent_max_turns: u32,
    /// MCP servers configuration (YAML array)
    #[serde(default)]
    pub mcp_servers: Option<Vec<McpServerConfig>>,
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

fn default_model() -> String {
    "anthropic/claude-3.7-sonnet".to_string()
}

fn default_max_tokens() -> u32 {
    4096
}

fn default_max_turns() -> u32 {
    50
}

impl Default for Config {
    fn default() -> Self {
        Self {
            openrouter_api_key: None,
            openrouter_model: default_model(),
            openrouter_max_tokens: default_max_tokens(),
            agent_max_turns: default_max_turns(),
            mcp_servers: None,
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

        Ok(config)
    }
}
