//! Registry for managing and creating sub-agents.
//!
//! The registry resolves each role's `model:` alias against the shared
//! [`ModelRegistry`] at construction (parse at the boundary) and stores the
//! resolved roles. Building a live sub-agent from a role is done by
//! [`crate::pipeline::SubAgentDeps`], which owns the orchestrator's build
//! context (tools, env, event sink) — the registry itself stays lean.

use crate::config::{ModelRegistry, PipelineConfig, ResolvedModel, SkillFilter};
use std::collections::HashMap;

/// A role resolved against the model registry, ready to build.
///
/// Holds the resolved model (wire id + credentials via `provider_config`)
/// plus the role's prompt and optional bash env, merged into the sub-agent's
/// bash tool when it is built.
#[derive(Clone, Debug)]
pub(crate) struct ResolvedRole {
    pub(crate) model: ResolvedModel,
    pub(crate) prompt: String,
    pub(crate) env: Option<HashMap<String, String>>,
    pub(crate) skills: SkillFilter,
    pub(crate) agents_md: bool,
}

/// A registry of sub-agents with factory methods.
#[derive(Clone, Debug)]
pub struct SubAgentRegistry {
    roles: HashMap<String, ResolvedRole>,
}

impl SubAgentRegistry {
    /// Build the registry from pipeline config, resolving every role's
    /// `model:` alias against `model_registry` and validating each role's
    /// `skills:` filter against `known_skills` (the discovered skill names).
    ///
    /// # Errors
    /// - empty role name,
    /// - empty prompt,
    /// - a `model:` alias not present in the registry,
    /// - a `skills:` filter that sets both lists or names an unknown skill.
    pub fn new(
        pipeline_config: &PipelineConfig,
        model_registry: &ModelRegistry,
        known_skills: &[String],
    ) -> Result<Self, SubAgentError> {
        let mut roles = HashMap::with_capacity(pipeline_config.agents.len());

        for (name, def) in &pipeline_config.agents {
            if name.is_empty() {
                return Err(SubAgentError::EmptyRoleName);
            }
            if def.prompt.is_empty() {
                return Err(SubAgentError::EmptyPrompt(name.clone()));
            }
            def.skills
                .validate(name, known_skills)
                .map_err(SubAgentError::BadSkillFilter)?;

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
                    skills: def.skills.clone(),
                    agents_md: def.agents_md,
                },
            );
        }

        Ok(Self { roles })
    }

    /// Check if a role exists in the registry.
    pub fn has_agent(&self, name: &str) -> bool {
        self.roles.contains_key(name)
    }

    /// List all available role names.
    pub fn list_agents(&self) -> Vec<&str> {
        self.roles.keys().map(|s| s.as_str()).collect()
    }

    /// The resolved role (model + prompt + env) for a name — consumed by the
    /// sub-agent build path.
    pub(crate) fn role(&self, name: &str) -> Option<&ResolvedRole> {
        self.roles.get(name)
    }
}

/// Errors that can occur when working with sub-agents
#[derive(Debug, thiserror::Error)]
pub enum SubAgentError {
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

    #[error("{0}")]
    BadSkillFilter(String),
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
            skills: crate::config::SkillFilter::default(),
            agents_md: false,
        }
    }

    fn pipeline_with(agents: Vec<(&str, AgentDefinition)>) -> PipelineConfig {
        PipelineConfig {
            enabled: true,
            orchestrator_prompt: None,
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
        let reg = SubAgentRegistry::new(&cfg, &registry(), &[]).expect("registry builds");

        let resolved = &reg.role("reviewer").expect("role resolved").model;
        assert_eq!(resolved.alias, "sonnet");
        assert_eq!(resolved.model_name, "anthropic/claude-3.7-sonnet");
    }

    /// An omitted `model:` falls back to the registry's `default_model`.
    #[test]
    fn role_without_model_falls_back_to_default_alias() {
        let cfg = pipeline_with(vec![("researcher", role(None, "research"))]);
        let reg = SubAgentRegistry::new(&cfg, &registry(), &[]).expect("registry builds");

        let resolved = &reg.role("researcher").expect("role resolved").model;
        assert_eq!(resolved.alias, "flash", "default_model is `flash`");
    }

    /// A role naming an alias that does not exist is a clear boot-time
    /// error, not a silent fallback. (Parse at the boundary.)
    #[test]
    fn unknown_role_alias_is_rejected_at_construction() {
        let cfg = pipeline_with(vec![("ghost", role(Some("does-not-exist"), "x"))]);
        let err = SubAgentRegistry::new(&cfg, &registry(), &[])
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
        let err =
            SubAgentRegistry::new(&cfg, &registry(), &[]).expect_err("empty prompt must fail");
        assert!(matches!(err, SubAgentError::EmptyPrompt(name) if name == "blank"));
    }

    /// An empty role name is rejected at construction.
    #[test]
    fn empty_role_name_is_rejected() {
        let cfg = pipeline_with(vec![("", role(Some("flash"), "prompt"))]);
        let err = SubAgentRegistry::new(&cfg, &registry(), &[]).expect_err("empty name must fail");
        assert!(matches!(err, SubAgentError::EmptyRoleName));
    }

    /// A role's `skills:` filter naming an unknown skill is rejected against
    /// the discovered skill set (typo catch at the boundary).
    #[test]
    fn unknown_skill_name_is_rejected() {
        let mut def = role(Some("flash"), "research");
        def.skills = crate::config::SkillFilter {
            enabled: true,
            disabled: vec![],
            only: vec!["ghost-skill".into()],
        };
        let cfg = pipeline_with(vec![("researcher", def)]);
        let err = SubAgentRegistry::new(&cfg, &registry(), &["github".into()])
            .expect_err("unknown skill must fail construction");
        assert!(matches!(err, SubAgentError::BadSkillFilter(_)));
    }

    /// A valid `skills:` filter is stored through construction.
    #[test]
    fn role_skill_filter_is_stored() {
        let mut def = role(Some("flash"), "research");
        def.skills = crate::config::SkillFilter {
            enabled: true,
            disabled: vec![],
            only: vec!["github".into()],
        };
        let cfg = pipeline_with(vec![("researcher", def)]);
        let reg =
            SubAgentRegistry::new(&cfg, &registry(), &["github".into()]).expect("registry builds");
        assert_eq!(
            reg.role("researcher").map(|r| r.skills.only.clone()),
            Some(vec!["github".to_string()]),
        );
    }

    /// The role's `env:` is stored through construction (merged into the
    /// sub-agent's bash env when it is built).
    #[test]
    fn role_env_is_stored_through_construction() {
        let mut def = role(Some("flash"), "research");
        def.env = Some(HashMap::from([(
            "REVIEW_STRICT".to_string(),
            "1".to_string(),
        )]));
        let cfg = pipeline_with(vec![("researcher", def)]);
        let reg = SubAgentRegistry::new(&cfg, &registry(), &[]).expect("registry builds");

        assert_eq!(
            reg.role("researcher")
                .and_then(|r| r.env.as_ref())
                .and_then(|e| e.get("REVIEW_STRICT"))
                .map(String::as_str),
            Some("1"),
        );
    }
}
