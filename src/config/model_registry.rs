//! Multi-model registry — providers list, models with aliases, and a
//! resolver that maps a user-typed alias to a fully-formed
//! [`ProviderConfig`] the existing `create_provider` already understands.
//!
//! See `multi-model.md` for the locked design. Highlights:
//! - `providers:` is a YAML *list*, each entry owns its `models:` list.
//! - Provider `name` is informational only (no cross-references).
//! - Model `alias` is optional; falls back to `name`.
//! - Aliases are globally unique and must match `^[a-z0-9_-]+$`.
//! - The literal alias `unknown` is reserved (used as the sentinel for
//!   pre-v4 conversations whose model wasn't recorded).
//! - `default_model` is required iff any models are declared.
//!
//! The registry's only job is *alias → existing-shape*. Everything
//! downstream (`create_provider`, `ProviderInfo`, the agent itself)
//! stays as it was. *(reuse the seam that already exists)*

use crate::config::{
    LlamaCppConfig, OllamaConfig, OpenAIConfig, OpenRouterConfig, ProviderConfig, ProviderType,
};
use serde::Deserialize;
use std::collections::HashMap;

/// Reserved alias used as the model_alias for pre-v4 conversations on
/// disk that didn't yet carry the field. Always rejected at config load
/// (an explicit user-declared alias `unknown` is a config error) and
/// always fails activation in `/load` with the canonical
/// `Model 'unknown' not available.` error.
pub const RESERVED_UNAVAILABLE_ALIAS: &str = "unknown";

/// One entry in the top-level `providers:` list. Owns its credentials
/// and its `models:` list.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ProviderEntry {
    /// Informational name (shown in `/conversations` and the `/model`
    /// listing as the parenthesised provider context). NOT referenced
    /// by anything else — keep it human-readable.
    pub name: String,
    /// Provider kind: `openai` | `openrouter` | `llamacpp` | `ollama`.
    /// Maps 1:1 to the existing [`ProviderType`].
    #[serde(rename = "type")]
    pub kind: ProviderType,
    /// API key (when applicable for the kind).
    #[serde(default)]
    pub api_key: Option<String>,
    /// Base URL override (when applicable for the kind).
    #[serde(default)]
    pub base_url: Option<String>,
    /// Models exposed by this provider.
    #[serde(default)]
    pub models: Vec<ModelEntry>,
}

/// One model declared inside a provider's `models:` list.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ModelEntry {
    /// The wire id sent to the API (e.g. `gpt-4o`,
    /// `anthropic/claude-3.7-sonnet`, `qwen2.5-coder:14b`). No munging.
    pub name: String,
    /// User-facing handle for `/model` and `/load`. Optional —
    /// defaults to `name` if absent. Globally unique across all
    /// providers; must match `^[a-z0-9_-]+$`.
    #[serde(default)]
    pub alias: Option<String>,
    /// Optional max-tokens override for this model.
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// Optional temperature override (Ollama, OpenAI compatible).
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Optional context-size override for Ollama.
    #[serde(default)]
    pub num_ctx: Option<usize>,
    /// Optional pass-through extra params for LlamaCpp.
    #[serde(default)]
    pub extra_params: Option<serde_json::Value>,
    /// Optional context-window override (overrides auto-detection from
    /// model name in `ContextManager`).
    #[serde(default)]
    pub context_window_override: Option<usize>,
}

/// A model resolved against a provider — alias canonicalised, full
/// `ProviderConfig` built and ready for `create_provider`.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    /// Canonical user handle (alias if declared, else `name`).
    pub alias: String,
    /// Wire id (the same as the source `ModelEntry::name`).
    pub model_name: String,
    /// Informational provider name from the parent `ProviderEntry`.
    pub provider_name: String,
    /// Provider kind — useful for status messages without inspecting
    /// the variant.
    pub provider_kind: ProviderType,
    /// Existing-shape provider config that `create_provider` consumes.
    pub provider_config: ProviderConfig,
    /// Optional context-window override carried from the model entry.
    pub context_window_override: Option<usize>,
}

/// Errors raised while validating a `ModelRegistry` from a config.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RegistryError {
    #[error(
        "duplicate alias `{alias}` declared on `{first_provider}/{first_model}` and `{second_provider}/{second_model}`"
    )]
    DuplicateAlias {
        alias: String,
        first_provider: String,
        first_model: String,
        second_provider: String,
        second_model: String,
    },

    #[error("alias `{0}` is reserved (used internally for pre-v4 conversations)")]
    ReservedAlias(String),

    #[error(
        "alias `{alias}` on `{provider}/{model}` does not match required pattern `[a-z0-9_-]+`"
    )]
    InvalidAlias {
        alias: String,
        provider: String,
        model: String,
    },

    #[error("default_model `{alias}` does not match any declared alias. Available: {available}")]
    UnknownDefault { alias: String, available: String },

    #[error("default_model is required when at least one model is declared")]
    MissingDefault,
}

/// Resolved alias → model lookup, plus the boot default.
///
/// Built once at config load via [`ModelRegistry::build`]; thereafter
/// looked up by alias on `/model` and `/load`.
#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    by_alias: HashMap<String, ResolvedModel>,
    default_alias: Option<String>,
}

impl ModelRegistry {
    /// Build and validate a registry from the providers list and
    /// optional default alias.
    ///
    /// # Errors
    /// - duplicate aliases across the whole tree,
    /// - any alias matching the reserved literal `unknown`,
    /// - any alias not matching `^[a-z0-9_-]+$`,
    /// - `default_model` referencing a non-existent alias,
    /// - `default_model` missing when models are declared.
    pub fn build(
        providers: &[ProviderEntry],
        default_model: Option<&str>,
    ) -> Result<Self, RegistryError> {
        let mut by_alias: HashMap<String, ResolvedModel> = HashMap::new();
        // Track first-seen origin for nicer duplicate error messages.
        let mut origins: HashMap<String, (String, String)> = HashMap::new();

        for prov in providers {
            for model in &prov.models {
                let alias = model.alias.clone().unwrap_or_else(|| model.name.clone());

                if alias == RESERVED_UNAVAILABLE_ALIAS {
                    return Err(RegistryError::ReservedAlias(alias));
                }
                if !is_valid_alias(&alias) {
                    return Err(RegistryError::InvalidAlias {
                        alias,
                        provider: prov.name.clone(),
                        model: model.name.clone(),
                    });
                }
                if let Some((first_prov, first_model)) = origins.get(&alias) {
                    return Err(RegistryError::DuplicateAlias {
                        alias: alias.clone(),
                        first_provider: first_prov.clone(),
                        first_model: first_model.clone(),
                        second_provider: prov.name.clone(),
                        second_model: model.name.clone(),
                    });
                }
                origins.insert(alias.clone(), (prov.name.clone(), model.name.clone()));

                let provider_config = build_provider_config(prov, model);
                by_alias.insert(
                    alias.clone(),
                    ResolvedModel {
                        alias,
                        model_name: model.name.clone(),
                        provider_name: prov.name.clone(),
                        provider_kind: prov.kind.clone(),
                        provider_config,
                        context_window_override: model.context_window_override,
                    },
                );
            }
        }

        let default_alias = match default_model {
            Some(d) if !d.is_empty() => {
                if !by_alias.contains_key(d) {
                    let mut available: Vec<&str> = by_alias.keys().map(|s| s.as_str()).collect();
                    available.sort_unstable();
                    return Err(RegistryError::UnknownDefault {
                        alias: d.to_string(),
                        available: if available.is_empty() {
                            "(none)".to_string()
                        } else {
                            available.join(", ")
                        },
                    });
                }
                Some(d.to_string())
            }
            _ if !by_alias.is_empty() => return Err(RegistryError::MissingDefault),
            _ => None,
        };

        Ok(Self {
            by_alias,
            default_alias,
        })
    }

    /// Empty registry — used when no `providers:` block was declared
    /// and the legacy single-provider config path is in effect.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Whether any models were declared.
    pub fn is_empty(&self) -> bool {
        self.by_alias.is_empty()
    }

    /// Number of declared models.
    pub fn len(&self) -> usize {
        self.by_alias.len()
    }

    /// The boot alias (`default_model:`).
    pub fn default_alias(&self) -> Option<&str> {
        self.default_alias.as_deref()
    }

    /// Look up a model by alias.
    pub fn resolve(&self, alias: &str) -> Option<&ResolvedModel> {
        self.by_alias.get(alias)
    }

    /// Whether the registry contains a given alias.
    pub fn contains(&self, alias: &str) -> bool {
        self.by_alias.contains_key(alias)
    }

    /// All declared aliases sorted alphabetically — for `/model` listing
    /// and error messages.
    pub fn aliases_sorted(&self) -> Vec<String> {
        let mut v: Vec<String> = self.by_alias.keys().cloned().collect();
        v.sort_unstable();
        v
    }

    /// Iterate all (alias, ResolvedModel) pairs in alphabetical alias
    /// order.
    pub fn iter_sorted(&self) -> Vec<(&str, &ResolvedModel)> {
        let mut v: Vec<(&str, &ResolvedModel)> =
            self.by_alias.iter().map(|(a, m)| (a.as_str(), m)).collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        v
    }
}

/// `^[a-z0-9_-]+$` — the alias pattern. Kept as a hand-rolled check
/// instead of pulling in `regex` for a five-character grammar. *(don't
/// be too clever)*
fn is_valid_alias(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Translate a (provider entry, model entry) pair into the
/// existing-shape `ProviderConfig` that `create_provider` already
/// consumes. Defaults are taken from the per-provider sub-config's
/// existing default fns wherever the entry leaves a slot empty.
fn build_provider_config(prov: &ProviderEntry, model: &ModelEntry) -> ProviderConfig {
    match prov.kind {
        ProviderType::OpenRouter => ProviderConfig::OpenRouter(OpenRouterConfig {
            api_key: prov.api_key.clone(),
            model: model.name.clone(),
            max_tokens: model.max_tokens.unwrap_or(default_max_tokens()),
        }),
        ProviderType::OpenAI => ProviderConfig::OpenAI(OpenAIConfig {
            api_key: prov.api_key.clone(),
            base_url: prov
                .base_url
                .clone()
                .unwrap_or_else(default_openai_base_url),
            model: model.name.clone(),
            max_tokens: model.max_tokens.unwrap_or(default_max_tokens()),
        }),
        ProviderType::LlamaCpp => ProviderConfig::LlamaCpp(LlamaCppConfig {
            api_key: prov.api_key.clone(),
            base_url: prov
                .base_url
                .clone()
                .unwrap_or_else(default_llamacpp_base_url),
            model: model.name.clone(),
            max_tokens: model.max_tokens.unwrap_or(default_max_tokens()),
            extra_params: model.extra_params.clone(),
        }),
        ProviderType::Ollama => ProviderConfig::Ollama(OllamaConfig {
            base_url: prov
                .base_url
                .clone()
                .unwrap_or_else(default_ollama_base_url),
            model: model.name.clone(),
            temperature: model.temperature,
            num_ctx: model.num_ctx,
        }),
    }
}

// Local copies of the defaults used by the existing per-provider
// sub-configs. Kept private to this module — when the parent module
// changes a default, this file changes too. *(necessarily same — they
// really are the same default)*
fn default_max_tokens() -> u64 {
    4096
}
fn default_openai_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}
fn default_llamacpp_base_url() -> String {
    "http://localhost:8080".to_string()
}
fn default_ollama_base_url() -> String {
    "http://localhost:11434".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn or_provider() -> ProviderEntry {
        ProviderEntry {
            name: "openrouter".into(),
            kind: ProviderType::OpenRouter,
            api_key: Some("sk-or-test".into()),
            base_url: None,
            models: vec![
                ModelEntry {
                    name: "anthropic/claude-3.7-sonnet".into(),
                    alias: Some("sonnet".into()),
                    max_tokens: Some(8192),
                    temperature: None,
                    num_ctx: None,
                    extra_params: None,
                    context_window_override: None,
                },
                ModelEntry {
                    name: "anthropic/claude-opus-4".into(),
                    alias: Some("opus".into()),
                    max_tokens: None,
                    temperature: None,
                    num_ctx: None,
                    extra_params: None,
                    context_window_override: None,
                },
            ],
        }
    }

    fn oai_provider() -> ProviderEntry {
        ProviderEntry {
            name: "openai".into(),
            kind: ProviderType::OpenAI,
            api_key: Some("sk-test".into()),
            base_url: None,
            models: vec![
                ModelEntry {
                    name: "gpt-4o".into(),
                    alias: Some("oai-gpt4".into()),
                    max_tokens: Some(4000),
                    temperature: None,
                    num_ctx: None,
                    extra_params: None,
                    context_window_override: None,
                },
                ModelEntry {
                    name: "o3".into(), // no alias — addressable by name
                    alias: None,
                    max_tokens: None,
                    temperature: None,
                    num_ctx: None,
                    extra_params: None,
                    context_window_override: None,
                },
            ],
        }
    }

    #[test]
    fn build_resolves_aliases_and_default() {
        let providers = vec![or_provider(), oai_provider()];
        let reg = ModelRegistry::build(&providers, Some("sonnet")).expect("should build");

        assert_eq!(reg.len(), 4);
        assert_eq!(reg.default_alias(), Some("sonnet"));
        assert!(reg.contains("sonnet"));
        assert!(reg.contains("opus"));
        assert!(reg.contains("oai-gpt4"));
        assert!(reg.contains("o3"), "unaliased model addressable by name");
    }

    #[test]
    fn unaliased_model_is_addressable_by_its_wire_name() {
        let providers = vec![oai_provider()];
        let reg = ModelRegistry::build(&providers, Some("o3")).expect("should build");
        let resolved = reg.resolve("o3").unwrap();
        assert_eq!(resolved.alias, "o3");
        assert_eq!(resolved.model_name, "o3");
    }

    #[test]
    fn duplicate_alias_across_providers_is_rejected() {
        let mut a = or_provider();
        a.models[0].alias = Some("dup".into());
        let mut b = oai_provider();
        b.models[0].alias = Some("dup".into());

        let err = ModelRegistry::build(&[a, b], Some("dup")).unwrap_err();
        match err {
            RegistryError::DuplicateAlias { alias, .. } => assert_eq!(alias, "dup"),
            other => panic!("expected DuplicateAlias, got {other:?}"),
        }
    }

    #[test]
    fn reserved_alias_unknown_is_rejected_at_config_load() {
        let mut p = or_provider();
        p.models[0].alias = Some(RESERVED_UNAVAILABLE_ALIAS.into());
        let err = ModelRegistry::build(&[p], Some(RESERVED_UNAVAILABLE_ALIAS)).unwrap_err();
        assert!(matches!(err, RegistryError::ReservedAlias(_)));
    }

    #[test]
    fn invalid_alias_is_rejected() {
        let mut p = or_provider();
        p.models[0].alias = Some("Bad Alias!".into());
        let err = ModelRegistry::build(&[p], Some("opus")).unwrap_err();
        assert!(matches!(err, RegistryError::InvalidAlias { .. }));
    }

    #[test]
    fn unknown_default_model_fails_to_load() {
        let providers = vec![or_provider()];
        let err = ModelRegistry::build(&providers, Some("ghost")).unwrap_err();
        match err {
            RegistryError::UnknownDefault { alias, available } => {
                assert_eq!(alias, "ghost");
                assert!(available.contains("sonnet"));
                assert!(available.contains("opus"));
            }
            other => panic!("expected UnknownDefault, got {other:?}"),
        }
    }

    #[test]
    fn missing_default_with_models_is_rejected() {
        let providers = vec![or_provider()];
        let err = ModelRegistry::build(&providers, None).unwrap_err();
        assert_eq!(err, RegistryError::MissingDefault);
    }

    #[test]
    fn empty_registry_with_no_default_is_fine() {
        let reg = ModelRegistry::build(&[], None).expect("empty + None should work");
        assert!(reg.is_empty());
        assert_eq!(reg.default_alias(), None);
    }

    #[test]
    fn resolved_model_carries_provider_config_with_overrides() {
        let providers = vec![or_provider()];
        let reg = ModelRegistry::build(&providers, Some("sonnet")).unwrap();

        let resolved = reg.resolve("sonnet").unwrap();
        assert_eq!(resolved.provider_kind, ProviderType::OpenRouter);
        assert_eq!(resolved.provider_name, "openrouter");
        match &resolved.provider_config {
            ProviderConfig::OpenRouter(c) => {
                assert_eq!(c.model, "anthropic/claude-3.7-sonnet");
                assert_eq!(c.max_tokens, 8192, "per-model override applied");
                assert_eq!(c.api_key.as_deref(), Some("sk-or-test"));
            }
            other => panic!("wrong variant: {other:?}"),
        }

        // Model with no max_tokens override falls back to the default.
        let opus = reg.resolve("opus").unwrap();
        match &opus.provider_config {
            ProviderConfig::OpenRouter(c) => assert_eq!(c.max_tokens, 4096, "default applied"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn aliases_sorted_is_alphabetical() {
        let providers = vec![or_provider(), oai_provider()];
        let reg = ModelRegistry::build(&providers, Some("sonnet")).unwrap();
        let aliases = reg.aliases_sorted();
        assert_eq!(aliases, vec!["o3", "oai-gpt4", "opus", "sonnet"]);
    }
}
