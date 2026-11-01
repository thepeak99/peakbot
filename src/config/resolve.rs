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
    AgentDef, ConfigFile, Env, General, ProfileDef, ProfileEntry, RepoFile, SubagentDef, TabColor,
    VectorDbConfig,
};
use super::migrate::is_legacy;
use super::model_registry::canonical_alias;
use super::named::{NamedFailure, named_failure};
use super::remote::{self, RemoteProfile};
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

/// One catalogue entry: a local profile (bindable to a session) or a
/// remote profile (relayed, never bound locally) — design §4.2.
#[derive(Debug)]
pub enum CatalogEntry {
    Local(Arc<ResolvedProfile>),
    Remote(Arc<RemoteProfile>),
}

/// The selectable profiles. ONE ordered list (I1): repo entries first, then
/// global in file order, local and remote mixed. CE1: at least one Local.
#[derive(Debug)]
pub struct Catalog {
    entries: Vec<CatalogEntry>,
}

impl Catalog {
    /// The entries in catalog order (repo first, then global file order).
    pub fn entries(&self) -> impl Iterator<Item = &CatalogEntry> {
        self.entries.iter()
    }

    /// The first agent of the first LOCAL profile — a remote entry listed
    /// first never becomes the default (CE1 guarantees a local exists).
    pub fn default_bound(&self) -> Bound {
        let profile = self
            .entries
            .iter()
            .find_map(local_entry)
            .expect("CE1: at least one local profile");
        Bound::new(Arc::clone(profile), 0)
    }

    /// LOCAL only: remotes have no local agents and are never a `Bound`.
    pub fn profile(&self, name: &str) -> Option<&Arc<ResolvedProfile>> {
        self.iter().find(|p| p.name == name)
    }

    /// The remote entry with this name, if any.
    pub fn remote(&self, name: &str) -> Option<&Arc<RemoteProfile>> {
        self.entries
            .iter()
            .find(|e| matches!(e, CatalogEntry::Remote(r) if r.name == name))
            .and_then(|e| match e {
                CatalogEntry::Remote(r) => Some(r),
                _ => None,
            })
    }

    /// LOCAL only, in catalog order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<ResolvedProfile>> {
        self.entries.iter().filter_map(local_entry)
    }

    /// `(profile, agent)` → a Bound with NO overrides; `None` if either name
    /// is unknown — never a silent default (§5.3). LOCAL only.
    pub fn resolve(&self, profile: &str, agent: &str) -> Option<Bound> {
        let profile = self.profile(profile)?;
        let agent_index = profile.agents.iter().position(|a| a.main.name == agent)?;
        Some(Bound::new(Arc::clone(profile), agent_index))
    }

    /// The session's starting `Bound`: the pinned profile's first agent
    /// when pinned, else the default (first LOCAL profile, first agent). A
    /// pin names only GLOBAL local profiles — repo files are ignored under
    /// a pin (§4.3 PIN), and a remote profile can't be pinned at all.
    pub fn start_bound(&self, pin: Option<&str>) -> Result<Bound, String> {
        let Some(pin) = pin else {
            return Ok(self.default_bound());
        };
        if self.remote(pin).is_some() {
            return Err(format!("--profile can't pin a remote profile ('{pin}')"));
        }
        let profile = self
            .iter()
            .find(|p| p.name == pin && matches!(p.origin, Origin::Global))
            .cloned();
        let Some(profile) = profile else {
            let configured: Vec<&str> = self
                .iter()
                .filter(|p| matches!(p.origin, Origin::Global))
                .map(|p| p.name.as_str())
                .collect();
            return Err(format!(
                "--profile '{pin}': unknown profile. Configured profiles: {}",
                configured.join(", ")
            ));
        };
        Ok(Bound::new(profile, 0))
    }

    /// The wire projection for Settings and AppState (§8.7), in catalog
    /// order (repo first). `inherited` is derived, not stored (Z4). Remote
    /// entries carry `origin: "remote"`, `agents: []` (the relay fills them
    /// for the active remote tab, §6.3) and a resolved colour — and never
    /// the token, URL or fingerprint.
    pub fn infos(&self) -> Vec<ProfileInfo> {
        self.entries
            .iter()
            .map(|e| match e {
                CatalogEntry::Local(p) => profile_info(p),
                CatalogEntry::Remote(r) => ProfileInfo {
                    name: r.name.clone(),
                    origin: "remote".to_string(),
                    overrides_global: false,
                    agents: Vec::new(),
                    color: r.color,
                },
            })
            .collect()
    }
}

fn local_entry(e: &CatalogEntry) -> Option<&Arc<ResolvedProfile>> {
    match e {
        CatalogEntry::Local(p) => Some(p),
        CatalogEntry::Remote(_) => None,
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
    /// `ui.color`, else derived from the name (RP8: always set).
    pub color: TabColor,
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
    /// Always set (RP8): `ui.color`, else derived from the name.
    pub color: TabColor,
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

/// A conversation's `(profile, agent)` plus its subagent model overrides.
/// I2: a session holds exactly one.
/// I9: every key of `subagent_models` names a subagent of `agent()`; only
/// `with_subagent_models` writes the map (the field is private, so no
/// struct literal outside this module can break I9).
#[derive(Debug, Clone)]
pub struct Bound {
    pub profile: Arc<ResolvedProfile>,
    pub agent_index: usize,
    subagent_models: BTreeMap<String, ResolvedModel>,
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

    /// Whether two bounds name the same `(profile, agent)` pair — the
    /// subagent model overrides are NOT part of the pair (cd-rebind).
    pub fn same_pair(&self, other: &Bound) -> bool {
        self.profile.name == other.profile.name && self.agent().main.name == other.agent().main.name
    }

    /// The wire projection, with the override map as aliases.
    pub fn selection(&self) -> Selection {
        Selection {
            profile: self.profile.name.clone(),
            agent: self.agent().main.name.clone(),
            subagent_models: self
                .subagent_models
                .iter()
                .map(|(subagent, model)| (subagent.clone(), model.alias.clone()))
                .collect(),
        }
    }

    /// A `Bound` with NO overrides: an agent pick is a fresh setup, so the
    /// clearing is structural, not a rule (I12).
    pub fn new(profile: Arc<ResolvedProfile>, agent_index: usize) -> Bound {
        Bound {
            profile,
            agent_index,
            subagent_models: BTreeMap::new(),
        }
    }

    /// REPLACES (never merges) the override map. Valid entries apply even
    /// when others are rejected; rejections come back in subagent-name
    /// order (plan §5.1).
    pub fn with_subagent_models(
        self,
        registry: &ModelRegistry,
        wanted: &BTreeMap<String, String>,
    ) -> (Bound, Vec<OverrideRejection>) {
        let mut subagent_models = BTreeMap::new();
        let mut rejected = Vec::new();
        for (subagent, alias) in wanted {
            match self
                .agent()
                .subagents
                .iter()
                .find(|s| &s.member.name == subagent)
            {
                None => rejected.push(OverrideRejection::UnknownSubagent {
                    subagent: subagent.clone(),
                }),
                Some(_) => match registry.resolve(alias) {
                    None => rejected.push(OverrideRejection::UnknownAlias {
                        subagent: subagent.clone(),
                        alias: alias.clone(),
                    }),
                    Some(model) => {
                        subagent_models.insert(subagent.clone(), model.clone());
                    }
                },
            }
        }
        (
            Bound {
                subagent_models,
                ..self
            },
            rejected,
        )
    }

    /// I10 — the effective subagent model, the ONE rule delegation uses:
    /// override, else the orchestrator's effective model when the sub is
    /// *inherited* (alias == the agent's, explicit-equal included, Z4),
    /// else the sub's configured model. `sub` must be one of
    /// `self.agent().subagents`.
    pub fn subagent_model<'a>(
        &'a self,
        sub: &'a Subagent,
        orchestrator: &'a ResolvedModel,
    ) -> &'a ResolvedModel {
        if let Some(model) = self.subagent_models.get(&sub.member.name) {
            return model;
        }
        if sub.member.model.alias == self.agent().main.model.alias {
            orchestrator
        } else {
            &sub.member.model
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
        let mut entries: Vec<CatalogEntry> = Vec::new();
        // (name, origin, target profile) of the parsed remotes, for RP4.
        let mut remotes: Vec<(String, String, String)> = Vec::new();
        if let (Some(repo_file), Some(src)) = (&repo_file, &repo) {
            // `<root>/.peakbot/config.yaml` → `<root>`.
            let root = src
                .path
                .parent()
                .and_then(Path::parent)
                .unwrap_or(Path::new(""));
            for (name, entry) in repo_file.profiles.iter() {
                match entry {
                    // RP7: a `remote` block is a global-file-only kind.
                    ProfileEntry::Remote(_) => {
                        repo_sink.push(
                            vec![key("profiles"), key(name)],
                            "remote profiles are only allowed in the global config.yaml",
                        );
                    }
                    // A repo profile can't replace a global remote definition.
                    ProfileEntry::Local(def)
                        if file
                            .profiles
                            .get(name)
                            .is_some_and(|g| matches!(g, ProfileEntry::Remote(_))) =>
                    {
                        repo_sink.push(
                            vec![key("profiles"), key(name)],
                            format!("'{name}' would replace the remote profile '{name}' — rename one of them"),
                        );
                    }
                    ProfileEntry::Local(def) => {
                        let origin = Origin::Repo {
                            overrides_global: file.profiles.get(name).is_some(),
                        };
                        if let Some(profile) =
                            resolver.profile(&mut repo_sink, name, def, origin, Some(root))
                        {
                            entries.push(CatalogEntry::Local(Arc::new(profile)));
                        }
                    }
                }
            }
        }
        for (name, entry) in file.profiles.iter() {
            let shadowed = repo_file
                .as_ref()
                .is_some_and(|r| r.profiles.get(name).is_some());
            if shadowed {
                continue;
            }
            match entry {
                ProfileEntry::Local(def) => {
                    if let Some(profile) =
                        resolver.profile(&mut global_sink, name, def, Origin::Global, None)
                    {
                        entries.push(CatalogEntry::Local(Arc::new(profile)));
                    }
                }
                ProfileEntry::Remote(rdef) => {
                    // A remote name is a URL path segment (`/r/<name>/…`);
                    // `.`/`..` would collapse in browsers and defeat the
                    // `/r/` re-rooting (design §7), so they are a parse error.
                    if matches!(name.as_str(), "." | "..") {
                        global_sink.push(
                            vec![key("profiles"), key(name)],
                            format!("'{name}' isn't a valid remote profile name"),
                        );
                        continue;
                    }
                    match remote::parse_remote(name, &rdef.remote, rdef.ui.color) {
                        Ok(remote_profile) => {
                            remotes.push((
                                name.clone(),
                                remote_profile.endpoint.origin_without_slash(),
                                remote_profile.target.profile.clone(),
                            ));
                            entries.push(CatalogEntry::Remote(Arc::new(remote_profile)));
                        }
                        // §4.4: one error per field, keyed at the block.
                        Err(field_errors) => {
                            for e in field_errors {
                                global_sink.push(
                                    vec![key("profiles"), key(name), key("remote"), key(e.field)],
                                    e.message,
                                );
                            }
                        }
                    }
                }
            }
        }
        // RP4: (origin, target profile) is unique across remote profiles.
        for (i, later) in remotes.iter().enumerate() {
            for earlier in &remotes[..i] {
                if later.1 == earlier.1 && later.2 == earlier.2 {
                    global_sink.push(
                        vec![key("profiles"), key(&later.0)],
                        format!(
                            "'{}' and '{}' both point at profile '{}' on {}",
                            later.0, earlier.0, later.2, later.1
                        ),
                    );
                }
            }
        }
        // CE1 (desktop MVP): the catalog needs a local profile to bind a
        // session — judged on the FILE, so a failing local profile reports
        // its own errors only, not a second "no local" on top.
        if !file.profiles.is_empty()
            && !file
                .profiles
                .iter()
                .any(|(_, e)| matches!(e, ProfileEntry::Local(_)))
        {
            global_sink.push(vec![key("profiles")], CE1_MESSAGE);
        }
        let locals: Vec<&ResolvedProfile> = entries
            .iter()
            .filter_map(local_entry)
            .map(|p| p.as_ref())
            .collect();
        check_shared_vector_dbs(&locals, &mut global_sink, &mut repo_sink);
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
            catalog: Catalog { entries },
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
/// CE1 (desktop MVP): a profiles map with no LOCAL profile at all.
const CE1_MESSAGE: &str = "declare at least one local profile (one with agents)";

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
fn check_shared_vector_dbs(profiles: &[&ResolvedProfile], global: &mut Sink, repo: &mut Sink) {
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
            color: def.ui.color.unwrap_or_else(|| TabColor::derived(name)),
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
        color: p.color,
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
// Per-member subagent model overrides live INSIDE `Bound` (I9, private map +
// validating constructor) and the effective-model rule is the one place that
// decides what a delegation runs on (I10).
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

// ─────────────────────────────────────────────────────────────────────────────
// cd-rebind RED tests: `Bound::same_pair` — the pair test the `/cd` rebind
// uses to decide between keeping the session's active model (same pair) and
// starting on the new agent's configured model (changed pair). Written
// before the implementation exists; must fail on the `todo!` stub.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod cd_rebind_tests {
    use std::collections::BTreeMap;

    use super::*;

    /// The plan's shared Rust fixture (§9): `dev/team` (with subagents), a
    /// subagent-free `dev/solo`, and `research/main`.
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

    fn bound(cfg: &Config, profile: &str, agent: &str) -> Bound {
        cfg.catalog
            .resolve(profile, agent)
            .unwrap_or_else(|| panic!("fixture bound {profile}/{agent} must exist"))
    }

    /// cd-rebind: the SAME profile and agent with different subagent model
    /// overrides is the same pair — the overrides are not part of the pair.
    #[test]
    fn same_pair_true_for_same_agent_with_different_overrides() {
        let cfg = config();
        let plain = bound(&cfg, "dev", "team");
        let (overridden, rejected) = plain
            .clone()
            .with_subagent_models(&cfg.registry, &map(&[("tester", "opus")]));
        assert!(rejected.is_empty(), "the override is valid: {rejected:?}");
        assert!(
            plain.selection().subagent_models.is_empty()
                && !overridden.selection().subagent_models.is_empty(),
            "fixture sanity: the two bounds really differ in overrides"
        );
        assert!(
            plain.same_pair(&overridden),
            "the subagent model overrides are not part of the pair"
        );
        assert!(
            overridden.same_pair(&plain),
            "…and the pair test is symmetric"
        );
    }

    /// cd-rebind: a DIFFERENT agent (even in the same profile) is a
    /// different pair.
    #[test]
    fn same_pair_false_for_different_agent() {
        let cfg = config();
        let team = bound(&cfg, "dev", "team");
        let solo = bound(&cfg, "dev", "solo");
        assert!(
            !team.same_pair(&solo),
            "a different agent is a different pair"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab colour (remote-profiles design T1, §4.2/§4.3/RP8): `ProfileUi.color`,
// the resolved `ProfileInfo.color` (ui.color, else `TabColor::derived(name)`),
// and the field-path error shape for an unknown colour name.
// Written BEFORE the fields exist — compile-RED until T1 lands.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tab_color_resolve_tests {
    use super::*;
    use crate::config::file::TabColor;

    /// `dev` has a CONFIGURED colour that differs from its derived one
    /// (derived("dev") = Teal, so Pink proves the config wins); `work` and
    /// `home` have none, so they fall back to the name-derived colours
    /// (Red and Purple per the golden table in `file.rs`).
    const GLOBAL_YAML: &str = r#"
providers:
  - name: patchnotes
    type: openai
    base_url: https://ai.patchnotes.com/v1
    api_key: pn-test
    models:
      - { name: anthropic/claude-sonnet-4.5, alias: sonnet, max_tokens: 8192 }
profiles:
  dev:
    ui: { color: pink }
    agents:
      team: { model: sonnet }
  work:
    agents:
      solo: { model: sonnet }
  home:
    agents:
      solo: { model: sonnet }
"#;

    /// One repo profile with a configured colour, one without.
    const REPO_YAML: &str = r#"
profiles:
  site:
    ui: { color: blue }
    agents:
      solo: { model: sonnet }
  ops:
    agents:
      solo: { model: sonnet }
"#;

    fn ctx() -> ParseCtx<'static> {
        ParseCtx { known_skills: &[] }
    }

    fn parse(global: &str, repo: Option<&str>) -> Result<Config, ConfigErrors> {
        Config::parse(
            Source {
                path: Path::new("/global/config.yaml"),
                text: global,
            },
            repo.map(|text| Source {
                path: Path::new("/repo/.peakbot/config.yaml"),
                text,
            }),
            &ctx(),
        )
    }

    fn info(cfg: &Config, name: &str) -> ProfileInfo {
        cfg.catalog
            .infos()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("catalog must contain '{name}'"))
    }

    #[test]
    fn ui_color_parses_and_wins_over_the_derived_one() {
        let cfg = parse(GLOBAL_YAML, None).expect("`ui.color: pink` must parse");
        // derived("dev") is Teal — Pink can only come from the config.
        assert_eq!(info(&cfg, "dev").color, TabColor::Pink);
    }

    #[test]
    fn ui_color_absent_is_derived_deterministically_from_the_name() {
        let cfg = parse(GLOBAL_YAML, None).expect("fixture must parse");
        assert_eq!(info(&cfg, "work").color, TabColor::derived("work"));
        assert_eq!(info(&cfg, "home").color, TabColor::derived("home"));
        assert_eq!(info(&cfg, "work").color, TabColor::Red);
        assert_eq!(info(&cfg, "home").color, TabColor::Purple);

        // Same file, second parse: the derived colours are stable.
        let again = parse(GLOBAL_YAML, None).expect("fixture must parse again");
        assert_eq!(info(&cfg, "work").color, info(&again, "work").color);
        assert_eq!(info(&cfg, "home").color, info(&again, "home").color);

        // Different names are expected to land on different colours.
        assert_ne!(info(&cfg, "work").color, info(&cfg, "home").color);
    }

    #[test]
    fn unknown_colour_name_is_a_field_path_keyed_parse_error() {
        let bad = GLOBAL_YAML.replace("color: pink", "color: magenta");
        let err = parse(&bad, None).expect_err("an unknown colour name must not parse");
        let errors = err.0;
        assert_eq!(
            errors.len(),
            1,
            "one error for the one bad colour: {errors:?}"
        );
        let e = &errors[0];
        assert_eq!(e.file, ConfigFileKind::Global);
        assert_eq!(
            e.path,
            vec![
                PathSeg::Key("profiles".into()),
                PathSeg::Key("dev".into()),
                PathSeg::Key("ui".into()),
                PathSeg::Key("color".into()),
            ],
            "the error points at profiles.dev.ui.color"
        );
        assert!(
            e.message.contains("magenta"),
            "the message must name the bad value: {}",
            e.message
        );
    }

    /// RP8: `ProfileInfo.color` is always set — the SPA never derives a
    /// colour, so every catalogue entry carries a resolved one.
    #[test]
    fn every_profile_info_carries_a_resolved_colour() {
        let cfg = parse(GLOBAL_YAML, None).expect("fixture must parse");
        for p in cfg.catalog.infos() {
            let json = serde_json::to_value(&p).expect("ProfileInfo serialises");
            assert!(
                json["color"].is_string() && !json["color"].as_str().unwrap().is_empty(),
                "catalogue entry '{}' must carry a colour: {json}",
                p.name
            );
        }
    }

    /// The catalogue projection is the wire shape the SPA reads
    /// (`AppState.catalog`): `name` plus the lowercase `color`, round-trip.
    #[test]
    fn profile_info_serialises_name_and_colour_and_round_trips() {
        let cfg = parse(GLOBAL_YAML, None).expect("fixture must parse");
        let p = info(&cfg, "dev");
        let json = serde_json::to_string(&p).expect("ProfileInfo serialises");
        assert!(json.contains(r#""name":"dev""#), "json = {json}");
        assert!(json.contains(r#""color":"pink""#), "json = {json}");

        let back: ProfileInfo = serde_json::from_str(&json).expect("ProfileInfo deserialises");
        assert_eq!(back, p, "the wire projection round-trips");
        assert_eq!(back.color, TabColor::Pink);
    }

    /// Repo profiles are local profiles: `ui.color` resolves exactly like
    /// the global ones (configured wins, absent derives from the name).
    #[test]
    fn repo_profile_colour_resolves_like_global() {
        let cfg = parse(GLOBAL_YAML, Some(REPO_YAML)).expect("fixture must parse");
        assert_eq!(info(&cfg, "site").color, TabColor::Blue);
        assert_eq!(info(&cfg, "ops").color, TabColor::derived("ops"));
        assert_eq!(info(&cfg, "ops").color, TabColor::Green);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// T3 RED tests (remote-profiles design, task 3): the `remote` profile kind.
// Written BEFORE the types exist, against the locked spec: §4.1 (the YAML
// that must parse), §4.2 (the resolved `RemoteProfile`/`Endpoint`/`Trust`/
// `CaFingerprint`/`RemoteToken`/`RemoteTarget` and the `CatalogEntry`
// catalogue), §4.4 (every parse error — one shape, keyed by field path),
// RP1–RP8 and CE1. They must fail to compile until `config::remote` and
// the `CatalogEntry` catalogue land.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod t3_remote_profile_tests {
    use std::path::Path;

    use crate::config::remote::{Endpoint, Trust};

    use super::*;

    /// The fixture CA pin — 64 lowercase hex, the only shape besides
    /// `system` that `ca` accepts (RP2).
    const CA_PIN: &str = "sha256:3f9a0123456789abcdef0123456789abcdef0123456789abcdef0123456789ab";

    /// A fully valid `remote:` block (the lines under the profile name).
    const VALID_BLOCK: &str = "    remote:\n      url: https://10.2.0.48:7823\n      token: pito\n      ca: sha256:3f9a0123456789abcdef0123456789abcdef0123456789abcdef0123456789ab\n      profile: dev";

    /// The §4.1 example, verbatim (plus the provider the parse needs): a
    /// local `dev` with a configured colour, then one remote profile per
    /// trust/plaintext case.
    const GLOBAL_YAML: &str = r#"
providers:
  - name: patchnotes
    type: openai
    base_url: https://ai.patchnotes.com/v1
    api_key: pn-test
    models:
      - { name: anthropic/claude-sonnet-4.5, alias: sonnet, max_tokens: 8192 }
profiles:
  dev:                                   # local profile — unchanged, may gain ui.color
    ui: { color: teal }
    agents:
      team: { model: sonnet }
  home-dev:                              # REMOTE profile: `remote` (+ optional ui.color) and nothing else
    remote:
      url: https://10.2.0.48:7823
      token: pito
      ca: sha256:3f9a0123456789abcdef0123456789abcdef0123456789abcdef0123456789ab
      profile: dev
    ui: { color: purple }
  vps-web:
    remote:
      url: https://bot.example.ts.net
      token: 9c1e
      ca: system
      profile: web
  sandbox:
    remote: { url: http://127.0.0.1:7824, token: s3cret, profile: dev }
"#;

    fn ctx() -> ParseCtx<'static> {
        ParseCtx { known_skills: &[] }
    }

    fn parse(global: &str, repo: Option<&str>) -> Result<Config, ConfigErrors> {
        Config::parse(
            Source {
                path: Path::new("/global/config.yaml"),
                text: global,
            },
            repo.map(|text| Source {
                path: Path::new("/repo/.peakbot/config.yaml"),
                text,
            }),
            &ctx(),
        )
    }

    /// One global config: the remote entry spelled by `entry_body` (the
    /// indented lines under the profile name) plus a minimal local profile,
    /// so CE1 has a local to stand on.
    fn with_remote_entry(entry_name: &str, entry_body: &str) -> String {
        format!(
            "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - {{ name: anthropic/claude-sonnet-4.5, alias: sonnet, max_tokens: 8192 }}\nprofiles:\n  {entry_name}:\n{entry_body}  local:\n    agents:\n      a:\n        model: sonnet\n"
        )
    }

    /// `VALID_BLOCK` with one `field:` line's value replaced.
    fn block_with(field: &str, value: &str) -> String {
        let prefix = format!("      {field}:");
        let mut out = String::new();
        for line in VALID_BLOCK.lines() {
            if line.starts_with(&prefix) {
                out.push_str(&format!("{prefix} {value}\n"));
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }

    /// `VALID_BLOCK` with one line dropped.
    fn block_without(field: &str) -> String {
        let prefix = format!("      {field}:");
        VALID_BLOCK
            .lines()
            .filter(|line| !line.starts_with(&prefix))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    /// A plain-http block: `ca` is FORBIDDEN on http (row 7), so it has none.
    fn http_block(url: &str) -> String {
        format!("    remote:\n      url: {url}\n      token: pito\n      profile: dev\n")
    }

    /// `VALID_BLOCK` with one line appended.
    fn block_plus(extra_line: &str) -> String {
        format!("{VALID_BLOCK}\n{extra_line}\n")
    }

    /// A global config with two remote entries (in order) plus a local one.
    fn two_remote_entries(first: &str, second: &str) -> String {
        format!(
            "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - {{ name: anthropic/claude-sonnet-4.5, alias: sonnet, max_tokens: 8192 }}\nprofiles:\n  home-dev:\n{first}\n  home-dev-2:\n{second}\n  local:\n    agents:\n      a:\n        model: sonnet\n"
        )
    }

    fn entry_path(name: &str) -> Vec<PathSeg> {
        vec![key("profiles"), key(name)]
    }

    fn remote_block_path(name: &str) -> Vec<PathSeg> {
        vec![key("profiles"), key(name), key("remote")]
    }

    fn remote_path(name: &str, field: &str) -> Vec<PathSeg> {
        vec![key("profiles"), key(name), key("remote"), key(field)]
    }

    /// Exactly one error, at `path`; returns it for the message assert.
    fn one_err<'a>(err: &'a ConfigErrors, path: &[PathSeg], label: &str) -> &'a ConfigError {
        assert_eq!(err.0.len(), 1, "exactly one error for {label}: {err:?}");
        let e = &err.0[0];
        assert_eq!(&e.path, path, "error path for {label}: {e:?}");
        e
    }

    /// §4.4 row 1's message, with the offending key.
    fn excl_msg(key: &str) -> String {
        format!(
            "a remote profile has only `remote` and `ui.color` — `{key}` belongs to the profile on the remote server"
        )
    }

    /// §4.1 verbatim → §4.2 types: the three remote profiles resolve to the
    /// doc's `RemoteProfile` (parsed endpoint, trust, target, colour) and
    /// the local profile is untouched.
    #[test]
    fn section_4_1_yaml_resolves_to_the_doc_types() {
        let cfg = parse(GLOBAL_YAML, None).expect("the §4.1 example must parse");

        let home = cfg.catalog.remote("home-dev").expect("home-dev is remote");
        assert_eq!(home.name, "home-dev");
        match &home.endpoint {
            Endpoint::Tls { origin, trust } => {
                assert_eq!(origin.as_str(), "https://10.2.0.48:7823/");
                assert!(
                    matches!(trust, Trust::PinnedCa(f) if f.to_string() == CA_PIN),
                    "the pin is the 64-hex fingerprint: {trust:?}"
                );
            }
            other => panic!("home-dev is https — expected Tls, got {other:?}"),
        }
        assert_eq!(home.target.profile, "dev");
        assert_eq!(home.target.agent, None, "no agent pin in the fixture");
        assert_eq!(home.color, TabColor::Purple, "the configured colour wins");

        let vps = cfg.catalog.remote("vps-web").expect("vps-web is remote");
        match &vps.endpoint {
            Endpoint::Tls { origin, trust } => {
                assert_eq!(origin.as_str(), "https://bot.example.ts.net/");
                assert!(
                    matches!(trust, Trust::System),
                    "ca: system = explicit system trust"
                );
            }
            other => panic!("vps-web is https — expected Tls, got {other:?}"),
        }
        assert_eq!(vps.target.profile, "web");
        assert_eq!(
            vps.color,
            TabColor::derived("vps-web"),
            "no ui.color → derived from the name"
        );

        let sandbox = cfg.catalog.remote("sandbox").expect("sandbox is remote");
        match &sandbox.endpoint {
            Endpoint::LoopbackPlain { origin } => {
                assert_eq!(origin.as_str(), "http://127.0.0.1:7824/");
            }
            other => panic!("sandbox is loopback http — expected LoopbackPlain, got {other:?}"),
        }
        assert_eq!(sandbox.target.profile, "dev");
        assert_eq!(sandbox.color, TabColor::derived("sandbox"));

        // The local profile is unchanged: still local, still teal, one agent.
        let dev = cfg
            .catalog
            .profile("dev")
            .expect("dev stays in the catalog");
        assert_eq!(dev.color, TabColor::Teal);
        assert_eq!(dev.agents.len(), 1);
        assert!(
            cfg.catalog.remote("dev").is_none(),
            "dev is not a remote profile"
        );
    }

    /// `remote.agent` is the OPTIONAL agent pin: absent = `None`, present =
    /// carried into the target.
    #[test]
    fn remote_agent_pin_lands_in_the_target() {
        let yaml = with_remote_entry("home-dev", &block_plus("      agent: team"));
        let cfg = parse(&yaml, None).expect("an agent pin must parse");
        let home = cfg.catalog.remote("home-dev").expect("home-dev");
        assert_eq!(home.target.profile, "dev");
        assert_eq!(home.target.agent.as_deref(), Some("team"));
    }

    /// §7: the remote token is redacted — `RemoteToken`'s Debug AND Display
    /// are the literal `RemoteToken(***)`, and a `RemoteProfile`'s Debug
    /// carries no trace of the value.
    #[test]
    fn remote_token_debug_and_display_redact() {
        let cfg = parse(GLOBAL_YAML, None).expect("fixture must parse");
        let home = cfg.catalog.remote("home-dev").expect("home-dev");
        assert_eq!(home.token.to_string(), "RemoteToken(***)");
        assert_eq!(format!("{:?}", home.token), "RemoteToken(***)");
        let debug = format!("{home:?}");
        assert!(!debug.contains("pito"), "the token must not print: {debug}");

        let debug = format!("{:?}", cfg.catalog.remote("sandbox").expect("sandbox"));
        assert!(
            !debug.contains("s3cret"),
            "the token must not print: {debug}"
        );
    }

    /// §4.4 row 1: `remote` plus ANY local field is a parse error keyed at
    /// the profile, naming the offending key. `ui.color` is the only allowed
    /// `ui` key (it is in GLOBAL_YAML).
    #[test]
    fn remote_plus_a_local_field_is_mutually_exclusive() {
        let cases: &[(&str, &str)] = &[
            ("agents", "    agents:\n      a:\n        model: sonnet\n"),
            ("tools", "    tools: { disabled: [bash] }\n"),
            ("mcps", "    mcps: { only: [playwright] }\n"),
            ("fetch_url", "    fetch_url: { allow_post: true }\n"),
            ("env", "    env: { FOO: bar }\n"),
            ("memory", "    memory: { enabled: false }\n"),
            (
                "vector_db",
                "    vector_db:\n      db_path: /tmp/db\n      embeddings:\n        base_url: https://e.example/v1\n        model: m\n        dimensions: 8\n",
            ),
            ("ui.tabs", "    ui:\n      tabs: { disabled: [bash] }\n"),
        ];
        for (key, body) in cases {
            let yaml = with_remote_entry("home-dev", &block_plus(body));
            let err =
                parse(&yaml, None).expect_err(&format!("`{key}` next to `remote` must not parse"));
            let e = one_err(
                &err,
                &entry_path("home-dev"),
                &format!("mutual exclusion {key}"),
            );
            assert_eq!(e.file, ConfigFileKind::Global);
            assert_eq!(
                e.message,
                excl_msg(key),
                "the message must name the offending key"
            );
        }
    }

    /// `deny_unknown_fields` at the entry: a key that is neither `remote`,
    /// `ui` nor a known LOCAL field is serde's unknown-field error, keyed at
    /// the entry (§4.4 row 1 is for the known local keys only).
    #[test]
    fn remote_entry_unknown_key_is_a_serde_unknown_field_error() {
        let yaml = with_remote_entry("home-dev", &block_plus("    bogus: 1"));
        let err = parse(&yaml, None).expect_err("an unknown entry key must not parse");
        let e = one_err(&err, &entry_path("home-dev"), "unknown entry key");
        assert!(
            e.message.starts_with("unknown field `bogus`"),
            "the serde deny_unknown_fields wording: {}",
            e.message
        );
    }

    /// `RemoteUi` is `deny_unknown_fields`: `ui.color` is the only `ui` key
    /// a remote profile has (`ui.tabs` is the exclusion row, anything else
    /// is unknown).
    #[test]
    fn remote_ui_unknown_key_is_a_serde_unknown_field_error() {
        let yaml = with_remote_entry("home-dev", &block_plus("    ui: { bogus: 1 }"));
        let err = parse(&yaml, None).expect_err("an unknown remote ui key must not parse");
        let e = one_err(
            &err,
            &[key("profiles"), key("home-dev"), key("ui")],
            "unknown remote ui key",
        );
        assert!(
            e.message.starts_with("unknown field `bogus`"),
            "the serde deny_unknown_fields wording: {}",
            e.message
        );
    }

    /// An unknown `ui.color` name on a remote profile is the same
    /// field-path-keyed parse error as on a local one.
    #[test]
    fn remote_ui_unknown_colour_is_a_field_path_keyed_error() {
        let yaml = with_remote_entry("home-dev", &block_plus("    ui: { color: magenta }"));
        let err = parse(&yaml, None).expect_err("an unknown colour must not parse");
        let e = one_err(
            &err,
            &[key("profiles"), key("home-dev"), key("ui"), key("color")],
            "unknown remote colour",
        );
        assert!(
            e.message.contains("magenta"),
            "names the bad value: {}",
            e.message
        );
    }

    /// §7: no secret reaches a Debug print or an error. The file-layer
    /// `RemoteDef`, the whole `Config`, every parse error and the wire
    /// projection (`infos()` → AppState.catalog → the SPA) carry neither the
    /// token nor the URL.
    #[test]
    fn token_never_reaches_debug_errors_or_the_wire_projection() {
        use crate::config::file::ConfigFile;

        let file: ConfigFile = serde_yaml::from_str(GLOBAL_YAML).expect("file layer parses");
        let debug = format!("{file:?}");
        for secret in ["pito", "s3cret", "9c1e"] {
            assert!(
                !debug.contains(secret),
                "file-layer Debug leaks {secret}: {debug}"
            );
        }

        let cfg = parse(GLOBAL_YAML, None).expect("fixture must parse");
        let debug = format!("{cfg:?}");
        for secret in ["pito", "s3cret", "9c1e"] {
            assert!(
                !debug.contains(secret),
                "Config Debug leaks {secret}: {debug}"
            );
        }

        let wire = serde_json::to_string(&cfg.catalog.infos()).expect("infos serialise");
        for secret in [
            "pito",
            "s3cret",
            "9c1e",
            "10.2.0.48",
            "bot.example.ts.net",
            "3f9a0123",
        ] {
            assert!(
                !wire.contains(secret),
                "the wire projection leaks {secret}: {wire}"
            );
        }

        // A URL with the token in its userinfo must not be echoed back.
        let yaml = with_remote_entry(
            "home-dev",
            &block_with("url", "https://hunter2@10.2.0.48:7823"),
        );
        let err = parse(&yaml, None).expect_err("userinfo must not parse");
        let rendered = format!("{err:?}");
        assert!(
            !rendered.contains("hunter2"),
            "the error echoes the userinfo: {rendered}"
        );

        // A bad token must not be echoed back.
        let yaml = with_remote_entry("home-dev", &block_with("token", "top;secret"));
        let err = parse(&yaml, None).expect_err("a cookie-unsafe token must not parse");
        let rendered = format!("{err:?}");
        assert!(
            !rendered.contains("top;secret"),
            "the error echoes the token: {rendered}"
        );
    }

    /// Remote entries are reload-safe (next attach): editing a remote
    /// profile's token / url / pin is NOT a boot-only change.
    #[test]
    fn editing_a_remote_profile_is_not_a_boot_only_change() {
        let before = parse(GLOBAL_YAML, None).expect("fixture must parse");
        let edited = GLOBAL_YAML
            .replace("token: pito", "token: rotated")
            .replace("https://10.2.0.48:7823", "https://10.2.0.49:7823");
        let after = parse(&edited, None).expect("the edited fixture must parse");
        assert!(
            boot_only_diff(&before, &after).is_empty(),
            "remote edits apply on the next attach, not on restart"
        );
    }

    /// `deny_unknown_fields` on the `remote:` block: unknown keys inside it
    /// are serde unknown-field errors keyed at the block that holds them.
    #[test]
    fn remote_block_unknown_field_is_a_serde_error() {
        let yaml = with_remote_entry("home-dev", &block_plus("      bogus: 1"));
        let err = parse(&yaml, None).expect_err("an unknown remote key must not parse");
        let e = one_err(&err, &remote_block_path("home-dev"), "unknown remote key");
        assert!(
            e.message.starts_with("unknown field `bogus`"),
            "the serde deny_unknown_fields wording: {}",
            e.message
        );
    }

    /// §4.4 row 2: not a URL, or a scheme outside {https, http}.
    #[test]
    fn remote_url_bad_scheme_is_rejected() {
        for (label, url) in [
            ("not a URL", "notaurl"),
            ("ftp scheme", "ftp://example.com"),
        ] {
            let yaml = with_remote_entry("home-dev", &block_with("url", url));
            let err = parse(&yaml, None).expect_err(label);
            let e = one_err(&err, &remote_path("home-dev", "url"), label);
            assert_eq!(
                e.message,
                "must be an https:// URL, e.g. https://10.0.0.5:7823"
            );
        }
    }

    /// §4.4 row 3 + RP3: `http` on a non-loopback host is refused; loopback
    /// (127.0.0.0/8, `::1`, `localhost`) is the only plaintext.
    #[test]
    fn remote_url_http_non_loopback_is_rejected() {
        for url in [
            "http://10.0.0.5:7824",
            "http://example.com",
            // Look-alikes: a prefix/suffix of a loopback name is NOT loopback.
            "http://127.0.0.1.example.com",
            "http://localhost.example.com",
            "http://0.0.0.0:7824",
        ] {
            let yaml = with_remote_entry("home-dev", &http_block(url));
            let err = parse(&yaml, None).expect_err("http off loopback must not parse");
            let e = one_err(&err, &remote_path("home-dev", "url"), url);
            assert_eq!(
                e.message,
                "remote profiles connect over HTTPS — http is allowed only for 127.0.0.1 / localhost"
            );
        }
    }

    /// RP3: the loopback hosts (127.0.0.0/8, `::1`, `localhost`) are the ONLY
    /// plaintext, and resolve to `Endpoint::LoopbackPlain`.
    #[test]
    fn remote_url_http_loopback_hosts_resolve_to_loopback_plain() {
        for url in [
            "http://127.0.0.1:7824",
            "http://127.5.6.7:7824",
            "http://localhost:7824",
            "http://[::1]:7824",
        ] {
            let yaml = with_remote_entry("home-dev", &http_block(url));
            let cfg = parse(&yaml, None).unwrap_or_else(|e| panic!("{url} must parse: {e:?}"));
            let home = cfg.catalog.remote("home-dev").expect("home-dev");
            assert!(
                matches!(home.endpoint, Endpoint::LoopbackPlain { .. }),
                "{url} → LoopbackPlain, got {:?}",
                home.endpoint
            );
        }
    }

    /// Row 4 allows the bare `/` path: a trailing slash is the same
    /// `scheme://host[:port]`, not a path.
    #[test]
    fn remote_url_trailing_slash_is_accepted() {
        let yaml = with_remote_entry("home-dev", &block_with("url", "https://10.2.0.48:7823/"));
        let cfg = parse(&yaml, None).expect("a bare `/` path is not a path");
        let home = cfg.catalog.remote("home-dev").expect("home-dev");
        match &home.endpoint {
            Endpoint::Tls { origin, .. } => assert_eq!(origin.as_str(), "https://10.2.0.48:7823/"),
            other => panic!("expected Tls, got {other:?}"),
        }
    }

    /// §4.4 row 4: `scheme://host[:port]` only — a path, query, fragment or
    /// userinfo is refused.
    #[test]
    fn remote_url_with_path_query_fragment_userinfo_is_rejected() {
        let cases = [
            ("a path", "https://10.2.0.48:7823/base"),
            ("a query", "https://10.2.0.48:7823?token=pito"),
            ("a fragment", "https://10.2.0.48:7823#ws"),
            ("userinfo", "https://pito@10.2.0.48:7823"),
        ];
        for (label, url) in cases {
            let yaml = with_remote_entry("home-dev", &block_with("url", url));
            let err = parse(&yaml, None).expect_err(label);
            let e = one_err(&err, &remote_path("home-dev", "url"), label);
            assert_eq!(
                e.message,
                "only scheme://host[:port] — put the token in `token`"
            );
        }
    }

    /// §4.4 row 5: an empty token, or one with whitespace / `;` / `,` / `"`
    /// / control chars (cookie-unsafe), is refused.
    #[test]
    fn remote_token_rejects_empty_and_unsafe_chars() {
        let bad: &[(&str, &str)] = &[
            ("empty", "\"\""),
            ("whitespace", "a b"),
            ("semicolon", "a;b"),
            ("comma", "a,b"),
            ("double quote", "\"a\\\"b\""),
            ("control char", "\"a\\u0001b\""),
        ];
        for (label, value) in bad {
            let yaml = with_remote_entry("home-dev", &block_with("token", value));
            let err = parse(&yaml, None).expect_err(&format!("token {label} must not parse"));
            let e = one_err(&err, &remote_path("home-dev", "token"), label);
            assert_eq!(
                e.message,
                "the remote's web token (as printed by its `?token=…` link)"
            );
        }
    }

    /// §4.4 row 6 + RP2: `ca` is REQUIRED on https — no pin and no `system`
    /// is a parse error (there is no "https with default trust" state).
    #[test]
    fn remote_https_missing_ca_is_rejected() {
        let yaml = with_remote_entry("home-dev", &block_without("ca"));
        let err = parse(&yaml, None).expect_err("https without a ca must not parse");
        let e = one_err(&err, &remote_path("home-dev", "ca"), "missing ca");
        assert_eq!(
            e.message,
            "pin the server's CA (`ca: sha256:…` from its pairing link) or use `ca: system` for a publicly trusted certificate"
        );
    }

    /// §4.4 row 7: `ca` is FORBIDDEN on loopback http.
    #[test]
    fn remote_ca_on_loopback_http_is_rejected() {
        // Both `ca` shapes are refused on loopback http: a pin and `system`.
        for ca in [CA_PIN, "system"] {
            let yaml = with_remote_entry(
                "home-dev",
                &format!(
                    "    remote:\n      url: http://127.0.0.1:7824\n      token: pito\n      ca: {ca}\n      profile: dev\n"
                ),
            );
            let err = parse(&yaml, None).expect_err("ca on loopback http must not parse");
            let e = one_err(&err, &remote_path("home-dev", "ca"), "ca on http");
            assert_eq!(e.message, "`ca` applies to https only");
        }
    }

    /// §4.4 row 8: `ca` is `system` or `sha256:<64 lowercase hex>` — nothing
    /// else (short, long, non-hex, upper-case, prefix-less).
    #[test]
    fn remote_ca_must_be_system_or_64_lower_hex() {
        let bad: Vec<String> = vec![
            "sha256:zz".into(),
            "\"sha256:\"".into(),
            "xyz".into(),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "a".repeat(65)),
            format!("sha256:{}", "A".repeat(64)),
        ];
        for value in &bad {
            let yaml = with_remote_entry("home-dev", &block_with("ca", value));
            let err = parse(&yaml, None).expect_err(&format!("bad ca must not parse: {value}"));
            let e = one_err(&err, &remote_path("home-dev", "ca"), "bad ca");
            assert_eq!(e.message, "expected `system` or `sha256:<64 hex chars>`");
        }
    }

    /// §4.4 row 9: `remote.profile` / `remote.agent` obey the profile name
    /// rule — the existing N1 message, re-pathed into the `remote` block.
    #[test]
    fn remote_profile_and_agent_obey_the_name_rule() {
        let yaml = with_remote_entry("home-dev", &block_with("profile", "\"\""));
        let err = parse(&yaml, None).expect_err("an empty target profile must not parse");
        let e = one_err(
            &err,
            &remote_path("home-dev", "profile"),
            "empty remote.profile",
        );
        assert_eq!(
            e.message,
            "name '' must be 1–64 characters of letters, digits, space, '_', '.', '-' with no leading/trailing space"
        );

        let long = "a".repeat(65);
        let yaml = with_remote_entry("home-dev", &block_plus(&format!("      agent: {long}")));
        let err = parse(&yaml, None).expect_err("a 65-char agent pin must not parse");
        let e = one_err(&err, &remote_path("home-dev", "agent"), "long remote.agent");
        assert_eq!(
            e.message,
            format!(
                "name '{long}' must be 1–64 characters of letters, digits, space, '_', '.', '-' with no leading/trailing space"
            )
        );
    }

    /// `remote.profile` is REQUIRED (Q1): absent = serde missing-field at
    /// the `remote` block.
    #[test]
    fn remote_missing_profile_is_a_missing_field() {
        let yaml = with_remote_entry("home-dev", &block_without("profile"));
        let err = parse(&yaml, None).expect_err("remote.profile is required");
        let e = one_err(
            &err,
            &remote_block_path("home-dev"),
            "missing remote.profile",
        );
        assert_eq!(e.message, "missing field `profile`");
    }

    /// RP4 / §4.4 row 10: two remote profiles with the same (origin,
    /// profile) are a parse error on the LATER one, naming both profiles,
    /// the target and the origin.
    #[test]
    fn duplicate_remote_target_is_an_r4_error() {
        let yaml = two_remote_entries(VALID_BLOCK, VALID_BLOCK);
        let err = parse(&yaml, None).expect_err("a duplicated (origin, profile) must not parse");
        let e = one_err(&err, &entry_path("home-dev-2"), "RP4 duplicate");
        assert_eq!(
            e.message,
            "'home-dev-2' and 'home-dev' both point at profile 'dev' on https://10.2.0.48:7823"
        );

        // Same origin, different target profiles: fine.
        let ok = two_remote_entries(VALID_BLOCK, &block_with("profile", "web"));
        parse(&ok, None).expect("different target profiles on one origin are fine");

        // Same target profile, different origins: fine.
        let ok = two_remote_entries(VALID_BLOCK, &block_with("url", "https://10.2.0.49:7823"));
        parse(&ok, None).expect("the same target profile on different origins is fine");
    }

    /// §4.4 row 11 + RP7: a `remote` block in the REPO file is a parse
    /// error — remote profiles exist only in the global file.
    #[test]
    fn repo_remote_profile_is_rejected() {
        // A name the global file does not use, so row 12 cannot also fire.
        let repo = "profiles:\n  repo-remote:\n    remote:\n      url: https://10.2.0.99:7823\n      token: pito\n      ca: sha256:3f9a0123456789abcdef0123456789abcdef0123456789abcdef0123456789ab\n      profile: dev\n";
        let err = parse(GLOBAL_YAML, Some(repo)).expect_err("a repo remote profile must not parse");
        let e = one_err(&err, &entry_path("repo-remote"), "repo remote profile");
        assert_eq!(e.file, ConfigFileKind::Repo);
        assert_eq!(
            e.message,
            "remote profiles are only allowed in the global config.yaml"
        );
    }

    /// §4.4 row 12: a repo profile with the same name as a GLOBAL REMOTE
    /// profile is a parse error — it would replace the remote definition.
    #[test]
    fn repo_profile_shadowing_a_global_remote_is_rejected() {
        let repo = "profiles:\n  home-dev:\n    agents:\n      a:\n        model: sonnet\n";
        let err =
            parse(GLOBAL_YAML, Some(repo)).expect_err("shadowing a remote profile must not parse");
        let e = one_err(&err, &entry_path("home-dev"), "repo shadows a remote");
        assert_eq!(e.file, ConfigFileKind::Repo);
        assert_eq!(
            e.message,
            "'home-dev' would replace the remote profile 'home-dev' — rename one of them"
        );
    }

    /// §4.4 row 13: `--profile` cannot pin a remote profile.
    #[test]
    fn start_bound_refuses_to_pin_a_remote_profile() {
        let cfg = parse(GLOBAL_YAML, None).expect("fixture must parse");
        let msg = cfg
            .catalog
            .start_bound(Some("home-dev"))
            .expect_err("a remote profile cannot be pinned");
        assert_eq!(msg, "--profile can't pin a remote profile ('home-dev')");
    }

    /// §4.4 row 14 + CE1 (desktop MVP): a profiles map with NO local
    /// profile (only remote entries) is a parse error at `profiles`.
    #[test]
    fn all_remote_profiles_are_a_no_local_profile_error() {
        let yaml = "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - { name: anthropic/claude-sonnet-4.5, alias: sonnet, max_tokens: 8192 }\nprofiles:\n  home-dev:\n    remote:\n      url: https://10.2.0.48:7823\n      token: pito\n      ca: sha256:3f9a0123456789abcdef0123456789abcdef0123456789abcdef0123456789ab\n      profile: dev\n";
        let err = parse(yaml, None).expect_err("a local-less profile set must not parse");
        let e = one_err(&err, &[key("profiles")], "no local profile");
        assert_eq!(
            e.message,
            "declare at least one local profile (one with agents)"
        );
    }

    /// "First = default" with a remote entry in the mix: `default_bound` /
    /// `start_bound` iterate the LOCAL entries only — a remote first in file
    /// order never becomes a session bound.
    #[test]
    fn default_bound_and_start_bound_skip_remote_entries() {
        // `home-dev` is FIRST in the file; the local `dev` follows.
        let yaml = "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - { name: anthropic/claude-sonnet-4.5, alias: sonnet, max_tokens: 8192 }\nprofiles:\n  home-dev:\n    remote:\n      url: https://10.2.0.48:7823\n      token: pito\n      ca: sha256:3f9a0123456789abcdef0123456789abcdef0123456789abcdef0123456789ab\n      profile: dev\n  dev:\n    agents:\n      team:\n        model: sonnet\n      solo:\n        model: sonnet\n";
        let cfg = parse(yaml, None).expect("a remote-first file must parse");
        let bound = cfg.catalog.default_bound();
        assert_eq!(
            bound.selection(),
            Selection {
                profile: "dev".into(),
                agent: "team".into(),
                ..Default::default()
            }
        );
        let bound = cfg
            .catalog
            .start_bound(None)
            .expect("no pin binds the first LOCAL profile");
        assert_eq!(bound.selection().profile, "dev");
        // A local pin still works, and binds that profile's first agent.
        let bound = cfg
            .catalog
            .start_bound(Some("dev"))
            .expect("a local pin binds");
        assert_eq!(bound.selection().profile, "dev");
        assert_eq!(bound.selection().agent, "team");
    }

    /// `Catalog::resolve` is local-only: a remote name never resolves to a
    /// `Bound` (it has no local agents).
    #[test]
    fn resolve_ignores_remote_entries() {
        let cfg = parse(GLOBAL_YAML, None).expect("fixture must parse");
        assert!(
            cfg.catalog.resolve("home-dev", "dev").is_none(),
            "a remote profile has no local agents"
        );
        assert!(cfg.catalog.resolve("home-dev", "team").is_none());
        assert!(
            cfg.catalog.resolve("dev", "team").is_some(),
            "local lookups are unchanged"
        );
    }

    /// §4.2: `infos()` lists remote entries with `origin: "remote"`,
    /// `agents: []` (the relay fills them for the active remote tab, §6.3)
    /// and a resolved colour; local entries keep their origin and agents.
    #[test]
    fn infos_list_remote_entries_with_origin_and_empty_agents() {
        let cfg = parse(GLOBAL_YAML, None).expect("fixture must parse");
        let infos = cfg.catalog.infos();

        let home = infos
            .iter()
            .find(|p| p.name == "home-dev")
            .expect("home-dev in infos");
        assert_eq!(home.origin, "remote");
        assert!(
            home.agents.is_empty(),
            "remote entries carry no agents: {home:?}"
        );
        assert_eq!(home.color, TabColor::Purple);

        let sandbox = infos
            .iter()
            .find(|p| p.name == "sandbox")
            .expect("sandbox in infos");
        assert_eq!(sandbox.origin, "remote");
        assert!(sandbox.agents.is_empty());
        assert_eq!(sandbox.color, TabColor::derived("sandbox"));

        let dev = infos
            .iter()
            .find(|p| p.name == "dev")
            .expect("dev in infos");
        assert_eq!(dev.origin, "global");
        assert_eq!(dev.agents.len(), 1);
        assert_eq!(dev.agents[0].name, "team");
    }

    /// §4.2: the catalog keeps ONE ordered list of `CatalogEntry` — repo
    /// first, then global in file order, LOCAL and REMOTE entries mixed.
    #[test]
    fn catalog_entries_keep_repo_first_then_global_file_order() {
        let repo = "profiles:\n  repo-a:\n    agents:\n      a:\n        model: sonnet\n";
        let cfg = parse(GLOBAL_YAML, Some(repo)).expect("fixture must parse");
        let entries: Vec<&CatalogEntry> = cfg.catalog.entries().collect();
        assert_eq!(entries.len(), 5, "one repo + four global entries");
        let names: Vec<String> = entries
            .iter()
            .map(|e| match e {
                CatalogEntry::Local(p) => p.name.clone(),
                CatalogEntry::Remote(r) => r.name.clone(),
            })
            .collect();
        assert_eq!(
            names,
            vec!["repo-a", "dev", "home-dev", "vps-web", "sandbox"],
            "repo first, then the global file order"
        );
        assert!(matches!(&entries[1], CatalogEntry::Local(_)));
        assert!(matches!(&entries[2], CatalogEntry::Remote(_)));
    }
}
