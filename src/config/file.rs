//! Parse types for the 0.24 config layout (spec §3, §5.2): the raw serde
//! shapes of the global `config.yaml` and the per-repo `.peakbot/config.yaml`.
//!
//! Every struct is `#[serde(deny_unknown_fields)]`. Only raw serde behaviour
//! lives here — the friendly rule messages (E1–E3, M1, S1, P3, …) are emitted
//! by `Config::parse` (A4). Consumed by `Config::parse` in the config cutover.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::Deserialize;

use super::named::Named;
use super::{
    ContextConfig, EmbeddingsConfig, FetchUrlConfig, HttpConfig, McpServerConfig, MemoryConfig,
    NameFilter, ProviderEntry, RetryConfig, TimeoutsConfig, UploadsConfig,
};

/// Environment variable map — a `BTreeMap` so prompt rendering and tests see
/// a deterministic (sorted) order.
pub type Env = BTreeMap<String, String>;

/// The global `config.yaml`: exactly four top-level blocks (spec §3.1).
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// REQUIRED, non-empty list; E1 is checked in `Config::parse`.
    pub providers: Vec<ProviderEntry>,
    /// Renamed from `mcp_servers`; boot-only.
    #[serde(default)]
    pub mcps: Vec<McpServerConfig>,
    /// REQUIRED ordered map; E2 is checked in `Config::parse`. First = default.
    pub profiles: Named<ProfileDef>,
    #[serde(default)]
    pub general: General,
}

/// The per-repo `.peakbot/config.yaml`: `profiles:` and nothing else (RP1).
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RepoFile {
    pub profiles: Named<ProfileDef>,
}

/// One profile: the self-contained capability set of a `(profile, agent)`
/// pair (spec §3.3).
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProfileDef {
    /// NameFilter over built-in tools; `delegate` is not filterable (F3).
    #[serde(default)]
    pub tools: NameFilter,
    /// NameFilter over the names in top-level `mcps`.
    #[serde(default)]
    pub mcps: NameFilter,
    #[serde(default)]
    pub fetch_url: FetchUrlConfig,
    /// Base bash env for the agent AND its subagents.
    #[serde(default)]
    pub env: Env,
    #[serde(default)]
    pub memory: MemoryConfig,
    /// Absent = no doc tools; presence = on (the old `enabled` is gone).
    #[serde(default)]
    pub vector_db: Option<VectorDbConfig>,
    #[serde(default)]
    pub ui: ProfileUi,
    /// REQUIRED ordered map; E3 is checked in `Config::parse`. First = default.
    pub agents: Named<AgentDef>,
}

/// One agent: today's orchestrator (spec §3.3). `model`/`prompt` stay
/// `Option` so `Config::parse` (A4) can emit the friendly M1/S1 messages.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentDef {
    /// Starting model alias; `/model` may switch it per conversation.
    #[serde(default)]
    pub model: Option<String>,
    /// Absent = the built-in persona.
    #[serde(default)]
    pub persona: Option<String>,
    /// `false` ⇒ the persona is the whole static head (A1 in `Config::parse`).
    #[serde(default = "default_true")]
    pub core_guidance: bool,
    /// Rendered as `# Instructions`.
    #[serde(default)]
    pub prompt: Option<String>,
    /// NameFilter over discovered skills (unknown names warn, F6).
    #[serde(default)]
    pub skills: NameFilter,
    #[serde(default = "default_true")]
    pub agents_md: bool,
    /// Overlays the profile env for THIS agent only.
    #[serde(default)]
    pub env: Env,
    /// Absent/empty = single agent, no `delegate` tool (derived, not stored).
    #[serde(default)]
    pub subagents: Named<SubagentDef>,
}

/// One subagent. No `persona` — deny_unknown_fields rejects it and A4
/// rewrites that failure into the P3 message.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SubagentDef {
    /// Absent = the parent agent's configured model.
    #[serde(default)]
    pub model: Option<String>,
    /// REQUIRED, non-blank (S1 in `Config::parse`).
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub skills: NameFilter,
    /// Default false, unlike the main agent.
    #[serde(default)]
    pub agents_md: bool,
    #[serde(default)]
    pub env: Env,
}

/// The `general` block (spec §3.3): every key defaulted.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct General {
    /// Boot-only; the ONE web-server config.
    #[serde(default)]
    pub web: WebConfig,
    /// Absent = no `web_search` (presence of the block = on).
    #[serde(default)]
    pub search: Option<SearchConfig>,
    #[serde(default = "default_context")]
    pub context: ContextConfig,
    /// Boot-only; the old `auto_resume`/`max_conversations` keys are dead.
    #[serde(default)]
    pub conversation: ConversationConfig,
    #[serde(default = "default_max_turns")]
    pub agent_max_turns: usize,
    #[serde(default = "default_true")]
    pub cost_tracking: bool,
    #[serde(default)]
    pub retry: RetryConfig,
    #[serde(default)]
    pub timeouts: TimeoutsConfig,
    /// Boot-only.
    #[serde(default)]
    pub http: HttpConfig,
    /// Boot-only.
    #[serde(default)]
    pub uploads: UploadsConfig,
}

impl Default for General {
    fn default() -> Self {
        Self {
            web: WebConfig::default(),
            search: None,
            context: default_context(),
            conversation: ConversationConfig::default(),
            agent_max_turns: default_max_turns(),
            cost_tracking: true,
            retry: RetryConfig::default(),
            timeouts: TimeoutsConfig::default(),
            http: HttpConfig::default(),
            uploads: UploadsConfig::default(),
        }
    }
}

/// 0.24 `general.web` (spec §3.3): the old reaper/TLS fields plus `bind`
/// (a real `SocketAddr`) and `token`.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    /// `CLI --bind > this > default 127.0.0.1:7823`.
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
    /// `CLI --token > $PEAKBOT_WEB_TOKEN > this`.
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub tls: bool,
    #[serde(default = "default_session_ttl_secs")]
    pub session_ttl_secs: u64,
    #[serde(default = "default_reaper_tick_secs")]
    pub reaper_tick_secs: u64,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            token: None,
            tls: false,
            session_ttl_secs: default_session_ttl_secs(),
            reaper_tick_secs: default_reaper_tick_secs(),
        }
    }
}

/// 0.24 `general.search` — SearXNG, the old `SearXngConfig` minus `enabled`
/// (presence of the block = on).
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    /// G6 (`must not be empty`) is checked in `Config::parse`.
    pub base_url: String,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_max_results")]
    pub max_results: u32,
    #[serde(default)]
    pub bearer_token: Option<String>,
}

/// 0.24 per-profile `vector_db` — the old struct minus `enabled` (presence
/// of the block = on).
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VectorDbConfig {
    pub db_path: String,
    pub embeddings: EmbeddingsConfig,
}

/// 0.24 `general.conversation` — the old struct minus the dead
/// `auto_resume`/`max_conversations` keys.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConversationConfig {
    #[serde(default = "default_true")]
    pub auto_save: bool,
    #[serde(default)]
    pub storage_dir: Option<PathBuf>,
}

// Hand-rolled, NOT derived: a derived `Default` would give
// `auto_save: false` (bool's default) and silently disable persistence.
impl Default for ConversationConfig {
    fn default() -> Self {
        Self {
            auto_save: true,
            storage_dir: None,
        }
    }
}

/// 0.24 per-profile `ui` block: `tabs` is a plain `NameFilter` (default =
/// allow all) instead of the old `Option<NameFilter>`.
#[derive(Debug, Deserialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ProfileUi {
    #[serde(default)]
    pub tabs: NameFilter,
}

fn default_bind() -> SocketAddr {
    "127.0.0.1:7823"
        .parse()
        .expect("static bind default is a valid SocketAddr")
}
fn default_session_ttl_secs() -> u64 {
    600
}
fn default_reaper_tick_secs() -> u64 {
    60
}
fn default_timeout_seconds() -> u64 {
    30
}
fn default_max_results() -> u32 {
    10
}
fn default_max_turns() -> usize {
    50
}
fn default_true() -> bool {
    true
}
/// `ContextConfig` has no `Default` impl (and adding one to the old module
/// is out of scope), so the serde default is spelled out here — the same
/// values the old per-field defaults produced.
fn default_context() -> ContextConfig {
    ContextConfig {
        threshold: 0.8,
        keep_recent: 5,
        enabled: true,
        compaction_model: None,
    }
}
