//! The 0.24 config boundary (spec §5.3, §5.4, §6.8): global (+ optional repo)
//! YAML text in, one trusted [`Config`] out. Every rule of the §4.3 table
//! that is not raw serde lives here; the types below can only be built by
//! [`Config::parse`], so their invariants hold by construction and nothing
//! downstream re-checks them.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_path_to_error::Segment;
use serde_yaml::Value;

use super::errors::{ConfigError, ConfigErrors, ConfigFileKind, PathSeg, dotted};
use super::file::{
    AgentDef, ConfigFile, Env, General, ProfileDef, RepoFile, SubagentDef, VectorDbConfig,
};
use super::migrate::is_legacy;
use super::model_registry::canonical_alias;
use super::named::{NamedFailure, named_failure};
use super::{
    BUILTIN_TOOL_NAMES, EmbeddingsConfig, FetchUrlConfig, McpServerConfig, MemoryConfig,
    ModelRegistry, NameFilter, ProviderEntry, RegistryError, ResolvedModel,
};
use crate::vector::normalize_lexical;

/// The Web tabs a profile's `ui.tabs` may name, in canonical order. The old
/// `profile` tab is gone and `settings` is new (spec §3.3).
const TAB_NAMES: [&str; 7] = [
    "session", "todo", "files", "tasks", "bash", "agents", "settings",
];

const L1_MESSAGE: &str =
    "config.yaml uses the pre-0.24 layout — run 'peakbot config migrate' (writes config.yaml.bak)";
const F1_MESSAGE: &str = "set either `disabled` (blocklist) or `only` (allowlist), not both";

/// One input file: its absolute path (for messages and repo anchoring) and
/// its text. `parse` never touches the filesystem itself.
pub struct Source<'a> {
    pub path: &'a Path,
    pub text: &'a str,
}

/// Facts the caller discovered; only used for F6 warnings.
pub struct ParseCtx<'a> {
    pub known_skills: &'a [String],
}

/// The one trusted configuration value.
#[derive(Debug)]
pub struct Config {
    pub registry: Arc<ModelRegistry>,
    pub mcps: Vec<McpServerConfig>,
    pub general: General,
    pub catalog: Catalog,
    /// Never fatal (F6); shown by Settings validation and printed once at boot.
    pub warnings: Vec<String>,
}

/// The selectable profiles. I1: non-empty, repo-origin profiles first, so
/// "default" is always `[0]`. I7: a repo profile never coexists with a
/// same-named global one.
#[derive(Debug)]
pub struct Catalog {
    profiles: Vec<Arc<ResolvedProfile>>,
}

impl Catalog {
    /// The first agent of the first profile.
    pub fn default_bound(&self) -> Bound {
        Bound {
            profile: Arc::clone(&self.profiles[0]),
            agent_index: 0,
        }
    }

    pub fn profile(&self, name: &str) -> Option<&Arc<ResolvedProfile>> {
        self.profiles.iter().find(|p| p.name == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<ResolvedProfile>> {
        self.profiles.iter()
    }
}

/// A fully resolved profile. I1: `agents` is non-empty and `[0]` is the
/// default. I8: `tools` never names `delegate`; `mcps` only names declared
/// servers.
#[derive(Debug)]
pub struct ResolvedProfile {
    pub name: String,
    pub origin: Origin,
    pub tools: NameFilter,
    pub mcps: NameFilter,
    pub fetch_url: FetchUrlConfig,
    pub env: Env,
    pub memory: MemoryConfig,
    /// `None` = no doc tools.
    pub vector: Option<VectorSpec>,
    /// Already filtered from `TAB_NAMES`, in canonical order.
    pub tabs: Vec<&'static str>,
    pub agents: Vec<ResolvedAgent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Global,
    Repo { overrides_global: bool },
}

/// One agent and its subagents. I3: the `delegate` tool exists iff
/// `subagents` is non-empty — derived, never stored.
#[derive(Debug)]
pub struct ResolvedAgent {
    pub main: Member,
    /// The optional `# Instructions` text; `Some` ⇒ non-blank.
    pub prompt: Option<String>,
    pub head: Head,
    pub subagents: Vec<Subagent>,
}

/// The static prompt head. I4: `Whole` ⇔ `core_guidance: false`, and it
/// always carries a non-blank persona.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    BuiltIn,
    /// Trimmed persona, followed by the core guidance.
    Persona(String),
    /// Trimmed persona that is the whole head (no core guidance).
    Whole(String),
}

/// What a main agent and a subagent share. I5: no prompt and no persona
/// field — a subagent has nowhere to put one. I6: the model is resolved.
#[derive(Debug)]
pub struct Member {
    pub name: String,
    pub model: ResolvedModel,
    pub skills: NameFilter,
    pub agents_md: bool,
    /// Overlaid on the profile env at runtime.
    pub env: Env,
}

/// I5: `prompt` is non-blank — it is the subagent's whole persona.
#[derive(Debug)]
pub struct Subagent {
    pub member: Member,
    pub prompt: String,
}

#[derive(Debug)]
pub struct VectorSpec {
    pub db_path: DbPathSpec,
    pub embeddings: EmbeddingsConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbPathSpec {
    /// Absolute, `~/…`, or repo-relative made absolute at parse time.
    Fixed(PathBuf),
    /// A relative path in a global profile: resolved per session cwd.
    SessionRelative(PathBuf),
}

impl DbPathSpec {
    fn path(&self) -> &Path {
        match self {
            DbPathSpec::Fixed(p) | DbPathSpec::SessionRelative(p) => p,
        }
    }
}

/// The wire/persistence projection of a [`Bound`]; never stored beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub profile: String,
    pub agent: String,
}

/// A conversation's `(profile, agent)`. I2: a session holds exactly one.
#[derive(Debug, Clone)]
pub struct Bound {
    pub profile: Arc<ResolvedProfile>,
    pub agent_index: usize,
}

impl Bound {
    pub fn agent(&self) -> &ResolvedAgent {
        &self.profile.agents[self.agent_index]
    }

    pub fn selection(&self) -> Selection {
        Selection {
            profile: self.profile.name.clone(),
            agent: self.agent().main.name.clone(),
        }
    }
}

/// The boot-only blocks that differ between the booted and a fresh config,
/// in a stable order. This is the ONE boot-only list (spec §5.1).
pub fn boot_only_diff(boot: &Config, fresh: &Config) -> Vec<&'static str> {
    let (b, f) = (&boot.general, &fresh.general);
    [
        ("mcps", boot.mcps != fresh.mcps),
        ("general.web", b.web != f.web),
        ("general.http", b.http != f.http),
        ("general.uploads", b.uploads != f.uploads),
        ("general.conversation", b.conversation != f.conversation),
    ]
    .into_iter()
    .filter_map(|(key, changed)| changed.then_some(key))
    .collect()
}

impl Config {
    /// THE boundary: global (+ optional repo) YAML → trusted `Config`. No IO
    /// and no env reads (beyond `~` in `db_path`). Serde errors stop at the
    /// first; semantic errors are all collected, global file first.
    pub fn parse(
        global: Source<'_>,
        repo: Option<Source<'_>>,
        ctx: &ParseCtx<'_>,
    ) -> Result<Config, ConfigErrors> {
        // Duplicate keys make the `Value` parse fail; such a file skips the
        // legacy check and the typed parse below reports the duplicate.
        if serde_yaml::from_str::<Value>(global.text).is_ok_and(|doc| is_legacy(&doc)) {
            return Err(ConfigErrors(vec![ConfigError {
                file: ConfigFileKind::Global,
                path: Vec::new(),
                message: L1_MESSAGE.to_string(),
            }]));
        }
        let file: ConfigFile = typed(ConfigFileKind::Global, global.text)?;

        let mut global_sink = Sink::new(ConfigFileKind::Global);
        let mut repo_sink = Sink::new(ConfigFileKind::Repo);
        let repo_file = match &repo {
            Some(src) => parse_repo(src, global.path, &mut repo_sink)?,
            None => None,
        };

        let declared: Vec<String> = file
            .providers
            .iter()
            .flat_map(|p| p.models.iter().map(move |m| canonical_alias(p, m)))
            .collect();
        let registry = build_registry(&file.providers, &declared, &mut global_sink);
        check_mcps(&file.mcps, &mut global_sink);

        let mut resolver = Resolver {
            registry: registry.as_ref(),
            declared: &declared,
            mcp_names: file.mcps.iter().map(|s| s.name.as_str()).collect(),
            known_skills: ctx.known_skills,
            warnings: Vec::new(),
        };

        if file.profiles.is_empty() {
            global_sink.push(vec![key("profiles")], E2_MESSAGE);
        }
        let mut resolved = Vec::new();
        if let (Some(repo_file), Some(src)) = (&repo_file, &repo) {
            // `<root>/.peakbot/config.yaml` → `<root>`.
            let root = src
                .path
                .parent()
                .and_then(Path::parent)
                .unwrap_or(Path::new(""));
            for (name, def) in repo_file.profiles.iter() {
                let origin = Origin::Repo {
                    overrides_global: file.profiles.get(name).is_some(),
                };
                resolved.push(resolver.profile(&mut repo_sink, name, def, origin, Some(root)));
            }
        }
        for (name, def) in file.profiles.iter() {
            let shadowed = repo_file
                .as_ref()
                .is_some_and(|r| r.profiles.get(name).is_some());
            if !shadowed {
                resolved.push(resolver.profile(&mut global_sink, name, def, Origin::Global, None));
            }
        }
        let profiles: Vec<ResolvedProfile> = resolved.into_iter().flatten().collect();
        check_shared_vector_dbs(&profiles, &mut global_sink, &mut repo_sink);
        resolver.general(&mut global_sink, &file.general);

        let errors: Vec<ConfigError> = global_sink
            .errors
            .into_iter()
            .chain(repo_sink.errors)
            .collect();
        if !errors.is_empty() {
            return Err(ConfigErrors(errors));
        }
        let warnings = resolver.warnings;
        Ok(Config {
            // Every path that leaves the registry unbuilt reported an error.
            registry: Arc::new(registry.expect("registry built when no errors")),
            mcps: file.mcps,
            general: file.general,
            catalog: Catalog {
                profiles: profiles.into_iter().map(Arc::new).collect(),
            },
            warnings,
        })
    }
}

const E2_MESSAGE: &str = "declare at least one profile — the first one is the default";

/// RP1 for every non-`profiles` key (collected), else the typed parse.
fn parse_repo(
    src: &Source<'_>,
    global_path: &Path,
    sink: &mut Sink,
) -> Result<Option<RepoFile>, ConfigErrors> {
    if let Ok(Value::Mapping(root)) = serde_yaml::from_str::<Value>(src.text) {
        let before = sink.errors.len();
        for k in root.keys().filter(|k| k.as_str() != Some("profiles")) {
            let shown = match k.as_str() {
                Some(s) => s.to_string(),
                None => serde_yaml::to_string(k)
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            };
            sink.push(
                Vec::new(),
                format!(
                    "only `profiles:` is allowed in a repo config — move `{shown}:` to the global config ({})",
                    global_path.display()
                ),
            );
        }
        if sink.errors.len() > before {
            return Ok(None);
        }
    }
    typed(ConfigFileKind::Repo, src.text).map(Some)
}

fn typed<T: DeserializeOwned>(file: ConfigFileKind, text: &str) -> Result<T, ConfigErrors> {
    serde_path_to_error::deserialize(serde_yaml::Deserializer::from_str(text))
        .map_err(|err| ConfigErrors(vec![serde_error(file, &err)]))
}

/// Re-paths a serde failure to segments and swaps in the friendly §4.3
/// message where the table asks for one (E1–E3, P3, N1, N2).
fn serde_error(
    file: ConfigFileKind,
    err: &serde_path_to_error::Error<serde_yaml::Error>,
) -> ConfigError {
    let mut path: Vec<PathSeg> = err
        .path()
        .iter()
        .filter_map(|seg| match seg {
            Segment::Seq { index } => Some(PathSeg::Index(*index)),
            Segment::Map { key } => Some(PathSeg::Key(key.clone())),
            Segment::Enum { variant } => Some(PathSeg::Key(variant.clone())),
            Segment::Unknown => None,
        })
        .collect();
    // serde_path_to_error records the rejected key itself as the last
    // segment; the table (P1/P2/P3) points at the mapping that holds it.
    if let Some(PathSeg::Key(last)) = path.last()
        && err
            .inner()
            .to_string()
            .contains(&format!("unknown field `{last}`"))
    {
        path.pop();
    }
    let raw = bare_message(err.inner(), &path);

    let missing = raw
        .strip_prefix("missing field `")
        .and_then(|m| m.strip_suffix('`'))
        .map(str::to_string);
    let message = match (missing.as_deref(), path.len()) {
        (Some("providers"), 0) => {
            path.push(key("providers"));
            E1_MESSAGE.to_string()
        }
        (Some("profiles"), 0) => {
            path.push(key("profiles"));
            E2_MESSAGE.to_string()
        }
        (Some("agents"), 2) => {
            path.push(key("agents"));
            E3_MESSAGE.to_string()
        }
        _ if raw.starts_with("unknown field `persona`")
            && path.len() >= 2
            && path[path.len() - 2] == key("subagents") =>
        {
            "subagents have no persona — the prompt is the subagent's whole persona".to_string()
        }
        _ => match named_failure(&raw) {
            Some(NamedFailure::Invalid(name)) => {
                path.push(key(name));
                raw.clone()
            }
            Some(NamedFailure::Duplicate(name)) => {
                let noun = match path.last() {
                    Some(PathSeg::Key(k)) if k == "profiles" => "profile",
                    Some(PathSeg::Key(k)) if k == "agents" => "agent",
                    Some(PathSeg::Key(k)) if k == "subagents" => "subagent",
                    _ => "name",
                };
                format!("duplicate {noun} '{name}'")
            }
            None => raw.clone(),
        },
    };
    ConfigError {
        file,
        path,
        message,
    }
}

const E1_MESSAGE: &str = "declare at least one provider with at least one model";
const E3_MESSAGE: &str = "declare at least one agent — the first one is the default";

/// serde_yaml decorates its messages with its own path prefix and a position
/// suffix; the boundary reports the path as segments, so both go.
fn bare_message(err: &serde_yaml::Error, path: &[PathSeg]) -> String {
    let mut msg = err.to_string();
    if let Some(loc) = err.location() {
        let suffix = format!(" at line {} column {}", loc.line(), loc.column());
        if let Some(stripped) = msg.strip_suffix(&suffix) {
            msg = stripped.to_string();
        }
    }
    let mut yaml_path = String::new();
    for seg in path {
        match seg {
            PathSeg::Key(k) if yaml_path.is_empty() => yaml_path.push_str(k),
            PathSeg::Key(k) => {
                yaml_path.push('.');
                yaml_path.push_str(k);
            }
            PathSeg::Index(i) => yaml_path.push_str(&format!("[{i}]")),
        }
    }
    match msg.strip_prefix(&format!("{yaml_path}: ")) {
        Some(stripped) if !path.is_empty() => stripped.to_string(),
        _ => msg,
    }
}

/// E1, else the registry with the first declared alias as its (soon removed)
/// default; R1 errors are located at the offending alias.
fn build_registry(
    providers: &[ProviderEntry],
    declared: &[String],
    sink: &mut Sink,
) -> Option<ModelRegistry> {
    let Some(first) = declared.first() else {
        sink.push(vec![key("providers")], E1_MESSAGE);
        return None;
    };
    match ModelRegistry::build(providers, Some(first)) {
        Ok(registry) => Some(registry),
        Err(err) => {
            sink.push(registry_error_path(providers, &err), err.to_string());
            None
        }
    }
}

/// `RegistryError` names the alias, not its position: find the entry the
/// same way `build` walks them (a duplicate is reported on its second use).
fn registry_error_path(providers: &[ProviderEntry], err: &RegistryError) -> Vec<PathSeg> {
    let (alias, occurrence) = match err {
        RegistryError::DuplicateAlias { alias, .. } => (alias, 1),
        RegistryError::InvalidAlias { alias, .. } => (alias, 0),
        _ => return vec![key("providers")],
    };
    providers
        .iter()
        .enumerate()
        .flat_map(|(i, p)| {
            p.models
                .iter()
                .enumerate()
                .map(move |(j, m)| (i, j, canonical_alias(p, m)))
        })
        .filter(|(_, _, a)| a == alias)
        .nth(occurrence)
        .map(|(i, j, _)| {
            vec![
                key("providers"),
                PathSeg::Index(i),
                key("models"),
                PathSeg::Index(j),
                key("alias"),
            ]
        })
        .unwrap_or_else(|| vec![key("providers")])
}

/// Names non-empty and unique, plus the unchanged per-server `validate()`.
fn check_mcps(servers: &[McpServerConfig], sink: &mut Sink) {
    for (i, server) in servers.iter().enumerate() {
        let path = vec![key("mcps"), PathSeg::Index(i)];
        if server.name.trim().is_empty() {
            sink.push(at(&path, &["name"]), "must not be empty");
        } else if servers[..i].iter().any(|s| s.name == server.name) {
            sink.push(
                at(&path, &["name"]),
                format!("duplicate MCP server '{}'", server.name),
            );
        }
        if let Err(msg) = server.validate() {
            sink.push(path, msg);
        }
    }
}

/// V2: one DB file needs one embedding model. Reported on the later profile,
/// in the file that declares it.
fn check_shared_vector_dbs(profiles: &[ResolvedProfile], global: &mut Sink, repo: &mut Sink) {
    for (i, profile) in profiles.iter().enumerate() {
        let Some(spec) = &profile.vector else {
            continue;
        };
        let clash = profiles[..i].iter().find(|earlier| {
            earlier
                .vector
                .as_ref()
                .is_some_and(|e| e.db_path == spec.db_path && e.embeddings != spec.embeddings)
        });
        if let Some(earlier) = clash {
            let sink = match profile.origin {
                Origin::Global => &mut *global,
                Origin::Repo { .. } => &mut *repo,
            };
            sink.push(
                vec![key("profiles"), key(&profile.name), key("vector_db")],
                format!(
                    "db_path '{}' is also used by profile '{}' with different embeddings — one DB file needs one embedding model",
                    spec.db_path.path().display(),
                    earlier.name
                ),
            );
        }
    }
}

/// Errors of one file, in the order they are found.
struct Sink {
    file: ConfigFileKind,
    errors: Vec<ConfigError>,
}

impl Sink {
    fn new(file: ConfigFileKind) -> Self {
        Self {
            file,
            errors: Vec::new(),
        }
    }

    fn push(&mut self, path: Vec<PathSeg>, message: impl Into<String>) {
        self.errors.push(ConfigError {
            file: self.file,
            path,
            message: message.into(),
        });
    }
}

/// The global facts every profile resolves against (spec §6.8: repo profiles
/// see the global registry, `mcps` names and skills too).
struct Resolver<'a> {
    /// `None` when the registry itself failed (already reported): alias
    /// checks are skipped rather than piling M2 noise on top.
    registry: Option<&'a ModelRegistry>,
    /// Aliases in declaration order, for M2's "declared:" list.
    declared: &'a [String],
    mcp_names: Vec<&'a str>,
    known_skills: &'a [String],
    warnings: Vec<String>,
}

impl Resolver<'_> {
    /// Every check runs before anything is assembled, so all errors of the
    /// profile are collected; `None` means at least one was reported.
    fn profile(
        &mut self,
        sink: &mut Sink,
        name: &str,
        def: &ProfileDef,
        origin: Origin,
        repo_root: Option<&Path>,
    ) -> Option<ResolvedProfile> {
        let base = vec![key("profiles"), key(name)];
        let before = sink.errors.len();

        let filterable = filterable_tools();
        check_filter(sink, &at(&base, &["tools"]), &def.tools, |tool| {
            if tool == "delegate" {
                Some("'delegate' is not a filterable tool — it is on automatically when an agent has subagents".to_string())
            } else if filterable.contains(&tool) {
                None
            } else {
                Some(format!(
                    "unknown tool '{tool}'. Known tools: {}.",
                    filterable.join(", ")
                ))
            }
        });
        let mcp_names = &self.mcp_names;
        check_filter(sink, &at(&base, &["mcps"]), &def.mcps, |server| {
            if mcp_names.contains(&server) {
                None
            } else if mcp_names.is_empty() {
                Some(format!(
                    "names MCP server '{server}', but no MCP servers are declared in mcps"
                ))
            } else {
                Some(format!(
                    "unknown MCP server '{server}'. Declared servers: {}.",
                    mcp_names.join(", ")
                ))
            }
        });
        check_filter(sink, &at(&base, &["ui", "tabs"]), &def.ui.tabs, |tab| {
            (!TAB_NAMES.contains(&tab))
                .then(|| format!("unknown tab '{tab}'. Known tabs: {}.", TAB_NAMES.join(", ")))
        });
        let vector = def
            .vector_db
            .as_ref()
            .map(|v| vector_spec(sink, &base, v, repo_root));

        if def.agents.is_empty() {
            sink.push(at(&base, &["agents"]), E3_MESSAGE);
        }
        let agents: Vec<Option<ResolvedAgent>> = def
            .agents
            .iter()
            .map(|(agent, agent_def)| self.agent(sink, &base, agent, agent_def))
            .collect();

        if sink.errors.len() > before {
            return None;
        }
        Some(ResolvedProfile {
            name: name.to_string(),
            origin,
            tools: def.tools.clone(),
            mcps: def.mcps.clone(),
            fetch_url: def.fetch_url.clone(),
            env: def.env.clone(),
            memory: def.memory.clone(),
            vector: vector.flatten(),
            tabs: TAB_NAMES
                .into_iter()
                .filter(|tab| def.ui.tabs.allows(tab))
                .collect(),
            agents: agents.into_iter().collect::<Option<_>>()?,
        })
    }

    fn agent(
        &mut self,
        sink: &mut Sink,
        profile_path: &[PathSeg],
        name: &str,
        def: &AgentDef,
    ) -> Option<ResolvedAgent> {
        let path = at(profile_path, &["agents", name]);
        let model = match &def.model {
            Some(alias) => self.model(sink, at(&path, &["model"]), alias),
            None => {
                sink.push(at(&path, &["model"]), "Required — pick a model alias.");
                None
            }
        };
        let head = match (def.persona.as_deref().map(str::trim), def.core_guidance) {
            (Some(""), _) => {
                sink.push(
                    at(&path, &["persona"]),
                    "must not be blank — remove the key to use the built-in persona",
                );
                None
            }
            (None, false) => {
                sink.push(
                    at(&path, &["core_guidance"]),
                    "core_guidance: false needs a persona — with core guidance off, the persona is the whole prompt head",
                );
                None
            }
            (None, true) => Some(Head::BuiltIn),
            (Some(persona), true) => Some(Head::Persona(persona.to_string())),
            (Some(persona), false) => Some(Head::Whole(persona.to_string())),
        };
        let prompt = match &def.prompt {
            Some(p) if p.trim().is_empty() => {
                sink.push(
                    at(&path, &["prompt"]),
                    "must not be blank — remove the key for no instructions",
                );
                None
            }
            other => Some(other.clone()),
        };
        let skills = self.skills(sink, &at(&path, &["skills"]), &def.skills);
        let subagents: Vec<Option<Subagent>> = def
            .subagents
            .iter()
            .map(|(sub, sub_def)| self.subagent(sink, &path, sub, sub_def, model.as_ref()))
            .collect();

        Some(ResolvedAgent {
            main: Member {
                name: name.to_string(),
                model: model?,
                skills: skills?,
                agents_md: def.agents_md,
                env: def.env.clone(),
            },
            prompt: prompt?,
            head: head?,
            subagents: subagents.into_iter().collect::<Option<_>>()?,
        })
    }

    fn subagent(
        &mut self,
        sink: &mut Sink,
        agent_path: &[PathSeg],
        name: &str,
        def: &SubagentDef,
        parent_model: Option<&ResolvedModel>,
    ) -> Option<Subagent> {
        let path = at(agent_path, &["subagents", name]);
        let reserved = name == "orchestrator";
        if reserved {
            sink.push(
                path.clone(),
                "'orchestrator' is a reserved name — pick another subagent name",
            );
        }
        // No `model` → the parent agent's CONFIGURED model, not a registry default.
        let model = match &def.model {
            Some(alias) => self.model(sink, at(&path, &["model"]), alias),
            None => parent_model.cloned(),
        };
        let prompt = match &def.prompt {
            Some(p) if !p.trim().is_empty() => Some(p.clone()),
            _ => {
                sink.push(
                    at(&path, &["prompt"]),
                    "Required — a subagent needs a prompt.",
                );
                None
            }
        };
        let skills = self.skills(sink, &at(&path, &["skills"]), &def.skills);

        if reserved {
            return None;
        }
        Some(Subagent {
            member: Member {
                name: name.to_string(),
                model: model?,
                skills: skills?,
                agents_md: def.agents_md,
                env: def.env.clone(),
            },
            prompt: prompt?,
        })
    }

    fn model(&self, sink: &mut Sink, path: Vec<PathSeg>, alias: &str) -> Option<ResolvedModel> {
        let registry = self.registry?;
        let resolved = registry.resolve(alias).cloned();
        if resolved.is_none() {
            sink.push(
                path,
                format!(
                    "Unknown model alias {alias} — declared: {}",
                    self.declared.join(", ")
                ),
            );
        }
        resolved
    }

    /// F1 is an error; unknown names are only F6 warnings because the skill
    /// universe depends on the cwd.
    fn skills(
        &mut self,
        sink: &mut Sink,
        path: &[PathSeg],
        filter: &NameFilter,
    ) -> Option<NameFilter> {
        let (known, warnings) = (self.known_skills, &mut self.warnings);
        let shape_ok = check_filter(sink, path, filter, |skill| {
            if !known.iter().any(|k| k == skill) {
                warnings.push(format!(
                    "⚠ {}: no skill named '{skill}' is installed here",
                    dotted(path)
                ));
            }
            None
        });
        shape_ok.then(|| filter.clone())
    }

    /// G1–G7 and the compaction model (M2), in field order.
    fn general(&self, sink: &mut Sink, general: &General) {
        let base = [key("general")];
        if general
            .web
            .token
            .as_deref()
            .is_some_and(|t| t.trim().is_empty())
        {
            sink.push(
                at(&base, &["web", "token"]),
                "must not be blank — remove the key for no token",
            );
        }
        if general
            .search
            .as_ref()
            .is_some_and(|s| s.base_url.trim().is_empty())
        {
            sink.push(
                at(&base, &["search", "base_url"]),
                "must not be empty — remove `search:` to turn web search off",
            );
        }
        let threshold = general.context.threshold;
        if !(threshold > 0.0 && threshold <= 1.0) {
            sink.push(
                at(&base, &["context", "threshold"]),
                format!("must be greater than 0 and at most 1, got {threshold}"),
            );
        }
        if let Some(alias) = &general.context.compaction_model {
            self.model(sink, at(&base, &["context", "compaction_model"]), alias);
        }
        if general.agent_max_turns == 0 {
            sink.push(at(&base, &["agent_max_turns"]), "must be at least 1");
        }
        if let Err(msg) = general.timeouts.validate() {
            // The existing message names its field (`timeouts.<field> must …`).
            let field = msg
                .strip_prefix("timeouts.")
                .and_then(|rest| rest.split(' ').next());
            let path = match field {
                Some(field) => at(&base, &["timeouts", field]),
                None => at(&base, &["timeouts"]),
            };
            sink.push(path, msg);
        }
        let uploads = &general.uploads;
        if !(1..=1024).contains(&uploads.max_file_mb) {
            sink.push(
                at(&base, &["uploads", "max_file_mb"]),
                format!("must be 1..=1024, got {}", uploads.max_file_mb),
            );
        }
        if !(1..=100).contains(&uploads.max_files) {
            sink.push(
                at(&base, &["uploads", "max_files"]),
                format!("must be 1..=100, got {}", uploads.max_files),
            );
        }
    }
}

/// F1, then `name_error` for every listed name. Returns whether the shape
/// was valid.
fn check_filter(
    sink: &mut Sink,
    path: &[PathSeg],
    filter: &NameFilter,
    mut name_error: impl FnMut(&str) -> Option<String>,
) -> bool {
    if !filter.disabled.is_empty() && !filter.only.is_empty() {
        sink.push(path.to_vec(), F1_MESSAGE);
        return false;
    }
    for name in filter.disabled.iter().chain(&filter.only) {
        if let Some(message) = name_error(name) {
            sink.push(path.to_vec(), message);
        }
    }
    true
}

/// `delegate` is on iff an agent has subagents, so it is not filterable.
fn filterable_tools() -> Vec<&'static str> {
    BUILTIN_TOOL_NAMES
        .iter()
        .copied()
        .filter(|tool| *tool != "delegate")
        .collect()
}

/// V1, then the `DbPathSpec`. `None` = an error was reported.
fn vector_spec(
    sink: &mut Sink,
    profile_path: &[PathSeg],
    def: &VectorDbConfig,
    repo_root: Option<&Path>,
) -> Option<VectorSpec> {
    if def.embeddings.dimensions == 0 {
        sink.push(
            at(profile_path, &["vector_db", "embeddings", "dimensions"]),
            "must be greater than 0",
        );
        return None;
    }
    Some(VectorSpec {
        db_path: db_path_spec(&def.db_path, repo_root),
        embeddings: def.embeddings.clone(),
    })
}

/// Absolute or `~` → `Fixed`; relative → anchored to the repo root for a
/// repo profile, or left session-relative for a global one (today's
/// behaviour).
fn db_path_spec(raw: &str, repo_root: Option<&Path>) -> DbPathSpec {
    if let Some(home_path) = expand_home(raw) {
        return DbPathSpec::Fixed(normalize_lexical(&home_path));
    }
    let path = Path::new(raw);
    if path.is_absolute() {
        return DbPathSpec::Fixed(normalize_lexical(path));
    }
    match repo_root {
        Some(root) => DbPathSpec::Fixed(normalize_lexical(&root.join(path))),
        None => DbPathSpec::SessionRelative(path.to_path_buf()),
    }
}

/// `~` / `~/…` against the home dir. Without a home dir the path is left
/// as written (relative).
fn expand_home(raw: &str) -> Option<PathBuf> {
    let rest = if raw == "~" {
        ""
    } else {
        raw.strip_prefix("~/")?
    };
    dirs::home_dir().map(|home| home.join(rest))
}

fn key(k: &str) -> PathSeg {
    PathSeg::Key(k.to_string())
}

fn at(base: &[PathSeg], keys: &[&str]) -> Vec<PathSeg> {
    base.iter()
        .cloned()
        .chain(keys.iter().map(|k| key(k)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P2/P4 keep serde's wording, but without serde_yaml's own path prefix
    /// and position suffix — the path travels as segments.
    #[test]
    fn serde_messages_lose_the_yaml_path_prefix_and_position() {
        let text = "providers: []\nprofiles: {}\ngeneral:\n  web:\n    tls: 3\n  bogus: 1\n";
        let err = typed::<ConfigFile>(ConfigFileKind::Global, text).unwrap_err();
        let e = &err.0[0];
        assert_eq!(e.path, vec![key("general"), key("web"), key("tls")]);
        assert_eq!(e.message, "invalid type: integer `3`, expected a boolean");

        let text = "providers: []\nprofiles: {}\ngeneral:\n  bogus: 1\n";
        let err = typed::<ConfigFile>(ConfigFileKind::Global, text).unwrap_err();
        let e = &err.0[0];
        assert_eq!(e.path, vec![key("general")]);
        assert!(
            e.message
                .starts_with("unknown field `bogus`, expected one of `web`")
                && !e.message.contains(" at line "),
            "got: {}",
            e.message
        );
    }
}
