//! The 0.24 config boundary (spec §5.3, §5.4, §6.8): global (+ optional repo)
//! YAML text in, one trusted [`Config`] out. Every rule of the §4.3 table
//! that is not raw serde lives here; the types below can only be built by
//! [`Config::parse`], so their invariants hold by construction and nothing
//! downstream re-checks them.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
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
    ModelRegistry, NameFilter, ProviderEntry, RegistryError, ResolvedModel, TAB_NAMES,
    get_config_file_path,
};
use crate::vector::normalize_lexical;

/// L1 (§4.4). Public because reload quotes it for a legacy global.
pub const L1_MESSAGE: &str = "config.yaml uses the pre-0.24 layout — restart PeakBot to migrate it automatically (the original is kept as config.yaml.pre-0.24)";
const RP2_MESSAGE: &str = "this repo config uses the pre-0.24 layout — PeakBot migrates it automatically on the next start or /cd into this directory";
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

/// Which config files [`Config::load`] reads. Never stored: the caller
/// derives it (a `--profile` pin forces `Ignore` — repo files never load
/// under a pin, spec §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoMode {
    /// Read `<cwd>/.peakbot/config.yaml` when it exists.
    Include,
    /// Skip the repo file.
    Ignore,
}

/// Why [`Config::load`] failed. `parse` itself stays IO-free; only the
/// file reads and the legacy pre-check live in `load`.
#[derive(Debug)]
pub enum LoadError {
    /// The global config file does not exist. The repo file never
    /// replaces it.
    Missing(PathBuf),
    /// The global file uses the pre-0.24 layout. Boot auto-migrates
    /// before this is reachable; reload and Settings surface it.
    Legacy(PathBuf),
    /// The files parsed but violate the §4.3 rules.
    Invalid(ConfigErrors),
    /// A config file could not be read.
    Io(PathBuf, io::Error),
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

    /// `(profile, agent)` → a Bound with NO overrides; `None` if either name
    /// is unknown — never a silent default (§5.3).
    pub fn resolve(&self, profile: &str, agent: &str) -> Option<Bound> {
        let profile = self.profile(profile)?;
        let agent_index = profile.agents.iter().position(|a| a.main.name == agent)?;
        Some(Bound {
            profile: Arc::clone(profile),
            agent_index,
        })
    }

    /// The session's starting `Bound`: the pinned profile's first agent
    /// when pinned, else the default (first profile, first agent). A pin
    /// names only GLOBAL profiles — repo files are ignored under a pin,
    /// so a repo-origin profile is unknown and not listed (§4.3 PIN).
    pub fn start_bound(&self, pin: Option<&str>) -> Result<Bound, String> {
        let Some(pin) = pin else {
            return Ok(self.default_bound());
        };
        let profile = self
            .profiles
            .iter()
            .find(|p| p.name == pin && matches!(p.origin, Origin::Global))
            .cloned();
        let Some(profile) = profile else {
            let configured: Vec<&str> = self
                .profiles
                .iter()
                .filter(|p| matches!(p.origin, Origin::Global))
                .map(|p| p.name.as_str())
                .collect();
            return Err(format!(
                "--profile '{pin}': unknown profile. Configured profiles: {}",
                configured.join(", ")
            ));
        };
        Ok(Bound {
            profile,
            agent_index: 0,
        })
    }

    /// The wire projection for Settings and AppState (§8.7), in catalog
    /// order (repo first). `inherited` is derived, not stored (Z4).
    pub fn infos(&self) -> Vec<ProfileInfo> {
        self.profiles.iter().map(|p| profile_info(p)).collect()
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

/// The wire projection of a profile for Settings and AppState (§8.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub name: String,
    /// `"repo"` or `"global"`. Owned (not `&'static str`) so `ProfileInfo`
    /// can derive `Deserialize<'de>` — `AppState`/`OutboundMessage` need it.
    pub origin: String,
    /// A repo profile shadowing a same-named global one.
    pub overrides_global: bool,
    pub agents: Vec<AgentInfo>,
}

/// The wire projection of an agent (§8.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    /// The agent's model alias.
    pub model: String,
    pub subagents: Vec<SubagentInfo>,
}

/// The wire projection of a subagent (§8.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubagentInfo {
    pub name: String,
    /// The subagent's model alias (the agent's when `inherited`).
    pub model: String,
    /// Derived, never stored: the subagent's model alias equals the
    /// agent's (Z4).
    pub inherited: bool,
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

#[derive(Debug, Clone)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub profile: String,
    pub agent: String,
    /// Subagent name → model alias, ONLY for overridden subagents (I9).
    /// Omitted from JSON when empty; absent in JSON = empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub subagent_models: BTreeMap<String, String>,
}

/// A conversation's `(profile, agent)`. I2: a session holds exactly one.
#[derive(Debug, Clone)]
pub struct Bound {
    pub profile: Arc<ResolvedProfile>,
    pub agent_index: usize,
}

/// Why a subagent model override could not apply (plan §5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverrideRejection {
    /// The key names no subagent of the bound agent.
    UnknownSubagent { subagent: String },
    /// The alias is not in the registry.
    UnknownAlias { subagent: String, alias: String },
}

impl Bound {
    pub fn agent(&self) -> &ResolvedAgent {
        &self.profile.agents[self.agent_index]
    }

    pub fn selection(&self) -> Selection {
        Selection {
            profile: self.profile.name.clone(),
            agent: self.agent().main.name.clone(),
            subagent_models: BTreeMap::new(),
        }
    }

    /// A `Bound` with NO overrides (plan §5.1).
    ///
    /// RED stub (T1): signature only — T2 lands the implementation.
    pub fn new(profile: Arc<ResolvedProfile>, agent_index: usize) -> Bound {
        let _ = (profile, agent_index);
        todo!("T2: Bound::new")
    }

    /// REPLACES (never merges) the override map. Valid entries apply even
    /// when others are rejected; rejections come back in subagent-name
    /// order (plan §5.1).
    ///
    /// RED stub (T1): signature only — T2 lands the implementation.
    pub fn with_subagent_models(
        self,
        registry: &ModelRegistry,
        wanted: &BTreeMap<String, String>,
    ) -> (Bound, Vec<OverrideRejection>) {
        let _ = (self, registry, wanted);
        todo!("T2: Bound::with_subagent_models")
    }

    /// I10 — the effective subagent model. `sub` must be one of
    /// `self.agent().subagents` (plan §5.1).
    ///
    /// RED stub (T1): signature only — T2 lands the implementation.
    pub fn subagent_model<'a>(
        &'a self,
        sub: &'a Subagent,
        orchestrator: &'a ResolvedModel,
    ) -> &'a ResolvedModel {
        let _ = (self, sub, orchestrator);
        todo!("T2: Bound::subagent_model (I10)")
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

    /// IO wrapper used by boot and reload (§5.4): reads the global file
    /// and — when `repo == RepoMode::Include` — `<cwd>/.peakbot/config.yaml`,
    /// then calls [`Self::parse`]. A missing global file is
    /// `LoadError::Missing`; the repo file never replaces it.
    pub fn load(cwd: &Path, repo: RepoMode, ctx: &ParseCtx<'_>) -> Result<Config, LoadError> {
        let Some(global_path) = get_config_file_path() else {
            // No platform config dir (no HOME) — the path is unknown, so
            // report the cwd the caller asked to load from.
            return Err(LoadError::Missing(cwd.to_path_buf()));
        };
        if !global_path.exists() {
            return Err(LoadError::Missing(global_path));
        }
        let text = std::fs::read_to_string(&global_path)
            .map_err(|e| LoadError::Io(global_path.clone(), e))?;
        // The same check as `parse` step 2, run here so a legacy file maps
        // to `Legacy` (auto-migration territory) instead of `Invalid`.
        if serde_yaml::from_str::<Value>(&text).is_ok_and(|doc| is_legacy(&doc)) {
            return Err(LoadError::Legacy(global_path));
        }
        let repo_path = cwd.join(".peakbot").join("config.yaml");
        let repo_text = match repo {
            RepoMode::Include if repo_path.exists() => Some(
                std::fs::read_to_string(&repo_path)
                    .map_err(|e| LoadError::Io(repo_path.clone(), e))?,
            ),
            _ => None,
        };
        let global = Source {
            path: &global_path,
            text: &text,
        };
        let repo_src = repo_text.as_ref().map(|t| Source {
            path: &repo_path,
            text: t,
        });
        match Self::parse(global, repo_src, ctx) {
            Ok(cfg) => Ok(cfg),
            Err(errors) => Err(LoadError::Invalid(errors)),
        }
    }
}

const E2_MESSAGE: &str = "declare at least one profile — the first one is the default";

/// RP1 for every non-`profiles` key (collected), else the typed parse.
fn parse_repo(
    src: &Source<'_>,
    global_path: &Path,
    sink: &mut Sink,
) -> Result<Option<RepoFile>, ConfigErrors> {
    let doc = serde_yaml::from_str::<Value>(src.text);
    // RP2 replaces RP1: one error for the whole file, not one per old key.
    if doc.as_ref().is_ok_and(is_legacy) {
        sink.push(Vec::new(), RP2_MESSAGE);
        return Ok(None);
    }
    if let Ok(Value::Mapping(root)) = doc {
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

/// E1, else the registry; R1 errors are located at the offending alias.
fn build_registry(
    providers: &[ProviderEntry],
    declared: &[String],
    sink: &mut Sink,
) -> Option<ModelRegistry> {
    if declared.is_empty() {
        sink.push(vec![key("providers")], E1_MESSAGE);
        return None;
    }
    match ModelRegistry::build(providers) {
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

        check_filter(sink, &at(&base, &["tools"]), &def.tools, |tool| {
            if tool == "delegate" {
                Some("'delegate' is not a filterable tool — it is on automatically when an agent has subagents".to_string())
            } else if BUILTIN_TOOL_NAMES.contains(&tool) {
                None
            } else {
                Some(format!(
                    "unknown tool '{tool}'. Known tools: {}.",
                    BUILTIN_TOOL_NAMES.join(", ")
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

fn profile_info(p: &ResolvedProfile) -> ProfileInfo {
    ProfileInfo {
        name: p.name.clone(),
        origin: match p.origin {
            Origin::Global => "global".to_string(),
            Origin::Repo { .. } => "repo".to_string(),
        },
        overrides_global: matches!(
            p.origin,
            Origin::Repo {
                overrides_global: true
            }
        ),
        agents: p.agents.iter().map(agent_info).collect(),
    }
}

fn agent_info(a: &ResolvedAgent) -> AgentInfo {
    let model = a.main.model.alias.clone();
    AgentInfo {
        name: a.main.name.clone(),
        model: model.clone(),
        subagents: a
            .subagents
            .iter()
            .map(|s| SubagentInfo {
                name: s.member.name.clone(),
                model: s.member.model.alias.clone(),
                inherited: s.member.model.alias == a.main.model.alias,
            })
            .collect(),
    }
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

// ─────────────────────────────────────────────────────────────────────────────
// T1 RED tests (plan "T1 core API finish"). Written BEFORE the items exist,
// against the locked spec: §4.3 (PIN message), §5.3 (Catalog/Selection/Bound),
// §5.4 (Config::load, RepoMode, LoadError), §8.7 (ProfileInfo wire shape),
// plus Z4 (derived `inherited`). They must fail to compile until T1 lands.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod t1_tests {
    use super::super::get_config_file_path;
    use super::*;

    /// A valid 0.24 global config (spec §3.1, trimmed): two profiles, `dev`
    /// first; `dev` has two agents (`team` first) and four subagents covering
    /// every `inherited` case.
    const GLOBAL_YAML: &str = r#"
providers:
  - name: patchnotes
    type: openai
    base_url: https://ai.patchnotes.com/v1
    api_key: pn-test
    models:
      - { name: anthropic/claude-sonnet-4.5, alias: sonnet, max_tokens: 8192 }
      - { name: anthropic/claude-opus-4.1, alias: opus, max_tokens: 8192 }
      - { name: qwen3-coder, alias: qwen, max_tokens: 32768 }
profiles:
  dev:
    agents:
      team:
        model: sonnet
        subagents:
          researcher:
            model: qwen
            prompt: Gather background, cite sources, hand back a brief.
          senior:
            model: opus
            prompt: Review the design and the code.
          tester:
            prompt: Write the failing tests first.
          mirror:
            model: sonnet
            prompt: Second pair of eyes on the same model.
      solo:
        model: sonnet
  research:
    agents:
      scholar:
        model: opus
"#;

    /// A valid repo config (spec §3.2, trimmed): one new profile plus one
    /// that shadows the global `dev` (replace, never merge — spec §2).
    const REPO_YAML: &str = r#"
profiles:
  peakbot-dev:
    agents:
      team:
        model: sonnet
        subagents:
          mid: { model: qwen, prompt: Implement the well-scoped piece you are given. }
  dev:
    agents:
      solo: { model: qwen }
"#;

    /// A 0.23-shaped global file: top-level `provider:`/`default_model:` are
    /// legacy signs with no new-only marker (spec §7.1) → `Legacy`, not a
    /// parse failure.
    const LEGACY_GLOBAL_YAML: &str = r#"
provider:
  name: patchnotes
  type: openai
  base_url: https://ai.patchnotes.com/v1
  api_key: pn-test
default_model: patchnotes/anthropic/claude-sonnet-4.5
"#;

    fn ctx() -> ParseCtx<'static> {
        ParseCtx { known_skills: &[] }
    }

    /// `Config::parse` over the fixtures; the paths only anchor the repo.
    fn parse(repo: Option<&str>) -> Config {
        Config::parse(
            Source {
                path: Path::new("/global/config.yaml"),
                text: GLOBAL_YAML,
            },
            repo.map(|text| Source {
                path: Path::new("/repo/.peakbot/config.yaml"),
                text,
            }),
            &ctx(),
        )
        .expect("fixture config must parse")
    }

    fn catalog(repo: Option<&str>) -> Catalog {
        parse(repo).catalog
    }

    /// Restores `XDG_CONFIG_HOME` on drop, so a panic cannot leak env state.
    struct XdgGuard(Option<String>);

    impl XdgGuard {
        fn new(dir: &Path) -> Self {
            let original = std::env::var("XDG_CONFIG_HOME").ok();
            unsafe { std::env::set_var("XDG_CONFIG_HOME", dir) };
            Self(original)
        }
    }

    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => unsafe { std::env::set_var("XDG_CONFIG_HOME", value) },
                None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
            }
        }
    }

    /// `XDG_CONFIG_HOME` pointed at a fresh tempdir (the global config is
    /// written when `Some`); the second return value is the session cwd.
    /// Held under the crate-wide env lock, like the legacy config tests.
    struct TempHome {
        // Field order is drop order: restore the env, delete the dir, and only
        // then release the lock.
        _guard: XdgGuard,
        _xdg: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl TempHome {
        fn new(global: Option<&str>) -> (Self, tempfile::TempDir) {
            // A failing test must not poison the lock for the others.
            let _lock = super::super::CONFIG_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let _xdg = tempfile::tempdir().expect("XDG tempdir");
            let _guard = XdgGuard::new(_xdg.path());
            if let Some(text) = global {
                let config_dir =
                    super::super::get_config_dir().expect("config dir under XDG_CONFIG_HOME");
                std::fs::create_dir_all(&config_dir).expect("config dir");
                std::fs::write(config_dir.join("config.yaml"), text).expect("write global config");
            }
            let cwd = tempfile::tempdir().expect("cwd tempdir");
            (
                Self {
                    _guard,
                    _xdg,
                    _lock,
                },
                cwd,
            )
        }

        fn write_repo(&self, cwd: &Path, text: &str) {
            let repo_dir = cwd.join(".peakbot");
            std::fs::create_dir_all(&repo_dir).expect(".peakbot dir");
            std::fs::write(repo_dir.join("config.yaml"), text).expect("write repo config");
        }
    }

    /// §5.4: a missing global file is `LoadError::Missing(<global path>)` —
    /// even under `RepoMode::Include`, the repo file never replaces it.
    #[test]
    fn load_missing_global_is_missing() {
        let (_home, cwd) = TempHome::new(None);
        let err = Config::load(cwd.path(), RepoMode::Include, &ctx()).expect_err("no global file");
        let expected = get_config_file_path().expect("path under XDG_CONFIG_HOME");
        assert!(
            matches!(err, LoadError::Missing(p) if p == expected),
            "expected LoadError::Missing({})",
            expected.display()
        );
    }

    /// §5.4: `RepoMode::Include` reads `<cwd>/.peakbot/config.yaml`; the repo
    /// profiles lead the catalog (I1) and the shadowed global drops out (I7).
    #[test]
    fn load_include_reads_repo_file() {
        let (home, cwd) = TempHome::new(Some(GLOBAL_YAML));
        home.write_repo(cwd.path(), REPO_YAML);
        let cfg = Config::load(cwd.path(), RepoMode::Include, &ctx()).expect("include loads");
        let names: Vec<&str> = cfg.catalog.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["peakbot-dev", "dev", "research"]);
        let peakbot_dev = cfg
            .catalog
            .profile("peakbot-dev")
            .expect("repo profile loaded");
        assert!(
            matches!(
                peakbot_dev.origin,
                Origin::Repo {
                    overrides_global: false
                }
            ),
            "a repo profile without a same-named global is Repo without override"
        );
    }

    /// §2/§5.4: `RepoMode::Ignore` (the `--profile` pin) never reads the repo
    /// file — the catalog is the global one, untouched.
    #[test]
    fn load_ignore_skips_repo_file() {
        let (home, cwd) = TempHome::new(Some(GLOBAL_YAML));
        home.write_repo(cwd.path(), REPO_YAML);
        let cfg = Config::load(cwd.path(), RepoMode::Ignore, &ctx()).expect("ignore loads");
        let names: Vec<&str> = cfg.catalog.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["dev", "research"]);
        assert!(
            cfg.catalog
                .iter()
                .all(|p| matches!(p.origin, Origin::Global)),
            "no repo profile may survive RepoMode::Ignore"
        );
    }

    /// §5.4/§7.1: a legacy global file is `LoadError::Legacy(<path>)` — not
    /// `Invalid` (boot auto-migrates first; this is the reload/Settings path).
    #[test]
    fn load_legacy_global_is_legacy() {
        let (_home, cwd) = TempHome::new(Some(LEGACY_GLOBAL_YAML));
        let err = Config::load(cwd.path(), RepoMode::Include, &ctx()).expect_err("legacy file");
        let expected = get_config_file_path().expect("path under XDG_CONFIG_HOME");
        assert!(
            matches!(err, LoadError::Legacy(p) if p == expected),
            "expected LoadError::Legacy({})",
            expected.display()
        );
    }

    /// §5.4: an unreadable global (a directory at the path) is
    /// `LoadError::Io` with the global path — not `Missing`, not
    /// `Invalid`.
    #[test]
    fn load_unreadable_global_is_io() {
        let (_home, cwd) = TempHome::new(None);
        let path = get_config_file_path().expect("path under XDG_CONFIG_HOME");
        std::fs::create_dir_all(&path).expect("directory at the config path");
        let err =
            Config::load(cwd.path(), RepoMode::Include, &ctx()).expect_err("unreadable global");
        assert!(
            matches!(err, LoadError::Io(p, _) if p == path),
            "expected LoadError::Io({})",
            path.display()
        );
    }

    /// §5.4: a new-layout file that violates the §4.3 rules (here E1 +
    /// E2) is `LoadError::Invalid` carrying the collected errors — not
    /// `Legacy` (no legacy signs) and not `Missing`.
    #[test]
    fn load_invalid_global_is_invalid() {
        let (_home, cwd) = TempHome::new(Some("providers: []\nprofiles: {}\n"));
        let err = Config::load(cwd.path(), RepoMode::Include, &ctx()).expect_err("invalid file");
        match err {
            LoadError::Invalid(errors) => assert!(!errors.0.is_empty()),
            other => panic!("expected LoadError::Invalid, got {other:?}"),
        }
    }

    /// §4.3 PIN, verbatim: an unknown pin lists the global profile names in
    /// declaration order.
    #[test]
    fn start_bound_pin_unknown_message() {
        let catalog = catalog(None);
        let msg = catalog.start_bound(Some("nope")).expect_err("unknown pin");
        assert_eq!(
            msg,
            "--profile 'nope': unknown profile. Configured profiles: dev, research"
        );
    }

    /// §4.3 PIN: the pin names a GLOBAL profile. A repo-origin profile (repo
    /// files are ignored under a pin) is unknown, and is not offered as a
    /// configured one.
    #[test]
    fn start_bound_pin_rejects_repo_origin_profile() {
        let catalog = catalog(Some(REPO_YAML));
        let msg = catalog
            .start_bound(Some("peakbot-dev"))
            .expect_err("a repo profile cannot be pinned");
        let prefix = "--profile 'peakbot-dev': unknown profile. Configured profiles: ";
        let listed = msg.strip_prefix(prefix).unwrap_or_else(|| {
            panic!("message must start with the PIN text, got: {msg}");
        });
        assert!(
            !listed.split(", ").any(|name| name == "peakbot-dev"),
            "repo profiles are not 'configured' globally, got: {msg}"
        );
    }

    /// "First = default" (§1.3): no pin → first profile's first agent; a
    /// valid pin → that global profile's first agent.
    #[test]
    fn start_bound_pin_picks_first_agent() {
        let catalog = catalog(None);
        let bound = catalog.start_bound(None).expect("no pin binds the default");
        assert_eq!(
            bound.selection(),
            Selection {
                profile: "dev".into(),
                agent: "team".into(),
                ..Default::default()
            }
        );
        let bound = catalog
            .start_bound(Some("research"))
            .expect("known pin binds the pin");
        assert_eq!(
            bound.selection(),
            Selection {
                profile: "research".into(),
                agent: "scholar".into(),
                ..Default::default()
            }
        );
    }

    /// §5.3: `resolve` needs BOTH names — a known profile with an unknown
    /// agent (and vice versa) is `None`, never a silent default.
    #[test]
    fn catalog_resolve_requires_both_names() {
        let catalog = catalog(None);
        let ok = catalog.resolve("dev", "team").expect("both names exist");
        assert_eq!(
            ok.selection(),
            Selection {
                profile: "dev".into(),
                agent: "team".into(),
                ..Default::default()
            }
        );
        assert!(catalog.resolve("dev", "ghost").is_none(), "unknown agent");
        assert!(
            catalog.resolve("ghost", "team").is_none(),
            "unknown profile"
        );
        assert!(catalog.resolve("ghost", "ghost").is_none(), "both unknown");
    }

    /// §8.7/I1/I7: `infos()` is the wire projection in catalog order — repo
    /// first, `origin` as "repo"/"global", `overrides_global` on the shadow.
    #[test]
    fn infos_repo_first_with_override_flag() {
        let infos = catalog(Some(REPO_YAML)).infos();
        assert_eq!(infos.len(), 3, "repo profiles plus the unshadowed globals");
        assert_eq!(infos[0].name, "peakbot-dev");
        assert_eq!(infos[0].origin, "repo");
        assert!(!infos[0].overrides_global);
        assert_eq!(infos[1].name, "dev");
        assert_eq!(infos[1].origin, "repo");
        assert!(infos[1].overrides_global, "repo `dev` shadows global `dev`");
        assert_eq!(
            infos[1].agents.len(),
            1,
            "replace, never merge: only the repo agent"
        );
        assert_eq!(infos[1].agents[0].name, "solo");
        assert_eq!(infos[2].name, "research");
        assert_eq!(infos[2].origin, "global");
        assert!(!infos[2].overrides_global);
    }

    /// Z4: `inherited` is DERIVED as "subagent model == agent model" — true
    /// for an absent model AND for an explicit-but-equal one, false otherwise.
    #[test]
    fn infos_inherited_when_model_equal() {
        let infos = catalog(None).infos();
        let team = &infos[0].agents[0];
        assert_eq!(team.name, "team");
        assert_eq!(team.model, "sonnet");
        assert_eq!(team.subagents.len(), 4);
        let subs = &team.subagents;
        let names: Vec<&str> = subs.iter().map(|s| s.name.as_str()).collect();
        let models: Vec<&str> = subs.iter().map(|s| s.model.as_str()).collect();
        let inherited: Vec<bool> = subs.iter().map(|s| s.inherited).collect();
        assert_eq!(names, ["researcher", "senior", "tester", "mirror"]);
        assert_eq!(models, ["qwen", "opus", "sonnet", "sonnet"]);
        assert_eq!(
            inherited,
            [false, false, true, true],
            "absent model and explicit-equal model are both inherited"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RED tests (selection-models plan §9.1, items 1–9): per-member subagent model
// overrides live INSIDE `Bound` (I9, private map + validating constructor) and
// the effective-model rule is the one place that decides what a delegation
// runs on (I10). Written against the §5.1 signatures; `Bound::new`,
// `with_subagent_models` and `subagent_model` are `todo!()` stubs until T2.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod subagent_model_tests {
    use std::collections::BTreeMap;

    use super::*;

    /// The plan's shared Rust fixture (§9).
    const YAML: &str = r#"
providers:
  - name: anthropic
    type: anthropic
    api_key: sk-ant-test
    models:
      - { name: claude-opus-5,   alias: opus,   context_size: 200000 }
      - { name: claude-sonnet-5, alias: sonnet, context_size: 200000 }
      - { name: claude-haiku-5,  alias: haiku,  context_size: 200000 }
general: { context: { enabled: false } }
profiles:
  dev:
    agents:
      team:
        model: sonnet
        subagents:
          reviewer: { prompt: "Review." }
          tester:   { prompt: "Test.",  model: haiku }
          twin:     { prompt: "Twin.",  model: sonnet }
      solo: { model: opus }
  research:
    agents:
      main: { model: haiku }
"#;

    fn config() -> Config {
        Config::parse(
            Source {
                path: Path::new("/global/config.yaml"),
                text: YAML,
            },
            None,
            &ParseCtx { known_skills: &[] },
        )
        .expect("the shared team-models fixture parses")
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// `dev › team` built through the constructor the plan mandates.
    fn team(cfg: &Config) -> Bound {
        let profile = Arc::clone(cfg.catalog.profile("dev").expect("dev"));
        let idx = profile
            .agents
            .iter()
            .position(|a| a.main.name == "team")
            .expect("team");
        Bound::new(profile, idx)
    }

    fn model<'a>(cfg: &'a Config, alias: &str) -> &'a ResolvedModel {
        cfg.registry.resolve(alias).expect("fixture alias")
    }

    /// The alias a delegation to `sub` would run on.
    fn effective(bound: &Bound, sub: &str, orchestrator: &ResolvedModel) -> String {
        let s = bound
            .agent()
            .subagents
            .iter()
            .find(|s| s.member.name == sub)
            .expect("fixture subagent");
        bound.subagent_model(s, orchestrator).alias.clone()
    }

    /// 1. A freshly built `Bound` has no overrides.
    #[test]
    fn bound_new_has_no_overrides() {
        let cfg = config();
        assert!(team(&cfg).selection().subagent_models.is_empty());
    }

    /// 2. Valid entries apply, with no rejections, and show in the projection.
    #[test]
    fn with_subagent_models_applies_valid_overrides() {
        let cfg = config();
        let (bound, rejected) =
            team(&cfg).with_subagent_models(&cfg.registry, &map(&[("tester", "opus")]));
        assert!(rejected.is_empty(), "got: {rejected:?}");
        assert_eq!(
            bound.selection().subagent_models,
            map(&[("tester", "opus")])
        );
    }

    /// 3. The map is REPLACED, never merged.
    #[test]
    fn with_subagent_models_replaces_not_merges() {
        let cfg = config();
        let (first, _) =
            team(&cfg).with_subagent_models(&cfg.registry, &map(&[("tester", "opus")]));
        let (second, rejected) =
            first.with_subagent_models(&cfg.registry, &map(&[("reviewer", "haiku")]));
        assert!(rejected.is_empty(), "got: {rejected:?}");
        assert_eq!(
            second.selection().subagent_models,
            map(&[("reviewer", "haiku")]),
            "the previous `tester` entry must be gone"
        );
    }

    /// 4. A key naming no subagent of the agent is rejected and not stored.
    #[test]
    fn with_subagent_models_rejects_unknown_subagent() {
        let cfg = config();
        let (bound, rejected) =
            team(&cfg).with_subagent_models(&cfg.registry, &map(&[("ghost", "opus")]));
        assert_eq!(
            rejected,
            vec![OverrideRejection::UnknownSubagent {
                subagent: "ghost".into()
            }]
        );
        assert!(bound.selection().subagent_models.is_empty());
    }

    /// 5. An alias outside the registry is rejected and not stored.
    #[test]
    fn with_subagent_models_rejects_unknown_alias() {
        let cfg = config();
        let (bound, rejected) =
            team(&cfg).with_subagent_models(&cfg.registry, &map(&[("tester", "nope")]));
        assert_eq!(
            rejected,
            vec![OverrideRejection::UnknownAlias {
                subagent: "tester".into(),
                alias: "nope".into()
            }]
        );
        assert!(bound.selection().subagent_models.is_empty());
    }

    /// 6. Valid entries still apply when others are rejected.
    #[test]
    fn with_subagent_models_keeps_valid_entries_alongside_rejections() {
        let cfg = config();
        let (bound, rejected) = team(&cfg).with_subagent_models(
            &cfg.registry,
            &map(&[("ghost", "opus"), ("tester", "opus")]),
        );
        assert_eq!(
            rejected,
            vec![OverrideRejection::UnknownSubagent {
                subagent: "ghost".into()
            }],
            "exactly one rejection: the ghost"
        );
        assert_eq!(
            bound.selection().subagent_models,
            map(&[("tester", "opus")])
        );
    }

    /// 7. I10: override > inherited (rides the orchestrator's EFFECTIVE model,
    ///    explicit-equal included) > configured.
    #[test]
    fn subagent_model_precedence_override_inherited_configured() {
        let cfg = config();
        let opus = model(&cfg, "opus");
        let sonnet = model(&cfg, "sonnet");

        // Orchestrator on opus, no overrides.
        let bound = team(&cfg);
        assert_eq!(
            effective(&bound, "reviewer", opus),
            "opus",
            "inherited follows"
        );
        assert_eq!(
            effective(&bound, "twin", opus),
            "opus",
            "explicit-equal is inherited (Z4)"
        );
        assert_eq!(
            effective(&bound, "tester", opus),
            "haiku",
            "explicit stays configured"
        );

        // With overrides on tester and reviewer.
        let (bound, rejected) = team(&cfg).with_subagent_models(
            &cfg.registry,
            &map(&[("tester", "sonnet"), ("reviewer", "haiku")]),
        );
        assert!(rejected.is_empty(), "got: {rejected:?}");
        assert_eq!(
            effective(&bound, "tester", opus),
            "sonnet",
            "override beats configured"
        );
        assert_eq!(
            effective(&bound, "reviewer", opus),
            "haiku",
            "override beats inherited"
        );
        assert_eq!(
            effective(&bound, "twin", opus),
            "opus",
            "no override: still inherited"
        );

        // Orchestrator on its configured model.
        let bound = team(&cfg);
        assert_eq!(effective(&bound, "reviewer", sonnet), "sonnet");
    }

    /// 8. `Catalog::resolve` takes NAMES and returns a `Bound` with no overrides.
    #[test]
    fn catalog_resolve_takes_names_and_returns_no_overrides() {
        let cfg = config();
        let bound = cfg
            .catalog
            .resolve("dev", "team")
            .expect("both names exist");
        assert_eq!(bound.agent().main.name, "team");
        assert!(bound.selection().subagent_models.is_empty());
        assert!(
            cfg.catalog.resolve("dev", "ghost").is_none(),
            "unknown agent"
        );
    }

    /// 9. `Selection` JSON: the map is omitted when empty and defaults to empty
    ///    when absent.
    #[test]
    fn selection_json_omits_empty_subagent_models() {
        let plain = Selection {
            profile: "dev".into(),
            agent: "team".into(),
            subagent_models: BTreeMap::new(),
        };
        assert_eq!(
            serde_json::to_string(&plain).unwrap(),
            r#"{"profile":"dev","agent":"team"}"#
        );

        let with = Selection {
            subagent_models: map(&[("tester", "opus")]),
            ..plain
        };
        assert_eq!(
            serde_json::to_string(&with).unwrap(),
            r#"{"profile":"dev","agent":"team","subagent_models":{"tester":"opus"}}"#
        );

        let back: Selection =
            serde_json::from_str(r#"{"profile":"dev","agent":"team"}"#).expect("parses");
        assert!(back.subagent_models.is_empty(), "absent key = empty map");
    }
}
