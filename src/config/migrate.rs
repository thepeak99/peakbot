//! Pre-0.24 → 0.24 config migration; deleted in 0.26.
//!
//! Pure `Value → Value` (spec §7). It never goes through the old typed
//! `Config`: that would fill in defaults and lose the user's key order.

use serde_yaml::{Mapping, Value};

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
/// The old serde default of `vector_db.db_path`; 0.24 requires the key.
const OLD_VECTOR_DB_PATH: &str = "./.peakbot/vectors.db";
const DELEGATE_NOTE: &str =
    "tools: 'delegate' was blocked; delegation is now on whenever an agent has subagents.";

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
    let (providers, default_model) = providers(old);
    let head = Head::of(old);

    // Called in the order their notes are pinned: global notes, then agent
    // notes, then per-profile notes.
    let tools = get(old, "tools").and_then(|t| strip_delegate(t, &mut delegate_blocked));
    let search = search(old, &mut notes);
    let vector_db = vector_db(old, &mut notes);
    let conversation = conversation(old, &mut notes);
    let agents = agents(old, default_model.as_ref(), &head, &mut notes)?;

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
    let mut overlays: Vec<(&str, &Value)> = get(old, "profiles")
        .and_then(Value::as_mapping)
        .into_iter()
        .flatten()
        .filter_map(|(name, overlay)| Some((name.as_str()?, overlay)))
        .collect();
    // The old map was an unordered HashMap; sorting makes the output stable.
    overlays.sort_by_key(|(name, _)| *name);
    let overlaid: Vec<(String, Profile)> = overlays
        .into_iter()
        .map(|(name, overlay)| {
            overlay_profile(name, overlay, &base, &mut notes, &mut delegate_blocked)
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
/// way the old registry did (`describe_legacy`), under the alias `default`.
/// Returns the providers and the base agent's model.
fn providers(old: &Value) -> (Option<Value>, Option<Value>) {
    let listed =
        get(old, "providers").is_some_and(|p| p.as_sequence().is_none_or(|s| !s.is_empty()));
    match get(old, "provider") {
        Some(legacy) if !listed => (Some(legacy_provider(legacy)), Some("default".into())),
        _ => (
            get(old, "providers").cloned(),
            get(old, "default_model").cloned(),
        ),
    }
}

fn legacy_provider(legacy: &Value) -> Value {
    let kind = get(legacy, "type").cloned();
    let field = |key: &str| get(legacy, "config").and_then(|c| get(c, key)).cloned();

    let mut model = Mapping::new();
    insert_some(&mut model, "name", field("model"));
    model.insert("alias".into(), "default".into());
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
    insert_some(&mut provider, "name", kind.clone());
    insert_some(&mut provider, "type", kind);
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
    Value::Sequence(vec![Value::Mapping(provider)])
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

/// The base profile's agents: `main`, one per `pipelines` entry, then the
/// legacy `pipeline:` block as `team`.
fn agents(
    old: &Value,
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

    let mut main = Agent {
        name: BASE_AGENT.to_string(),
        model: default_model.cloned(),
        ..Agent::default()
    };
    main.take_head(head);
    let mut agents = vec![main];
    for (index, pipeline) in pipelines.iter().enumerate() {
        let name = get(pipeline, "name")
            .and_then(Value::as_str)
            .ok_or(MigrateError::PipelineWithoutName(index))?;
        let orchestrator = get(pipeline, "orchestrator");
        let members = get(pipeline, "agents");
        agents.push(pipeline_agent(
            name,
            orchestrator,
            members,
            default_model,
            head,
            notes,
        ));
    }
    if let Some(team) = team {
        let orchestrator = get(team, "orchestrator_prompt")
            .map(|prompt| Value::Mapping(Mapping::from_iter([("prompt".into(), prompt.clone())])));
        let members = get(team, "agents");
        agents.push(pipeline_agent(
            "team",
            orchestrator.as_ref(),
            members,
            default_model,
            head,
            notes,
        ));
    }
    Ok(agents)
}

fn pipeline_agent(
    old_name: &str,
    orchestrator: Option<&Value>,
    members: Option<&Value>,
    default_model: Option<&Value>,
    head: &Head,
    notes: &mut Vec<String>,
) -> Agent {
    let name = if old_name == BASE_AGENT {
        let renamed = format!("{BASE_AGENT}-2");
        notes.push(format!(
            "pipeline '{old_name}' was renamed to '{renamed}' (name collision with the base agent)."
        ));
        renamed
    } else {
        old_name.to_string()
    };
    let orchestrator_field = |key: &str| orchestrator.and_then(|o| get(o, key));
    let mut agent = Agent {
        name,
        pipeline: Some(old_name.to_string()),
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
    agent.subagents = members
        .and_then(Value::as_mapping)
        .into_iter()
        .flatten()
        .filter_map(|(name, def)| {
            Some(subagent(
                name.as_str()?,
                def,
                default_model,
                agent.model.as_ref(),
            ))
        })
        .collect();
    agent
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

/// An old overlay profile → a standalone profile: a deep copy of `base`
/// with the overlay applied (§7.3).
fn overlay_profile(
    old_name: &str,
    overlay: &Value,
    base: &Profile,
    notes: &mut Vec<String>,
    delegate_blocked: &mut bool,
) -> (String, Profile) {
    let name = if old_name == BASE_PROFILE {
        let renamed = format!("{BASE_PROFILE}-2");
        notes.push(format!(
            "profile '{old_name}' was renamed to '{renamed}' (name collision with the base profile)."
        ));
        renamed
    } else {
        old_name.to_string()
    };
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
    }
    if let Some(ui) = get(overlay, "ui") {
        profile.ui = strip_profile_tab(ui, &name, notes);
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
    (name, profile)
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
