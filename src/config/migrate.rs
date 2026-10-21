//! Pre-0.24 → 0.24 config migration; deleted in 0.26.
//!
//! Pure `Value → Value` (spec §7) plus the `migrate_file` IO wrapper (§7.5).
//! The pure half never goes through the old typed `Config`: that would fill
//! in defaults and lose the user's key order.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_yaml::{Mapping, Value};

use crate::PEAKBOT_VERSION;
use crate::config::errors::ConfigErrors;
use crate::config::resolve::{Config, ParseCtx, Source};
use crate::config::save_config_at;

/// Top-level keys that only the pre-0.24 layout has (spec §7.1).
const LEGACY_TOP: [&str; 22] = [
    "provider",
    "default_model",
    "persona",
    "system_prompt",
    "agent_max_turns",
    "mcp_servers",
    "searxng",
    "cost_tracking",
    "context",
    "memory",
    "conversation",
    "bash",
    "fetch_url",
    "retry",
    "pipeline",
    "pipelines",
    "vector_db",
    "web",
    "uploads",
    "tools",
    "http",
    "timeouts",
];

/// Keys of an old overlay profile (one without `agents`), spec §7.1.
const LEGACY_PROFILE: [&str; 8] = [
    "tools",
    "memory",
    "fetch_url",
    "pipelines",
    "mcp_servers",
    "system_prompt",
    "ui",
    "agents_md",
];

const BASE_AGENT: &str = "main";
const BASE_PROFILE: &str = "default";
/// The agent name of the legacy `pipeline:` block.
const TEAM_AGENT: &str = "team";
/// The old serde default of `vector_db.db_path`; 0.24 requires the key.
const OLD_VECTOR_DB_PATH: &str = "./.peakbot/vectors.db";
const DELEGATE_NOTE: &str =
    "tools: 'delegate' was blocked; delegation is now on whenever an agent has subagents.";
const BLANK_SYSTEM_PROMPT_NOTE: &str =
    "blank system_prompt was dropped (0.23 refused to start with it).";
/// 0.23's `default_model()` for the openrouter provider.
const OLD_DEFAULT_MODEL: &str = "anthropic/claude-3.7-sonnet";
const OLD_DEFAULT_PROVIDER: &str = "openrouter";
/// The alias the old registry synthesized for the legacy single provider.
const LEGACY_ALIAS: &str = "default";

pub struct Migration {
    pub config: Value,
    /// Semantic changes the user should know about, in a fixed order.
    pub notes: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("the config root is not a mapping")]
    RootNotAMapping,
    #[error("`pipelines` is not a list")]
    PipelinesNotAList,
    #[error("pipelines[{0}] has no `name`")]
    PipelineWithoutName(usize),
    #[error("{0}: a name must be a string")]
    NonStringName(String),
    #[error("provider: has no `type`")]
    LegacyProviderType,
    #[error("provider.config.model is required for type {0}")]
    LegacyProviderModel(String),
}

/// §7.1: legacy iff a legacy sign is present AND no new-only marker is, so a
/// stray old key in a current file gets the normal P1 error instead.
pub fn is_legacy(doc: &Value) -> bool {
    let Some(root) = doc.as_mapping() else {
        return false;
    };
    let profiles = || {
        root.get("profiles")
            .and_then(Value::as_mapping)
            .into_iter()
            .flat_map(|m| m.values())
            .filter_map(Value::as_mapping)
    };
    let current = root.contains_key("general")
        || root.contains_key("mcps")
        || profiles().any(|p| p.contains_key("agents"));
    let overlay_profile = profiles()
        .any(|p| !p.contains_key("agents") && LEGACY_PROFILE.iter().any(|k| p.contains_key(*k)));
    !current && (LEGACY_TOP.iter().any(|k| root.contains_key(*k)) || overlay_profile)
}

/// §7.2–7.3: the old layout → the 0.24 layout. No IO, deterministic.
pub fn migrate(old: &Value) -> Result<Migration, MigrateError> {
    if !old.is_mapping() {
        return Err(MigrateError::RootNotAMapping);
    }
    let mut notes = Vec::new();
    let mut delegate_blocked = false;
    let (providers, default_model) = providers(old, &mut notes)?;
    if is_blank(old, "system_prompt") {
        notes.push(BLANK_SYSTEM_PROMPT_NOTE.to_string());
    }
    let head = Head::of(old);

    // Called in the order their notes are pinned: global notes, then agent
    // notes, then per-profile notes.
    let tools = get(old, "tools").and_then(|t| strip_delegate(t, &mut delegate_blocked));
    let search = search(old, &mut notes);
    let vector_db = vector_db(old, &mut notes);
    let conversation = conversation(old, &mut notes);
    let mut main = Agent {
        name: BASE_AGENT.to_string(),
        model: default_model.clone(),
        ..Agent::default()
    };
    main.take_head(&head);
    let mut agents = vec![main];
    agents.extend(pipeline_agents(
        old,
        BASE_AGENT,
        default_model.as_ref(),
        &head,
        &mut notes,
    )?);

    let base = Profile {
        tools,
        mcps: None,
        fetch_url: get(old, "fetch_url").cloned(),
        env: get(old, "bash").and_then(|b| get(b, "env")).cloned(),
        memory: get(old, "memory").cloned(),
        vector_db,
        ui: None,
        agents,
    };
    let mut overlays: Vec<(&str, &Value)> = Vec::new();
    for (name, overlay) in get(old, "profiles")
        .and_then(Value::as_mapping)
        .into_iter()
        .flatten()
    {
        let name = name
            .as_str()
            .ok_or_else(|| MigrateError::NonStringName(format!("profiles.{}", key_text(name))))?;
        overlays.push((name, overlay));
    }
    // The old map was an unordered HashMap; sorting makes the output stable.
    overlays.sort_by_key(|(name, _)| *name);
    let mut names = Namer::new(
        BASE_PROFILE,
        overlays.iter().map(|(name, _)| *name),
        "profile",
    );
    let overlaid: Vec<(String, Profile)> = overlays
        .into_iter()
        .map(|(old_name, overlay)| {
            let name = names.assign(old_name, &mut notes);
            let profile = overlay_profile(&name, overlay, &base, &mut notes, &mut delegate_blocked);
            (name, profile)
        })
        .collect();
    let mut profiles = Mapping::new();
    profiles.insert(BASE_PROFILE.into(), base.into_value());
    for (name, profile) in overlaid {
        profiles.insert(name.into(), profile.into_value());
    }

    if delegate_blocked {
        notes.insert(0, DELEGATE_NOTE.to_string());
    }

    let mut general = Mapping::new();
    insert_some(&mut general, "web", get(old, "web").cloned());
    insert_some(&mut general, "search", search);
    insert_some(&mut general, "context", get(old, "context").cloned());
    insert_some(&mut general, "conversation", conversation);
    for key in [
        "agent_max_turns",
        "cost_tracking",
        "retry",
        "timeouts",
        "http",
        "uploads",
    ] {
        insert_some(&mut general, key, get(old, key).cloned());
    }

    let mut config = Mapping::new();
    insert_some(&mut config, "providers", providers);
    insert_some(&mut config, "mcps", get(old, "mcp_servers").cloned());
    config.insert("profiles".into(), Value::Mapping(profiles));
    if !general.is_empty() {
        config.insert("general".into(), Value::Mapping(general));
    }
    Ok(Migration {
        config: Value::Mapping(config),
        notes,
    })
}

// ── Repo files (§7.6) ──────────────────────────────────────────────────────

/// Repo keys that changed the default profile in 0.23.
const REPO_APPLIED: [&str; 9] = [
    "tools",
    "memory",
    "fetch_url",
    "bash",
    "default_model",
    "persona",
    "system_prompt",
    "pipelines",
    "pipeline",
];
/// Repo keys that had an effect in 0.23 but are machine-wide in 0.24.
const REPO_MACHINE_WIDE: [&str; 9] = [
    "provider",
    "providers",
    "mcp_servers",
    "searxng",
    "agent_max_turns",
    "cost_tracking",
    "context",
    "conversation",
    "retry",
];
/// Repo keys 0.23 parsed but never applied.
const REPO_NO_EFFECT: [&str; 6] = [
    "vector_db",
    "web",
    "uploads",
    "http",
    "timeouts",
    "profiles",
];
const PROFILE_ORDER: [&str; 8] = [
    "tools",
    "mcps",
    "fetch_url",
    "env",
    "memory",
    "vector_db",
    "ui",
    "agents",
];
const AGENT_ORDER: [&str; 8] = [
    "model",
    "persona",
    "core_guidance",
    "prompt",
    "skills",
    "agents_md",
    "env",
    "subagents",
];

/// §7.6: a legacy repo file → `{profiles: …}` only, built on the CURRENT
/// global file. `global` must already be a valid 0.24 document;
/// `global_path` is used only inside note texts. No IO, deterministic.
pub fn migrate_repo(
    old_repo: &Value,
    global: &Value,
    global_path: &Path,
) -> Result<Migration, MigrateError> {
    let Some(root) = old_repo.as_mapping() else {
        return Err(MigrateError::RootNotAMapping);
    };
    let no_profiles = |notes| Migration {
        config: Value::Mapping(Mapping::from_iter([(
            "profiles".into(),
            Value::Mapping(Mapping::new()),
        )])),
        notes,
    };

    let mut notes = Vec::new();
    for (key, value) in root {
        if value.is_null() {
            continue;
        }
        match key.as_str() {
            Some(k) if REPO_APPLIED.contains(&k) => {}
            Some(k) if REPO_MACHINE_WIDE.contains(&k) => notes.push(format!(
                "'{k}' can no longer be set per repo and was dropped — it is machine-wide now; set it in {} if you still want it.",
                global_path.display()
            )),
            Some(k) if REPO_NO_EFFECT.contains(&k) => notes.push(format!(
                "'{k}' had no effect in a repo config before 0.24 and was dropped."
            )),
            // 0.23 ignored the whole file over one unknown key.
            _ => {
                return Ok(no_profiles(vec![format!(
                    "this file had no effect before 0.24 (0.23 ignored it because of the unknown key '{}'), so nothing was carried over.",
                    key.as_str().map_or_else(|| key_text(key), str::to_string)
                )]));
            }
        }
    }
    // 0.23 replaced the pipelines only with a non-empty list.
    let pipelines =
        get(old_repo, "pipelines").filter(|p| p.as_sequence().is_none_or(|list| !list.is_empty()));
    let applied = |key: &str| match key {
        "pipelines" => pipelines.is_some(),
        _ => get(old_repo, key).is_some(),
    };
    // Without a copy, later global edits keep reaching this repo.
    if !REPO_APPLIED.iter().any(|k| applied(k)) {
        return Ok(no_profiles(notes));
    }
    let Some((profile_name, base)) = global
        .get("profiles")
        .and_then(Value::as_mapping)
        .and_then(|profiles| profiles.iter().next())
    else {
        return Ok(no_profiles(notes));
    };
    let profile_name = name_text(profile_name);
    let mut profile = base.as_mapping().cloned().unwrap_or_default();

    if let Some(tools) = get(old_repo, "tools") {
        let mut delegate_blocked = false;
        match strip_delegate(tools, &mut delegate_blocked) {
            Some(tools) => profile.insert("tools".into(), tools),
            None => profile.remove("tools"),
        };
        if delegate_blocked {
            notes.push(DELEGATE_NOTE.to_string());
        }
    }
    for key in ["memory", "fetch_url"] {
        insert_some(&mut profile, key, get(old_repo, key).cloned());
    }
    // 0.23 replaced the whole `bash` block, so the env goes wholesale.
    insert_some(
        &mut profile,
        "env",
        get(old_repo, "bash").and_then(|b| get(b, "env")).cloned(),
    );

    let agents = profile
        .get("agents")
        .and_then(Value::as_mapping)
        .cloned()
        .unwrap_or_default();
    let Some((main_key, main)) = agents.iter().next() else {
        return Ok(no_profiles(notes));
    };
    let main_name = name_text(main_key);
    let mut main = main.as_mapping().cloned().unwrap_or_default();

    // The 0.23 slot rule (`merge_with`): a non-blank system_prompt wins.
    let mut applied_settings = Vec::new();
    if let Some(model) = get(old_repo, "default_model") {
        main.insert("model".into(), model.clone());
        applied_settings.push("default_model");
    }
    if let Some(full) = non_blank(old_repo, "system_prompt") {
        main.insert("persona".into(), full.clone());
        main.insert("core_guidance".into(), false.into());
        applied_settings.push("system_prompt");
    } else {
        if is_blank(old_repo, "system_prompt") {
            notes.push(BLANK_SYSTEM_PROMPT_NOTE.to_string());
        }
        if let Some(persona) = get(old_repo, "persona") {
            // A blank persona meant the built-in one, and it still cleared
            // the global system_prompt.
            match non_blank_value(Some(persona)) {
                Some(persona) => main.insert("persona".into(), persona.clone()),
                None => main.remove("persona"),
            };
            main.remove("core_guidance");
            applied_settings.push("persona");
        }
    }
    let main = Value::Mapping(reorder(main, &AGENT_ORDER));

    let replace_agents = pipelines.is_some() || get(old_repo, "pipeline").is_some();
    let agents = if replace_agents {
        let field = |key: &str| get(&main, key);
        let head = match (field("persona"), field("core_guidance")) {
            (Some(persona), Some(Value::Bool(false))) => Head::Full(persona.clone()),
            (Some(persona), _) => Head::Persona(persona.clone()),
            (None, _) => Head::Builtin,
        };
        let fallback = field("model").cloned();
        let extra = pipeline_agents(old_repo, &main_name, fallback.as_ref(), &head, &mut notes)?;
        let mut replaced = Mapping::from_iter([(main_key.clone(), main)]);
        for agent in extra {
            replaced.insert(agent.name.clone().into(), agent.into_value());
        }
        replaced
    } else {
        let mut kept = agents.clone();
        if !applied_settings.is_empty() && kept.len() > 1 {
            let others: Vec<String> = kept
                .keys()
                .skip(1)
                .map(|k| format!("'{}'", name_text(k)))
                .collect();
            notes.push(format!(
                "the settings {} from this file now apply only to agent '{main_name}'; agents {} keep their global settings.",
                applied_settings.join(", "),
                others.join(", ")
            ));
        }
        kept.insert(main_key.clone(), main);
        kept
    };
    profile.insert("agents".into(), Value::Mapping(agents));

    notes.push(format!(
        "profile '{profile_name}' in this repo is now a copy of the global profile '{profile_name}' with this file's settings applied — later edits to the global '{profile_name}' no longer reach this repo, and the other global profiles do not include this file's settings."
    ));
    let profiles = Mapping::from_iter([(
        Value::from(profile_name),
        Value::Mapping(reorder(profile, &PROFILE_ORDER)),
    )]);
    Ok(Migration {
        config: Value::Mapping(Mapping::from_iter([(
            "profiles".into(),
            Value::Mapping(profiles),
        )])),
        notes,
    })
}

/// A name key of the (valid) global file; always a string there.
fn name_text(key: &Value) -> String {
    key.as_str().map_or_else(|| key_text(key), str::to_string)
}

/// `map` with the keys of `order` first, in that order: a key set on a copy
/// lands at the end of a `Mapping` otherwise.
fn reorder(map: Mapping, order: &[&str]) -> Mapping {
    let mut out = Mapping::new();
    for key in order {
        if let Some(value) = map.get(*key) {
            out.insert((*key).into(), value.clone());
        }
    }
    for (key, value) in map {
        if !out.contains_key(&key) {
            out.insert(key, value);
        }
    }
    out
}

// ── IO wrapper (§7.5); deleted in 0.26 ─────────────────────────────────────

/// What `migrate_file` did; the CLI and the HTTP endpoint print/return it.
#[derive(Debug)]
pub struct MigrateReport {
    /// The `config.yaml` that now holds the migrated bytes.
    pub path: PathBuf,
    /// The `.bak` holding the previous bytes.
    pub backup: PathBuf,
    /// Human notes (token move, repo file, warnings), in order.
    pub notes: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum MigrateFileError {
    /// Not an accident: a second run is a no-op the CLI exits 0 for.
    #[error("config.yaml already uses the 0.24 layout — nothing to do.")]
    AlreadyCurrent,
    #[error("failed to read {0}: {1}")]
    Read(PathBuf, String),
    #[error("failed to parse {0}: {1}")]
    Parse(PathBuf, String),
    #[error("migrated config does not validate: {0}")]
    InvalidResult(String),
    #[error("failed to write the migrated config: {0}")]
    Write(String),
    #[error(transparent)]
    Migrate(#[from] MigrateError),
}

/// §7.5: read, migrate, validate — then write. A failed migration touches
/// neither `config.yaml` nor `config.yaml.bak`.
pub fn migrate_file(config_path: &Path, cwd: &Path) -> Result<MigrateReport, MigrateFileError> {
    let text = std::fs::read_to_string(config_path)
        .map_err(|e| MigrateFileError::Read(config_path.to_path_buf(), e.to_string()))?;
    let old: Value = serde_yaml::from_str(&text)
        .map_err(|e| MigrateFileError::Parse(config_path.to_path_buf(), e.to_string()))?;
    if !is_legacy(&old) {
        return Err(MigrateFileError::AlreadyCurrent);
    }
    let Migration {
        mut config,
        mut notes,
    } = migrate(&old)?;

    let config_dir = config_path.parent().ok_or_else(|| {
        MigrateFileError::Read(config_path.to_path_buf(), "no parent directory".to_string())
    })?;

    // Step 3: the web-token file becomes `general.web.token` (if absent).
    let token_path = config_dir.join("web-token");
    let token = std::fs::read_to_string(&token_path)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    if let Some(token) = &token
        && insert_web_token(&mut config, token)
    {
        notes.push(format!(
            "web-token file moved into general.web.token; renamed {}/web-token → web-token.bak.",
            config_dir.display()
        ));
    }

    // Steps 4–5: header + serialize, then validate BEFORE any write.
    let body =
        serde_yaml::to_string(&config).map_err(|e| MigrateFileError::Write(e.to_string()))?;
    let header = format!(
        "# migrated from the pre-0.24 layout by peakbot {PEAKBOT_VERSION} — previous file: config.yaml.bak"
    );
    let yaml = format!("{header}\n{body}");
    let ctx = ParseCtx { known_skills: &[] };
    Config::parse(
        Source {
            path: config_path,
            text: &yaml,
        },
        None,
        &ctx,
    )
    .map_err(|errors| MigrateFileError::InvalidResult(format_parse_errors(&errors)))?;

    // Step 6: backup + atomic write.
    let outcome =
        save_config_at(config_dir, &yaml).map_err(|e| MigrateFileError::Write(e.to_string()))?;

    // Step 7: rename the token file away; a failure is a note, not an error.
    if token.is_some() {
        let token_bak = config_dir.join("web-token.bak");
        if let Err(e) = std::fs::rename(&token_path, &token_bak) {
            notes.push(format!(
                "warning: could not rename web-token → web-token.bak: {e}"
            ));
        }
    }

    // Step 8: a legacy repo file is noted, never touched.
    let repo_config = cwd.join(".peakbot/config.yaml");
    if let Ok(text) = std::fs::read_to_string(&repo_config) {
        let legacy = serde_yaml::from_str::<Value>(&text).is_ok_and(|doc| {
            doc.as_mapping()
                .is_some_and(|m| m.keys().any(|k| k.as_str() != Some("profiles")))
        });
        if legacy {
            notes.push(format!(
                "{} uses the old layout and was NOT migrated — a repo config may now only contain profiles:",
                repo_config.display()
            ));
        }
    }

    Ok(MigrateReport {
        path: outcome.path,
        backup: outcome
            .backup
            .unwrap_or_else(|| config_dir.join("config.yaml.bak")),
        notes,
    })
}

/// The old static-prompt slot: a non-blank `system_prompt` wins over `persona`.
enum Head {
    Builtin,
    Persona(Value),
    Full(Value),
}

impl Head {
    fn of(old: &Value) -> Self {
        if let Some(full) = non_blank(old, "system_prompt") {
            Head::Full(full.clone())
        } else if let Some(persona) = non_blank(old, "persona") {
            Head::Persona(persona.clone())
        } else {
            Head::Builtin
        }
    }
}

#[derive(Clone)]
struct Profile {
    tools: Option<Value>,
    mcps: Option<Value>,
    fetch_url: Option<Value>,
    env: Option<Value>,
    memory: Option<Value>,
    vector_db: Option<Value>,
    ui: Option<Value>,
    agents: Vec<Agent>,
}

#[derive(Clone, Default)]
struct Agent {
    name: String,
    /// The old pipeline name, which a profile's `pipelines` filter matches;
    /// `None` for the base agent, which no filter can remove.
    pipeline: Option<String>,
    model: Option<Value>,
    persona: Option<Value>,
    core_guidance: Option<bool>,
    prompt: Option<Value>,
    agents_md: Option<bool>,
    subagents: Vec<Subagent>,
}

#[derive(Clone)]
struct Subagent {
    name: String,
    model: Option<Value>,
    prompt: Option<Value>,
    skills: Option<Value>,
    agents_md: Option<bool>,
    env: Option<Value>,
}

impl Profile {
    /// Keys in `ProfileDef` field order.
    fn into_value(self) -> Value {
        let mut map = Mapping::new();
        insert_some(&mut map, "tools", self.tools);
        insert_some(&mut map, "mcps", self.mcps);
        insert_some(&mut map, "fetch_url", self.fetch_url);
        insert_some(&mut map, "env", self.env);
        insert_some(&mut map, "memory", self.memory);
        insert_some(&mut map, "vector_db", self.vector_db);
        insert_some(&mut map, "ui", self.ui);
        let agents = self
            .agents
            .into_iter()
            .map(|a| (Value::from(a.name.clone()), a.into_value()))
            .collect();
        map.insert("agents".into(), Value::Mapping(agents));
        Value::Mapping(map)
    }
}

impl Agent {
    fn take_head(&mut self, head: &Head) {
        match head {
            Head::Builtin => {}
            Head::Persona(persona) => self.persona = Some(persona.clone()),
            Head::Full(full) => {
                self.persona = Some(full.clone());
                self.core_guidance = Some(false);
            }
        }
    }

    /// Keys in `AgentDef` field order.
    fn into_value(self) -> Value {
        let mut map = Mapping::new();
        insert_some(&mut map, "model", self.model);
        insert_some(&mut map, "persona", self.persona);
        insert_some(
            &mut map,
            "core_guidance",
            self.core_guidance.map(Value::Bool),
        );
        insert_some(&mut map, "prompt", self.prompt);
        insert_some(&mut map, "agents_md", self.agents_md.map(Value::Bool));
        if !self.subagents.is_empty() {
            let subagents = self
                .subagents
                .into_iter()
                .map(|s| (Value::from(s.name.clone()), s.into_value()))
                .collect();
            map.insert("subagents".into(), Value::Mapping(subagents));
        }
        Value::Mapping(map)
    }
}

impl Subagent {
    /// Keys in `SubagentDef` field order.
    fn into_value(self) -> Value {
        let mut map = Mapping::new();
        insert_some(&mut map, "model", self.model);
        insert_some(&mut map, "prompt", self.prompt);
        insert_some(&mut map, "skills", self.skills);
        insert_some(&mut map, "agents_md", self.agents_md.map(Value::Bool));
        insert_some(&mut map, "env", self.env);
        Value::Mapping(map)
    }
}

/// `providers` verbatim; else the legacy single `provider:` synthesized the
/// way the old registry did (`describe_legacy`), under the alias `default`;
/// else 0.23's built-in default provider. Returns the providers and the base
/// agent's model.
fn providers(
    old: &Value,
    notes: &mut Vec<String>,
) -> Result<(Option<Value>, Option<Value>), MigrateError> {
    let listed =
        get(old, "providers").is_some_and(|p| p.as_sequence().is_none_or(|s| !s.is_empty()));
    if listed {
        return Ok((
            get(old, "providers").cloned(),
            get(old, "default_model").cloned(),
        ));
    }
    let providers = match get(old, "provider") {
        Some(legacy) => legacy_provider(legacy, notes)?,
        None => {
            notes.push(format!(
                "no provider was configured; added the 0.23 default ({OLD_DEFAULT_PROVIDER}, {OLD_DEFAULT_MODEL}) as alias '{LEGACY_ALIAS}' — add your api_key under providers."
            ));
            let model = Mapping::from_iter([
                ("name".into(), OLD_DEFAULT_MODEL.into()),
                ("alias".into(), LEGACY_ALIAS.into()),
            ]);
            let provider = Mapping::from_iter([
                ("name".into(), OLD_DEFAULT_PROVIDER.into()),
                ("type".into(), OLD_DEFAULT_PROVIDER.into()),
                (
                    "models".into(),
                    Value::Sequence(vec![Value::Mapping(model)]),
                ),
            ]);
            Value::Sequence(vec![Value::Mapping(provider)])
        }
    };
    Ok((Some(providers), Some(LEGACY_ALIAS.into())))
}

/// Absent fields stay absent: the 0.24 registry's defaults (base_url,
/// max_tokens, prompt_caching) are the 0.23 serde defaults.
fn legacy_provider(legacy: &Value, notes: &mut Vec<String>) -> Result<Value, MigrateError> {
    let kind = get(legacy, "type")
        .and_then(Value::as_str)
        .ok_or(MigrateError::LegacyProviderType)?;
    let field = |key: &str| get(legacy, "config").and_then(|c| get(c, key)).cloned();

    let model_name = match field("model") {
        Some(name) => name,
        // Only openrouter had a serde default for `model` in 0.23.
        None if kind == OLD_DEFAULT_PROVIDER => {
            notes.push(format!(
                "provider.config.model was not set; used the 0.23 default '{OLD_DEFAULT_MODEL}'."
            ));
            OLD_DEFAULT_MODEL.into()
        }
        None => return Err(MigrateError::LegacyProviderModel(kind.to_string())),
    };
    if field("max_image_base64_bytes").is_some() {
        notes.push(
            "provider.config.max_image_base64_bytes has no 0.24 equivalent and was dropped (the 0.23 legacy-provider path did not apply it either)."
                .to_string(),
        );
    }

    let mut model = Mapping::new();
    model.insert("name".into(), model_name);
    model.insert("alias".into(), LEGACY_ALIAS.into());
    for key in [
        "max_tokens",
        "temperature",
        "extra_params",
        "prompt_caching",
        "vision",
    ] {
        insert_some(&mut model, key, field(key));
    }

    let mut provider = Mapping::new();
    provider.insert("name".into(), kind.into());
    provider.insert("type".into(), kind.into());
    for key in [
        "api_key",
        "base_url",
        "preserve_reasoning",
        "display_reasoning",
    ] {
        insert_some(&mut provider, key, field(key));
    }
    provider.insert(
        "models".into(),
        Value::Sequence(vec![Value::Mapping(model)]),
    );
    Ok(Value::Sequence(vec![Value::Mapping(provider)]))
}

/// `searxng` → `general.search`: presence of the block now means "on".
fn search(old: &Value, notes: &mut Vec<String>) -> Option<Value> {
    let searxng = get(old, "searxng")?;
    if is_false(searxng, "enabled") {
        notes.push("searxng was disabled; the search setting was dropped.".to_string());
        return None;
    }
    non_blank(searxng, "base_url")?;
    Some(without(searxng, &["enabled"]))
}

/// The base profile's `vector_db`: presence of the block now means "on".
fn vector_db(old: &Value, notes: &mut Vec<String>) -> Option<Value> {
    let vector_db = get(old, "vector_db")?;
    if is_false(vector_db, "enabled") {
        notes.push("vector_db was disabled; the vector_db setting was dropped.".to_string());
        return None;
    }
    let Some(fields) = vector_db.as_mapping() else {
        return Some(vector_db.clone());
    };
    let mut out = Mapping::new();
    out.insert(
        "db_path".into(),
        get(vector_db, "db_path")
            .cloned()
            .unwrap_or_else(|| OLD_VECTOR_DB_PATH.into()),
    );
    for (key, value) in fields {
        if key != "enabled" && key != "db_path" {
            out.insert(key.clone(), value.clone());
        }
    }
    Some(Value::Mapping(out))
}

fn conversation(old: &Value, notes: &mut Vec<String>) -> Option<Value> {
    let conversation = get(old, "conversation")?;
    let Some(fields) = conversation.as_mapping() else {
        return Some(conversation.clone());
    };
    let mut fields = fields.clone();
    let auto_resume = fields.remove("auto_resume").is_some();
    let max_conversations = fields.remove("max_conversations").is_some();
    if auto_resume || max_conversations {
        notes.push(
            "conversation.auto_resume / max_conversations were removed — they had no effect."
                .to_string(),
        );
    }
    (!fields.is_empty()).then_some(Value::Mapping(fields))
}

/// The agents after the base one: one per `pipelines` entry, then the legacy
/// `pipeline:` block as `team`. `base` is the base agent's name, which they
/// must not take.
fn pipeline_agents(
    old: &Value,
    base: &str,
    default_model: Option<&Value>,
    head: &Head,
    notes: &mut Vec<String>,
) -> Result<Vec<Agent>, MigrateError> {
    // Decided first: its "dropped" note is a global note.
    let team = match get(old, "pipeline") {
        Some(block) if get(block, "enabled") == Some(&Value::Bool(true)) => Some(block),
        Some(_) => {
            notes.push("pipeline was disabled and was dropped.".to_string());
            None
        }
        None => None,
    };
    let pipelines = match get(old, "pipelines") {
        None => &[][..],
        Some(Value::Sequence(list)) => list.as_slice(),
        Some(_) => return Err(MigrateError::PipelinesNotAList),
    };
    let mut pipeline_names = Vec::with_capacity(pipelines.len());
    for (index, pipeline) in pipelines.iter().enumerate() {
        let name = get(pipeline, "name").ok_or(MigrateError::PipelineWithoutName(index))?;
        let name = name
            .as_str()
            .ok_or_else(|| MigrateError::NonStringName(format!("pipelines[{index}].name")))?;
        pipeline_names.push(name);
    }

    let reserved = pipeline_names
        .iter()
        .copied()
        .chain(team.map(|_| TEAM_AGENT));
    let mut names = Namer::new(base, reserved, "pipeline");
    let mut agents = Vec::new();
    for (index, (pipeline, old_name)) in pipelines.iter().zip(pipeline_names).enumerate() {
        let name = names.assign(old_name, notes);
        agents.push(pipeline_agent(
            Origin {
                name,
                old_name,
                path: format!("pipelines[{index}].agents"),
            },
            get(pipeline, "orchestrator"),
            get(pipeline, "agents"),
            default_model,
            head,
            notes,
        )?);
    }
    if let Some(team) = team {
        let orchestrator = get(team, "orchestrator_prompt")
            .map(|prompt| Value::Mapping(Mapping::from_iter([("prompt".into(), prompt.clone())])));
        let name = names.assign(TEAM_AGENT, notes);
        agents.push(pipeline_agent(
            Origin {
                name,
                old_name: TEAM_AGENT,
                path: "pipeline.agents".to_string(),
            },
            orchestrator.as_ref(),
            get(team, "agents"),
            default_model,
            head,
            notes,
        )?);
    }
    Ok(agents)
}

/// Where a pipeline agent came from: its output name, the old pipeline name a
/// profile filter matches, and the YAML path of its members for errors.
struct Origin<'a> {
    name: String,
    old_name: &'a str,
    path: String,
}

fn pipeline_agent(
    origin: Origin<'_>,
    orchestrator: Option<&Value>,
    members: Option<&Value>,
    default_model: Option<&Value>,
    head: &Head,
    notes: &mut Vec<String>,
) -> Result<Agent, MigrateError> {
    let orchestrator_field = |key: &str| orchestrator.and_then(|o| get(o, key));
    let mut agent = Agent {
        name: origin.name,
        pipeline: Some(origin.old_name.to_string()),
        model: orchestrator_field("model").or(default_model).cloned(),
        prompt: orchestrator_field("prompt").cloned(),
        ..Agent::default()
    };
    match (head, non_blank_value(orchestrator_field("persona"))) {
        (Head::Full(_), _) | (Head::Persona(_), None) => agent.take_head(head),
        (_, Some(persona)) => agent.persona = Some(persona.clone()),
        // Old orchestrators dropped the built-in persona; 0.24 agents keep it.
        (Head::Builtin, None) => notes.push(format!(
            "agent '{}' previously ran without the built-in persona; it now gets it — set a persona to change that.",
            agent.name
        )),
    }
    for (name, def) in members.and_then(Value::as_mapping).into_iter().flatten() {
        let name = name.as_str().ok_or_else(|| {
            MigrateError::NonStringName(format!("{}.{}", origin.path, key_text(name)))
        })?;
        agent
            .subagents
            .push(subagent(name, def, default_model, agent.model.as_ref()));
    }
    Ok(agent)
}

fn subagent(
    name: &str,
    def: &Value,
    default_model: Option<&Value>,
    agent_model: Option<&Value>,
) -> Subagent {
    // Old fallback was `default_model`, the new one is the agent's model:
    // pin the old one wherever they differ (§7.4).
    let model = get(def, "model")
        .or(default_model.filter(|m| Some(*m) != agent_model))
        .cloned();
    Subagent {
        name: name.to_string(),
        model,
        prompt: get(def, "prompt").cloned(),
        skills: get(def, "skills").cloned(),
        agents_md: (get(def, "agents_md") == Some(&Value::Bool(true))).then_some(true),
        env: get(def, "env").cloned(),
    }
}

/// An old overlay profile → a standalone profile named `name`: a deep copy of
/// `base` with the overlay applied (§7.3).
fn overlay_profile(
    name: &str,
    overlay: &Value,
    base: &Profile,
    notes: &mut Vec<String>,
    delegate_blocked: &mut bool,
) -> Profile {
    let mut profile = base.clone();
    if let Some(tools) = get(overlay, "tools") {
        profile.tools = strip_delegate(tools, delegate_blocked);
    }
    if let Some(memory) = get(overlay, "memory") {
        profile.memory = Some(memory.clone());
    }
    if let Some(fetch_url) = get(overlay, "fetch_url") {
        profile.fetch_url = Some(fetch_url.clone());
    }
    if let Some(mcps) = get(overlay, "mcp_servers") {
        profile.mcps = Some(mcps.clone());
    }
    if let Some(filter) = get(overlay, "pipelines") {
        profile.agents.retain(|agent| {
            let keep = agent
                .pipeline
                .as_deref()
                .is_none_or(|pipeline| filter_allows(filter, pipeline));
            if !keep {
                notes.push(format!(
                    "profile '{name}': pipelines filter removed agent '{}'.",
                    agent.name
                ));
            }
            keep
        });
    }
    // The profile's system_prompt outranked even pipeline personas.
    if let Some(full) = non_blank(overlay, "system_prompt") {
        let head = Head::Full(full.clone());
        profile.agents.iter_mut().for_each(|a| a.take_head(&head));
    } else if is_blank(overlay, "system_prompt") {
        notes.push(format!(
            "profile '{name}': blank system_prompt was dropped (0.23 refused to use this profile)."
        ));
    }
    if let Some(ui) = get(overlay, "ui") {
        profile.ui = strip_profile_tab(ui, name, notes);
    }
    // The old profile flag was a ceiling over the subagents' opt-in too.
    if is_false(overlay, "agents_md") {
        for agent in &mut profile.agents {
            agent.agents_md = Some(false);
            for sub in &mut agent.subagents {
                sub.agents_md = Some(false);
            }
        }
    }
    profile
}

/// `delegate` is no longer filterable; it is on whenever subagents exist.
fn strip_delegate(tools: &Value, delegate_blocked: &mut bool) -> Option<Value> {
    let stripped = strip_name(tools, "delegate");
    *delegate_blocked |= stripped.in_disabled;
    stripped.filter
}

/// The `profile` tab is gone in 0.24 (spec §3.3).
fn strip_profile_tab(ui: &Value, profile: &str, notes: &mut Vec<String>) -> Option<Value> {
    let Some(tabs) = get(ui, "tabs") else {
        return Some(ui.clone());
    };
    let stripped = strip_name(tabs, "profile");
    if stripped.in_disabled || stripped.in_only {
        notes.push(format!(
            "profile '{profile}': ui.tabs entry 'profile' removed (the profile tab no longer exists)."
        ));
    }
    let mut ui = ui.as_mapping().cloned().unwrap_or_default();
    match stripped.filter {
        Some(tabs) => ui.insert("tabs".into(), tabs),
        None => ui.remove("tabs"),
    };
    (!ui.is_empty()).then_some(Value::Mapping(ui))
}

struct Stripped {
    /// `None` once nothing is left of the filter.
    filter: Option<Value>,
    in_disabled: bool,
    in_only: bool,
}

/// Removes `name` from a NameFilter's lists. An allowlist emptied this way
/// becomes `enabled: false`: an absent `only` would mean "allow all".
fn strip_name(filter: &Value, name: &str) -> Stripped {
    let Some(fields) = filter.as_mapping() else {
        return Stripped {
            filter: Some(filter.clone()),
            in_disabled: false,
            in_only: false,
        };
    };
    let mut fields = fields.clone();
    let in_disabled = remove_from_list(&mut fields, "disabled", name);
    let in_only = remove_from_list(&mut fields, "only", name);
    if in_only && !fields.contains_key("only") {
        fields.insert("enabled".into(), false.into());
    }
    Stripped {
        filter: (!fields.is_empty()).then_some(Value::Mapping(fields)),
        in_disabled,
        in_only,
    }
}

/// Drops the key once the list is empty, so no `disabled: []` noise remains.
fn remove_from_list(fields: &mut Mapping, key: &str, name: &str) -> bool {
    let Some(Value::Sequence(list)) = fields.get_mut(key) else {
        return false;
    };
    let before = list.len();
    list.retain(|entry| entry.as_str() != Some(name));
    let removed = list.len() != before;
    if removed && list.is_empty() {
        fields.remove(key);
    }
    removed
}

/// The old `NameFilter::allows` over a raw `Value`.
fn filter_allows(filter: &Value, name: &str) -> bool {
    let list = |key: &str| {
        get(filter, key)
            .and_then(Value::as_sequence)
            .map(Vec::as_slice)
            .unwrap_or_default()
    };
    let listed = |key: &str| list(key).iter().any(|entry| entry.as_str() == Some(name));
    if is_false(filter, "enabled") {
        false
    } else if !list("only").is_empty() {
        listed("only")
    } else {
        !listed("disabled")
    }
}

/// `key` of a mapping, treating an explicit `null` as absent.
fn get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key).filter(|v| !v.is_null())
}

fn is_false(value: &Value, key: &str) -> bool {
    get(value, key) == Some(&Value::Bool(false))
}

/// Old prompt slots treated a whitespace-only string as absent.
fn non_blank<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    non_blank_value(get(value, key))
}

/// A present, whitespace-only string: 0.23 rejected it in `system_prompt`.
fn is_blank(value: &Value, key: &str) -> bool {
    get(value, key)
        .and_then(Value::as_str)
        .is_some_and(|s| s.trim().is_empty())
}

/// A mapping key as the user wrote it, for `NonStringName` paths.
fn key_text(key: &Value) -> String {
    serde_yaml::to_string(key)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// The §7.3 `unique` helper. Every original name is reserved up front, so a
/// renamed entry never steals the name a later entry keeps.
struct Namer {
    used: HashSet<String>,
    reserved: HashSet<String>,
    /// `pipeline` or `profile`, for the rename note.
    kind: &'static str,
}

impl Namer {
    /// `base` is named first and always keeps its name.
    fn new<'a>(base: &str, originals: impl Iterator<Item = &'a str>, kind: &'static str) -> Self {
        let mut reserved: HashSet<String> = originals.map(str::to_string).collect();
        reserved.insert(base.to_string());
        Namer {
            used: HashSet::from([base.to_string()]),
            reserved,
            kind,
        }
    }

    fn assign(&mut self, original: &str, notes: &mut Vec<String>) -> String {
        if self.used.insert(original.to_string()) {
            return original.to_string();
        }
        let renamed = (2..)
            .map(|k| format!("{original}-{k}"))
            .find(|c| !self.used.contains(c) && !self.reserved.contains(c))
            .expect("an unbounded range always yields a free name");
        self.used.insert(renamed.clone());
        notes.push(format!(
            "{} '{original}' was renamed to '{renamed}' (name collision).",
            self.kind
        ));
        renamed
    }
}

fn non_blank_value(value: Option<&Value>) -> Option<&Value> {
    value.filter(|v| v.as_str().is_none_or(|s| !s.trim().is_empty()))
}

fn without(value: &Value, keys: &[&str]) -> Value {
    let mut out = value.clone();
    if let Some(fields) = out.as_mapping_mut() {
        for key in keys {
            fields.remove(*key);
        }
    }
    out
}

fn insert_some(map: &mut Mapping, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        map.insert(key.into(), value);
    }
}

/// Sets `general.web.token`, keeping key order: `web` first inside `general`,
/// `general` last at the top level. False when the token is already present.
fn insert_web_token(config: &mut Value, token: &str) -> bool {
    let Some(root) = config.as_mapping_mut() else {
        return false;
    };
    let general = root
        .entry("general".into())
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    let Some(general) = general.as_mapping_mut() else {
        return false;
    };
    if !general.contains_key("web") {
        // Mapping is insertion-ordered, so rebuild with `web` first.
        let rest = std::mem::take(general);
        let mut rebuilt = Mapping::new();
        rebuilt.insert("web".into(), Value::Mapping(Mapping::new()));
        for (key, value) in rest {
            rebuilt.insert(key, value);
        }
        *general = rebuilt;
    }
    let Some(web) = general.get_mut("web").and_then(Value::as_mapping_mut) else {
        return false;
    };
    if web.get("token").is_some_and(|t| !t.is_null()) {
        return false;
    }
    web.insert("token".into(), Value::String(token.to_string()));
    true
}

/// The §4.1 error list as one `;`-joined sentence for the error message.
fn format_parse_errors(errors: &ConfigErrors) -> String {
    errors
        .0
        .iter()
        .map(|e| {
            if e.path.is_empty() {
                e.message.clone()
            } else {
                format!("{}: {}", e.dotted_path(), e.message)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}
