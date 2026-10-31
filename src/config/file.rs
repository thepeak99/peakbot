//! Parse types for the 0.24 config layout (spec §3, §5.2): the raw serde
//! shapes of the global `config.yaml` and the per-repo `.peakbot/config.yaml`.
//!
//! Every struct is `#[serde(deny_unknown_fields)]`. Only raw serde behaviour
//! lives here — the friendly rule messages (E1–E3, M1, S1, P3, …) are emitted
//! by `Config::parse` (A4). Consumed by `Config::parse` in the config cutover.

use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
    pub profiles: Named<ProfileEntry>,
    #[serde(default)]
    pub general: General,
}

/// The per-repo `.peakbot/config.yaml`: `profiles:` and nothing else (RP1).
/// A `remote` entry here is a parse error (RP7, checked in `Config::parse`).
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RepoFile {
    pub profiles: Named<ProfileEntry>,
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

/// One entry of the `profiles:` map: a local profile (agents, tools, …) or
/// a remote profile (`remote:` block) — design §4.2. Hand-written, NOT
/// `untagged` (its errors are useless): the kind is discriminated by the
/// PRESENCE of the `remote` key, and every field is deserialised through
/// the incoming `MapAccess` so `serde_path_to_error` keeps full path
/// fidelity (`profiles.<n>.remote.*`, `…ui.color`, …).
#[derive(Debug, Clone, PartialEq)]
// Parse-time type, a handful per config: boxing would only complicate the shape the design fixes.
#[allow(clippy::large_enum_variant)]
pub enum ProfileEntry {
    Local(ProfileDef),
    Remote(RemoteProfileDef),
}

impl ProfileEntry {
    /// The local definition, `None` for a remote entry.
    pub fn as_local(&self) -> Option<&ProfileDef> {
        match self {
            ProfileEntry::Local(def) => Some(def),
            ProfileEntry::Remote(_) => None,
        }
    }
}

/// The `remote:` (+ optional `ui.color`) entry shape: nothing else is
/// expressible on a remote profile (RP1).
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RemoteProfileDef {
    pub remote: RemoteDef,
    #[serde(default)]
    pub ui: RemoteUi,
}

/// The `remote:` block, raw strings: the §4.4 rules (URL shape, cookie-safe
/// token, CA pin, name rule) are applied in `Config::parse` via
/// `remote::parse_remote` (A4).
#[derive(Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RemoteDef {
    pub url: String,
    pub token: String,
    #[serde(default)]
    pub ca: Option<String>,
    pub profile: String,
    #[serde(default)]
    pub agent: Option<String>,
}

// Hand-rolled Debug (design §7): the token must not reach a
// `format!("{file:?}")` of the file layer.
impl fmt::Debug for RemoteDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteDef")
            .field("url", &self.url)
            .field("token", &"***")
            .field("ca", &self.ca)
            .field("profile", &self.profile)
            .field("agent", &self.agent)
            .finish()
    }
}

/// The only `ui` key a remote profile has: `ui.tabs` is the §4.4 row-1
/// exclusion, anything else is unknown.
#[derive(Debug, Deserialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct RemoteUi {
    #[serde(default)]
    pub color: Option<TabColor>,
}

/// `ui:` as BOTH kinds share it: `tabs` is local-only (row 1 checks it),
/// `color` is shared. Read into this union because the kind is only known
/// after the whole entry has been seen.
#[derive(Deserialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
struct UiShape {
    #[serde(default)]
    tabs: Option<NameFilter>,
    #[serde(default)]
    color: Option<TabColor>,
}

/// §4.4 row 1: `remote` plus a local-only key, naming the key.
fn remote_local_field_message(key: &str) -> String {
    format!(
        "a remote profile has only `remote` and `ui.color` — `{key}` belongs to the profile on the remote server"
    )
}

impl<'de> Deserialize<'de> for ProfileEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntryVisitor;

        impl<'de> serde::de::Visitor<'de> for EntryVisitor {
            type Value = ProfileEntry;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a profile entry (local fields, or a `remote` block)")
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut access: M,
            ) -> Result<ProfileEntry, M::Error> {
                let mut remote: Option<RemoteDef> = None;
                let mut ui: Option<UiShape> = None;
                let mut tools: Option<NameFilter> = None;
                let mut mcps: Option<NameFilter> = None;
                let mut fetch_url: Option<FetchUrlConfig> = None;
                let mut env: Option<Env> = None;
                let mut memory: Option<MemoryConfig> = None;
                let mut vector_db: Option<VectorDbConfig> = None;
                let mut agents: Option<Named<AgentDef>> = None;
                while let Some(key) = access.next_key::<String>()? {
                    match key.as_str() {
                        "remote" => remote = Some(access.next_value()?),
                        "ui" => ui = Some(access.next_value()?),
                        "tools" => tools = Some(access.next_value()?),
                        "mcps" => mcps = Some(access.next_value()?),
                        "fetch_url" => fetch_url = Some(access.next_value()?),
                        "env" => env = Some(access.next_value()?),
                        "memory" => memory = Some(access.next_value()?),
                        "vector_db" => vector_db = Some(access.next_value()?),
                        "agents" => agents = Some(access.next_value()?),
                        // deny_unknown_fields, at the entry (P1-style path).
                        _ => {
                            return Err(serde::de::Error::unknown_field(
                                &key,
                                &[
                                    "remote",
                                    "ui",
                                    "tools",
                                    "mcps",
                                    "fetch_url",
                                    "env",
                                    "memory",
                                    "vector_db",
                                    "agents",
                                ],
                            ));
                        }
                    }
                }

                match &remote {
                    // Row 1: a remote profile has only `remote` and `ui.color`.
                    // Fixed key order keeps the named key deterministic.
                    Some(remote) => {
                        let offending = [
                            (agents.is_some(), "agents"),
                            (tools.is_some(), "tools"),
                            (mcps.is_some(), "mcps"),
                            (fetch_url.is_some(), "fetch_url"),
                            (env.is_some(), "env"),
                            (memory.is_some(), "memory"),
                            (vector_db.is_some(), "vector_db"),
                            (ui.as_ref().is_some_and(|u| u.tabs.is_some()), "ui.tabs"),
                        ]
                        .into_iter()
                        .find(|(present, _)| *present)
                        .map(|(_, key)| key);
                        if let Some(key) = offending {
                            return Err(serde::de::Error::custom(remote_local_field_message(key)));
                        }
                        Ok(ProfileEntry::Remote(RemoteProfileDef {
                            remote: remote.clone(),
                            ui: RemoteUi {
                                color: ui.as_ref().and_then(|u| u.color),
                            },
                        }))
                    }
                    None => Ok(ProfileEntry::Local(ProfileDef {
                        tools: tools.unwrap_or_default(),
                        mcps: mcps.unwrap_or_default(),
                        fetch_url: fetch_url.unwrap_or_default(),
                        env: env.unwrap_or_default(),
                        memory: memory.unwrap_or_default(),
                        vector_db,
                        ui: ProfileUi {
                            tabs: ui.as_ref().and_then(|u| u.tabs.clone()).unwrap_or_default(),
                            color: ui.as_ref().and_then(|u| u.color),
                        },
                        // Same serde wording as the derived `ProfileDef`.
                        agents: agents
                            .ok_or_else(|| serde::de::Error::custom("missing field `agents`"))?,
                    })),
                }
            }
        }

        deserializer.deserialize_map(EntryVisitor)
    }
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

impl ConversationConfig {
    /// `storage_dir`, else `<data_local_dir>/peakbot/conversations` (the
    /// 0.23 default, unchanged).
    pub fn storage_dir_or_default(&self) -> PathBuf {
        self.storage_dir.clone().unwrap_or_else(|| {
            dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("peakbot")
                .join("conversations")
        })
    }
}

/// 0.24 per-profile `ui` block: `tabs` is a plain `NameFilter` (default =
/// allow all) instead of the old `Option<NameFilter>`.
#[derive(Debug, Deserialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ProfileUi {
    #[serde(default)]
    pub tabs: NameFilter,
    /// Accent for this profile's tabs; absent = derived from the name.
    #[serde(default)]
    pub color: Option<TabColor>,
}

/// The eight-slot per-profile accent palette (remote-profiles design §4.2).
/// The variant order IS the `derived` contract, and the lowercase serde
/// names are the config/wire contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabColor {
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Purple,
    Pink,
}

impl TabColor {
    /// `FNV-1a-32(name) % 8` over the palette order — a stable per-name
    /// colour, so the SPA never derives one (RP8).
    pub fn derived(name: &str) -> TabColor {
        const PALETTE: [TabColor; 8] = [
            TabColor::Red,
            TabColor::Orange,
            TabColor::Yellow,
            TabColor::Green,
            TabColor::Teal,
            TabColor::Blue,
            TabColor::Purple,
            TabColor::Pink,
        ];
        let mut hash: u32 = 2_166_136_261;
        for b in name.as_bytes() {
            hash ^= u32::from(*b);
            hash = hash.wrapping_mul(16_777_619);
        }
        PALETTE[(hash % 8) as usize]
    }
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

// ─────────────────────────────────────────────────────────────────────────────
// Tab colour (remote-profiles design T1, §4.2): `TabColor` + `derived()`.
// Written BEFORE the type exists — compile-RED until T1 lands.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tab_color_tests {
    use super::TabColor;

    /// The golden table is a stability contract (design §9, T1): these six
    /// names map to these colours on every machine, in every release.
    /// Each entry is `FNV-1a-32(name) % 8` over the enum order
    /// [Red, Orange, Yellow, Green, Teal, Blue, Purple, Pink].
    const GOLDEN: &[(&str, TabColor)] = &[
        ("dev", TabColor::Teal),
        ("work", TabColor::Red),
        ("home", TabColor::Purple),
        ("personal", TabColor::Blue),
        ("ops", TabColor::Green),
        ("design", TabColor::Pink),
    ];

    #[test]
    fn derived_golden_table_is_the_stability_contract() {
        for (name, expected) in GOLDEN {
            assert_eq!(TabColor::derived(name), *expected, "derived({name})");
        }
    }

    #[test]
    fn derived_is_deterministic_and_name_sensitive() {
        // Same name → same colour, every time (no clock, no random).
        assert_eq!(TabColor::derived("dev"), TabColor::derived("dev"));
        // The six golden names land on six DISTINCT colours: renaming a
        // profile is expected to change its colour (a collision is
        // overridable via `ui.color` — that is the point of eight slots).
        let mut seen: Vec<TabColor> = Vec::new();
        for (name, _) in GOLDEN {
            let color = TabColor::derived(name);
            assert!(
                !seen.contains(&color),
                "golden names must not collide: {name}"
            );
            seen.push(color);
        }
    }

    #[test]
    fn serde_is_lowercase_and_round_trips_all_eight() {
        let all = [
            TabColor::Red,
            TabColor::Orange,
            TabColor::Yellow,
            TabColor::Green,
            TabColor::Teal,
            TabColor::Blue,
            TabColor::Purple,
            TabColor::Pink,
        ];
        let names = [
            "red", "orange", "yellow", "green", "teal", "blue", "purple", "pink",
        ];
        for (color, name) in all.iter().zip(names) {
            let json = serde_json::to_string(color).expect("TabColor serialises");
            assert_eq!(json, format!("\"{name}\""), "the wire name is lowercase");
            assert_eq!(
                &serde_json::from_str::<TabColor>(&json).expect("round-trip"),
                color
            );
        }
    }

    #[test]
    fn serde_rejects_unknown_and_miscased_colour_names() {
        assert!(
            serde_json::from_str::<TabColor>("\"magenta\"").is_err(),
            "no such colour in the eight-slot palette"
        );
        // Case is part of the wire contract.
        assert!(serde_json::from_str::<TabColor>("\"Teal\"").is_err());
        assert!(serde_json::from_str::<TabColor>("\"teal \"").is_err());
    }
}
