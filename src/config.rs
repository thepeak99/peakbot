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

impl Config {
    pub fn load() -> Result<Self, envy::Error> {
        envy::from_env::<Config>()
    }
}
