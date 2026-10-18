//! A3 — parse-struct RED tests (config-v2 spec §3 final YAML, §5.2 parse types).
//!
//! **Status: compile-fail until A3 lands.** `peakbot::config::file` does not
//! exist yet; this file targets the locked parse types from spec §5.2
//! (`ConfigFile`, `RepoFile`, `ProfileDef`, `AgentDef`, `SubagentDef`,
//! `General` and the variant types) and fails to compile — the RED state we
//! want.
//!
//! The variants are tested at their `peakbot::config::file::` paths (spec
//! §5.6): `WebConfig` with `bind: SocketAddr` + `token`, `SearchConfig`
//! (= SearXngConfig minus `enabled`), `VectorDbConfig` minus `enabled`,
//! `ConversationConfig` minus `auto_resume`/`max_conversations` and with
//! `Default`. Unchanged types are imported from their current home
//! (`peakbot::config::…`).
//!
//! Friendly messages (P3, E1–E3, M1, S1, …) belong to `Config::parse` (A4)
//! and are deliberately NOT tested here — raw serde behaviour only.

use std::net::SocketAddr;

use peakbot::config::file::{
    AgentDef, ConfigFile, ConversationConfig, General, ProfileDef, RepoFile, SearchConfig,
    SubagentDef, VectorDbConfig, WebConfig,
};
use peakbot::config::{
    ContextConfig, FetchUrlConfig, HttpConfig, McpAuth, McpTransportType, MemoryConfig, NameFilter,
    ProviderType, RetryConfig, TimeoutsConfig, UploadsConfig,
};

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/config_v2")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn parse_global(yaml: &str) -> Result<ConfigFile, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}

/// Minimal valid global config; `extra` is appended at the top level.
fn minimal_global(extra: &str) -> String {
    format!(
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m\n        alias: a\nprofiles:\n  one:\n    agents:\n      main:\n        model: a\n{extra}"
    )
}

fn filter(enabled: bool, disabled: &[&str], only: &[&str]) -> NameFilter {
    NameFilter {
        enabled,
        disabled: disabled.iter().map(|s| s.to_string()).collect(),
        only: only.iter().map(|s| s.to_string()).collect(),
    }
}

// ---------- §3.1 global example fixture ----------

#[test]
fn global_example_fixture_deserializes_and_keeps_order() {
    let cfg: ConfigFile = parse_global(&fixture("global_example.yaml"))
        .expect("the spec's global example must deserialize into ConfigFile");

    // providers (2) and mcps (2) survive as the unchanged types.
    assert_eq!(cfg.providers.len(), 2);
    assert_eq!(cfg.providers[0].name, "patchnotes");
    assert_eq!(cfg.providers[0].kind, ProviderType::OpenAI);
    assert_eq!(cfg.providers[0].models.len(), 3);
    let aliases: Vec<&str> = cfg.providers[0]
        .models
        .iter()
        .map(|m| m.alias.as_deref().unwrap())
        .collect();
    assert_eq!(aliases, ["sonnet", "opus", "qwen"]);
    assert_eq!(cfg.providers[1].name, "local");
    assert_eq!(cfg.providers[1].kind, ProviderType::Ollama);
    assert_eq!(cfg.providers[1].models[0].context_size, Some(32768));

    assert_eq!(cfg.mcps.len(), 2);
    assert_eq!(cfg.mcps[0].name, "playwright");
    assert_eq!(cfg.mcps[0].transport_type(), McpTransportType::Stdio);
    assert_eq!(cfg.mcps[0].command.as_deref(), Some("npx"));
    assert_eq!(
        cfg.mcps[0].args.as_deref(),
        Some(["@playwright/mcp@latest".to_string()].as_slice())
    );
    assert_eq!(cfg.mcps[1].name, "linear");
    assert_eq!(
        cfg.mcps[1].transport_type(),
        McpTransportType::StreamableHttp
    );
    assert_eq!(
        cfg.mcps[1].url.as_deref(),
        Some("https://mcp.linear.app/mcp")
    );
    assert_eq!(
        cfg.mcps[1].auth,
        Some(McpAuth::Oauth {
            client_id: None,
            client_secret: None,
            scopes: Vec::new()
        })
    );

    // ORDER: profiles dev, research; agents team, solo; subagents
    // researcher, senior, tester.
    let profile_names: Vec<&str> = cfg.profiles.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(profile_names, ["dev", "research"]);
    let dev = cfg.profiles.get("dev").expect("profile dev");
    let agent_names: Vec<&str> = dev.agents.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(agent_names, ["team", "solo"]);
    let team = dev.agents.get("team").expect("agent team");
    let sub_names: Vec<&str> = team.subagents.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(sub_names, ["researcher", "senior", "tester"]);
}

#[test]
fn global_example_agent_and_subagent_fields() {
    let cfg: ConfigFile = parse_global(&fixture("global_example.yaml")).expect("fixture parses");

    let dev = cfg.profiles.get("dev").expect("profile dev");
    let team = dev.agents.get("team").expect("agent team");
    assert_eq!(team.model.as_deref(), Some("sonnet"));
    assert!(
        team.persona
            .as_deref()
            .is_some_and(|p| p.starts_with("You are the tech lead.")
                && p.contains("review everything that comes back.")),
        "team persona must be the block scalar from the fixture"
    );
    assert_eq!(
        team.prompt.as_deref(),
        Some("Research first, then design, test, build.")
    );
    assert!(team.core_guidance, "core_guidance defaults to true");
    assert!(team.agents_md, "agents_md defaults to true for agents");

    let researcher = team
        .subagents
        .get("researcher")
        .expect("subagent researcher");
    assert_eq!(researcher.model.as_deref(), Some("qwen"));
    assert!(researcher.prompt.is_some());

    let senior = team.subagents.get("senior").expect("subagent senior");
    assert_eq!(senior.model.as_deref(), Some("opus"));
    assert_eq!(senior.skills, filter(true, &[], &["github", "gitea"]));

    let tester = team.subagents.get("tester").expect("subagent tester");
    assert_eq!(tester.model, None, "absent subagent model stays None");
    assert!(
        !tester.agents_md,
        "agents_md defaults to false for subagents"
    );

    let solo = dev.agents.get("solo").expect("agent solo");
    assert_eq!(solo.model.as_deref(), Some("sonnet"));
    assert!(
        solo.subagents.is_empty(),
        "absent subagents map stays empty"
    );

    let scholar = cfg
        .profiles
        .get("research")
        .expect("profile research")
        .agents
        .get("scholar")
        .expect("agent scholar");
    assert_eq!(scholar.model.as_deref(), Some("opus"));
    assert!(!scholar.core_guidance, "scholar sets core_guidance: false");
    assert!(
        scholar
            .persona
            .as_deref()
            .is_some_and(|p| p.starts_with("You are a meticulous research assistant."))
    );
    assert!(!scholar.agents_md, "scholar sets agents_md: false");
}

#[test]
fn global_example_profile_fields() {
    let cfg: ConfigFile = parse_global(&fixture("global_example.yaml")).expect("fixture parses");

    let dev = cfg.profiles.get("dev").expect("profile dev");
    assert_eq!(
        dev.tools,
        filter(true, &["powershell", "powershell_bg"], &[])
    );
    assert_eq!(dev.mcps, filter(true, &[], &["playwright", "linear"]));
    assert_eq!(dev.fetch_url, FetchUrlConfig { allow_post: false });
    assert_eq!(dev.env.get("GIT_PAGER"), Some(&"cat".to_string()));
    assert_eq!(
        dev.memory,
        MemoryConfig {
            enabled: true,
            threshold_bytes: 51200
        }
    );
    assert!(
        dev.vector_db.is_none(),
        "dev has no vector_db in the fixture"
    );
    assert_eq!(dev.ui.tabs, filter(true, &["bash"], &[]));

    let research = cfg.profiles.get("research").expect("profile research");
    assert_eq!(
        research.tools,
        filter(
            true,
            &[],
            &[
                "web_search",
                "fetch_page",
                "fetch_url",
                "think",
                "todo",
                "file_create"
            ]
        )
    );
    assert_eq!(
        research.mcps,
        filter(false, &[], &[]),
        "mcps: {{ enabled: false }} = no MCP tools"
    );
    assert_eq!(
        research.memory,
        MemoryConfig {
            enabled: false,
            threshold_bytes: 51200
        }
    );
    let v = research
        .vector_db
        .as_ref()
        .expect("research has a vector_db");
    assert_eq!(v.db_path, "~/research/research.db");
    assert_eq!(v.embeddings.base_url, "https://embed.example.com/v1");
    assert_eq!(v.embeddings.api_key.as_deref(), Some("emb-44aa9f2c"));
    assert_eq!(v.embeddings.model, "bge-m3");
    assert_eq!(v.embeddings.dimensions, 1024);
}

#[test]
fn global_example_general_block() {
    let cfg: ConfigFile = parse_global(&fixture("global_example.yaml")).expect("fixture parses");
    let g = &cfg.general;

    assert_eq!(
        g.web.bind,
        "127.0.0.1:7823".parse::<SocketAddr>().unwrap(),
        "web.bind must parse as a SocketAddr"
    );
    assert_eq!(g.web.token, None);
    assert!(!g.web.tls);
    assert_eq!(g.web.session_ttl_secs, 600);
    assert_eq!(g.web.reaper_tick_secs, 60);

    let s = g.search.as_ref().expect("fixture declares search");
    assert_eq!(s.base_url, "https://searx.example.com");
    assert_eq!(s.timeout_seconds, 30);
    assert_eq!(s.max_results, 10);
    assert_eq!(s.bearer_token, None);

    assert_eq!(
        g.context,
        ContextConfig {
            threshold: 0.8,
            keep_recent: 5,
            enabled: true,
            compaction_model: Some("qwen".to_string())
        }
    );
    assert_eq!(g.conversation.auto_save, true);
    assert_eq!(g.conversation.storage_dir, None);
    assert_eq!(g.agent_max_turns, 50);
    assert!(g.cost_tracking);
    assert_eq!(
        g.retry,
        RetryConfig {
            max_retries: 3,
            initial_delay_ms: 1000,
            max_delay_ms: 30000,
            backoff_factor: 2.0
        }
    );
    assert_eq!(
        g.timeouts,
        TimeoutsConfig {
            tool_secs: 1800,
            delegate_secs: 7200
        }
    );
    assert_eq!(
        g.http,
        HttpConfig {
            connect_timeout_secs: 30,
            read_timeout_secs: 1800
        }
    );
    assert_eq!(
        g.uploads,
        UploadsConfig {
            max_file_mb: 50,
            max_files: 10
        }
    );
}

// ---------- §3.2 repo example fixture ----------

#[test]
fn repo_example_fixture_deserializes_and_keeps_order() {
    let repo: RepoFile = serde_yaml::from_str(&fixture("repo_example.yaml"))
        .expect("the spec's repo example must deserialize into RepoFile");

    let profile_names: Vec<&str> = repo.profiles.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(profile_names, ["peakbot-dev", "dev"]);

    let pbd = repo
        .profiles
        .get("peakbot-dev")
        .expect("profile peakbot-dev");
    let agent_names: Vec<&str> = pbd.agents.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(agent_names, ["team", "quick"]);
    let team = pbd.agents.get("team").expect("agent team");
    let sub_names: Vec<&str> = team.subagents.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(sub_names, ["mid", "senior", "tester"]);

    // Env is a BTreeMap (spec §5.2): deterministic (sorted) order.
    let env_keys: Vec<&str> = pbd.env.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(env_keys, ["CARGO_TERM_COLOR", "RUST_LOG"]);
    assert_eq!(pbd.env.get("RUST_LOG"), Some(&"peakbot=info".to_string()));

    let v = pbd.vector_db.as_ref().expect("peakbot-dev has a vector_db");
    assert_eq!(v.db_path, "./.peakbot/vectors.db");
    assert_eq!(v.embeddings.api_key.as_deref(), Some("emb-77b01c9d"));

    assert_eq!(
        team.persona.as_deref(),
        Some("You lead the PeakBot team. Zen review before any code.")
    );
    let quick = pbd.agents.get("quick").expect("agent quick");
    assert_eq!(quick.model.as_deref(), Some("qwen"));

    let dev = repo
        .profiles
        .get("dev")
        .expect("profile dev (replaces global)");
    assert_eq!(dev.mcps, filter(true, &[], &["playwright", "linear"]));
    let agent_names: Vec<&str> = dev.agents.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(agent_names, ["solo"]);
    assert_eq!(
        dev.agents.get("solo").expect("agent solo").model.as_deref(),
        Some("qwen")
    );
}

// ---------- defaults ----------

#[test]
fn general_absent_gives_full_defaults() {
    let cfg = parse_global(&minimal_global("")).expect("minimal config must parse");
    assert_eq!(cfg.mcps.len(), 0, "absent mcps: defaults to []");
    let g = &cfg.general;

    assert_eq!(g.web.bind, "127.0.0.1:7823".parse::<SocketAddr>().unwrap());
    assert_eq!(g.web.token, None);
    assert!(!g.web.tls);
    assert_eq!(g.web.session_ttl_secs, 600);
    assert_eq!(g.web.reaper_tick_secs, 60);
    assert!(g.search.is_none(), "absent search: means no web_search");
    assert_eq!(
        g.context,
        ContextConfig {
            threshold: 0.8,
            keep_recent: 5,
            enabled: true,
            compaction_model: None
        }
    );
    assert_eq!(g.conversation.auto_save, true);
    assert_eq!(g.conversation.storage_dir, None);
    assert_eq!(g.agent_max_turns, 50);
    assert!(g.cost_tracking);
    assert_eq!(g.retry, RetryConfig::default());
    assert_eq!(g.timeouts, TimeoutsConfig::default());
    assert_eq!(g.http, HttpConfig::default());
    assert_eq!(g.uploads, UploadsConfig::default());
}

#[test]
fn agent_and_subagent_field_defaults() {
    let agent: AgentDef =
        serde_yaml::from_str("model: a\n").expect("a model-only agent must parse");
    assert_eq!(agent.persona, None);
    assert!(agent.core_guidance, "core_guidance defaults to true");
    assert_eq!(agent.prompt, None);
    assert_eq!(agent.skills, NameFilter::default());
    assert!(agent.env.is_empty());
    assert!(
        agent.subagents.is_empty(),
        "absent subagents map stays empty"
    );

    let sub: SubagentDef =
        serde_yaml::from_str("prompt: do the thing\n").expect("a prompt-only subagent must parse");
    assert_eq!(sub.model, None, "absent subagent model stays None");
    assert!(!sub.agents_md, "agents_md defaults to false for subagents");
    assert_eq!(sub.skills, NameFilter::default());
    assert!(sub.env.is_empty());
}

#[test]
fn agent_model_and_subagent_prompt_stay_none_when_absent() {
    // The parse structs keep `model` / `prompt` as `Option` ONLY so that
    // `Config::parse` (A4) can emit the friendly M1 / S1 messages. At the raw
    // serde layer, absence must stay `None` — not a serde error.
    let agent: AgentDef =
        serde_yaml::from_str("persona: hi\n").expect("agent without model must still parse");
    assert_eq!(agent.model, None);

    let sub: SubagentDef =
        serde_yaml::from_str("model: a\n").expect("subagent without prompt must still parse");
    assert_eq!(sub.prompt, None);
}

// ---------- web.bind is a SocketAddr ----------

#[test]
fn web_bind_parses_as_socket_addr() {
    let cfg = parse_global(&minimal_global(
        "general:\n  web:\n    bind: 0.0.0.0:8080\n",
    ))
    .expect("a valid bind address must parse");
    assert_eq!(
        cfg.general.web.bind,
        "0.0.0.0:8080".parse::<SocketAddr>().unwrap()
    );
}

#[test]
fn web_bind_rejects_non_socket_addr() {
    let err = parse_global(&minimal_global(
        "general:\n  web:\n    bind: not-a-socket\n",
    ))
    .expect_err("bind must parse as a socket address (P4)");
    assert!(!err.to_string().is_empty());
    let err = parse_global(&minimal_global(
        "general:\n  web:\n    bind: 1.2.3.4:99999\n",
    ))
    .expect_err("an out-of-range port is not a socket address (P4)");
    assert!(!err.to_string().is_empty());
}

// ---------- deny_unknown_fields everywhere (spec §1.3 / §5.2) ----------
// Raw serde wording only — the P1/P2/P3 rewrites happen in Config::parse (A4).

#[test]
fn config_file_rejects_unknown_top_level_key() {
    let err = parse_global(&minimal_global("pipelines: []\n"))
        .expect_err("unknown top-level key must be rejected");
    assert!(
        err.to_string().contains("pipelines"),
        "serde must name the offending field, got: {err}"
    );
}

#[test]
fn profile_def_rejects_unknown_key() {
    let err =
        serde_yaml::from_str::<ProfileDef>("agents:\n  main:\n    model: a\nmcp_servers: []\n")
            .expect_err("unknown profile key must be rejected");
    assert!(
        err.to_string().contains("mcp_servers"),
        "serde must name the offending field, got: {err}"
    );
}

#[test]
fn agent_def_rejects_unknown_key() {
    let err = serde_yaml::from_str::<AgentDef>("model: a\nsystem_prompt: x\n")
        .expect_err("unknown agent key must be rejected");
    assert!(
        err.to_string().contains("system_prompt"),
        "serde must name the offending field, got: {err}"
    );
}

#[test]
fn subagent_def_rejects_persona_key() {
    // Raw serde rejects `persona` on a subagent; A4 rewrites this exact
    // failure into the P3 message.
    let err = serde_yaml::from_str::<SubagentDef>("prompt: x\npersona: y\n")
        .expect_err("persona on a subagent must be rejected");
    assert!(
        err.to_string().contains("persona"),
        "serde must name the offending field, got: {err}"
    );
}

#[test]
fn general_rejects_unknown_key() {
    let err = serde_yaml::from_str::<General>("searxng:\n  base_url: x\n")
        .expect_err("the old searxng: key must be rejected (it is general.search now)");
    assert!(
        err.to_string().contains("searxng"),
        "serde must name the offending field, got: {err}"
    );
}

#[test]
fn web_config_rejects_unknown_key() {
    let err = serde_yaml::from_str::<WebConfig>("bind: 127.0.0.1:9\nhost: x\n")
        .expect_err("unknown web key must be rejected");
    assert!(
        err.to_string().contains("host"),
        "serde must name the offending field, got: {err}"
    );
}

#[test]
fn search_config_rejects_enabled_key() {
    // `enabled` was removed from search: presence of the block = on.
    let err = serde_yaml::from_str::<SearchConfig>("base_url: https://x\nenabled: true\n")
        .expect_err("search.enabled must be rejected");
    assert!(
        err.to_string().contains("enabled"),
        "serde must name the offending field, got: {err}"
    );
}

#[test]
fn vector_db_config_rejects_enabled_key() {
    // `enabled` was removed from vector_db: presence of the block = on.
    let err = serde_yaml::from_str::<VectorDbConfig>(
        "db_path: ./x.db\nembeddings:\n  base_url: https://x\n  model: m\n  dimensions: 8\nenabled: true\n",
    )
    .expect_err("vector_db.enabled must be rejected");
    assert!(
        err.to_string().contains("enabled"),
        "serde must name the offending field, got: {err}"
    );
}

#[test]
fn conversation_config_rejects_auto_resume_key() {
    // auto_resume / max_conversations had no readers and are removed.
    let err = serde_yaml::from_str::<ConversationConfig>("auto_save: true\nauto_resume: true\n")
        .expect_err("conversation.auto_resume must be rejected");
    assert!(
        err.to_string().contains("auto_resume"),
        "serde must name the offending field, got: {err}"
    );
}

#[test]
fn repo_file_rejects_unknown_top_level_key() {
    let err = serde_yaml::from_str::<RepoFile>(
        "general: {}\nprofiles:\n  one:\n    agents:\n      main:\n        model: a\n",
    )
    .expect_err("a repo file may only contain profiles:");
    assert!(
        err.to_string().contains("general"),
        "serde must name the offending field, got: {err}"
    );
}
