use serde::Deserialize;
use std::collections::HashMap;
use directories_next::ProjectDirs;

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
    /// MCP servers configuration (JSON array)
    #[serde(default)]
    pub mcp_servers: Option<String>,
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
    ProjectDirs::from("com", "peakbot", "peakbot")
        .map(|dirs| dirs.config_dir().to_path_buf())
}

/// Load configuration from YAML file in the platform config directory
fn load_yaml_config() -> Option<Config> {
    let config_dir = get_config_dir()?;
    let config_path = config_dir.join("config.yaml");
    
    if !config_path.exists() {
        tracing::debug!("No YAML config found at {}", config_path.display());
        return None;
    }
    
    let content = std::fs::read_to_string(&config_path).ok()?;
    let config: Config = serde_yaml::from_str(&content).ok()?;
    
    tracing::info!("Loaded YAML config from {}", config_path.display());
    Some(config)
}

impl Config {
    /// Load configuration from YAML file first, then environment variables.
    /// Environment variables take precedence over YAML config.
    pub fn load() -> Result<Self, envy::Error> {
        // Start with YAML config if present, otherwise use defaults
        let mut config = load_yaml_config().unwrap_or_default();
        
        // Load environment variables and merge (env vars override YAML/defaults)
        let env_config = envy::from_env::<Config>()?;
        
        // Override with env vars if they are set
        if let Some(api_key) = env_config.openrouter_api_key {
            config.openrouter_api_key = Some(api_key);
        }
        if !env_config.openrouter_model.is_empty() {
            config.openrouter_model = env_config.openrouter_model;
        }
        if env_config.openrouter_max_tokens != default_max_tokens() {
            config.openrouter_max_tokens = env_config.openrouter_max_tokens;
        }
        if env_config.agent_max_turns != default_max_turns() {
            config.agent_max_turns = env_config.agent_max_turns;
        }
        if env_config.mcp_servers.is_some() {
            config.mcp_servers = env_config.mcp_servers;
        }
        
        Ok(config)
    }
}
