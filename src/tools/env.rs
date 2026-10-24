//! The per-profile tool configuration, identical for the agent and its
//! subagents (spec §5.5): `ToolEnv` + `McpSource`.
//!
//! `ToolEnv::for_profile` pulls each field from its spec'd source:
//!
//! ```text
//! tools       ← profile.tools
//! fetch_url   ← profile.fetch_url
//! search      ← general.search
//! vector      ← VectorStore::open(&profile.vector); None when the profile has no `vector:`
//! mcp         ← shared handles + profile.mcps filter
//! base_env    ← profile.env (the bash base env)
//! timeouts    ← general.timeouts
//! max_turns   ← general.agent_max_turns
//! ```
//!
//! `bash_env` is exactly `profile.env ⊕ member.env` (member wins, spec §6.2)
//! — never a superset of the parent process env.
//!
//! **MCP test seam (stated, minimal).** `McpServerHandle` has no public
//! constructor (private `name`/`tools`/`service` fields plus a live service
//! connection), so a real server cannot be faked cheaply. The filter is
//! therefore tested at the tool-*name* level, through two `#[cfg(test)]`
//! seams that mirror the group shape `tools()` derives from the handles —
//! `(server name, tool names in per-server order)`:
//!
//! ```text
//! #[cfg(test)]
//! impl McpSource {
//!     /// Test-only constructor: no live handles, just the groups
//!     /// `tools()` would see, plus the filter to apply.
//!     pub fn for_test(groups: &[(String, Vec<String>)], filter: NameFilter) -> McpSource;
//!     /// Test-only projection: the tool names `tools()` would produce.
//!     pub fn tool_names(&self) -> Vec<String>;
//! }
//! ```

// No production caller yet: ToolEnv is wired into the providers in T7,
// so the module is dead code by design until then.
#![allow(dead_code)]

use std::sync::Arc;

use rig_core::tool::ToolDyn;

use crate::config::file::{Env, SearchConfig};
use crate::config::resolve::{Config, Member, ResolvedProfile};
use crate::config::{FetchUrlConfig, NameFilter, TimeoutsConfig};
use crate::vector::VectorStore;
use crate::{McpServerHandle, mcp_tools_for_profile};

/// The per-profile tool configuration, identical for the agent and its
/// subagents (spec §5.5).
pub struct ToolEnv {
    /// `profile.tools` — the built-in tool filter.
    pub tools: NameFilter,
    /// `profile.fetch_url`.
    pub fetch_url: FetchUrlConfig,
    /// `general.search` — absent = no `web_search`.
    pub search: Option<SearchConfig>,
    /// `VectorStore::open(&profile.vector)` via the process-wide handle
    /// registry; `None` = no doc tools.
    pub vector: Option<VectorStore>,
    /// Shared MCP handles + the profile's `mcps` filter.
    pub mcp: McpSource,
    /// `profile.env` — the bash base env.
    pub base_env: Env,
    /// `general.timeouts`.
    pub timeouts: TimeoutsConfig,
    /// `general.agent_max_turns`.
    pub max_turns: usize,
}

impl ToolEnv {
    /// Build the per-profile tool environment. `mcp` are the process's
    /// live MCP handles (shared with the subagents); the profile's
    /// `mcps` NameFilter is folded into the `McpSource`.
    pub fn for_profile(
        cfg: &Config,
        p: &ResolvedProfile,
        mcp: &Arc<Vec<McpServerHandle>>,
    ) -> ToolEnv {
        // `open` only constructs the embeddings client; if it ever fails the
        // profile degrades to "no doc tools" instead of failing the build.
        let vector = match &p.vector {
            Some(spec) => match VectorStore::open(spec) {
                Ok(store) => Some(store),
                Err(e) => {
                    tracing::warn!("profile '{}': vector store not built: {e}", p.name);
                    None
                }
            },
            None => None,
        };
        ToolEnv {
            tools: p.tools.clone(),
            fetch_url: p.fetch_url.clone(),
            search: cfg.general.search.clone(),
            vector,
            mcp: McpSource::new(Arc::clone(mcp), p.mcps.clone()),
            base_env: p.env.clone(),
            timeouts: cfg.general.timeouts.clone(),
            max_turns: cfg.general.agent_max_turns,
        }
    }

    /// `base_env` overlaid with `m.env` — the member wins on conflict
    /// (spec §6.2). The result is exactly that overlay, never the parent
    /// process env.
    pub fn bash_env(&self, m: &Member) -> Env {
        let mut env = self.base_env.clone();
        for (k, v) in &m.env {
            env.insert(k.clone(), v.clone());
        }
        env
    }
}

/// The profile's view of the process's live MCP servers: shared handles
/// plus the profile's `mcps` NameFilter.
pub struct McpSource {
    handles: Arc<Vec<McpServerHandle>>,
    filter: NameFilter,
    /// Test stand-in for live handles: `(server, tool names)` groups.
    #[cfg(test)]
    test_groups: Option<Vec<(String, Vec<String>)>>,
}

impl McpSource {
    fn new(handles: Arc<Vec<McpServerHandle>>, filter: NameFilter) -> Self {
        Self {
            handles,
            filter,
            #[cfg(test)]
            test_groups: None,
        }
    }

    /// The allowed servers' tools, built fresh per call (spec §5.5):
    /// `mcp_tools_for_profile` over the groups derived from the handles,
    /// with `Some(&filter)`; `McpTool: Clone` makes the rebuild cheap.
    pub fn tools(&self) -> Vec<Box<dyn ToolDyn>> {
        let groups = self
            .handles
            .iter()
            .map(|h| {
                (
                    h.name().to_string(),
                    h.tools()
                        .iter()
                        .cloned()
                        .map(|t| Box::new(t) as Box<dyn ToolDyn>)
                        .collect(),
                )
            })
            .collect();
        mcp_tools_for_profile(groups, Some(&self.filter))
    }
}

#[cfg(test)]
impl McpSource {
    /// Test-only constructor: no live handles, just the groups `tools()`
    /// would see, plus the filter to apply.
    pub fn for_test(groups: &[(String, Vec<String>)], filter: NameFilter) -> McpSource {
        McpSource {
            handles: Arc::new(Vec::new()),
            filter,
            test_groups: Some(groups.to_vec()),
        }
    }

    /// Test-only projection: the tool names `tools()` would produce — the
    /// same `NameFilter::allows` gate `mcp_tools_for_profile` applies.
    pub fn tool_names(&self) -> Vec<String> {
        let groups = self.test_groups.clone().unwrap_or_else(|| {
            self.handles
                .iter()
                .map(|h| {
                    (
                        h.name().to_string(),
                        h.tools().iter().map(|t| t.name()).collect(),
                    )
                })
                .collect()
        });
        groups
            .into_iter()
            .filter(|(name, _)| self.filter.allows(name))
            .flat_map(|(_, tools)| tools)
            .collect()
    }
}

#[cfg(test)]
mod t3_tests {
    use super::*;
    use crate::McpServerHandle;
    use crate::config::NameFilter;
    use crate::config::file::Env;
    use crate::config::resolve::{Config, ParseCtx, ResolvedProfile, Source};
    use std::sync::Arc;

    /// A valid 0.24 global config (same shape as the t1_tests fixture): one
    /// provider, one profile `dev` with a base `env:`, one agent whose member
    /// `env:` overlaps the profile env — the §6.2 overlay.
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
    env:
      PROFILE_ONLY: from-profile
      SHARED: profile-value
    agents:
      solo:
        model: sonnet
        env:
          SHARED: member-value
          MEMBER_ONLY: from-member
"#;

    fn ctx() -> ParseCtx<'static> {
        ParseCtx { known_skills: &[] }
    }

    /// `Config::parse` over the fixture; the path only anchors the global.
    fn parse() -> Config {
        Config::parse(
            Source {
                path: std::path::Path::new("/global/config.yaml"),
                text: GLOBAL_YAML,
            },
            None,
            &ctx(),
        )
        .expect("T3 fixture config must parse")
    }

    fn dev_profile(cfg: &Config) -> &ResolvedProfile {
        cfg.catalog
            .profile("dev")
            .expect("fixture profile `dev` must exist")
            .as_ref()
    }

    /// No MCP servers are faked: an empty shared handle list.
    fn empty_mcp_handles() -> Arc<Vec<McpServerHandle>> {
        Arc::new(Vec::new())
    }

    /// The expected overlay: profile env with the member env on top.
    fn expected_overlay() -> Env {
        let mut want = Env::new();
        want.insert("PROFILE_ONLY".to_string(), "from-profile".to_string());
        want.insert("SHARED".to_string(), "member-value".to_string());
        want.insert("MEMBER_ONLY".to_string(), "from-member".to_string());
        want
    }

    /// §6.2: `bash env = profile.env ⊕ agent.main.env` — the member's value
    /// wins on conflict; profile-only and member-only keys both survive.
    #[test]
    fn bash_env_member_overrides_profile() {
        let cfg = parse();
        let profile = dev_profile(&cfg);
        let env = ToolEnv::for_profile(&cfg, profile, &empty_mcp_handles());

        let got = env.bash_env(&profile.agents[0].main);

        assert_eq!(
            got,
            expected_overlay(),
            "member env overlays the profile env; the member wins on `SHARED`"
        );
    }

    /// The bash env is NOT the parent process env: the result is exactly
    /// `profile.env ⊕ member.env` — a variable set in the test process must
    /// not appear, and the map equals the configured overlay wholesale
    /// (wholesale equality already excludes every inherited variable).
    #[test]
    fn bash_env_is_not_parent_env() {
        // Serialized with the other env-mutating config tests; the guard
        // restores the process env on drop so a panic can't leak state.
        let _env_guard = crate::config::CONFIG_ENV_LOCK.lock().unwrap();
        struct LeakGuard;
        impl Drop for LeakGuard {
            fn drop(&mut self) {
                unsafe { std::env::remove_var("PEAKBOT_T3_LEAK_CHECK") };
            }
        }
        unsafe { std::env::set_var("PEAKBOT_T3_LEAK_CHECK", "leaked") };
        let _leak = LeakGuard;

        let cfg = parse();
        let profile = dev_profile(&cfg);
        let env = ToolEnv::for_profile(&cfg, profile, &empty_mcp_handles());

        let got = env.bash_env(&profile.agents[0].main);

        assert_eq!(
            got,
            expected_overlay(),
            "bash env is exactly profile.env ⊕ member.env — \
             never a superset of the parent process env"
        );
        assert!(
            !got.contains_key("PEAKBOT_T3_LEAK_CHECK"),
            "a variable from the parent process env must not reach the bash env"
        );
    }

    /// `profile.mcps: { only: [alpha] }` ⇒ only alpha's tools reach the
    /// agent, in per-server order. (Filter tested at the tool-name level —
    /// see the module doc for the stated seam.)
    #[test]
    fn mcp_source_only_filter() {
        let groups = vec![
            ("alpha".to_string(), vec!["alpha_tool".to_string()]),
            (
                "beta".to_string(),
                vec!["beta_tool".to_string(), "beta_tool_2".to_string()],
            ),
        ];
        let source = McpSource::for_test(
            &groups,
            NameFilter {
                only: vec!["alpha".to_string()],
                ..Default::default()
            },
        );

        assert_eq!(
            source.tool_names(),
            vec!["alpha_tool".to_string()],
            "only: [alpha] ⇒ only alpha's tools, per-server order kept"
        );
    }

    /// `profile.mcps: { enabled: false }` ⇒ no MCP tools at all (the servers
    /// keep running; only the tool list is empty — spec §5.5 / §3.3).
    #[test]
    fn mcp_source_enabled_false_yields_none() {
        let groups = vec![
            ("alpha".to_string(), vec!["alpha_tool".to_string()]),
            ("beta".to_string(), vec!["beta_tool".to_string()]),
        ];
        let source = McpSource::for_test(
            &groups,
            NameFilter {
                enabled: false,
                ..Default::default()
            },
        );

        assert!(
            source.tool_names().is_empty(),
            "enabled: false ⇒ no MCP tools at all"
        );
    }

    /// `ToolEnv::for_profile` on a profile with no `vector:` key yields
    /// `vector: None` (no doc tools, no store built) — and pulls every other
    /// field from its spec'd source (§5.5).
    #[test]
    fn for_profile_vector_none_when_absent() {
        let cfg = parse();
        let profile = dev_profile(&cfg);
        let handles = empty_mcp_handles();

        let env = ToolEnv::for_profile(&cfg, profile, &handles);

        assert!(
            env.vector.is_none(),
            "no `vector:` key ⇒ ToolEnv.vector is None"
        );

        // for_profile reads each field from its spec'd source (§5.5).
        assert_eq!(env.tools, profile.tools, "tools = profile.tools");
        assert_eq!(
            env.fetch_url, profile.fetch_url,
            "fetch_url = profile.fetch_url"
        );
        assert_eq!(
            env.search,
            cfg.general.search.clone(),
            "search = general.search"
        );
        assert_eq!(env.base_env, profile.env, "base_env = profile.env");
        assert_eq!(
            env.timeouts, cfg.general.timeouts,
            "timeouts = general.timeouts"
        );
        assert_eq!(
            env.max_turns, cfg.general.agent_max_turns,
            "max_turns = general.agent_max_turns"
        );
    }
}
