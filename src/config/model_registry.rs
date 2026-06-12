//! Multi-model registry — providers list, models with aliases, and a
//! resolver that maps a user-typed alias to a fully-formed
//! [`ProviderConfig`] the existing `create_provider` already understands.
//!
//! See `multi-model.md` for the locked design. Highlights:
//! - `providers:` is a YAML *list*, each entry owns its `models:` list.
//! - Provider `name` is informational only (no cross-references).
//! - Model `alias` is optional; falls back to `name`.
//! - Aliases are globally unique and must match `^[A-Za-z0-9_./:-]+$`.
//! - `default_model` is required iff any models are declared.
//!
//! Conversations persist `(provider_name, model_name)` — never the alias.
//! Aliases are UI sugar; the wire identity is the re-activation key. See
//! [`ModelRegistry::find_by_wire_id`].
//!
//! The registry's only job is *alias → existing-shape*. Everything
//! downstream (`create_provider`, `ProviderInfo`, the agent itself)
//! stays as it was. *(reuse the seam that already exists)*

use crate::config::{
    AnthropicConfig, LlamaCppConfig, OllamaConfig, OpenAIConfig, OpenRouterConfig, ProviderConfig,
    ProviderType,
};
use serde::Deserialize;
use std::collections::HashMap;

/// One entry in the top-level `providers:` list. Owns its credentials
/// and its `models:` list.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    /// The wire id sent to the API (e.g. `gpt-4o`,
    /// `anthropic/claude-3.7-sonnet`, `qwen2.5-coder:14b`). No munging.
    pub name: String,
    /// User-facing handle for `/model` and `/load`. **Optional — when
    /// absent, the model is addressable only as
    /// `<provider_name>/<model_name>` (the FULL qualified handle).
    /// The bare model leaf alone is never accepted.** Globally unique
    /// across all providers; must match `^[A-Za-z0-9_./:-]+$`.
    #[serde(default)]
    pub alias: Option<String>,
    /// Optional max-tokens override for this model.
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// Optional temperature override (Ollama, OpenAI compatible).
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Optional pass-through extra params for LlamaCpp.
    #[serde(default)]
    pub extra_params: Option<serde_json::Value>,
    /// Anthropic prompt-caching mode for this model (Anthropic provider only).
    /// `None` means `off`. See [`crate::config::AnthropicCaching`].
    #[serde(default)]
    pub prompt_caching: Option<crate::config::AnthropicCaching>,
    /// Per-model context size in tokens. When `None`, resolved at
    /// registry-build time via
    /// [`crate::context_manager::auto_detect_context_size`].
    ///
    /// Accepts the legacy field name `context_window_override` for
    /// backward compatibility.
    #[serde(default, alias = "context_window_override", alias = "context_window")]
    pub context_size: Option<usize>,
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
    /// Resolved context size in tokens. Eagerly computed at
    /// registry-build time as `model.context_size` if set, otherwise
    /// via [`crate::context_manager::auto_detect_context_size`].
    /// Downstream consumers read this directly — no Option dance.
    pub context_size: usize,
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

    #[error(
        "alias `{alias}` on `{provider}/{model}` does not match required pattern `[A-Za-z0-9_./:-]+`"
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
    /// - any alias not matching `^[A-Za-z0-9_./:-]+$`,
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
                // Canonical handle:
                //   • alias if declared,
                //   • else `<provider.name>/<model.name>` — the FULL
                //     wire id, not a leaf-stripped form. This keeps
                //     the auto-generated handle predictable from the
                //     config alone (the user can construct the
                //     `default_model:` value without reading source)
                //     and preserves any namespace prefix the wire id
                //     carries (`minimax/MiniMax-M2.7` →
                //     `<prov>/minimax/MiniMax-M2.7`). *(principle of
                //     least astonishment)*
                let alias = model
                    .alias
                    .clone()
                    .unwrap_or_else(|| format!("{}/{}", prov.name, model.name));

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
                let context_size = model.context_size.unwrap_or_else(|| {
                    crate::context_manager::auto_detect_context_size(&model.name)
                });
                by_alias.insert(
                    alias.clone(),
                    ResolvedModel {
                        alias,
                        model_name: model.name.clone(),
                        provider_name: prov.name.clone(),
                        provider_kind: prov.kind.clone(),
                        provider_config,
                        context_size,
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

    /// Look up a model by its persisted wire identity
    /// `(provider_name, model_name)`. This is the **stable** re-activation
    /// key for `/load` — aliases are mutable user handles in `config.yaml`,
    /// the wire identity is what was actually sent to the API.
    ///
    /// Returns `None` if no provider in the current registry exposes
    /// that exact `(provider_name, model_name)` pair. Callers in `/load`
    /// surface the canonical
    /// `❌ Model '<provider>/<model>' not available.` diagnostic in
    /// that case and leave the current conversation untouched.
    pub fn find_by_wire_id(&self, provider_name: &str, model_name: &str) -> Option<&ResolvedModel> {
        self.by_alias
            .values()
            .find(|m| m.provider_name == provider_name && m.model_name == model_name)
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

/// `^[A-Za-z0-9_./:-]+$` — the alias pattern. Permits the punctuation
/// real model wire ids actually carry (`/` for OpenRouter/HF namespaces,
/// `.` for version numbers, `:` for Ollama tags, `-` and `_` for
/// everything else) plus uppercase letters. Rejects spaces and shell
/// metacharacters — which is the actual safety boundary. Kept as a
/// hand-rolled check instead of pulling in `regex` for a small grammar.
/// *(don't be too clever)*
fn is_valid_alias(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':'))
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
        ProviderType::Anthropic => ProviderConfig::Anthropic(AnthropicConfig {
            api_key: prov.api_key.clone(),
            base_url: prov
                .base_url
                .clone()
                .unwrap_or_else(default_anthropic_base_url),
            model: model.name.clone(),
            max_tokens: model.max_tokens.unwrap_or(default_max_tokens()),
            prompt_caching: model.prompt_caching.clone().unwrap_or_default(),
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
fn default_anthropic_base_url() -> String {
    "https://api.anthropic.com".to_string()
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
                    extra_params: None,
                    prompt_caching: None,
                    context_size: None,
                },
                ModelEntry {
                    name: "anthropic/claude-opus-4".into(),
                    alias: Some("opus".into()),
                    max_tokens: None,
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    context_size: None,
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
                    extra_params: None,
                    prompt_caching: None,
                    context_size: None,
                },
                ModelEntry {
                    name: "o3".into(), // no alias — addressable by name
                    alias: None,
                    max_tokens: None,
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    context_size: None,
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
        assert!(
            reg.contains("openai/o3"),
            "unaliased model addressable by qualified handle <provider>/<leaf>"
        );
    }

    #[test]
    fn unaliased_model_uses_provider_qualified_handle() {
        let providers = vec![oai_provider()];
        let reg = ModelRegistry::build(&providers, Some("openai/o3")).expect("should build");
        let resolved = reg.resolve("openai/o3").unwrap();
        assert_eq!(resolved.alias, "openai/o3");
        assert_eq!(
            resolved.model_name, "o3",
            "model_name is still the raw wire id"
        );
        // The bare wire id is no longer a registry handle.
        assert!(!reg.contains("o3"));
    }

    /// Regression for the case where the wire id itself contains
    /// slashes (e.g. `minimax/MiniMax-M2.7`). The user's reported
    /// config used `default_model: patchnotes/minimax/MiniMax-M2.7`
    /// and PeakBot must resolve it via the
    /// `<provider_name>/<model_name>` rule — concatenation, not
    /// "split by slash". The bare wire id (with its own slash)
    /// must NOT be addressable. *(principle of least astonishment)*
    #[test]
    fn unaliased_wire_id_with_slash_keeps_provider_prefix() {
        let prov = ProviderEntry {
            name: "patchnotes".into(),
            kind: ProviderType::LlamaCpp,
            api_key: Some("sk-test".into()),
            base_url: Some("https://ai.patchnotes.com/v1".into()),
            models: vec![ModelEntry {
                name: "minimax/MiniMax-M2.7".into(),
                alias: None,
                max_tokens: None,
                temperature: None,
                extra_params: None,
                prompt_caching: None,
                context_size: None,
            }],
        };
        let reg = ModelRegistry::build(&[prov], Some("patchnotes/minimax/MiniMax-M2.7"))
            .expect("should build");
        assert!(reg.contains("patchnotes/minimax/MiniMax-M2.7"));
        // Without the provider prefix the model is invisible.
        assert!(!reg.contains("minimax/MiniMax-M2.7"));
        // And the unqualified leaf alone is also invisible.
        assert!(!reg.contains("MiniMax-M2.7"));
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
        assert_eq!(aliases, vec!["oai-gpt4", "openai/o3", "opus", "sonnet"]);
    }

    /// Real-world wire ids carry `/`, `.`, `:`, and uppercase letters
    /// (OpenRouter `minimax/MiniMax-M2.7`, Ollama `qwen2.5-coder:14b`,
    /// HF `mistralai/Mistral-7B-Instruct-v0.3`). When the user omits
    /// an explicit `alias:`, the canonical handle becomes
    /// `<provider.name>/<model.name>` — the FULL wire id, namespace
    /// and all, so the handle is predictable from the config alone.
    /// *(reject spaces and shell metacharacters, not punctuation that
    /// real ids actually use)*
    #[test]
    fn unaliased_model_with_slash_dot_and_uppercase_is_accepted() {
        let prov = ProviderEntry {
            name: "patchnotes".into(),
            kind: ProviderType::OpenRouter,
            api_key: Some("sk-or-test".into()),
            base_url: None,
            models: vec![ModelEntry {
                name: "minimax/MiniMax-M2.7".into(),
                alias: None,
                max_tokens: None,
                temperature: None,
                extra_params: None,
                prompt_caching: None,
                context_size: None,
            }],
        };
        let reg = ModelRegistry::build(&[prov], Some("patchnotes/minimax/MiniMax-M2.7"))
            .expect("qualified handle <prov>/<full wire id> should be valid");
        let resolved = reg.resolve("patchnotes/minimax/MiniMax-M2.7").unwrap();
        assert_eq!(resolved.alias, "patchnotes/minimax/MiniMax-M2.7");
        // The wire id is still the raw model name.
        assert_eq!(resolved.model_name, "minimax/MiniMax-M2.7");
    }

    /// Ollama tags use `:` (e.g. `qwen2.5-coder:14b`) and have no `/`.
    /// Handle becomes `ollama/<wire-id>` — same shape as everywhere
    /// else: `<provider.name>/<full model.name>`.
    #[test]
    fn unaliased_ollama_tag_with_colon_is_accepted() {
        let prov = ProviderEntry {
            name: "ollama".into(),
            kind: ProviderType::Ollama,
            api_key: None,
            base_url: None,
            models: vec![ModelEntry {
                name: "qwen2.5-coder:14b".into(),
                alias: None,
                max_tokens: None,
                temperature: None,
                extra_params: None,
                prompt_caching: None,
                context_size: None,
            }],
        };
        let reg = ModelRegistry::build(&[prov], Some("ollama/qwen2.5-coder:14b"))
            .expect("colon-bearing Ollama tag with no `/` keeps full id as leaf");
        assert!(reg.contains("ollama/qwen2.5-coder:14b"));
    }

    /// Per-model `context_size` is eagerly resolved into
    /// `ResolvedModel.context_size`. With `None` we fall back to the
    /// auto-detect helper (single source of truth shared with the
    /// legacy boot path).
    #[test]
    fn context_size_resolved_eagerly_from_per_model_field() {
        let prov = ProviderEntry {
            name: "openrouter".into(),
            kind: ProviderType::OpenRouter,
            api_key: Some("sk".into()),
            base_url: None,
            models: vec![
                ModelEntry {
                    name: "anthropic/claude-3.7-sonnet".into(),
                    alias: Some("sonnet".into()),
                    max_tokens: None,
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    context_size: None, // → auto-detect → 200_000
                },
                ModelEntry {
                    name: "custom-model-no-table-entry".into(),
                    alias: Some("custom".into()),
                    max_tokens: None,
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    context_size: Some(42), // explicit → 42
                },
            ],
        };
        let reg = ModelRegistry::build(&[prov], Some("sonnet")).unwrap();
        assert_eq!(reg.resolve("sonnet").unwrap().context_size, 200_000);
        assert_eq!(reg.resolve("custom").unwrap().context_size, 42);
    }

    /// Regression for the user-reported bug: with a config like
    /// ```yaml
    /// providers:
    ///   - name: patchnotes
    ///     models:
    ///       - name: minimax/MiniMax-M2.7
    /// default_model: patchnotes/minimax/MiniMax-M2.7
    /// ```
    /// the auto-generated alias for an unaliased model must be
    /// `<provider.name>/<full model.name>`, not the leaf-stripped form.
    /// The leaf strategy silently dropped the `minimax/` namespace and
    /// produced `patchnotes/MiniMax-M2.7`, which is unpredictable from
    /// the config alone — the user can't construct the handle they need
    /// to put in `default_model:` without reading the source. The full
    /// wire id is the obvious, predictable choice. *(principle of least
    /// astonishment)*
    #[test]
    fn unaliased_model_handle_uses_full_wire_id_not_leaf() {
        let prov = ProviderEntry {
            name: "patchnotes".into(),
            kind: ProviderType::LlamaCpp,
            api_key: Some("sk".into()),
            base_url: None,
            models: vec![ModelEntry {
                name: "minimax/MiniMax-M2.7".into(),
                alias: None,
                max_tokens: None,
                temperature: None,
                extra_params: None,
                prompt_caching: None,
                context_size: None,
            }],
        };
        let reg = ModelRegistry::build(&[prov], Some("patchnotes/minimax/MiniMax-M2.7"))
            .expect("default referencing <provider>/<full wire id> must resolve");
        assert!(
            reg.contains("patchnotes/minimax/MiniMax-M2.7"),
            "auto-generated alias must preserve the full wire id, got: {:?}",
            reg.aliases_sorted()
        );
        assert!(
            !reg.contains("patchnotes/MiniMax-M2.7"),
            "leaf-stripped handle must NOT exist (it loses the namespace)"
        );
    }

    /// The legacy field name `context_window_override:` must still
    /// deserialise into the `context_size` field.
    #[test]
    fn legacy_context_window_override_field_name_still_parses() {
        let yaml = "
name: openrouter
type: openrouter
api_key: sk
models:
  - name: anthropic/claude-3.7-sonnet
    alias: sonnet
    context_window_override: 12345
";
        let prov: ProviderEntry = serde_yaml::from_str(yaml).expect("legacy field name parses");
        assert_eq!(prov.models[0].context_size, Some(12345));
    }

    /// The alternative field name `context_size:` must also deserialise
    /// into `context_size` (aliases are additive, not exclusive).
    #[test]
    fn context_size_field_name_also_parses() {
        let yaml = "
name: openrouter
type: openrouter
api_key: sk
models:
  - name: minimax/MiniMax-M2.7
    alias: minimax
    context_size: 204000
";
        let prov: ProviderEntry = serde_yaml::from_str(yaml).expect("context_size field parses");
        assert_eq!(prov.models[0].context_size, Some(204000));
    }

    // ── find_by_wire_id ──────────────────────────────────────────────────

    /// `find_by_wire_id` resolves on `(provider_name, model_name)` —
    /// the stable persistence key. This is the seam that lets `/load`
    /// re-activate a saved conversation even after the user has
    /// renamed the alias in `config.yaml`.
    #[test]
    fn find_by_wire_id_returns_resolved_match() {
        let prov = or_provider();
        let reg = ModelRegistry::build(&[prov], Some("opus")).expect("build");

        let resolved = reg
            .find_by_wire_id("openrouter", "anthropic/claude-opus-4")
            .expect("must resolve by wire id");
        assert_eq!(resolved.alias, "opus");
        assert_eq!(resolved.model_name, "anthropic/claude-opus-4");
        assert_eq!(resolved.provider_name, "openrouter");
    }

    /// Rename the alias in config — re-building the registry assigns
    /// a different alias, but the wire-id lookup is unchanged.
    #[test]
    fn find_by_wire_id_is_stable_across_alias_rename() {
        let mut prov = or_provider();
        prov.models[0].alias = Some("the-old-name".into());
        let reg_before = ModelRegistry::build(&[prov.clone()], Some("the-old-name")).unwrap();

        prov.models[0].alias = Some("the-new-name".into());
        let reg_after = ModelRegistry::build(&[prov], Some("the-new-name")).unwrap();

        let before = reg_before
            .find_by_wire_id("openrouter", "anthropic/claude-3.7-sonnet")
            .unwrap();
        let after = reg_after
            .find_by_wire_id("openrouter", "anthropic/claude-3.7-sonnet")
            .unwrap();
        assert_eq!(before.model_name, after.model_name);
        assert_eq!(before.provider_name, after.provider_name);
        assert_eq!(before.alias, "the-old-name");
        assert_eq!(after.alias, "the-new-name");
    }

    /// Unknown `(provider, model)` tuple → `None`. `/load` surfaces this
    /// as the canonical `Model 'x/y' not available.` diagnostic.
    #[test]
    fn find_by_wire_id_misses_cleanly_on_unknown_tuple() {
        let prov = or_provider();
        let reg = ModelRegistry::build(&[prov], Some("opus")).expect("build");

        assert!(reg.find_by_wire_id("openrouter", "no-such-model").is_none());
        assert!(
            reg.find_by_wire_id("no-such-provider", "anthropic/claude-opus-4")
                .is_none()
        );
    }
}
