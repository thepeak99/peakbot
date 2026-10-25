//! A4 — `Config::parse` + `errors` + `boot_only_diff` RED tests (config-v2
//! spec §4.3 rule table, §5.3 resolved types, §5.4 parse, §6.8 repo merge,
//! §7.1 legacy detection, §11.1 items 2–13).
//!
//! **Status: compile-fail until A4 lands.** `peakbot::config::errors` and
//! `peakbot::config::resolve` do not exist yet; this file targets the locked
//! shapes from spec §4.1/§5.3/§5.4 and fails to compile — the RED state we
//! want. Everything else in this file compiles against already-landed code
//! (A2 `named.rs`, A3 `file.rs`, the old `peakbot::config` types).
//!
//! Assumed API (spec-literal; where the spec is silent, the simplest shape):
//! ```ignore
//! // peakbot::config::errors
//! pub enum ConfigFileKind { Global, Repo }          // Debug + PartialEq
//! pub enum PathSeg { Key(String), Index(usize) }    // Debug + PartialEq
//! pub struct ConfigError {                          // Debug + Clone
//!     pub file: ConfigFileKind,
//!     pub path: Vec<PathSeg>,                       // [] = document root
//!     pub message: String,                          // no path prefix
//! }
//! impl ConfigError {
//!     pub fn dotted_path(&self) -> String;          // segments joined with '.'
//! }
//! pub struct ConfigErrors(pub Vec<ConfigError>);    // Debug
//!
//! // peakbot::config::resolve
//! pub struct Source<'a> { pub path: &'a Path, pub text: &'a str }
//! pub struct ParseCtx<'a> { pub known_skills: &'a [String] }
//! pub struct Config {                               // Debug
//!     pub registry: Arc<ModelRegistry>,
//!     pub mcps: Vec<McpServerConfig>,
//!     pub general: General,                         // file.rs General
//!     pub catalog: Catalog,
//!     pub warnings: Vec<String>,                    // F6 etc.
//! }
//! impl Config {
//!     pub fn parse(global: Source, repo: Option<Source>, ctx: &ParseCtx)
//!         -> Result<Config, ConfigErrors>;
//! }
//! pub struct Catalog;   // default_bound() -> Bound, profile(&str) -> Option<&Arc<ResolvedProfile>>,
//!                       // resolve(&Selection) -> Option<Bound>, infos(), iter()
//! pub struct ResolvedProfile { name, origin: Origin, tools: NameFilter,
//!     mcps: NameFilter, fetch_url, env: Env, memory,
//!     vector: Option<VectorSpec>, tabs: Vec<&'static str>,
//!     agents: Vec<ResolvedAgent> }
//! pub enum Origin { Global, Repo { overrides_global: bool } }  // Debug + PartialEq
//! pub struct ResolvedAgent { main: Member, prompt: Option<String>,
//!     head: Head, subagents: Vec<Subagent> }
//! pub enum Head { BuiltIn, Persona(String), Whole(String) }    // Debug + PartialEq
//! pub struct Member { name: String, model: ResolvedModel, skills: NameFilter,
//!     agents_md: bool, env: Env }
//! pub struct Subagent { member: Member, prompt: String }
//! pub struct VectorSpec { pub db_path: DbPathSpec, pub embeddings: EmbeddingsConfig }
//! pub enum DbPathSpec { Fixed(PathBuf), SessionRelative(PathBuf) } // Debug + PartialEq
//! #[derive(Clone, PartialEq, Eq, Debug)] pub struct Selection {
//!     pub profile: String, pub agent: String }
//! pub struct Bound { pub profile: Arc<ResolvedProfile>, pub agent_index: usize }
//! impl Bound { pub fn selection(&self) -> Selection }
//! pub fn boot_only_diff(boot: &Config, fresh: &Config) -> Vec<&'static str>;
//! ```
//!
//! Ambiguities resolved (flagged in the A4 report):
//! - G1 message = the EXISTING `TimeoutsConfig::validate` text
//!   (`timeouts.<field> must be 1..=86400 seconds (got N)`) with the PATH
//!   re-pathed to `general.timeouts.<field>` ("existing text, re-pathed").
//! - F4 no-servers-declared message is verbatim from the table, including the
//!   leading `names MCP server '<x>'…` wording.
//! - The F6 warning's `<path>` is the dot-joined path
//!   (`profiles.<p>.agents.<a>.skills`).
//! - E1/E2 "missing" sub-case: parse must rewrite serde's missing-field
//!   failure into the E1/E2 message (tested).
//! - V2's `<path>` is the resolved (here: absolute) db_path.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use peakbot::config::errors::{ConfigError, ConfigErrors, ConfigFileKind, PathSeg};
use peakbot::config::resolve::{
    Config, DbPathSpec, Head, Origin, ParseCtx, Selection, Source, boot_only_diff,
};
use peakbot::config::{
    ContextConfig, HttpConfig, NameFilter, RetryConfig, TimeoutsConfig, UploadsConfig,
};

const GLOBAL_PATH: &str = "/home/u/.config/peakbot/config.yaml";
const REPO_PATH: &str = "/home/u/work/peakbot/.peakbot/config.yaml";

/// §4.3 L1, verbatim (em dash U+2014).
const L1_MSG: &str = "config.yaml uses the pre-0.24 layout — restart PeakBot to migrate it automatically (the original is kept as config.yaml.pre-0.24)";

/// The minimal valid global config: one provider, two aliases (`a`, `b`),
/// one profile `p`, one agent `a` on model `a`.
const BASE: &str = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n";

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/config_v2")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn ctx() -> ParseCtx<'static> {
    ParseCtx { known_skills: &[] }
}

fn parse_global(yaml: &str) -> Result<Config, ConfigErrors> {
    Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text: yaml,
        },
        None,
        &ctx(),
    )
}

fn parse_global_repo(global: &str, repo: &str) -> Result<Config, ConfigErrors> {
    Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text: global,
        },
        Some(Source {
            path: Path::new(REPO_PATH),
            text: repo,
        }),
        &ctx(),
    )
}

fn parsed(yaml: &str) -> Config {
    parse_global(yaml).unwrap_or_else(|e| panic!("config must parse: {e:?}"))
}

/// The collected errors; panics (with the `Config` in the message) on `Ok`.
fn errs(res: Result<Config, ConfigErrors>) -> Vec<ConfigError> {
    match res {
        Ok(cfg) => panic!("expected a parse error, got Ok({cfg:?})"),
        Err(errors) => errors.0,
    }
}

fn key(s: &str) -> PathSeg {
    PathSeg::Key(s.to_string())
}

fn idx(n: usize) -> PathSeg {
    PathSeg::Index(n)
}

// ---------- §11.1 item 2: minimal valid config ----------

#[test]
fn minimal_valid_global_gives_one_profile_one_agent_and_default_selection() {
    let cfg = parsed(BASE);

    assert_eq!(cfg.catalog.iter().count(), 1);
    let p = cfg.catalog.profile("p").expect("profile p");
    assert_eq!(p.agents.len(), 1);
    assert!(cfg.warnings.is_empty(), "a valid config has no warnings");

    // First = default, everywhere (I1): the [0]/[0] pair.
    let sel = cfg.catalog.default_bound().selection();
    assert_eq!(
        sel,
        Selection {
            profile: "p".to_string(),
            agent: "a".to_string(),
            ..Default::default()
        }
    );
}

// ---------- §4.3 rule table: one case per row, path + message string-equal ----------

struct Row {
    name: &'static str,
    global: &'static str,
    repo: Option<&'static str>,
    file: ConfigFileKind,
    path: Vec<PathSeg>,
    /// `None` = serde wording (P2/P4): assert the path only.
    message: Option<&'static str>,
}

/// Two providers, one model each, both aliased `a` (the R1 duplicate).
const DUP_ALIAS_PROVIDERS: &str = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: a\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n";

fn rows() -> Vec<Row> {
    let mut r = Vec::new();
    let g = |name: &'static str, global: &'static str, path: Vec<PathSeg>, msg: &'static str| Row {
        name,
        global,
        repo: None,
        file: ConfigFileKind::Global,
        path,
        message: Some(msg),
    };

    // L1 — legacy layout, checked before any other parse.
    r.push(g("L1", "default_model: a\n", Vec::new(), L1_MSG));

    // P1 — unknown top-level key (serde wording, untouched).
    r.push(g(
        "P1",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\nmemory: {}\n",
        Vec::new(),
        "unknown field `memory`, expected one of `providers`, `mcps`, `profiles`, `general`",
    ));

    // P2 — any other unknown field: path only (serde wording).
    r.push(Row {
        name: "P2",
        global: "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  bogus: 1\n",
        repo: None,
        file: ConfigFileKind::Global,
        path: vec![key("general")],
        message: None,
    });

    // P3 — `persona` under a subagent: P2 rewritten to the dedicated message.
    r.push(g(
        "P3",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n        subagents:\n          s:\n            prompt: x\n            persona: y\n",
        vec![
            key("profiles"),
            key("p"),
            key("agents"),
            key("a"),
            key("subagents"),
            key("s"),
        ],
        "subagents have no persona — the prompt is the subagent's whole persona",
    ));

    // P4 — type/format error: path only (serde wording).
    r.push(Row {
        name: "P4",
        global: "providers: 42\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n",
        repo: None,
        file: ConfigFileKind::Global,
        path: vec![key("providers")],
        message: None,
    });

    // N1 — invalid name (en dash in "1–64").
    r.push(g(
        "N1",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  bad/name:\n    agents:\n      a:\n        model: a\n",
        vec![key("profiles"), key("bad/name")],
        "name 'bad/name' must be 1–64 characters of letters, digits, space, '_', '.', '-' with no leading/trailing space",
    ));

    // N2 — duplicate name, ×3 nouns; path = the map path.
    r.push(g(
        "N2-profile",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n  p:\n    agents:\n      q:\n        model: a\n",
        vec![key("profiles")],
        "duplicate profile 'p'",
    ));
    r.push(g(
        "N2-agent",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n      a:\n        model: b\n",
        vec![key("profiles"), key("p"), key("agents")],
        "duplicate agent 'a'",
    ));
    r.push(g(
        "N2-subagent",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n        subagents:\n          s:\n            prompt: x\n          s:\n            prompt: y\n",
        vec![
            key("profiles"),
            key("p"),
            key("agents"),
            key("a"),
            key("subagents"),
        ],
        "duplicate subagent 's'",
    ));

    // N3 — reserved subagent name.
    r.push(g(
        "N3",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n        subagents:\n          orchestrator:\n            prompt: x\n",
        vec![
            key("profiles"),
            key("p"),
            key("agents"),
            key("a"),
            key("subagents"),
            key("orchestrator"),
        ],
        "'orchestrator' is a reserved name — pick another subagent name",
    ));

    // E1 — providers empty.
    r.push(g(
        "E1",
        "providers: []\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n",
        vec![key("providers")],
        "declare at least one provider with at least one model",
    ));

    // E2 — profiles empty.
    r.push(g(
        "E2",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles: {}\n",
        vec![key("profiles")],
        "declare at least one profile — the first one is the default",
    ));

    // E3 — agents empty.
    r.push(g(
        "E3",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents: {}\n",
        vec![key("profiles"), key("p"), key("agents")],
        "declare at least one agent — the first one is the default",
    ));

    // R1 — registry error (representative: duplicate alias), existing Display
    // text from model_registry.rs.
    r.push(g(
        "R1",
        DUP_ALIAS_PROVIDERS,
        vec![
            key("providers"),
            idx(0),
            key("models"),
            idx(1),
            key("alias"),
        ],
        "duplicate alias `a` declared on `p/m1` and `p/m2`",
    ));

    // M1 — agent model missing.
    r.push(g(
        "M1",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        persona: hi\n",
        vec![key("profiles"), key("p"), key("agents"), key("a"), key("model")],
        "Required — pick a model alias.",
    ));

    // M2 — unknown alias: agent, subagent, compaction_model.
    const M2: &str = "Unknown model alias nope — declared: a, b";
    r.push(g(
        "M2-agent",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: nope\n",
        vec![key("profiles"), key("p"), key("agents"), key("a"), key("model")],
        M2,
    ));
    r.push(g(
        "M2-subagent",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n        subagents:\n          s:\n            model: nope\n            prompt: x\n",
        vec![
            key("profiles"),
            key("p"),
            key("agents"),
            key("a"),
            key("subagents"),
            key("s"),
            key("model"),
        ],
        M2,
    ));
    r.push(g(
        "M2-compaction",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  context:\n    compaction_model: nope\n",
        vec![key("general"), key("context"), key("compaction_model")],
        M2,
    ));

    // A1 — core_guidance: false without persona.
    r.push(g(
        "A1",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n        core_guidance: false\n",
        vec![key("profiles"), key("p"), key("agents"), key("a"), key("core_guidance")],
        "core_guidance: false needs a persona — with core guidance off, the persona is the whole prompt head",
    ));

    // A2 — blank persona.
    r.push(g(
        "A2",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n        persona: \"   \"\n",
        vec![key("profiles"), key("p"), key("agents"), key("a"), key("persona")],
        "must not be blank — remove the key to use the built-in persona",
    ));

    // A3 — blank agent prompt.
    r.push(g(
        "A3",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n        prompt: \"  \"\n",
        vec![key("profiles"), key("p"), key("agents"), key("a"), key("prompt")],
        "must not be blank — remove the key for no instructions",
    ));

    // S1 — subagent prompt missing (the blank variant has its own test).
    r.push(g(
        "S1",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n        subagents:\n          s:\n            model: a\n",
        vec![
            key("profiles"),
            key("p"),
            key("agents"),
            key("a"),
            key("subagents"),
            key("s"),
            key("prompt"),
        ],
        "Required — a subagent needs a prompt.",
    ));

    // F1 — NameFilter with both lists.
    r.push(g(
        "F1",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    tools:\n      disabled: [bash]\n      only: [think]\n    agents:\n      a:\n        model: a\n",
        vec![key("profiles"), key("p"), key("tools")],
        "set either `disabled` (blocklist) or `only` (allowlist), not both",
    ));

    // F2 — unknown tool; the known list is BUILTIN_TOOL_NAMES minus `delegate`.
    r.push(g(
        "F2",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    tools:\n      only: [frobnicate]\n    agents:\n      a:\n        model: a\n",
        vec![key("profiles"), key("p"), key("tools")],
        "unknown tool 'frobnicate'. Known tools: bash, bash_bg, doc_index, doc_search, fetch_page, fetch_url, file_create, file_insert, file_read, file_str_replace, list_directory, pdf_read, powershell, powershell_bg, think, todo, view_image, web_search.",
    ));

    // F3 — `delegate` named in tools.
    r.push(g(
        "F3",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    tools:\n      disabled: [delegate]\n    agents:\n      a:\n        model: a\n",
        vec![key("profiles"), key("p"), key("tools")],
        "'delegate' is not a filterable tool — it is on automatically when an agent has subagents",
    ));

    // F4 — unknown MCP name, with declared servers.
    r.push(g(
        "F4-declared",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nmcps:\n  - name: playwright\n    command: npx\n    args: [\"@playwright/mcp@latest\"]\n  - name: linear\n    type: streamable-http\n    url: https://mcp.linear.app/mcp\nprofiles:\n  p:\n    mcps:\n      only: [nope]\n    agents:\n      a:\n        model: a\n",
        vec![key("profiles"), key("p"), key("mcps")],
        "unknown MCP server 'nope'. Declared servers: playwright, linear.",
    ));

    // F4 — no servers declared (verbatim table wording).
    r.push(g(
        "F4-none",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    mcps:\n      only: [nope]\n    agents:\n      a:\n        model: a\n",
        vec![key("profiles"), key("p"), key("mcps")],
        "names MCP server 'nope', but no MCP servers are declared in mcps",
    ));

    // F5 — unknown tab; `profile` is no longer a tab, `settings` is.
    r.push(g(
        "F5",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    ui:\n      tabs:\n        only: [profile]\n    agents:\n      a:\n        model: a\n",
        vec![key("profiles"), key("p"), key("ui"), key("tabs")],
        "unknown tab 'profile'. Known tabs: session, todo, files, tasks, bash, agents, settings.",
    ));

    // V1 — embeddings dimensions == 0.
    r.push(g(
        "V1",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    vector_db:\n      db_path: /tmp/v1.db\n      embeddings:\n        base_url: https://e.example/v1\n        model: bge\n        dimensions: 0\n    agents:\n      a:\n        model: a\n",
        vec![
            key("profiles"),
            key("p"),
            key("vector_db"),
            key("embeddings"),
            key("dimensions"),
        ],
        "must be greater than 0",
    ));

    // V2 — same resolved db_path, different embeddings (error on the second).
    r.push(g(
        "V2",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  alpha:\n    vector_db:\n      db_path: /tmp/vectors-test/one.db\n      embeddings:\n        base_url: https://e.example/v1\n        model: bge-m3\n        dimensions: 1024\n    agents:\n      a:\n        model: a\n  beta:\n    vector_db:\n      db_path: /tmp/vectors-test/one.db\n      embeddings:\n        base_url: https://e.example/v1\n        model: bge-large\n        dimensions: 1024\n    agents:\n      a:\n        model: a\n",
        vec![key("profiles"), key("beta"), key("vector_db")],
        "db_path '/tmp/vectors-test/one.db' is also used by profile 'alpha' with different embeddings — one DB file needs one embedding model",
    ));

    // G1 — existing TimeoutsConfig::validate text, re-pathed.
    r.push(g(
        "G1",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  timeouts:\n    tool_secs: 0\n",
        vec![key("general"), key("timeouts"), key("tool_secs")],
        "timeouts.tool_secs must be 1..=86400 seconds (got 0)",
    ));

    // G2 / G3 — uploads bounds.
    r.push(g(
        "G2",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  uploads:\n    max_file_mb: 0\n",
        vec![key("general"), key("uploads"), key("max_file_mb")],
        "must be 1..=1024, got 0",
    ));
    r.push(g(
        "G3",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  uploads:\n    max_files: 0\n",
        vec![key("general"), key("uploads"), key("max_files")],
        "must be 1..=100, got 0",
    ));

    // G4 — context threshold bounds (0 here; 1.5 in context_threshold_bounds).
    r.push(g(
        "G4",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  context:\n    threshold: 0\n",
        vec![key("general"), key("context"), key("threshold")],
        "must be greater than 0 and at most 1, got 0",
    ));

    // G5 — agent_max_turns == 0.
    r.push(g(
        "G5",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  agent_max_turns: 0\n",
        vec![key("general"), key("agent_max_turns")],
        "must be at least 1",
    ));

    // G6 — empty search base_url.
    r.push(g(
        "G6",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  search:\n    base_url: ''\n",
        vec![key("general"), key("search"), key("base_url")],
        "must not be empty — remove `search:` to turn web search off",
    ));

    // G7 — blank web token.
    r.push(g(
        "G7",
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  web:\n    token: '   '\n",
        vec![key("general"), key("web"), key("token")],
        "must not be blank — remove the key for no token",
    ));

    // RP1 — repo file with a key other than `profiles` (file = Repo, root path).
    r.push(Row {
        name: "RP1",
        global: BASE,
        repo: Some("mcps: []\n"),
        file: ConfigFileKind::Repo,
        path: Vec::new(),
        message: Some(
            "only `profiles:` is allowed in a repo config — move `mcps:` to the global config (",
        ),
    });

    // RP2 — repo file is legacy-shaped: one error for the file, replacing RP1.
    r.push(Row {
        name: "RP2",
        global: BASE,
        repo: Some("default_model: a\n"),
        file: ConfigFileKind::Repo,
        path: Vec::new(),
        message: Some(
            "this repo config uses the pre-0.24 layout — PeakBot migrates it automatically on the next start or /cd into this directory",
        ),
    });

    r
}

#[test]
fn rule_table_every_row_of_section_4_3() {
    for row in rows() {
        let res = match row.repo {
            Some(repo) => parse_global_repo(row.global, repo),
            None => parse_global(row.global),
        };
        let es = errs(res);
        assert_eq!(es.len(), 1, "case {}: exactly one error", row.name);
        let e = &es[0];
        assert_eq!(e.file, row.file, "case {}: file kind", row.name);
        assert_eq!(e.path, row.path, "case {}: path segments", row.name);
        if let Some(msg) = row.message {
            // RP1: the message embeds the global file path; compare the
            // prefix and pin the suffix separately.
            if row.name == "RP1" {
                assert!(
                    e.message.starts_with(msg),
                    "case {}: message prefix, got: {}",
                    row.name,
                    e.message
                );
                assert!(
                    e.message.ends_with(&format!("({GLOBAL_PATH})")),
                    "case {}: message must name the global path, got: {}",
                    row.name,
                    e.message
                );
            } else {
                assert_eq!(e.message, msg, "case {}: message", row.name);
            }
        }
    }
}

// F6 is a WARNING, not an error (spec §4.3): parse succeeds, warning in
// `config.warnings`.
#[test]
fn unknown_skill_is_a_warning_not_an_error() {
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n        skills:\n          only: [ghost]\n";

    let cfg = parse_global(yaml).expect("an unknown skill must not be an error");
    assert_eq!(
        cfg.warnings,
        vec!["⚠ profiles.p.agents.a.skills: no skill named 'ghost' is installed here"]
    );

    // A known skill produces no warning.
    let skills = vec!["ghost".to_string()];
    let ctx = ParseCtx {
        known_skills: &skills,
    };
    let cfg = Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text: yaml,
        },
        None,
        &ctx,
    )
    .expect("a known skill must not be an error");
    assert!(cfg.warnings.is_empty(), "known skill: no warning");
}

// ---------- Collection: all semantic errors, global first, then document order ----------

#[test]
fn semantic_errors_are_collected_in_document_order() {
    // Three independent errors in one global file:
    // 1. M2  — agent a, unknown model          (document order 1)
    // 2. S1  — agent a's subagent, no prompt   (document order 2)
    // 3. A2  — agent b, blank persona          (document order 3)
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: nope\n        subagents:\n          s:\n            model: a\n      b:\n        model: a\n        persona: \"   \"\n";
    let es = errs(parse_global(yaml));
    assert_eq!(es.len(), 3, "all three errors must be collected: {es:?}");

    assert_eq!(
        es[0].path,
        vec![
            key("profiles"),
            key("p"),
            key("agents"),
            key("a"),
            key("model")
        ]
    );
    assert_eq!(es[0].message, "Unknown model alias nope — declared: a, b");

    assert_eq!(
        es[1].path,
        vec![
            key("profiles"),
            key("p"),
            key("agents"),
            key("a"),
            key("subagents"),
            key("s"),
            key("prompt"),
        ]
    );
    assert_eq!(es[1].message, "Required — a subagent needs a prompt.");

    assert_eq!(
        es[2].path,
        vec![
            key("profiles"),
            key("p"),
            key("agents"),
            key("b"),
            key("persona")
        ]
    );
    assert_eq!(
        es[2].message,
        "must not be blank — remove the key to use the built-in persona"
    );
}

#[test]
fn repo_errors_come_after_global_errors() {
    let global = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: nope\n";
    let repo = "profiles:\n  r:\n    agents:\n      a:\n        model: also-nope\n";
    let es = errs(parse_global_repo(global, repo));
    assert_eq!(
        es.len(),
        2,
        "global + repo errors must both be collected: {es:?}"
    );
    assert_eq!(es[0].file, ConfigFileKind::Global);
    assert_eq!(
        es[0].path,
        vec![
            key("profiles"),
            key("p"),
            key("agents"),
            key("a"),
            key("model")
        ]
    );
    assert_eq!(es[1].file, ConfigFileKind::Repo);
    assert_eq!(
        es[1].path,
        vec![
            key("profiles"),
            key("r"),
            key("agents"),
            key("a"),
            key("model")
        ]
    );
    assert_eq!(
        es[1].message,
        "Unknown model alias also-nope — declared: a, b"
    );
}

// ---------- Path fidelity: segment arrays, not dot-split strings ----------

const DOTTED_PROFILE_YAML: &str = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  v1.2 beta:\n    agents:\n      main:\n        persona: hi\n";

#[test]
fn path_segments_keep_dots_and_spaces_inside_names() {
    // Profile name `v1.2 beta` (dot + space, legal per §4.2) with a missing
    // agent model: the path must be whole segments, not a dot-split string.
    let es = errs(parse_global(DOTTED_PROFILE_YAML));
    assert_eq!(es.len(), 1);
    assert_eq!(
        es[0].path,
        vec![
            key("profiles"),
            key("v1.2 beta"),
            key("agents"),
            key("main"),
            key("model"),
        ],
        "the profile name must be ONE Key segment"
    );
    assert_eq!(es[0].message, "Required — pick a model alias.");
}

#[test]
fn dotted_path_renders_segments_for_the_cli() {
    let es = errs(parse_global(DOTTED_PROFILE_YAML));
    assert_eq!(es[0].dotted_path(), "profiles.v1.2 beta.agents.main.model");
}

#[test]
fn dotted_path_of_a_root_error_is_empty() {
    let es = errs(parse_global("default_model: a\n"));
    assert_eq!(es[0].dotted_path(), "");
}

// ---------- Head construction (A1 combos) ----------

#[test]
fn head_is_built_in_when_persona_is_absent() {
    let cfg = parsed(BASE);
    let a = &cfg.catalog.profile("p").expect("p").agents[0];
    assert_eq!(a.head, Head::BuiltIn);
}

#[test]
fn head_is_trimmed_persona_when_set() {
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n        persona: \"  You are the lead.  \"\n";
    let cfg = parsed(yaml);
    let a = &cfg.catalog.profile("p").expect("p").agents[0];
    assert_eq!(
        a.head,
        Head::Persona("You are the lead.".to_string()),
        "the persona must be trimmed"
    );
}

#[test]
fn head_is_whole_when_core_guidance_is_false() {
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n        core_guidance: false\n        persona: You are the whole head.\n";
    let cfg = parsed(yaml);
    let a = &cfg.catalog.profile("p").expect("p").agents[0];
    assert_eq!(a.head, Head::Whole("You are the whole head.".to_string()));
}

// ---------- Subagent model default + prompt shapes (I5, I6) ----------

#[test]
fn subagent_model_defaults_to_agents_configured_model() {
    // The registry default (first declared alias) is `first`; the agent
    // configures `second`. The subagent must inherit `second`, NOT the
    // registry default.
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: first\n      - name: m2\n        alias: second\nprofiles:\n  p:\n    agents:\n      a:\n        model: second\n        subagents:\n          s:\n            prompt: do it\n";
    let cfg = parsed(yaml);
    let a = &cfg.catalog.profile("p").expect("p").agents[0];
    assert_eq!(a.main.model.alias, "second");
    assert_eq!(
        a.subagents[0].member.model.alias, "second",
        "a subagent without a model inherits the agent's CONFIGURED model"
    );
}

#[test]
fn subagent_prompt_is_a_non_blank_string_agent_prompt_is_an_option() {
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n        prompt: Research first.\n        subagents:\n          s:\n            prompt: Gather context.\n";
    let cfg = parsed(yaml);
    let a = &cfg.catalog.profile("p").expect("p").agents[0];

    // Main agent: optional prompt.
    assert_eq!(a.prompt.as_deref(), Some("Research first."));
    let bare = parsed(BASE);
    assert_eq!(
        bare.catalog.profile("p").expect("p").agents[0].prompt,
        None,
        "absent agent prompt stays None"
    );

    // Subagent: plain non-blank String (I5: Member has no prompt field).
    assert_eq!(a.subagents[0].prompt, "Gather context.");
    assert!(!a.subagents[0].prompt.trim().is_empty());
}

#[test]
fn s1_blank_subagent_prompt_is_rejected_too() {
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n        subagents:\n          s:\n            prompt: \"   \"\n";
    let es = errs(parse_global(yaml));
    assert_eq!(es.len(), 1);
    assert_eq!(
        es[0].path,
        vec![
            key("profiles"),
            key("p"),
            key("agents"),
            key("a"),
            key("subagents"),
            key("s"),
            key("prompt"),
        ]
    );
    assert_eq!(es[0].message, "Required — a subagent needs a prompt.");
}

// ---------- NameFilter semantics on ResolvedProfile.mcps (I8) ----------

#[test]
fn only_empty_means_all_enabled_false_means_none() {
    let only_empty = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    mcps:\n      only: []\n    agents:\n      a:\n        model: a\n";
    let cfg = parsed(only_empty);
    let p = cfg.catalog.profile("p").expect("p");
    assert!(
        p.mcps.allows("anything"),
        "`only: []` means ALL (NameFilter rule)"
    );

    let disabled = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    mcps:\n      enabled: false\n    agents:\n      a:\n        model: a\n";
    let cfg = parsed(disabled);
    let p = cfg.catalog.profile("p").expect("p");
    assert!(!p.mcps.allows("anything"), "`enabled: false` means NONE");
}

// ---------- Vector: DbPathSpec construction + V2 ----------

const REPO_VECTOR_YAML: &str = "profiles:\n  r:\n    vector_db:\n      db_path: vectors.db\n      embeddings:\n        base_url: https://e.example/v1\n        model: bge\n        dimensions: 8\n    agents:\n      a:\n        model: a\n";

#[test]
fn repo_relative_db_path_anchors_to_repo_root() {
    let cfg = parse_global_repo(BASE, REPO_VECTOR_YAML).expect("repo vector must parse");
    let r = cfg.catalog.profile("r").expect("r");
    let spec = r.vector.as_ref().expect("vector_db present");
    // REPO_PATH = /home/u/work/peakbot/.peakbot/config.yaml → root
    // /home/u/work/peakbot.
    assert_eq!(
        spec.db_path,
        DbPathSpec::Fixed(PathBuf::from("/home/u/work/peakbot/vectors.db")),
        "a repo-relative db_path becomes Fixed(repo_root.join(p))"
    );
}

#[test]
fn global_relative_db_path_is_session_relative() {
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    vector_db:\n      db_path: vectors.db\n      embeddings:\n        base_url: https://e.example/v1\n        model: bge\n        dimensions: 8\n    agents:\n      a:\n        model: a\n";
    let cfg = parsed(yaml);
    let p = cfg.catalog.profile("p").expect("p");
    let spec = p.vector.as_ref().expect("vector_db present");
    assert_eq!(
        spec.db_path,
        DbPathSpec::SessionRelative(PathBuf::from("vectors.db")),
        "a global relative db_path resolves per session cwd"
    );
}

#[test]
fn absolute_and_home_db_paths_are_fixed() {
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    vector_db:\n      db_path: /abs/vectors.db\n      embeddings:\n        base_url: https://e.example/v1\n        model: bge\n        dimensions: 8\n    agents:\n      a:\n        model: a\n";
    let cfg = parsed(yaml);
    let spec = cfg
        .catalog
        .profile("p")
        .expect("p")
        .vector
        .as_ref()
        .expect("vector_db present");
    assert_eq!(
        spec.db_path,
        DbPathSpec::Fixed(PathBuf::from("/abs/vectors.db"))
    );

    // `~/…` → Fixed (the expanded home path is environment-dependent, so
    // only the variant is asserted).
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    vector_db:\n      db_path: ~/research/research.db\n      embeddings:\n        base_url: https://e.example/v1\n        model: bge\n        dimensions: 8\n    agents:\n      a:\n        model: a\n";
    let cfg = parsed(yaml);
    let spec = cfg
        .catalog
        .profile("p")
        .expect("p")
        .vector
        .as_ref()
        .expect("vector_db present");
    assert!(
        matches!(spec.db_path, DbPathSpec::Fixed(_)),
        "a home-relative db_path is Fixed, got: {:?}",
        spec.db_path
    );
}

#[test]
fn same_path_same_embeddings_ok() {
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  alpha:\n    vector_db:\n      db_path: /tmp/vectors-test/one.db\n      embeddings:\n        base_url: https://e.example/v1\n        model: bge\n        dimensions: 1024\n    agents:\n      a:\n        model: a\n  beta:\n    vector_db:\n      db_path: /tmp/vectors-test/one.db\n      embeddings:\n        base_url: https://e.example/v1\n        model: bge\n        dimensions: 1024\n    agents:\n      a:\n        model: a\n";
    let cfg = parsed(yaml);
    assert!(
        cfg.catalog
            .profile("alpha")
            .expect("alpha")
            .vector
            .is_some()
    );
    assert!(cfg.catalog.profile("beta").expect("beta").vector.is_some());
}

// ---------- Repo merge (§6.8) ----------

#[test]
fn repo_profiles_listed_first_and_become_default() {
    let repo = "profiles:\n  r1:\n    agents:\n      x:\n        model: a\n  r2:\n    agents:\n      y:\n        model: b\n";
    let cfg = parse_global_repo(BASE, repo).expect("repo profiles must parse");

    let names: Vec<&str> = cfg.catalog.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["r1", "r2", "p"], "repo profiles first, then global");

    let sel = cfg.catalog.default_bound().selection();
    assert_eq!(
        sel,
        Selection {
            profile: "r1".to_string(),
            agent: "x".to_string(),
            ..Default::default()
        },
        "the first repo profile is the default here"
    );
    assert_eq!(
        cfg.catalog.profile("r1").expect("r1").origin,
        Origin::Repo {
            overrides_global: false
        }
    );
    assert_eq!(cfg.catalog.profile("p").expect("p").origin, Origin::Global);
}

#[test]
fn repo_same_name_replaces_global_with_no_field_merge() {
    let global = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  shared:\n    env:\n      GIT_PAGER: cat\n    tools:\n      disabled: [powershell]\n    agents:\n      team:\n        model: a\n      solo:\n        model: b\n";
    let repo = "profiles:\n  shared:\n    agents:\n      solo:\n        model: a\n";
    let cfg = parse_global_repo(global, repo).expect("replacement must parse");

    let shared = cfg.catalog.profile("shared").expect("shared");
    assert_eq!(
        shared.origin,
        Origin::Repo {
            overrides_global: true
        }
    );
    // NONE of the global's fields survive: no merge (spec §1.4).
    assert!(
        shared.env.is_empty(),
        "the global profile's env must not survive the replacement"
    );
    assert_eq!(
        shared.tools,
        NameFilter::default(),
        "the global profile's tools filter must not survive"
    );
    let agent_names: Vec<&str> = shared.agents.iter().map(|a| a.main.name.as_str()).collect();
    assert_eq!(
        agent_names,
        ["solo"],
        "the repo profile's agents replace the global ones"
    );
    assert_eq!(shared.agents[0].main.model.alias, "a");
    // I7: exactly one `shared` in the catalog.
    let count = cfg.catalog.iter().filter(|p| p.name == "shared").count();
    assert_eq!(
        count, 1,
        "a repo profile never coexists with a same-named global"
    );
}

#[test]
fn repo_profile_may_use_global_alias_and_any_global_mcp() {
    // The global needs its own profile (E2: `profiles` is required in the
    // global file, spec §3.3/§6.8) — the repo profile is listed first anyway.
    let global = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nmcps:\n  - name: playwright\n    command: npx\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n";
    let repo = "profiles:\n  r:\n    mcps:\n      only: [playwright]\n    agents:\n      a:\n        model: a\n";
    let cfg = parse_global_repo(global, repo).expect("repo must see global aliases + mcps");
    let r = cfg.catalog.profile("r").expect("r");
    assert_eq!(
        r.agents[0].main.model.alias, "a",
        "global alias resolves in a repo profile"
    );
    assert!(
        r.mcps.allows("playwright"),
        "any global mcp may be enabled by name"
    );
}

#[test]
fn repo_errors_carry_repo_file_kind() {
    let repo = "profiles:\n  r:\n    agents:\n      a:\n        model: a\n        persona: '   '\n";
    let es = errs(parse_global_repo(BASE, repo));
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].file, ConfigFileKind::Repo);
    assert_eq!(
        es[0].path,
        vec![
            key("profiles"),
            key("r"),
            key("agents"),
            key("a"),
            key("persona")
        ]
    );
    assert_eq!(
        es[0].message,
        "must not be blank — remove the key to use the built-in persona"
    );
}

#[test]
fn global_edit_removing_alias_used_by_repo_profile_reports_repo_error() {
    // The global no longer declares `gone`; the repo profile still uses it.
    let repo = "profiles:\n  r:\n    agents:\n      a:\n        model: gone\n";
    let es = errs(parse_global_repo(BASE, repo));
    assert_eq!(
        es.len(),
        1,
        "the broken reference is a repo-file error: {es:?}"
    );
    assert_eq!(es[0].file, ConfigFileKind::Repo);
    assert_eq!(
        es[0].path,
        vec![
            key("profiles"),
            key("r"),
            key("agents"),
            key("a"),
            key("model")
        ]
    );
    assert_eq!(es[0].message, "Unknown model alias gone — declared: a, b");
}

#[test]
fn repo_file_rejects_every_non_profiles_key_with_one_error_each() {
    let repo = "general: {}\nmcps: []\nproviders: []\n";
    let es = errs(parse_global_repo(BASE, repo));
    assert_eq!(
        es.len(),
        3,
        "one error per offending key, in document order: {es:?}"
    );
    for (want_key, e) in ["general", "mcps", "providers"].iter().zip(&es) {
        assert_eq!(e.file, ConfigFileKind::Repo, "key {want_key}");
        assert_eq!(e.path, Vec::<PathSeg>::new(), "key {want_key}: root path");
        assert_eq!(
            e.message,
            format!(
                "only `profiles:` is allowed in a repo config — move `{want_key}:` to the global config ({GLOBAL_PATH})"
            ),
            "key {want_key}"
        );
    }
}

// ---------- The spec's own examples (fixtures) ----------

#[test]
fn spec_global_and_repo_examples_parse_to_the_effective_catalog() {
    let cfg = parse_global_repo(
        &fixture("global_example.yaml"),
        &fixture("repo_example.yaml"),
    )
    .expect("the spec's global + repo examples must parse");

    // §3.2: peakbot-dev (📁, default) · dev (📁, overrides global) · research.
    let names: Vec<&str> = cfg.catalog.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["peakbot-dev", "dev", "research"]);
    let sel = cfg.catalog.default_bound().selection();
    assert_eq!(
        sel,
        Selection {
            profile: "peakbot-dev".to_string(),
            agent: "team".to_string(),
            ..Default::default()
        }
    );

    let pbd = cfg.catalog.profile("peakbot-dev").expect("peakbot-dev");
    assert_eq!(
        pbd.origin,
        Origin::Repo {
            overrides_global: false
        }
    );
    let dev = cfg.catalog.profile("dev").expect("dev");
    assert_eq!(
        dev.origin,
        Origin::Repo {
            overrides_global: true
        }
    );
    assert_eq!(
        cfg.catalog.profile("research").expect("research").origin,
        Origin::Global
    );

    // The replaced `dev` keeps none of the global `dev`'s fields.
    assert!(dev.env.is_empty(), "global dev env must not survive");
    assert!(
        dev.tools.allows("powershell"),
        "global dev tools blocklist must not survive"
    );
    let agent_names: Vec<&str> = dev.agents.iter().map(|a| a.main.name.as_str()).collect();
    assert_eq!(agent_names, ["solo"]);

    // Repo-relative vector anchors to the repo root (REPO_PATH's root is
    // /home/u/work/peakbot).
    let db = &pbd.vector.as_ref().expect("peakbot-dev vector").db_path;
    match db {
        DbPathSpec::Fixed(p) => {
            assert!(
                p.starts_with(Path::new("/home/u/work/peakbot")),
                "repo db_path must anchor to the repo root: {p:?}"
            );
            assert!(p.ends_with(".peakbot/vectors.db"), "got: {p:?}");
        }
        other => panic!("expected a Fixed repo-anchored path, got: {other:?}"),
    }

    // The global profile's `~/…` path stays Fixed.
    let research = cfg.catalog.profile("research").expect("research");
    assert!(
        matches!(
            research.vector.as_ref().expect("research vector").db_path,
            DbPathSpec::Fixed(_)
        ),
        "a global home-relative db_path is Fixed"
    );

    // The repo `tester` subagent has no model → the agent's configured
    // model (`sonnet`).
    let team = &pbd.agents[0];
    assert_eq!(team.main.name, "team");
    let tester = team
        .subagents
        .iter()
        .find(|s| s.member.name == "tester")
        .expect("tester");
    assert_eq!(
        tester.member.model.alias, "sonnet",
        "repo subagent without a model inherits the agent's configured model"
    );
}

// ---------- Legacy detection (§7.1, L1) ----------

#[test]
fn each_legacy_top_key_alone_is_legacy() {
    // LEGACY_TOP, verbatim from §7.1.
    let keys = [
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
    for k in keys {
        let yaml = format!("{k}: {{}}\n");
        let es = errs(parse_global(&yaml));
        assert_eq!(es.len(), 1, "key {k}: exactly one (L1) error: {es:?}");
        assert_eq!(es[0].file, ConfigFileKind::Global, "key {k}");
        assert_eq!(es[0].path, Vec::<PathSeg>::new(), "key {k}: root path");
        assert_eq!(es[0].message, L1_MSG, "key {k}");
    }
}

#[test]
fn old_overlay_profile_is_legacy() {
    // A profile mapping without `agents` that carries a LEGACY_PROFILE key.
    let yaml = "profiles:\n  research:\n    tools:\n      enabled: false\n";
    let es = errs(parse_global(yaml));
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].path, Vec::<PathSeg>::new());
    assert_eq!(es[0].message, L1_MSG);
}

#[test]
fn new_only_marker_plus_stray_legacy_key_is_p1_not_l1() {
    // `general` marker + stray `memory` → NOT legacy: normal P1.
    let es = errs(parse_global("general: {}\nmemory: {}\n"));
    assert_eq!(es.len(), 1, "{es:?}");
    assert_eq!(es[0].path, Vec::<PathSeg>::new());
    assert_eq!(
        es[0].message,
        "unknown field `memory`, expected one of `providers`, `mcps`, `profiles`, `general`"
    );

    // `mcps` marker + stray `default_model` → P1, not L1.
    let es = errs(parse_global("mcps: []\ndefault_model: a\n"));
    assert_eq!(es.len(), 1, "{es:?}");
    assert_eq!(
        es[0].message,
        "unknown field `default_model`, expected one of `providers`, `mcps`, `profiles`, `general`"
    );

    // A profile with `agents` is a marker + stray `pipelines` → P1, not L1.
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\npipelines: []\n";
    let es = errs(parse_global(yaml));
    assert_eq!(es.len(), 1, "{es:?}");
    assert_eq!(
        es[0].message,
        "unknown field `pipelines`, expected one of `providers`, `mcps`, `profiles`, `general`"
    );
}

#[test]
fn providers_only_file_is_not_legacy_it_is_e2() {
    // No legacy signs and no new-only markers → falls through to the normal
    // parse, which reports E2 (profiles missing).
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\n";
    let es = errs(parse_global(yaml));
    assert_eq!(es.len(), 1, "{es:?}");
    assert_eq!(es[0].path, vec![key("profiles")]);
    assert_eq!(
        es[0].message,
        "declare at least one profile — the first one is the default"
    );
}

// ---------- general defaults + bounds (§11.1 item 12) ----------

#[test]
fn general_defaults_when_absent() {
    let cfg = parsed(BASE);
    let g = &cfg.general;

    assert_eq!(g.web.bind, "127.0.0.1:7823".parse::<SocketAddr>().unwrap());
    assert_eq!(g.web.token, None);
    assert!(!g.web.tls);
    assert_eq!(g.web.session_ttl_secs, 600);
    assert_eq!(g.web.reaper_tick_secs, 60);
    assert!(g.search.is_none(), "absent search = no web_search");
    assert_eq!(
        g.context,
        ContextConfig {
            threshold: 0.8,
            keep_recent: 5,
            enabled: true,
            compaction_model: None
        }
    );
    assert!(g.conversation.auto_save);
    assert_eq!(g.conversation.storage_dir, None);
    assert_eq!(g.agent_max_turns, 50);
    assert!(g.cost_tracking);
    assert_eq!(g.retry, RetryConfig::default());
    assert_eq!(g.timeouts, TimeoutsConfig::default());
    assert_eq!(g.http, HttpConfig::default());
    assert_eq!(g.uploads, UploadsConfig::default());
}

#[test]
fn web_bind_is_a_real_socket_addr() {
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  web:\n    bind: 0.0.0.0:8080\n";
    let cfg = parsed(yaml);
    assert_eq!(
        cfg.general.web.bind,
        "0.0.0.0:8080".parse::<SocketAddr>().unwrap()
    );
}

#[test]
fn web_bind_type_error_is_p4_at_the_bind_path() {
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  web:\n    bind: not-a-socket\n";
    let es = errs(parse_global(yaml));
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].file, ConfigFileKind::Global);
    assert_eq!(es[0].path, vec![key("general"), key("web"), key("bind")]);
    // P4: serde wording — the path is the contract, not the text.
    assert!(!es[0].message.is_empty());
}

#[test]
fn context_threshold_bounds() {
    // 1.0 is the inclusive upper bound: allowed.
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  context:\n    threshold: 1.0\n";
    let cfg = parsed(yaml);
    assert_eq!(cfg.general.context.threshold, 1.0);

    // 1.5 → G4.
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  context:\n    threshold: 1.5\n";
    let es = errs(parse_global(yaml));
    assert_eq!(es.len(), 1);
    assert_eq!(
        es[0].path,
        vec![key("general"), key("context"), key("threshold")]
    );
    assert_eq!(
        es[0].message,
        "must be greater than 0 and at most 1, got 1.5"
    );
}

// ---------- boot_only_diff (§5.1, §6.7 step 3) ----------

#[test]
fn boot_only_diff_identical_configs_is_empty() {
    assert_eq!(
        boot_only_diff(&parsed(BASE), &parsed(BASE)),
        Vec::<&'static str>::new()
    );
}

#[test]
fn boot_only_diff_lists_each_changed_boot_only_block() {
    let base = parsed(BASE);
    let cases = [
        (
            "mcps",
            "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\nmcps:\n  - name: playwright\n    command: npx\n",
        ),
        (
            "general.web",
            "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  web:\n    tls: true\n",
        ),
        (
            "general.http",
            "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  http:\n    connect_timeout_secs: 5\n",
        ),
        (
            "general.uploads",
            "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  uploads:\n    max_file_mb: 9\n",
        ),
        (
            "general.conversation",
            "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  conversation:\n    storage_dir: /tmp/convs\n",
        ),
    ];
    for (want, yaml) in cases {
        let fresh = parsed(yaml);
        assert_eq!(
            boot_only_diff(&base, &fresh),
            vec![want],
            "only the changed boot-only block {want} is listed"
        );
    }
}

#[test]
fn boot_only_diff_all_five_blocks_in_stable_order() {
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\nmcps:\n  - name: playwright\n    command: npx\ngeneral:\n  web:\n    tls: true\n  http:\n    connect_timeout_secs: 5\n  uploads:\n    max_file_mb: 9\n  conversation:\n    storage_dir: /tmp/convs\n";
    assert_eq!(
        boot_only_diff(&parsed(BASE), &parsed(yaml)),
        vec![
            "mcps",
            "general.web",
            "general.http",
            "general.uploads",
            "general.conversation"
        ],
        "the key order is stable"
    );
}

#[test]
fn boot_only_diff_ignores_reload_safe_changes() {
    let base = parsed(BASE);

    // Persona (profile level).
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n        persona: Fresh persona.\n";
    assert_eq!(
        boot_only_diff(&base, &parsed(yaml)),
        Vec::<&'static str>::new(),
        "a persona change is reload-safe"
    );

    // general.context.
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  context:\n    threshold: 0.9\n";
    assert_eq!(
        boot_only_diff(&base, &parsed(yaml)),
        Vec::<&'static str>::new(),
        "general.context is reload-safe"
    );

    // Profile tools.
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    tools:\n      disabled: [bash]\n    agents:\n      a:\n        model: a\n";
    assert_eq!(
        boot_only_diff(&base, &parsed(yaml)),
        Vec::<&'static str>::new(),
        "profile tools are reload-safe"
    );
}

// ---------- Tabs (new TAB_NAMES: no `profile`, adds `settings`) ----------

#[test]
fn tabs_default_is_the_new_canonical_seven() {
    let cfg = parsed(BASE);
    assert_eq!(
        cfg.catalog.profile("p").expect("p").tabs,
        [
            "session", "todo", "files", "tasks", "bash", "agents", "settings"
        ],
        "default = all tabs, canonical order; `profile` is gone, `settings` is new"
    );
}

#[test]
fn tabs_filtered_keep_canonical_order() {
    // `only` out of canonical order → canonical order.
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    ui:\n      tabs:\n        only: [agents, session]\n    agents:\n      a:\n        model: a\n";
    let cfg = parsed(yaml);
    assert_eq!(
        cfg.catalog.profile("p").expect("p").tabs,
        ["session", "agents"]
    );

    // `disabled` removes from the canonical list.
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    ui:\n      tabs:\n        disabled: [bash]\n    agents:\n      a:\n        model: a\n";
    let cfg = parsed(yaml);
    assert_eq!(
        cfg.catalog.profile("p").expect("p").tabs,
        ["session", "todo", "files", "tasks", "agents", "settings"]
    );
}

// ---------- F2's known-tools list excludes `delegate` (§5.6) ----------

#[test]
fn f2_known_tools_exclude_delegate() {
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\nprofiles:\n  p:\n    tools:\n      only: [frobnicate]\n    agents:\n      a:\n        model: a\n";
    let es = errs(parse_global(yaml));
    assert_eq!(es.len(), 1);
    // The old `BUILTIN_TOOL_NAMES` const still contains `delegate`; the NEW
    // filterable list must not. The F2 message pins the full list.
    assert!(
        !es[0].message.contains("delegate"),
        "the known-tools list must not contain `delegate`: {}",
        es[0].message
    );
    assert_eq!(
        es[0].message,
        "unknown tool 'frobnicate'. Known tools: bash, bash_bg, doc_index, doc_search, fetch_page, fetch_url, file_create, file_insert, file_read, file_str_replace, list_directory, pdf_read, powershell, powershell_bg, think, todo, view_image, web_search."
    );
}

// ---------- E1/E2 "missing" sub-cases (serde missing-field → friendly) ----------

#[test]
fn e1_missing_providers_field() {
    let yaml = "profiles:\n  p:\n    agents:\n      a:\n        model: a\n";
    let es = errs(parse_global(yaml));
    assert_eq!(
        es.len(),
        1,
        "a missing providers: is E1, not a raw serde error: {es:?}"
    );
    assert_eq!(es[0].path, vec![key("providers")]);
    assert_eq!(
        es[0].message,
        "declare at least one provider with at least one model"
    );
}

#[test]
fn e2_missing_profiles_field() {
    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n      - name: m2\n        alias: b\n";
    let es = errs(parse_global(yaml));
    assert_eq!(
        es.len(),
        1,
        "a missing profiles: is E2, not a raw serde error: {es:?}"
    );
    assert_eq!(es[0].path, vec![key("profiles")]);
    assert_eq!(
        es[0].message,
        "declare at least one profile — the first one is the default"
    );
}

// ---------- shipped examples ----------

/// The runnable examples under `examples/` are 0.24 configs: they must go
/// through THE boundary (`Config::parse`) cleanly, so a layout change can
/// never leave a sample that PeakBot refuses to boot.
#[test]
fn shipped_examples_parse() {
    for (dir, first_profile, agent_with_team) in [
        ("agent-team", "software-team", "lead"),
        ("agent-dev-team", "software-team", "lead"),
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples")
            .join(dir)
            .join("config.yaml");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let cfg = Config::parse(
            Source {
                path: &path,
                text: &text,
            },
            None,
            &ctx(),
        )
        .unwrap_or_else(|e| panic!("example {dir} must parse: {e:?}"));
        let bound = cfg.catalog.default_bound();
        assert_eq!(bound.profile.name, first_profile, "{dir}: default profile");
        assert_eq!(
            bound.agent().main.name,
            agent_with_team,
            "{dir}: default agent"
        );
        assert!(
            !bound.agent().subagents.is_empty(),
            "{dir}: the default agent leads a team (delegate is on)"
        );
    }
}
