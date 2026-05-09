//! Pipeline configuration for multi-agent pipelines.
//!
//! Defines how to configure sub-agents that the entrypoint can delegate to.

use serde::Deserialize;
use std::collections::HashMap;
use crate::config::ProviderType;

/// Multi-agent pipeline configuration
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct PipelineConfig {
    /// Whether multi-agent pipelines are enabled (default: false)
    #[serde(default)]
    pub enabled: bool,

    /// Sub-agent definitions keyed by agent name
    #[serde(default)]
    pub agents: HashMap<String, AgentDefinition>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            agents: HashMap::new(),
        }
    }
}

/// Definition of a sub-agent that can be delegated to
#[derive(Debug, Deserialize, Clone)]
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

    /// Validate the pipeline configuration
    pub fn validate(&self) -> Result<(), PipelineValidationError> {
        for (name, agent) in &self.agents {
            if name.is_empty() {
                return Err(PipelineValidationError::EmptyAgentName);
            }
            if agent.prompt.is_empty() {
                return Err(PipelineValidationError::EmptyPrompt(name.clone()));
            }
        }
        Ok(())
    }
}

/// Errors that can occur during pipeline validation
#[derive(Debug, thiserror::Error)]
pub enum PipelineValidationError {
    #[error("Agent name cannot be empty")]
    EmptyAgentName,

    #[error("Agent '{0}' has an empty prompt")]
    EmptyPrompt(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipeline_config_defaults() {
        let config: PipelineConfig = serde_yaml::from_str("").unwrap();
        assert!(!config.enabled);
        assert!(config.agents.is_empty());
    }

    #[test]
    fn test_pipeline_config_parsing() {
        let yaml = r#"
enabled: true
agents:
  researcher:
    type: openrouter
    model: gemini-flash
    prompt: "You research topics"
  coder:
    type: openrouter
    model: claude-sonnet
    prompt: "You write code"
"#;
        let config: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.enabled);
        assert_eq!(config.agents.len(), 2);
        assert_eq!(config.agent_names(), vec!["researcher", "coder"]);
    }

    #[test]
    fn test_validate_empty_name() {
        let yaml = r#"
enabled: true
agents:
  "":
    type: openrouter
    prompt: "Test"
"#;
        let config: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_empty_prompt() {
        let yaml = r#"
enabled: true
agents:
  test:
    type: openrouter
    prompt: ""
"#;
        let config: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.validate().is_err());
    }
}
