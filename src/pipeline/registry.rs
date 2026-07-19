//! Registry for managing and creating sub-agents.
//!
//! The registry resolves each role's `model:` alias against the shared
//! [`ModelRegistry`] at construction (parse at the boundary), then provides
//! a factory that builds a fresh `DynAgent` per delegation.

use crate::config::{ModelRegistry, PipelineConfig, ProviderConfig, ResolvedModel};
use crate::hooks::SessionHook;
use crate::providers::{DynAgent, ProviderInfo};
use rig_core::client::completion::CompletionClient;
use rig_core::providers::openrouter;
use std::collections::HashMap;

/// A role resolved against the model registry, ready to build.
///
/// Holds the resolved model (wire id + credentials via `provider_config`)
/// plus the role's prompt and optional bash env. The env is plumbed here
/// in Phase 2 and consumed when the sub-agent is built.
#[derive(Clone, Debug)]
struct ResolvedRole {
    model: ResolvedModel,
    prompt: String,
    env: Option<HashMap<String, String>>,
}

/// A registry of sub-agents with factory methods.
#[derive(Clone, Debug)]
pub struct SubAgentRegistry {
    roles: HashMap<String, ResolvedRole>,
}

impl SubAgentRegistry {
    /// Build the registry from pipeline config, resolving every role's
    /// `model:` alias against `model_registry`.
    ///
    /// # Errors
    /// - empty role name,
    /// - empty prompt,
    /// - a `model:` alias not present in the registry.
    pub fn new(
        pipeline_config: &PipelineConfig,
        model_registry: &ModelRegistry,
    ) -> Result<Self, SubAgentError> {
        let mut roles = HashMap::with_capacity(pipeline_config.agents.len());

        for (name, def) in &pipeline_config.agents {
            if name.is_empty() {
                return Err(SubAgentError::EmptyRoleName);
            }
            if def.prompt.is_empty() {
                return Err(SubAgentError::EmptyPrompt(name.clone()));
            }

            // Omitted `model:` → the registry default.
            let alias = def
                .model
                .as_deref()
                .unwrap_or(model_registry.default_alias());
            let model = model_registry.resolve(alias).cloned().ok_or_else(|| {
                SubAgentError::UnknownModel {
                    role: name.clone(),
                    alias: alias.to_string(),
                    available: model_registry.aliases_sorted().join(", "),
                }
            })?;

            roles.insert(
                name.clone(),
                ResolvedRole {
                    model,
                    prompt: def.prompt.clone(),
                    env: def.env.clone(),
                },
            );
        }

        Ok(Self { roles })
    }

    /// Create a fresh agent instance for a role.
    pub fn create_agent(&self, name: &str) -> Result<(DynAgent, ProviderInfo), SubAgentError> {
        let role = self
            .roles
            .get(name)
            .ok_or_else(|| SubAgentError::UnknownAgent(name.to_string()))?;

        let model = role.model.model_name.clone();

        // Transitional: build the client by matching the resolved
        // `provider_config`. Phase 4 replaces this whole body with the
        // shared `create_provider`/`add_builtin_tools` path (which also
        // wires the TEE hook and the full toolset).
        match &role.model.provider_config {
            ProviderConfig::OpenRouter(c) => {
                let client = openrouter::Client::builder()
                    .http_client(crate::http::client())
                    .api_key(c.api_key.as_deref().unwrap_or_default())
                    .build()
                    .map_err(|e| SubAgentError::ClientCreation(e.to_string()))?;

                let (hook, _receiver) = SessionHook::with_channel();
                let agent = client
                    .agent(&model)
                    .preamble(&role.prompt)
                    .max_tokens(c.max_tokens)
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
            ProviderConfig::OpenAI(c) => {
                let client = rig_core::providers::openai::Client::builder()
                    .http_client(crate::http::client())
                    .api_key(c.api_key.as_deref().unwrap_or_default())
                    .base_url(&c.base_url)
                    .build()
                    .map_err(|e| SubAgentError::ClientCreation(e.to_string()))?;

                let (hook, _receiver) = SessionHook::with_channel();
                let agent = client
                    .agent(&model)
                    .preamble(&role.prompt)
                    .max_tokens(c.max_tokens)
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
            ProviderConfig::Anthropic(c) => {
                let client = rig_core::providers::anthropic::Client::builder()
                    .http_client(crate::http::client())
                    .api_key(c.api_key.as_deref().unwrap_or_default())
                    .base_url(&c.base_url)
                    .build()
                    .map_err(|e| SubAgentError::ClientCreation(e.to_string()))?;

                let (hook, _receiver) = SessionHook::with_channel();
                let agent = client
                    .agent(&model)
                    .preamble(&role.prompt)
                    .max_tokens(c.max_tokens)
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
            ProviderConfig::LlamaCpp(c) => {
                let client = rig_core::providers::openai::Client::builder()
                    .http_client(crate::http::client())
                    .api_key(c.api_key.as_deref().unwrap_or_default())
                    .base_url(&c.base_url)
                    .build()
                    .map_err(|e| SubAgentError::ClientCreation(e.to_string()))?
                    .completions_api();

                let (hook, _receiver) = SessionHook::with_channel();
                let agent = client
                    .agent(&model)
                    .preamble(&role.prompt)
                    .max_tokens(c.max_tokens)
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
            ProviderConfig::Ollama(c) => {
                use rig_core::providers::ollama;

                let client = ollama::Client::builder()
                    .http_client(crate::http::client())
                    .base_url(&c.base_url)
                    .api_key(rig_core::client::Nothing)
                    .build()
                    .map_err(|e| SubAgentError::ClientCreation(e.to_string()))?;

                let mut builder = client
                    .agent(&model)
                    .preamble(&role.prompt)
                    .default_max_turns(50);
                if let Some(temp) = c.temperature {
                    builder = builder.temperature(temp as f64);
                }
                let agent = builder.build();

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

    /// Check if a role exists in the registry.
    pub fn has_agent(&self, name: &str) -> bool {
        self.roles.contains_key(name)
    }

    /// List all available role names.
    pub fn list_agents(&self) -> Vec<&str> {
        self.roles.keys().map(|s| s.as_str()).collect()
    }

    /// Get the prompt for a role (useful for documentation).
    pub fn get_agent_prompt(&self, name: &str) -> Option<&str> {
        self.roles.get(name).map(|r| r.prompt.as_str())
    }

    /// The resolved model for a role (test/introspection helper).
    #[cfg(test)]
    fn resolved_model(&self, name: &str) -> Option<&ResolvedModel> {
        self.roles.get(name).map(|r| &r.model)
    }

    /// The bash env for a role — consumed when the sub-agent is built.
    pub fn role_env(&self, name: &str) -> Option<&HashMap<String, String>> {
        self.roles.get(name).and_then(|r| r.env.as_ref())
    }
}

/// Errors that can occur when working with sub-agents
#[derive(Debug, thiserror::Error)]
pub enum SubAgentError {
    #[error("Unknown agent: {0}")]
    UnknownAgent(String),

    #[error("Role name cannot be empty")]
    EmptyRoleName,

    #[error("Role '{0}' has an empty prompt")]
    EmptyPrompt(String),

    #[error("Role '{role}' names unknown model alias `{alias}`. Available: {available}")]
    UnknownModel {
        role: String,
        alias: String,
        available: String,
    },

    #[error("Failed to create client: {0}")]
    ClientCreation(String),

    #[error("Failed to build agent: {0}")]
    AgentBuild(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentDefinition, ModelEntry, ModelRegistry, ProviderEntry, ProviderType};
    use std::collections::HashMap;

    /// A two-model registry: `flash` (default) and `sonnet`.
    fn registry() -> ModelRegistry {
        let prov = ProviderEntry {
            name: "openrouter".into(),
            kind: ProviderType::OpenRouter,
            api_key: Some("sk-or-test".into()),
            base_url: None,
            models: vec![
                ModelEntry {
                    name: "google/gemini-2.0-flash-001".into(),
                    alias: Some("flash".into()),
                    max_tokens: None,
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    vision: None,
                    context_size: None,
                },
                ModelEntry {
                    name: "anthropic/claude-3.7-sonnet".into(),
                    alias: Some("sonnet".into()),
                    max_tokens: Some(8192),
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    vision: None,
                    context_size: None,
                },
            ],
        };
        ModelRegistry::build(&[prov], Some("flash")).expect("registry builds")
    }

    fn role(model: Option<&str>, prompt: &str) -> AgentDefinition {
        AgentDefinition {
            model: model.map(str::to_string),
            prompt: prompt.to_string(),
            env: None,
        }
    }

    fn pipeline_with(agents: Vec<(&str, AgentDefinition)>) -> PipelineConfig {
        PipelineConfig {
            enabled: true,
            agents: agents
                .into_iter()
                .map(|(n, d)| (n.to_string(), d))
                .collect(),
        }
    }

    /// A role naming an existing alias resolves against the shared
    /// `ModelRegistry` — the resolved model carries the alias's wire id
    /// and per-model overrides.
    #[test]
    fn role_model_resolves_against_registry_alias() {
        let cfg = pipeline_with(vec![("reviewer", role(Some("sonnet"), "review"))]);
        let reg = SubAgentRegistry::new(&cfg, &registry()).expect("registry builds");

        let resolved = reg.resolved_model("reviewer").expect("role resolved");
        assert_eq!(resolved.alias, "sonnet");
        assert_eq!(resolved.model_name, "anthropic/claude-3.7-sonnet");
    }

    /// An omitted `model:` falls back to the registry's `default_model`.
    #[test]
    fn role_without_model_falls_back_to_default_alias() {
        let cfg = pipeline_with(vec![("researcher", role(None, "research"))]);
        let reg = SubAgentRegistry::new(&cfg, &registry()).expect("registry builds");

        let resolved = reg.resolved_model("researcher").expect("role resolved");
        assert_eq!(resolved.alias, "flash", "default_model is `flash`");
    }

    /// A role naming an alias that does not exist is a clear boot-time
    /// error, not a silent fallback. (Parse at the boundary.)
    #[test]
    fn unknown_role_alias_is_rejected_at_construction() {
        let cfg = pipeline_with(vec![("ghost", role(Some("does-not-exist"), "x"))]);
        let err = SubAgentRegistry::new(&cfg, &registry())
            .expect_err("unknown alias must fail construction");
        match err {
            SubAgentError::UnknownModel { role, alias, .. } => {
                assert_eq!(role, "ghost");
                assert_eq!(alias, "does-not-exist");
            }
            other => panic!("expected UnknownModel, got {other:?}"),
        }
    }

    /// An empty prompt is rejected at construction.
    #[test]
    fn empty_prompt_is_rejected() {
        let cfg = pipeline_with(vec![("blank", role(Some("flash"), ""))]);
        let err = SubAgentRegistry::new(&cfg, &registry()).expect_err("empty prompt must fail");
        assert!(matches!(err, SubAgentError::EmptyPrompt(name) if name == "blank"));
    }

    /// An empty role name is rejected at construction.
    #[test]
    fn empty_role_name_is_rejected() {
        let cfg = pipeline_with(vec![("", role(Some("flash"), "prompt"))]);
        let err = SubAgentRegistry::new(&cfg, &registry()).expect_err("empty name must fail");
        assert!(matches!(err, SubAgentError::EmptyRoleName));
    }

    /// The role's `env:` is stored through construction (Phase 4 merges it
    /// into the sub-agent's bash env; Phase 2 only plumbs it).
    #[test]
    fn role_env_is_stored_through_construction() {
        let mut def = role(Some("flash"), "research");
        def.env = Some(HashMap::from([(
            "REVIEW_STRICT".to_string(),
            "1".to_string(),
        )]));
        let cfg = pipeline_with(vec![("researcher", def)]);
        let reg = SubAgentRegistry::new(&cfg, &registry()).expect("registry builds");

        assert_eq!(
            reg.role_env("researcher")
                .and_then(|e| e.get("REVIEW_STRICT"))
                .map(String::as_str),
            Some("1"),
        );
    }
}
