//! Registry for managing and creating sub-agents.
//!
//! The registry holds agent definitions and provides factory methods
//! to create fresh agent instances from those definitions.

use crate::config::{AgentDefinition, PipelineConfig};
use crate::hooks::SessionHook;
use crate::providers::{DynAgent, ProviderInfo};
use anyhow::{Context, Result};
use rig_core::client::completion::CompletionClient;
use rig_core::providers::openrouter;
use std::collections::HashMap;

/// A registry of sub-agents with factory methods
#[derive(Clone)]
pub struct SubAgentRegistry {
    /// Agent definitions
    agents: HashMap<String, AgentDefinition>,
}

impl SubAgentRegistry {
    /// Create a new registry from pipeline configuration
    pub fn new(pipeline_config: &PipelineConfig) -> Self {
        let agents = pipeline_config
            .agents
            .iter()
            .map(|(name, def)| (name.clone(), def.clone()))
            .collect();

        Self { agents }
    }

    /// Create a new agent instance from a definition
    pub fn create_agent(&self, name: &str) -> Result<(DynAgent, ProviderInfo), SubAgentError> {
        let def = self
            .agents
            .get(name)
            .ok_or_else(|| SubAgentError::UnknownAgent(name.to_string()))?;

        let model = def.model.clone().unwrap_or_else(|| match def.agent_type {
            crate::config::ProviderType::OpenRouter => "anthropic/claude-3.7-sonnet".to_string(),
            crate::config::ProviderType::OpenAI => "gpt-4o".to_string(),
            crate::config::ProviderType::Anthropic => "claude-3-5-sonnet-latest".to_string(),
            crate::config::ProviderType::LlamaCpp => "llama3".to_string(),
            crate::config::ProviderType::Ollama => "llama3".to_string(),
        });
        let max_tokens = def.max_tokens.unwrap_or(4096);
        let base_url = def.base_url.clone();

        match def.agent_type {
            crate::config::ProviderType::OpenRouter => {
                let api_key = def
                    .api_key
                    .clone()
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
                let api_key = def
                    .api_key
                    .clone()
                    .context("OpenAI API key required for openai agents")
                    .map_err(|e| SubAgentError::Configuration(e.to_string()))?;

                let base = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());

                let client = rig_core::providers::openai::Client::builder()
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
            crate::config::ProviderType::Anthropic => {
                // Local Anthropic-compatible servers need no key, so an empty
                // default is fine (mirrors the LlamaCpp arm).
                let api_key = def.api_key.clone().unwrap_or_default();
                let base = base_url.unwrap_or_else(|| "https://api.anthropic.com".to_string());

                let client = rig_core::providers::anthropic::Client::builder()
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
                    DynAgent::Anthropic(agent),
                    ProviderInfo {
                        name: "anthropic".to_string(),
                        model: model.clone(),
                        supports_pricing: false,
                        supports_vision: crate::providers::supports_vision_for("anthropic", &model),
                    },
                ))
            }
            crate::config::ProviderType::LlamaCpp => {
                let api_key = def.api_key.clone().unwrap_or_default();
                let base = base_url.unwrap_or_else(|| "http://localhost:8080".to_string());

                let client = rig_core::providers::openai::Client::builder()
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
                use rig_core::providers::ollama;

                let base = base_url.unwrap_or_else(|| "http://localhost:11434".to_string());

                let client = ollama::Client::builder()
                    .base_url(&base)
                    .api_key(rig_core::client::Nothing)
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
