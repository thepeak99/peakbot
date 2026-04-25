//! Registry for managing and creating sub-agents.
//!
//! The registry holds agent definitions and provides factory methods
//! to create fresh agent instances from those definitions.

use crate::config::{AgentDefinition, PipelineConfig};
use crate::hooks::SessionHook;
use crate::providers::{DynAgent, ProviderInfo};
use anyhow::{Context, Result};
use rig::client::completion::CompletionClient;
use rig::providers::openrouter;
use std::collections::HashMap;

/// A registry of sub-agents with factory methods
#[derive(Clone)]
pub struct SubAgentRegistry {
    /// Agent definitions
    agents: HashMap<String, AgentDefinition>,
    /// API keys for different providers
    api_keys: HashMap<String, Option<String>>,
    /// Default configs for each provider type
    default_configs: HashMap<String, DefaultProviderConfig>,
}

/// Default configuration for each provider type
#[derive(Clone)]
struct DefaultProviderConfig {
    model: String,
    max_tokens: u64,
    base_url: Option<String>,
}

impl SubAgentRegistry {
    /// Create a new registry from pipeline configuration
    pub fn new(
        pipeline_config: &PipelineConfig,
        openrouter_api_key: Option<String>,
        openai_api_key: Option<String>,
        llamacpp_api_key: Option<String>,
        llamacpp_base_url: Option<String>,
        ollama_base_url: Option<String>,
    ) -> Self {
        let mut agents = HashMap::new();
        let mut api_keys = HashMap::new();
        let mut default_configs = HashMap::new();

        // Store API keys for each provider type
        api_keys.insert(
            "openrouter".to_string(),
            openrouter_api_key.filter(|k| !k.is_empty()),
        );
        api_keys.insert(
            "openai".to_string(),
            openai_api_key.filter(|k| !k.is_empty()),
        );
        api_keys.insert("llamacpp".to_string(), llamacpp_api_key);
        api_keys.insert("ollama".to_string(), None);

        // Default configs for OpenRouter
        default_configs.insert(
            "openrouter".to_string(),
            DefaultProviderConfig {
                model: "anthropic/claude-3.7-sonnet".to_string(),
                max_tokens: 4096,
                base_url: None,
            },
        );

        // Default configs for OpenAI
        default_configs.insert(
            "openai".to_string(),
            DefaultProviderConfig {
                model: "gpt-4o".to_string(),
                max_tokens: 4096,
                base_url: Some("https://api.openai.com/v1".to_string()),
            },
        );

        // Default configs for LlamaCpp
        default_configs.insert(
            "llamacpp".to_string(),
            DefaultProviderConfig {
                model: "llama3".to_string(),
                max_tokens: 4096,
                base_url: llamacpp_base_url.or(Some("http://localhost:8080".to_string())),
            },
        );

        // Default configs for Ollama
        default_configs.insert(
            "ollama".to_string(),
            DefaultProviderConfig {
                model: "llama3".to_string(),
                max_tokens: 4096,
                base_url: ollama_base_url.or(Some("http://localhost:11434".to_string())),
            },
        );

        // Copy agent definitions
        for (name, def) in &pipeline_config.agents {
            agents.insert(name.clone(), def.clone());
        }

        Self {
            agents,
            api_keys,
            default_configs,
        }
    }

    /// Create a new agent instance from a definition
    pub fn create_agent(&self, name: &str) -> Result<(DynAgent, ProviderInfo), SubAgentError> {
        let def = self
            .agents
            .get(name)
            .ok_or_else(|| SubAgentError::UnknownAgent(name.to_string()))?;

        let provider_name = def.agent_type.to_string();
        let api_key = self.api_keys.get(&provider_name).cloned().flatten();
        let default_config = self
            .default_configs
            .get(&provider_name)
            .ok_or_else(|| SubAgentError::UnsupportedProvider(provider_name.clone()))?;

        let model = def
            .model
            .clone()
            .unwrap_or_else(|| default_config.model.clone());
        let max_tokens = def.max_tokens.unwrap_or(default_config.max_tokens);
        let base_url = default_config.base_url.clone();

        match def.agent_type {
            crate::config::ProviderType::OpenRouter => {
                let api_key = api_key
                    .context("OpenRouter API key required for openrouter agents")
                    .map_err(|e| SubAgentError::Configuration(e.to_string()))?;

                let client = openrouter::Client::builder()
                    .api_key(&api_key)
                    .build()
                    .map_err(|e| SubAgentError::ClientCreation(e.to_string()))?;

                let (hook, _receiver) = SessionHook::with_channel();

                let agent = client
                    .agent(&model)
                    .preamble(&def.prompt)
                    .max_tokens(max_tokens)
                    .default_max_turns(50)
                    .hook(hook)
                    .build();

                Ok((
                    DynAgent::OpenRouter(agent),
                    ProviderInfo {
                        name: "openrouter".to_string(),
                        model: model.clone(),
                        supports_pricing: true,
                        supports_vision: crate::vision::model_supports_vision(&model),
                    },
                ))
            }
            crate::config::ProviderType::OpenAI => {
                let api_key = api_key
                    .context("OpenAI API key required for openai agents")
                    .map_err(|e| SubAgentError::Configuration(e.to_string()))?;

                let base = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());

                let client = rig::providers::openai::Client::builder()
                    .api_key(&api_key)
                    .base_url(&base)
                    .build()
                    .map_err(|e| SubAgentError::ClientCreation(e.to_string()))?;

                let (hook, _receiver) = SessionHook::with_channel();

                let agent = client
                    .agent(&model)
                    .preamble(&def.prompt)
                    .max_tokens(max_tokens)
                    .default_max_turns(50)
                    .hook(hook)
                    .build();

                Ok((
                    DynAgent::OpenAI(agent),
                    ProviderInfo {
                        name: "openai".to_string(),
                        model: model.clone(),
                        supports_pricing: true,
                        supports_vision: crate::vision::model_supports_vision(&model),
                    },
                ))
            }
            crate::config::ProviderType::LlamaCpp => {
                let api_key = api_key.unwrap_or_default();
                let base = base_url.unwrap_or_else(|| "http://localhost:8080".to_string());

                let client = rig::providers::openai::Client::builder()
                    .api_key(&api_key)
                    .base_url(&base)
                    .build()
                    .map_err(|e| SubAgentError::ClientCreation(e.to_string()))?
                    .completions_api();

                let (hook, _receiver) = SessionHook::with_channel();

                let agent = client
                    .agent(&model)
                    .preamble(&def.prompt)
                    .max_tokens(max_tokens)
                    .default_max_turns(50)
                    .hook(hook)
                    .build();

                Ok((
                    DynAgent::LlamaCpp(agent),
                    ProviderInfo {
                        name: "llamacpp".to_string(),
                        model: model.clone(),
                        supports_pricing: true,
                        supports_vision: crate::vision::model_supports_vision(&model),
                    },
                ))
            }
            crate::config::ProviderType::Ollama => {
                use rig::providers::ollama;

                let base = base_url.unwrap_or_else(|| "http://localhost:11434".to_string());

                let client = ollama::Client::builder()
                    .base_url(&base)
                    .api_key(rig::client::Nothing)
                    .build()
                    .map_err(|e| SubAgentError::ClientCreation(e.to_string()))?;

                let mut agent_builder = client
                    .agent(&model)
                    .preamble(&def.prompt)
                    .default_max_turns(50);

                if let Some(temp) = def.temperature {
                    agent_builder = agent_builder.temperature(temp as f64);
                }

                let agent = agent_builder.build();

                Ok((
                    DynAgent::Ollama(agent),
                    ProviderInfo {
                        name: "ollama".to_string(),
                        model: model.clone(),
                        supports_pricing: false,
                        supports_vision: crate::vision::model_supports_vision(&model),
                    },
                ))
            }
        }
    }

    /// Check if an agent exists in the registry
    pub fn has_agent(&self, name: &str) -> bool {
        self.agents.contains_key(name)
    }

    /// List all available agent names
    pub fn list_agents(&self) -> Vec<&str> {
        self.agents.keys().map(|s| s.as_str()).collect()
    }

    /// Get the prompt for an agent (useful for documentation)
    pub fn get_agent_prompt(&self, name: &str) -> Option<&str> {
        self.agents.get(name).map(|a| a.prompt.as_str())
    }
}

/// Errors that can occur when working with sub-agents
#[derive(Debug, thiserror::Error)]
pub enum SubAgentError {
    #[error("Unknown agent: {0}")]
    UnknownAgent(String),

    #[error("Unsupported provider type: {0}")]
    UnsupportedProvider(String),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Failed to create client: {0}")]
    ClientCreation(String),

    #[error("Failed to build agent: {0}")]
    AgentBuild(String),
}
