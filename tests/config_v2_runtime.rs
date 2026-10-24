//! RED integration tests for the 0.24 config runtime cutover — the
//! SELECTION / RELOAD / TOOLS / SUBAGENT items of spec §11.3 (plan task T6T;
//! written before the cutover, M1 only).
//!
//! The sibling `tests/config_v2_boot.rs` covers §11.3 items 3, 10, 14, 16,
//! 17 — not duplicated here. Item 9 (prompt recipes) is covered by
//! `tests/agent_prompt_tests.rs` (T4, committed in M1); items 11 and 15
//! need the web server / ported suites (M4 / T12); item 12 lives in
//! `tests/repl_tests.rs` (B10).
//!
//! Item → test map:
//! ```text
//!  - §11.3 item 1 (selection):
//!      `fresh_session_binds_first_profile_first_agent`   zero turns ⇒
//!                                                        first profile,
//!                                                        first agent
//!      `fresh_session_in_repo_binds_first_repo_profile`  repo profiles lead
//!                                                        the catalog and the
//!                                                        default
//!      `profile_command_picks_first_agent_and_resets_model`
//!                                                        `/profile x` ⇒ x's
//!                                                        first agent, model
//!                                                        = the agent's
//!                                                        configured alias
//!      `agent_command_switches_within_profile`           `/agent y` keeps
//!                                                        the profile
//!      `selection_refused_after_first_turn`              both axes refused
//!                                                        with the exact copy
//!      `profile_refused_under_pin_but_agent_switch_allowed`
//!                                                        pin: `/profile`
//!                                                        refused (the pin
//!                                                        itself applies),
//!                                                        `/agent` allowed
//!      `pipeline_is_not_a_selection_command`             `/pipeline`,
//!                                                        `/subagents` →
//!                                                        unknown command
//!  - §11.3 item 2 (model freedom):
//!      `model_switch_not_refused_for_subagent_agent`     the old lock is
//!                                                        gone; the rebuild
//!                                                        seam is Ok for a
//!                                                        subagent-bearing
//!                                                        bound and produces
//!                                                        `build_agent_prompt`
//!                                                        (ASSUMED API)
//!      `new_conversation_keeps_selection`                `/new` re-resolves
//!                                                        the saved selection
//!                                                        — no reset to
//!                                                        first/first
//!  - §11.3 item 4 (reload) + item 13 (bound stability):
//!      `reload_general_web_change_warns_restart`         `⚠ general.web
//!                                                        change ignored —
//!                                                        restart to apply.`
//!      `reload_broken_global_keeps_previous_config`      keep previous
//!                                                        config + warning
//!      `reload_broken_repo_falls_back_to_global_only`    global-only
//!                                                        catalog + warning
//!      `cd_with_turns_keeps_bound_and_old_persona`       with turns: the
//!                                                        Bound (and its
//!                                                        persona) is kept
//!                                                        exactly; only the
//!                                                        cwd parts refresh
//!      `reload_without_turns_reresolves_selection`       zero turns: the
//!                                                        selection is
//!                                                        re-resolved in the
//!                                                        fresh catalog — a
//!                                                        global persona edit
//!                                                        is picked up
//!      `reload_dropped_profile_with_turns_warns_and_keeps`
//!                                                        the keep-snapshot
//!                                                        warning, verbatim
//!      `reload_dropped_profile_without_turns_uses_default`
//!                                                        `ℹ Using <p> › <a>
//!                                                        (default here).`
//!  - §11.3 item 5 (per-repo shadowing):
//!      `repo_profile_replaces_global_in_tools_and_env`   the repo `dev`
//!                                                        replaces the global
//!                                                        `dev`: the global's
//!                                                        env/tools are
//!                                                        absent from the
//!                                                        built agent
//!  - §11.3 item 6 (subagents get the same tools and MCP):
//!      `subagent_gets_same_mcp_tools_and_builtins_minus_delegate`
//!      `mcps_disabled_profile_gives_neither_mcp_tools`
//!      `subagent_bash_env_is_profile_plus_subagent_env`  profile.env ⊕
//!                                                        sub.env, NOT the
//!                                                        main agent's env
//!  - §11.3 item 7 (delegate registration):
//!      `delegate_tool_registered_iff_agent_has_subagents`
//!      `tools_allowlist_cannot_remove_delegate`          F3
//!  - §11.3 item 8 (vector registry):
//!      `vector_fixed_path_shares_one_handle_across_rebuilds`
//!                                                        one redb handle per
//!                                                        Fixed path — a
//!                                                        reload's rebuilt
//!                                                        `VectorStore` does
//!                                                        not reopen the file
//!                                                        (ASSUMED API)
//!  - §11.3 item 13 (new web socket):
//!      `fresh_web_socket_load_sees_profile_added_after_boot`
//!                                                        a fresh
//!                                                        `Config::load` sees
//!                                                        edits saved since
//!                                                        boot
//!  - plan T10 (UI state/wire — the seams items 1 and 13 wire through):
//!      `ui_action_select_wire_roundtrip`                 `UiAction::Select`
//!                                                        (ASSUMED API)
//!      `appstate_settings_available_derived_from_pin`    `AppState`
//!                                                        catalog/selection/
//!                                                        profile_pinned +
//!                                                        `settings_available`
//!                                                        (ASSUMED API)
//! ```
//!
//! ASSUMED API — written against the config-v2-plan.md T7/T8a/T8c/T10
//! contracts; this file will not compile until the cutover lands (plan:
//! "Won't compile until T12"). Precise signatures — the builders' contract:
//!  1. `peakbot::tools::env::main_agent_tool_names(env: &ToolEnv,
//!     agent: &ResolvedAgent) -> Vec<String>` — T7: the tool names the main
//!     agent build registers: builtins per spec §6.2 (`BUILTIN` minus
//!     `delegate`, `∩ profile.tools.allows`, provider gates) + `env.mcp`
//!     tool names + `"delegate"` iff `!agent.subagents.is_empty()`. Pure,
//!     no I/O; `create_provider` derives its registration from it.
//!  2. `peakbot::tools::env::sub_agent_tool_names(env: &ToolEnv,
//!     sub: &Subagent) -> Vec<String>` — T7: the subagent build's tool
//!     names — the SAME builtins as the main agent (same `profile.tools`),
//!     never `delegate` (no nesting), plus the SAME `env.mcp` tools.
//!     **Visibility (T3/T7):** `crate::tools` is a PRIVATE module today
//!     (selected root re-exports only); the cutover must make
//!     `peakbot::tools::env::{ToolEnv, McpSource}` importable from
//!     integration tests (e.g. `pub mod tools;` in lib.rs) — nothing in
//!     this file can compile without it.
//!  3. `peakbot::tools::env::McpSource::for_test(groups: &[(String,
//!     Vec<String>)], filter: NameFilter) -> McpSource` and
//!     `McpSource::tool_names(&self) -> Vec<String>` — T3's `#[cfg(test)]`
//!     test seams, lifted to `pub` (no `#[cfg(test)]`) so the §11.3 item 6
//!     integration tests can inject MCP tool names without a live server.
//!  4. `peakbot::RebuildContext` (T8a) — replaces the pre-cutover
//!     `RebuildContext`:
//!     ```rust
//!     pub struct RebuildContext {
//!         pub config: Arc<Config>,
//!         pub bound: Bound,
//!         pub mcp_handles: Arc<Vec<McpServerHandle>>,
//!         pub shell_kind: Option<ShellKind>,
//!         pub skills: SkillRegistry,
//!         pub todo_tool: Option<TodoTool>,
//!         pub profile_pin: Option<String>,
//!     }
//!     impl RebuildContext { pub fn repo_mode(&self) -> RepoMode; }
//!     // `repo_mode`: `Ignore` under a pin, else `Include`.
//!     ```
//!  5. `peakbot::rebuild_agent_for_resolved(model: &ResolvedModel,
//!     bound: &Bound, skills: &SkillRegistry, shell: Option<ShellKind>,
//!     cwd: &Path) -> Result<String, String>` — T8a: the `/model` rebuild
//!     seam. The 0.23 model lock (`model_locked_message`) is deleted: this
//!     never refuses for selection reasons (a subagent-bearing bound
//!     included). The `Ok` payload is the rebuilt system prompt — exactly
//!     `build_agent_prompt(bound.agent(), bound.profile, skills, shell,
//!     cwd)` (T8a: "move to build_agent_prompt"); the model swap is the
//!     provider-side rebuild.
//!  6. `peakbot::reload_session_config(ctx: &mut RebuildContext,
//!     sm: &Arc<StateManager>, target_cwd: &Path) -> Vec<String>` — T8c /
//!     spec §6.7 steps 1–8: repo-file migration, `Config::load`,
//!     `boot_only_diff` restart warnings, adoption (`ctx.config = fresh`,
//!     `sm.set_catalog`), `rebind` (Bound kept with turns, re-resolved
//!     with zero turns), `sm.set_selection` — returning the system
//!     messages (exact copy strings per §6.7).
//!  7. `peakbot::state::StateManager` (T10) gains:
//!     `pub fn set_catalog(&self, infos: Vec<ProfileInfo>)`,
//!     `pub fn selection(&self) -> Selection`,
//!     `pub fn set_selection(&self, sel: Selection)` — the live
//!     `AppState` mirrors (step 5/7 of §6.7).
//!  8. `peakbot::UiAction::Select(Selection)` (T10) — the inbound
//!     `select` frame's action; external-tag serde wire shape
//!     `{"Select": {"profile": …, "agent": …}}`.
//!  9. `peakbot::ui::app_state::AppState` (T10) gains
//!     `pub catalog: Vec<ProfileInfo>`, `pub selection: Selection`,
//!     `pub profile_pinned: bool`; the serializer emits
//!     `settings_available = !profile_pinned`.
//! 10. `peakbot::vector::VectorStore::handle_for_test(&self,
//!     session_cwd: &Path) -> Result<Arc<VectorDB>, VectorStoreError>`
//!     (T2's `pub(crate)` accessor, made `pub`) — the `Arc::ptr_eq`
//!     accessor spec §11.3 item 8 asks for from an integration test.
//!
//! Gaps (not testable from a lib integration test at M1):
//!  - T8b's `handle_select(Selection)` runtime wiring (rollback on a
//!    failed rebuild) — needs the `AgentRunner` loop; the decision logic
//!    is covered by the `resolve_selection_choice` tests above.
//!  - The runtime "resets the model" write (the conversation's `model`
//!    field on a selection) — the pure seam asserts the applied model
//!    alias; conversation JSON persistence is the boot file's scope.
//!  - The full `/model` provider rebuild (`UiAction::SwitchModel` →
//!    `create_provider` on the new model) — the seam pinned above proves
//!    the lock is gone; the rebuild itself is T7/T8a.
//!  - The `sm.set_catalog` effect has no planned getter; the `AppState`
//!    shape is pinned by `appstate_settings_available_derived_from_pin`
//!    and the T10 wire snapshot.
//!
//! No network. Env isolation: `XDG_CONFIG_HOME` under a file-local lock
//! (the boot file's lock is file-private, so this crate keeps its own).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use peakbot::config::resolve::{
    Bound, Config, DbPathSpec, Origin, ParseCtx, ProfileInfo, RepoMode, Selection, Source,
    VectorSpec,
};
use peakbot::config::{NameFilter, ResolvedModel, get_config_dir};
use peakbot::selection::{
    SelectChoice, SelectSubmission, classify_select, resolve_saved_selection,
    resolve_selection_choice,
};
use peakbot::state::StateManager;
use peakbot::tools::env::{McpSource, ToolEnv};
use peakbot::ui::app_state::AppState;
use peakbot::vector::VectorStore;
use peakbot::{McpServerHandle, UiAction};

// ── env isolation (same pattern as tests/config_v2_boot.rs) ────────────────

static ENV_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

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

/// `XDG_CONFIG_HOME` pointed at a fresh tempdir, plus the session cwd and a
/// second dir (the `/cd` target). Field order is drop order: restore the
/// env, delete the dirs, release the lock.
struct TempHome {
    _guard: XdgGuard,
    _xdg: tempfile::TempDir,
    _cwd: tempfile::TempDir,
    _cwd2: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl TempHome {
    fn new() -> Self {
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _xdg = tempfile::tempdir().expect("xdg tempdir");
        let _guard = XdgGuard::new(_xdg.path());
        let _cwd = tempfile::tempdir().expect("cwd tempdir");
        let _cwd2 = tempfile::tempdir().expect("cwd2 tempdir");
        Self {
            _guard,
            _xdg,
            _cwd,
            _cwd2,
            _lock,
        }
    }

    fn config_dir(&self) -> PathBuf {
        get_config_dir().expect("config dir under XDG_CONFIG_HOME")
    }

    fn cwd(&self) -> &Path {
        self._cwd.path()
    }

    fn cwd2(&self) -> &Path {
        self._cwd2.path()
    }

    fn global_path(&self) -> PathBuf {
        self.config_dir().join("config.yaml")
    }

    fn repo_path(&self) -> PathBuf {
        self.cwd().join(".peakbot").join("config.yaml")
    }

    fn write_global(&self, text: &str) {
        let dir = self.config_dir();
        std::fs::create_dir_all(&dir).expect("config dir");
        std::fs::write(self.global_path(), text).expect("write global config");
    }

    fn write_repo(&self, text: &str) {
        let dir = self.cwd().join(".peakbot");
        std::fs::create_dir_all(&dir).expect(".peakbot dir");
        std::fs::write(self.repo_path(), text).expect("write repo config");
    }
}

fn ctx() -> ParseCtx<'static> {
    ParseCtx { known_skills: &[] }
}

fn parse_global(text: &str) -> Config {
    Config::parse(
        Source {
            path: Path::new("/global/config.yaml"),
            text,
        },
        None,
        &ctx(),
    )
    .expect("0.24 global parses")
}

fn parse_global_repo(global: &str, repo: &str) -> Config {
    Config::parse(
        Source {
            path: Path::new("/global/config.yaml"),
            text: global,
        },
        Some(Source {
            path: Path::new("/repo/.peakbot/config.yaml"),
            text: repo,
        }),
        &ctx(),
    )
    .expect("global + repo parses")
}

// ── fixture texts ───────────────────────────────────────────────────────────

/// A 0.24 global: two model aliases, one top-level MCP entry (`srv`), a
/// `dev` profile (agent `team` with one subagent `researcher` + agent
/// `solo`) and a `research` profile with MCP disabled.
const GLOBAL_TEAM: &str = "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - { name: anthropic/claude-sonnet-4.5, alias: sonnet }\n      - { name: anthropic/claude-opus-4.1, alias: opus }\nmcps:\n  - name: srv\n    command: npx\n    args: [\"@stub/mcp\"]\nprofiles:\n  dev:\n    env: { PROFILE_ENV: p }\n    agents:\n      team:\n        model: sonnet\n        persona: Team persona alpha.\n        env: { AGENT_ENV: a }\n        subagents:\n          researcher:\n            model: sonnet\n            prompt: Gather background.\n            env: { SUB_ENV: s }\n      solo:\n        model: opus\n  research:\n    mcps: { enabled: false }\n    agents:\n      scholar:\n        model: sonnet\n";

/// A 0.24 global for the reload matrix: one profile (`dev` › `main`) with a
/// persona, and a `general.web.bind` to flip.
const RELOAD_GLOBAL: &str = "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - { name: anthropic/claude-sonnet-4.5, alias: sonnet }\nprofiles:\n  dev:\n    agents:\n      main:\n        model: sonnet\n        persona: Old persona alpha.\ngeneral:\n  web:\n    bind: 127.0.0.1:7823\n";

/// The same global with the `dev` profile dropped (only `research` left).
const RELOAD_GLOBAL_NO_DEV: &str = "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - { name: anthropic/claude-sonnet-4.5, alias: sonnet }\nprofiles:\n  research:\n    agents:\n      scholar:\n        model: sonnet\n";

/// A 0.24 global whose `dev` profile carries an env var and a tools
/// blocklist the repo `dev` does not have.
const SHADOW_GLOBAL: &str = "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - { name: anthropic/claude-sonnet-4.5, alias: sonnet }\nprofiles:\n  dev:\n    env: { GLOBAL_ONLY: g }\n    tools: { disabled: [bash] }\n    agents:\n      main:\n        model: sonnet\n";

/// A repo file replacing the global `dev` (no merge — §6.8).
const SHADOW_REPO: &str = "profiles:\n  dev:\n    env: { REPO_ONLY: r }\n    agents:\n      main:\n        model: sonnet\n";

// ── item 1: selection ───────────────────────────────────────────────────────

/// §11.3 item 1: a fresh session (zero turns) binds the first profile's
/// first agent — `catalog.default_bound()`, exactly as spec §6.5 says.
#[test]
fn fresh_session_binds_first_profile_first_agent() {
    let cfg = parse_global(GLOBAL_TEAM);
    let bound = cfg.catalog.default_bound();
    assert_eq!(bound.profile.name, "dev", "the first profile");
    assert_eq!(bound.agent().main.name, "team", "its first agent");
    assert_eq!(bound.agent_index, 0);
    assert_eq!(
        bound.selection(),
        Selection {
            profile: "dev".to_string(),
            agent: "team".to_string(),
            ..Default::default()
        },
        "the selection is derived from the bound"
    );
}

/// §11.3 item 1: in a repo with a repo file, the fresh session is the FIRST
/// REPO profile — repo profiles lead the catalog (repo first, then the
/// unshadowed globals).
#[test]
fn fresh_session_in_repo_binds_first_repo_profile() {
    let repo = "profiles:\n  repo-dev:\n    agents:\n      main:\n        model: sonnet\n  repo-ops:\n    agents:\n      main:\n        model: sonnet\n";
    let cfg = parse_global_repo(GLOBAL_TEAM, repo);

    let names: Vec<&str> = cfg.catalog.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        ["repo-dev", "repo-ops", "dev", "research"],
        "repo profiles lead, in file order, then the globals"
    );

    let bound = cfg.catalog.default_bound();
    assert_eq!(
        bound.profile.name, "repo-dev",
        "the first repo profile is the default here"
    );
    assert!(
        matches!(bound.profile.origin, Origin::Repo { .. }),
        "the default is repo-origin"
    );
}

/// §11.3 item 1: `/profile x` picks x's FIRST agent and resets the model to
/// that agent's configured alias.
#[test]
fn profile_command_picks_first_agent_and_resets_model() {
    let cfg = parse_global(GLOBAL_TEAM);
    let cur = cfg
        .catalog
        .start_bound(None)
        .expect("the starting bound (dev/team)");
    assert_eq!(cur.agent().main.name, "team", "fixture sanity");

    match resolve_selection_choice(
        &cfg.catalog,
        &cur,
        SelectSubmission::Profile("research".into()),
        false,
        None,
    ) {
        SelectChoice::Apply(bound) => {
            assert_eq!(bound.profile.name, "research");
            assert_eq!(
                bound.agent_index, 0,
                "selecting a profile picks its first agent"
            );
            assert_eq!(bound.agent().main.name, "scholar");
            assert_eq!(
                bound.agent().main.model.alias,
                "sonnet",
                "the model resets to the agent's configured model"
            );
        }
        other => panic!("expected Apply, got {other:?}"),
    }
}

/// §11.3 item 1: `/agent y` switches within the current profile (the profile
/// object is kept — same `Arc`).
#[test]
fn agent_command_switches_within_profile() {
    let cfg = parse_global(GLOBAL_TEAM);
    let cur = cfg
        .catalog
        .start_bound(None)
        .expect("the starting bound (dev/team)");

    match resolve_selection_choice(
        &cfg.catalog,
        &cur,
        SelectSubmission::Agent("solo".into()),
        false,
        None,
    ) {
        SelectChoice::Apply(bound) => {
            assert!(
                Arc::ptr_eq(&cur.profile, &bound.profile),
                "the profile is kept on an agent switch"
            );
            assert_eq!(bound.agent().main.name, "solo");
            assert_eq!(bound.agent().main.model.alias, "opus");
        }
        other => panic!("expected Apply, got {other:?}"),
    }
}

/// §11.3 item 1: both axes are refused after the first turn, with the exact
/// copy strings (spec §6.6).
#[test]
fn selection_refused_after_first_turn() {
    let cfg = parse_global(GLOBAL_TEAM);
    let cur = cfg.catalog.start_bound(None).expect("the starting bound");

    match resolve_selection_choice(
        &cfg.catalog,
        &cur,
        SelectSubmission::Profile("research".into()),
        true,
        None,
    ) {
        SelectChoice::Refuse(msg) => assert_eq!(
            msg,
            "the profile is locked once the conversation has started — /new for a fresh one."
        ),
        other => panic!("expected Refuse, got {other:?}"),
    }

    match resolve_selection_choice(
        &cfg.catalog,
        &cur,
        SelectSubmission::Agent("solo".into()),
        true,
        None,
    ) {
        SelectChoice::Refuse(msg) => assert_eq!(
            msg,
            "the agent is locked once the conversation has started — /new for a fresh one."
        ),
        other => panic!("expected Refuse, got {other:?}"),
    }
}

/// §11.3 item 1: under a `--profile` pin, `/profile <other>` is refused with
/// the pin message (selecting the pinned profile itself applies), while
/// `/agent` still works within the pinned profile.
#[test]
fn profile_refused_under_pin_but_agent_switch_allowed() {
    let cfg = parse_global(GLOBAL_TEAM);
    let cur = cfg
        .catalog
        .start_bound(Some("dev"))
        .expect("the pinned bound");

    match resolve_selection_choice(
        &cfg.catalog,
        &cur,
        SelectSubmission::Profile("research".into()),
        false,
        Some("dev"),
    ) {
        SelectChoice::Refuse(msg) => assert_eq!(
            msg,
            "the profile is pinned by --profile dev — restart without --profile to switch."
        ),
        other => panic!("expected Refuse, got {other:?}"),
    }

    // The pinned profile itself applies.
    match resolve_selection_choice(
        &cfg.catalog,
        &cur,
        SelectSubmission::Profile("dev".into()),
        false,
        Some("dev"),
    ) {
        SelectChoice::Apply(bound) => assert_eq!(bound.profile.name, "dev"),
        other => panic!("selecting the pinned profile applies, got {other:?}"),
    }

    // Agent switches are allowed under a pin.
    match resolve_selection_choice(
        &cfg.catalog,
        &cur,
        SelectSubmission::Agent("solo".into()),
        false,
        Some("dev"),
    ) {
        SelectChoice::Apply(bound) => {
            assert_eq!(bound.profile.name, "dev");
            assert_eq!(bound.agent().main.name, "solo");
        }
        other => panic!("agent switches are allowed under a pin, got {other:?}"),
    }
}

/// §11.3 item 1: `/pipeline` (and the removed `/subagents`) are no longer
/// selection commands — they hit the generic unknown-command path.
#[test]
fn pipeline_is_not_a_selection_command() {
    assert_eq!(
        classify_select("/pipeline dev-team"),
        None,
        "`/pipeline <name>` is not a selection command"
    );
    assert_eq!(
        classify_select("/pipeline"),
        None,
        "bare `/pipeline` is not a selection command"
    );
    assert_eq!(
        classify_select("/subagents"),
        None,
        "the removed `/subagents` hits the unknown-command path too"
    );
}

// ── item 2: model freedom ───────────────────────────────────────────────────

/// §11.3 item 2: `/model <alias>` works with a subagent-bearing agent
/// selected — the old lock (`model_locked_message`) is gone. The T8a rebuild
/// seam is `Ok` for such a bound, and its output is exactly
/// `build_agent_prompt` for that bound.
/// ASSUMED API: `peakbot::rebuild_agent_for_resolved` (see header, #5).
#[test]
fn model_switch_not_refused_for_subagent_agent() {
    let cfg = parse_global(GLOBAL_TEAM);
    let bound = cfg
        .catalog
        .start_bound(None)
        .expect("the starting bound (dev/team)");
    assert!(
        !bound.agent().subagents.is_empty(),
        "fixture sanity: team has a subagent"
    );

    let other = cfg.registry.resolve("opus").expect("the opus alias");
    let cwd = Path::new("/work");
    let skills = peakbot::skills::SkillRegistry::default();

    let prompt = peakbot::rebuild_agent_for_resolved(other, &bound, &skills, None, cwd).expect(
        "a model switch is never refused for a subagent-bearing agent (the old lock is gone)",
    );
    assert_eq!(
        prompt,
        peakbot::build_agent_prompt(bound.agent(), &bound.profile, &skills, None, cwd),
        "the rebuild produces build_agent_prompt's output"
    );

    // A subagent-free agent is free too.
    let solo = Bound {
        profile: Arc::clone(&bound.profile),
        agent_index: 1,
    };
    assert!(
        solo.agent().subagents.is_empty(),
        "fixture sanity: solo has no subagents"
    );
    assert!(
        peakbot::rebuild_agent_for_resolved(other, &solo, &skills, None, cwd).is_ok(),
        "the model switch is free for a solo agent as well"
    );
}

/// §11.3 item 2: `/new` keeps `(profile, agent, model)` — a fresh
/// conversation re-resolves the saved selection (zero turns) instead of
/// resetting to first/first.
#[test]
fn new_conversation_keeps_selection() {
    let cfg = parse_global(GLOBAL_TEAM);
    let cur = cfg
        .catalog
        .start_bound(None)
        .expect("the starting bound (dev/team)");
    let sel = cur.selection();

    // The zero-turn reload rule (§6.7 step 6) re-resolves
    // `bound.selection()` — the same resume path `/new` runs.
    let (bound, warnings) = resolve_saved_selection(
        &cfg.catalog,
        &cfg.registry,
        Some(sel.profile.as_str()),
        Some(sel.agent.as_str()),
        &std::collections::BTreeMap::new(),
        None,
    );
    assert!(
        warnings.is_empty(),
        "no fallback: the selection is configured: {warnings:?}"
    );
    assert_eq!(bound.selection(), sel, "/new keeps (profile, agent)");
    assert!(
        bound.selection().subagent_models.is_empty(),
        "no saved overrides ⇒ none applied"
    );
    assert_eq!(
        bound.agent().main.model.alias,
        "sonnet",
        "…and the model is the agent's configured model"
    );
}

// ── item 4 + 13: reload semantics and bound stability ───────────────────────

/// ASSUMED API (T8a, see header #4): construct the cutover `RebuildContext`.
fn rebuild_ctx(config: Config, bound: Bound, pin: Option<String>) -> peakbot::RebuildContext {
    peakbot::RebuildContext {
        config: Arc::new(config),
        bound,
        mcp_handles: Arc::new(Vec::new()),
        shell_kind: None,
        skills: peakbot::skills::SkillRegistry::default(),
        todo_tool: None,
        profile_pin: pin,
    }
}

/// §11.3 item 4: an edit to `general.web` (a boot-only block) → the exact
/// restart warning; the new config is still adopted.
/// ASSUMED API: `peakbot::reload_session_config` (see header, #6).
#[test]
fn reload_general_web_change_warns_restart() {
    let home = TempHome::new();
    home.write_global(RELOAD_GLOBAL);
    let boot = Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("boot");
    let bound = boot.catalog.start_bound(None).expect("the default bound");
    let mut rc = rebuild_ctx(boot, bound, None);
    let sm = StateManager::new_arc();

    home.write_global(&RELOAD_GLOBAL.replace("127.0.0.1:7823", "127.0.0.1:9999"));

    let warnings = peakbot::reload_session_config(&mut rc, &sm, home.cwd());
    assert_eq!(
        warnings,
        vec!["⚠ general.web change ignored — restart to apply."],
        "the exact restart warning for the changed boot-only key, and nothing else"
    );
    assert_eq!(
        rc.config.general.web.bind.to_string(),
        "127.0.0.1:9999",
        "the new config is adopted even though the web change needs a restart"
    );
}

/// §11.3 item 4: a broken global → the previous config is kept + the exact
/// warning shape; the bound is untouched.
#[test]
fn reload_broken_global_keeps_previous_config() {
    let home = TempHome::new();
    home.write_global(RELOAD_GLOBAL);
    let boot = Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("boot");
    let bound = boot.catalog.start_bound(None).expect("the default bound");
    let mut rc = rebuild_ctx(boot, bound, None);
    let sm = StateManager::new_arc();

    let prev_config = Arc::clone(&rc.config);
    let prev_sel = rc.bound.selection();
    home.write_global(&format!("{RELOAD_GLOBAL}bogus_key: 1\n"));

    let warnings = peakbot::reload_session_config(&mut rc, &sm, home.cwd());
    assert_eq!(warnings.len(), 1, "one warning: {warnings:?}");
    assert!(
        warnings[0].starts_with("⚠ config reload failed: "),
        "the warning names the failure: {}",
        warnings[0]
    );
    assert!(
        warnings[0].ends_with("— keeping previous config."),
        "the warning says the previous config is kept: {}",
        warnings[0]
    );
    assert!(
        Arc::ptr_eq(&prev_config, &rc.config),
        "the previous config is kept"
    );
    assert_eq!(rc.bound.selection(), prev_sel, "the bound is kept");
}

/// §11.3 item 4: a broken repo file → global-only catalog + the warning that
/// names the repo file.
#[test]
fn reload_broken_repo_falls_back_to_global_only() {
    let home = TempHome::new();
    home.write_global(RELOAD_GLOBAL);
    home.write_repo("profiles:\n  dev:\n    agents:\n      main:\n        model: sonnet\n");
    let boot =
        Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("boot with the repo profile");
    assert_eq!(
        boot.catalog.iter().next().map(|p| p.name.as_str()),
        Some("dev"),
        "the repo dev leads before the break"
    );
    let bound = boot
        .catalog
        .start_bound(None)
        .expect("the default bound (repo dev)");
    let mut rc = rebuild_ctx(boot, bound, None);
    let sm = StateManager::new_arc();

    home.write_repo("profiles:\n  dev:\n    bogus: 1\n");

    let warnings = peakbot::reload_session_config(&mut rc, &sm, home.cwd());
    let repo_path = home.repo_path().to_string_lossy().to_string();
    assert!(
        warnings.iter().any(|w| w.starts_with("⚠ ")
            && w.contains(repo_path.as_str())
            && w.ends_with("— ignoring this repo's profiles.")),
        "the warning names the repo file: {warnings:?}"
    );
    assert!(
        rc.config
            .catalog
            .iter()
            .all(|p| matches!(p.origin, Origin::Global)),
        "the catalog is global-only after the repo failure"
    );
}

/// §11.3 item 13 (bound stability): with turns, a global edit to the bound
/// agent's persona + `/cd` → the prompt keeps the OLD persona; only the
/// cwd-derived parts refresh.
#[test]
fn cd_with_turns_keeps_bound_and_old_persona() {
    let home = TempHome::new();
    home.write_global(RELOAD_GLOBAL);
    let boot = Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("boot");
    let bound = boot
        .catalog
        .start_bound(None)
        .expect("the default bound (dev/main)");
    let mut rc = rebuild_ctx(boot, bound.clone(), None);
    let sm = StateManager::new_arc();
    sm.add_user_message("hello".to_string()); // the conversation has a turn

    home.write_global(&RELOAD_GLOBAL.replace("Old persona alpha.", "New persona beta."));

    let warnings = peakbot::reload_session_config(&mut rc, &sm, home.cwd2());
    assert!(
        warnings.is_empty(),
        "no warning: the profile name is still configured: {warnings:?}"
    );
    assert!(
        Arc::ptr_eq(&bound.profile, &rc.bound.profile),
        "with turns the Bound is kept exactly — the same profile object"
    );
    let prompt = peakbot::build_agent_prompt(
        rc.bound.agent(),
        &rc.bound.profile,
        &rc.skills,
        None,
        home.cwd2(),
    );
    assert!(
        prompt.contains("Old persona alpha."),
        "the prompt keeps the old persona while locked"
    );
    assert!(
        !prompt.contains("New persona beta."),
        "the edited persona is not picked up while locked"
    );
    assert!(
        prompt.contains(home.cwd2().to_string_lossy().as_ref()),
        "only the cwd parts refresh: the env block carries the new cwd"
    );
}

/// §11.3 item 4 + 13: with zero turns, a global persona edit + `/new`/`/cd`
/// → the selection is re-resolved in the fresh catalog and the NEW persona
/// is picked up; the selection is stamped back onto the `StateManager`.
/// ASSUMED API: `StateManager::selection`/`set_selection` (see header, #7).
#[test]
fn reload_without_turns_reresolves_selection() {
    let home = TempHome::new();
    home.write_global(RELOAD_GLOBAL);
    let boot = Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("boot");
    let bound = boot
        .catalog
        .start_bound(None)
        .expect("the default bound (dev/main)");
    // The boot catalog's `dev` Arc — the object a KEEP would pin.
    let boot_dev = Arc::clone(boot.catalog.profile("dev").expect("boot dev"));
    let mut rc = rebuild_ctx(boot, bound, None);
    let sm = StateManager::new_arc(); // zero turns

    home.write_global(&RELOAD_GLOBAL.replace("Old persona alpha.", "New persona beta."));

    let warnings = peakbot::reload_session_config(&mut rc, &sm, home.cwd2());
    assert!(
        warnings.is_empty(),
        "no warning: the selection is still configured: {warnings:?}"
    );
    assert_eq!(
        rc.bound.selection(),
        Selection {
            profile: "dev".to_string(),
            agent: "main".to_string(),
            ..Default::default()
        },
        "the same selection, re-resolved"
    );
    assert!(
        Arc::ptr_eq(rc.config.catalog.profile("dev").unwrap(), &rc.bound.profile),
        "the bound's profile is the FRESH catalog's (re-resolved, not kept)"
    );
    assert!(
        !Arc::ptr_eq(&boot_dev, &rc.bound.profile),
        "…and not the boot catalog's — the re-resolution ran on the fresh parse"
    );
    let prompt = peakbot::build_agent_prompt(
        rc.bound.agent(),
        &rc.bound.profile,
        &rc.skills,
        None,
        home.cwd2(),
    );
    assert!(
        prompt.contains("New persona beta."),
        "/new picks up the global persona edit"
    );
    assert_eq!(
        sm.selection(),
        Selection {
            profile: "dev".to_string(),
            agent: "main".to_string(),
            ..Default::default()
        },
        "the selection is stamped back onto the StateManager (step 7)"
    );
}

/// §11.3 item 4: `/cd` into a config where the bound profile is gone, WITH
/// turns → the snapshot is kept + the exact warning.
#[test]
fn reload_dropped_profile_with_turns_warns_and_keeps() {
    let home = TempHome::new();
    home.write_global(RELOAD_GLOBAL);
    let boot = Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("boot");
    let bound = boot
        .catalog
        .start_bound(None)
        .expect("the default bound (dev/main)");
    let mut rc = rebuild_ctx(boot, bound.clone(), None);
    let sm = StateManager::new_arc();
    sm.add_user_message("hello".to_string());

    home.write_global(RELOAD_GLOBAL_NO_DEV);

    let warnings = peakbot::reload_session_config(&mut rc, &sm, home.cwd2());
    let cwd2 = home.cwd2().to_string_lossy().to_string();
    assert_eq!(
        warnings,
        vec![format!(
            "⚠ Profile 'dev' is not configured in {cwd2}; this conversation keeps it until /new."
        )],
        "the exact keep-snapshot warning"
    );
    assert!(
        Arc::ptr_eq(&bound.profile, &rc.bound.profile),
        "the in-flight conversation keeps its profile"
    );
}

/// §11.3 item 4: the same drop, WITHOUT turns → the default here + the
/// exact info line.
#[test]
fn reload_dropped_profile_without_turns_uses_default() {
    let home = TempHome::new();
    home.write_global(RELOAD_GLOBAL);
    let boot = Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("boot");
    let bound = boot
        .catalog
        .start_bound(None)
        .expect("the default bound (dev/main)");
    let mut rc = rebuild_ctx(boot, bound, None);
    let sm = StateManager::new_arc(); // zero turns

    home.write_global(RELOAD_GLOBAL_NO_DEV);

    let warnings = peakbot::reload_session_config(&mut rc, &sm, home.cwd2());
    assert_eq!(
        warnings,
        vec!["ℹ Using research › scholar (default here)."],
        "the exact default-fallback line"
    );
    assert_eq!(
        rc.bound.selection(),
        Selection {
            profile: "research".to_string(),
            agent: "scholar".to_string(),
            ..Default::default()
        },
        "the default here is the bound now"
    );
}

/// §11.3 item 13: a new web socket (no `?convo=`) runs a fresh
/// `Config::load` — it sees a profile added to the global after boot (no
/// boot-cached catalog).
#[test]
fn fresh_web_socket_load_sees_profile_added_after_boot() {
    let home = TempHome::new();
    home.write_global(RELOAD_GLOBAL);
    let boot = Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("boot");
    let boot_names: Vec<&str> = boot.catalog.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(boot_names, vec!["dev"]);

    let added = RELOAD_GLOBAL.replace(
        "profiles:",
        "profiles:\n  research:\n    agents:\n      scholar:\n        model: sonnet\n",
    );
    home.write_global(&added);

    let fresh = Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("the fresh load");
    let fresh_names: Vec<&str> = fresh.catalog.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        fresh_names,
        vec!["research", "dev"],
        "the fresh load sees the profile added after boot"
    );
}

// ── item 5: per-repo shadowing ──────────────────────────────────────────────

/// §11.3 item 5: a repo `dev` REPLACES the global `dev` — the global `dev`'s
/// env and tool restrictions are absent in the built agent's bash env and
/// tool list.
/// ASSUMED API: `main_agent_tool_names` (see header, #1).
#[test]
fn repo_profile_replaces_global_in_tools_and_env() {
    let home = TempHome::new();
    home.write_global(SHADOW_GLOBAL);
    home.write_repo(SHADOW_REPO);
    let cfg = Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("load");

    let bound = cfg.catalog.start_bound(None).expect("the default bound");
    assert_eq!(bound.profile.name, "dev");
    assert!(
        matches!(
            bound.profile.origin,
            Origin::Repo {
                overrides_global: true
            }
        ),
        "the repo dev replaces the global dev"
    );
    assert_eq!(
        cfg.catalog.iter().count(),
        1,
        "the global dev is shadowed away entirely, not merged"
    );

    // Bash env: the repo profile's env, not the global's.
    let env = ToolEnv::for_profile(
        &cfg,
        &bound.profile,
        &Arc::new(Vec::<McpServerHandle>::new()),
    );
    let bash_env = env.bash_env(&bound.agent().main);
    assert_eq!(
        bash_env.get("REPO_ONLY").map(String::as_str),
        Some("r"),
        "the repo profile env is in effect"
    );
    assert!(
        !bash_env.contains_key("GLOBAL_ONLY"),
        "the global dev env is absent from the built agent's bash env"
    );

    // Tool list: the repo dev's (absent) restriction, not the global's.
    let main = peakbot::tools::env::main_agent_tool_names(&env, bound.agent());
    assert!(
        main.iter().any(|n| n == "bash"),
        "the built agent keeps bash (the repo dev does not block it): {main:?}"
    );

    // Contrast: the global-only config blocks bash.
    let global_only = parse_global(SHADOW_GLOBAL);
    let gprofile = global_only
        .catalog
        .profile("dev")
        .expect("the global dev in the global-only config");
    let genv = ToolEnv::for_profile(
        &global_only,
        gprofile,
        &Arc::new(Vec::<McpServerHandle>::new()),
    );
    let gmain = peakbot::tools::env::main_agent_tool_names(&genv, &gprofile.agents[0]);
    assert!(
        !gmain.iter().any(|n| n == "bash"),
        "the global dev restriction is NOT in effect for the bound (repo) agent: {gmain:?}"
    );
}

// ── item 6: subagents get the same tools and MCP ────────────────────────────

/// A `ToolEnv` for `profile` whose `McpSource` stands in for a live `srv`
/// MCP server exposing `srv_a`/`srv_b` (ASSUMED API #3: the lifted test
/// seams), filtered by the profile's `mcps` filter.
fn env_with_mcp(cfg: &Config, profile: &peakbot::config::resolve::ResolvedProfile) -> ToolEnv {
    let mut env = ToolEnv::for_profile(cfg, profile, &Arc::new(Vec::<McpServerHandle>::new()));
    let groups = vec![(
        "srv".to_string(),
        vec!["srv_a".to_string(), "srv_b".to_string()],
    )];
    env.mcp = McpSource::for_test(&groups, profile.mcps.clone());
    env
}

/// §11.3 item 6: the delegate's built subagent tool list == the main tool
/// list minus `delegate`, plus the same MCP tools.
/// ASSUMED API: `main_agent_tool_names` / `sub_agent_tool_names` (see
/// header, #1/#2).
#[test]
fn subagent_gets_same_mcp_tools_and_builtins_minus_delegate() {
    let cfg = parse_global(GLOBAL_TEAM);
    let dev = cfg.catalog.profile("dev").expect("the dev profile");
    let team = &dev.agents[0];
    assert_eq!(team.main.name, "team", "fixture sanity");
    let researcher = &team.subagents[0];
    assert_eq!(researcher.member.name, "researcher", "fixture sanity");

    let env = env_with_mcp(&cfg, dev);

    let main = peakbot::tools::env::main_agent_tool_names(&env, team);
    let sub = peakbot::tools::env::sub_agent_tool_names(&env, researcher);

    assert!(
        main.contains(&"delegate".to_string()),
        "the main agent (with subagents) has delegate: {main:?}"
    );
    for name in ["srv_a", "srv_b"] {
        assert!(
            main.contains(&name.to_string()),
            "the main agent gets the MCP tools: {main:?}"
        );
        assert!(
            sub.contains(&name.to_string()),
            "the subagent gets the SAME MCP tools: {sub:?}"
        );
    }
    assert!(
        !sub.contains(&"delegate".to_string()),
        "subagents never get delegate (no nesting): {sub:?}"
    );
    let mut expected = main;
    expected.retain(|n| n != "delegate");
    expected.sort();
    let mut sub_sorted = sub.clone();
    sub_sorted.sort();
    assert_eq!(
        sub_sorted, expected,
        "the subagent tool list is exactly the main tool list minus delegate"
    );
}

/// §11.3 item 6: `profile.mcps: {enabled: false}` → NEITHER the agent nor
/// its subagents get the MCP tools.
#[test]
fn mcps_disabled_profile_gives_neither_mcp_tools() {
    let cfg = parse_global(GLOBAL_TEAM);
    let research = cfg
        .catalog
        .profile("research")
        .expect("the research profile");
    let scholar = &research.agents[0];
    assert!(
        !research.mcps.enabled,
        "fixture sanity: research disables MCP"
    );

    let env = env_with_mcp(&cfg, research);
    assert!(
        env.mcp.tool_names().is_empty(),
        "the disabled filter yields no MCP tools"
    );

    let main = peakbot::tools::env::main_agent_tool_names(&env, scholar);
    for name in ["srv_a", "srv_b"] {
        assert!(
            !main.contains(&name.to_string()),
            "the agent gets no MCP tools: {main:?}"
        );
    }

    // A subagent of this profile would get none either.
    let ghost = peakbot::config::resolve::Subagent {
        member: peakbot::config::resolve::Member {
            name: "ghost".to_string(),
            model: scholar.main.model.clone(),
            skills: NameFilter::default(),
            agents_md: false,
            env: Default::default(),
        },
        prompt: "Stand by.".to_string(),
    };
    let sub = peakbot::tools::env::sub_agent_tool_names(&env, &ghost);
    for name in ["srv_a", "srv_b"] {
        assert!(
            !sub.contains(&name.to_string()),
            "nor do its subagents: {sub:?}"
        );
    }
}

/// §11.3 item 6: the subagent's bash env is `profile.env ⊕ sub.env` — NOT
/// the main agent's env (existing seam: `ToolEnv::bash_env`).
#[test]
fn subagent_bash_env_is_profile_plus_subagent_env() {
    let cfg = parse_global(GLOBAL_TEAM);
    let dev = cfg.catalog.profile("dev").expect("the dev profile");
    let team = &dev.agents[0];
    let researcher = &team.subagents[0];

    let env = ToolEnv::for_profile(&cfg, dev, &Arc::new(Vec::<McpServerHandle>::new()));

    let sub_env = env.bash_env(&researcher.member);
    assert_eq!(
        sub_env.get("PROFILE_ENV").map(String::as_str),
        Some("p"),
        "the profile env is the base"
    );
    assert_eq!(
        sub_env.get("SUB_ENV").map(String::as_str),
        Some("s"),
        "the subagent's own env overlays it"
    );
    assert!(
        !sub_env.contains_key("AGENT_ENV"),
        "the main agent's env is not visible to the subagent"
    );

    let main_env = env.bash_env(&team.main);
    assert_eq!(
        main_env.get("AGENT_ENV").map(String::as_str),
        Some("a"),
        "the main agent sees its own env"
    );
    assert!(!main_env.contains_key("SUB_ENV"), "…and not the subagent's");
}

// ── item 7: delegate registration ───────────────────────────────────────────

/// §11.3 item 7: an agent WITH subagents gets `delegate`; one without does
/// not. ASSUMED API: `main_agent_tool_names` (see header, #1).
#[test]
fn delegate_tool_registered_iff_agent_has_subagents() {
    let cfg = parse_global(GLOBAL_TEAM);
    let dev = cfg.catalog.profile("dev").expect("the dev profile");
    let env = ToolEnv::for_profile(&cfg, dev, &Arc::new(Vec::<McpServerHandle>::new()));

    let team_names = peakbot::tools::env::main_agent_tool_names(&env, &dev.agents[0]);
    let solo_names = peakbot::tools::env::main_agent_tool_names(&env, &dev.agents[1]);
    assert!(
        !dev.agents[0].subagents.is_empty() && dev.agents[1].subagents.is_empty(),
        "fixture sanity"
    );
    assert!(
        team_names.contains(&"delegate".to_string()),
        "an agent with subagents has delegate: {team_names:?}"
    );
    assert!(
        !solo_names.contains(&"delegate".to_string()),
        "an agent without subagents does not: {solo_names:?}"
    );
}

/// §11.3 item 7: `tools.only` (an allowlist) cannot remove `delegate` (F3) —
/// while it does remove the other tools.
#[test]
fn tools_allowlist_cannot_remove_delegate() {
    let cfg = parse_global(
        "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - { name: anthropic/claude-sonnet-4.5, alias: sonnet }\nprofiles:\n  dev:\n    tools: { only: [bash, todo] }\n    agents:\n      team:\n        model: sonnet\n        subagents:\n          researcher:\n            prompt: Gather background.\n",
    );
    let dev = cfg.catalog.profile("dev").expect("the dev profile");
    let env = ToolEnv::for_profile(&cfg, dev, &Arc::new(Vec::<McpServerHandle>::new()));

    let names = peakbot::tools::env::main_agent_tool_names(&env, &dev.agents[0]);
    assert!(
        names.contains(&"delegate".to_string()),
        "delegate is not filterable (F3): {names:?}"
    );
    assert!(
        !names.contains(&"web_search".to_string()) && !names.contains(&"file_read".to_string()),
        "the allowlist does remove the other tools: {names:?}"
    );
}

// ── item 8: vector registry ─────────────────────────────────────────────────

fn embeddings() -> peakbot::config::EmbeddingsConfig {
    peakbot::config::EmbeddingsConfig {
        base_url: "https://embed.example.com/v1".to_string(),
        api_key: Some("emb-test".to_string()),
        model: "bge-m3".to_string(),
        dimensions: 1024,
    }
}

/// §11.3 item 8: two profiles with the same `Fixed` vector path share ONE
/// redb handle (`Arc::ptr_eq` through the test accessor); the reload's
/// rebuilt `VectorStore` (a fresh `VectorStore::open` on the same spec, as
/// `ToolEnv::for_profile` does) does not reopen the file.
/// ASSUMED API: `VectorStore::handle_for_test` made `pub` (see header, #10).
#[tokio::test]
async fn vector_fixed_path_shares_one_handle_across_rebuilds() {
    let dir = tempfile::tempdir().expect("db tempdir");
    let db_path = dir.path().join("research.db");

    let spec_a = VectorSpec {
        db_path: DbPathSpec::Fixed(db_path.clone()),
        embeddings: embeddings(),
    };
    let spec_b = VectorSpec {
        db_path: DbPathSpec::Fixed(db_path.clone()),
        embeddings: embeddings(),
    };

    let store_a = VectorStore::open(&spec_a).expect("first profile's store");
    // "Reload rebuild": a fresh `VectorStore` on the same spec — exactly
    // what `ToolEnv::for_profile` does on every reload.
    let store_b = VectorStore::open(&spec_b).expect("the reload's rebuilt store");

    let handle_a = store_a.handle_for_test(dir.path()).await.expect("handle a");
    let handle_b = store_b.handle_for_test(dir.path()).await.expect("handle b");
    assert!(
        Arc::ptr_eq(&handle_a, &handle_b),
        "one redb handle per Fixed path — the reload rebuild does not reopen the file"
    );
}

// ── selection-models plan §9.1, items 43–45: overrides across a reload ──────

/// The plan's shared fixture, as a file body: three aliases on one provider,
/// `dev/team` (inherited `reviewer`, explicit `tester`, explicit-equal `twin`)
/// and a subagent-free `solo`, plus `research/main`.
const TEAM_MODELS_GLOBAL: &str = r#"providers:
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

/// The same file with the `opus` alias removed (`team` stays valid; `solo` is
/// moved to haiku so every agent still resolves).
fn team_models_global_without_opus() -> String {
    TEAM_MODELS_GLOBAL
        .replace(
            "      - { name: claude-opus-5,   alias: opus,   context_size: 200000 }\n",
            "",
        )
        .replace(
            "      solo: { model: opus }",
            "      solo: { model: haiku }",
        )
}

fn tester_opus() -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    map.insert("tester".to_string(), "opus".to_string());
    map
}

/// Boot a `RebuildContext` over `TEAM_MODELS_GLOBAL` whose bound already
/// carries `{tester: opus}` (built through the validating constructor on the
/// boot registry), with the matching selection published on a fresh `sm`.
fn rebuild_ctx_with_tester_opus(home: &TempHome) -> (peakbot::RebuildContext, Arc<StateManager>) {
    home.write_global(TEAM_MODELS_GLOBAL);
    let boot = Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("boot");
    let base = boot.catalog.resolve("dev", "team").expect("dev/team");
    let (bound, rejected) = base.with_subagent_models(&boot.registry, &tester_opus());
    assert!(
        rejected.is_empty(),
        "the boot override is valid: {rejected:?}"
    );
    let rc = rebuild_ctx(boot, bound, None);
    let sm = StateManager::new_arc();
    sm.set_selection(rc.bound.selection());
    (rc, sm)
}

/// §9.1/43: a zero-turn reload with an unchanged global keeps valid overrides
/// — no warning, in the bound AND in the published selection.
#[test]
fn reload_without_turns_keeps_valid_subagent_models() {
    let home = TempHome::new();
    let (mut rc, sm) = rebuild_ctx_with_tester_opus(&home);

    let warnings = peakbot::reload_session_config(&mut rc, &sm, home.cwd());
    assert!(
        warnings.is_empty(),
        "a still-valid override is quiet: {warnings:?}"
    );
    assert_eq!(
        rc.bound.selection().subagent_models,
        tester_opus(),
        "the bound"
    );
    assert_eq!(
        sm.selection().subagent_models,
        tester_opus(),
        "the published selection"
    );
}

/// §9.1/44: a zero-turn reload that removed the override's alias drops that
/// entry with the exact ⚠ and keeps the (still valid) agent.
#[test]
fn reload_without_turns_drops_vanished_override_alias_with_warning() {
    let home = TempHome::new();
    let (mut rc, sm) = rebuild_ctx_with_tester_opus(&home);

    home.write_global(&team_models_global_without_opus());

    let warnings = peakbot::reload_session_config(&mut rc, &sm, home.cwd());
    assert_eq!(
        warnings,
        vec!["⚠ Model 'opus' for subagent 'tester' is no longer configured; using its default."],
        "exactly the override warning — the agent itself is still configured"
    );
    assert!(
        rc.bound.selection().subagent_models.is_empty(),
        "the bound dropped it"
    );
    assert!(
        sm.selection().subagent_models.is_empty(),
        "the published selection dropped it"
    );
    assert_eq!(rc.bound.agent().main.name, "team", "the agent is kept");
}

/// §9.1/45: with turns, a reload keeps the conversation's definitions — the
/// overrides stay even though their alias vanished, with no override warning.
#[test]
fn reload_with_turns_keeps_overrides() {
    let home = TempHome::new();
    let (mut rc, sm) = rebuild_ctx_with_tester_opus(&home);
    sm.add_user_message("hi".to_string());

    home.write_global(&team_models_global_without_opus());

    let warnings = peakbot::reload_session_config(&mut rc, &sm, home.cwd());
    assert!(
        warnings.iter().all(|w| !w.contains("subagent")),
        "an in-flight conversation gets no override warning: {warnings:?}"
    );
    assert_eq!(
        rc.bound.selection().subagent_models,
        tester_opus(),
        "in-flight conversations keep their definitions"
    );
}

// ── plan T10: UI state / wire ───────────────────────────────────────────────

/// T10: `UiAction::Select { selection, model }` carries the selection (and
/// the optional orchestrator model); the wire shape round-trips.
/// ASSUMED API: the `Select` struct variant (see header, #8).
#[test]
fn ui_action_select_wire_roundtrip() {
    let action = UiAction::Select {
        selection: Selection {
            profile: "dev".to_string(),
            agent: "team".to_string(),
            ..Default::default()
        },
        model: None,
    };
    let json = serde_json::to_value(&action).expect("serialize");
    assert_eq!(
        json,
        serde_json::json!({ "Select": { "selection": { "profile": "dev", "agent": "team" } } }),
        "the wire shape of UiAction::Select"
    );
    let back: UiAction = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, action, "the frame round-trips");

    // With a model and an override map (selection-models plan §9.1/25).
    let mut map = std::collections::BTreeMap::new();
    map.insert("tester".to_string(), "opus".to_string());
    let action = UiAction::Select {
        selection: Selection {
            profile: "dev".to_string(),
            agent: "team".to_string(),
            subagent_models: map,
        },
        model: Some("opus".to_string()),
    };
    let json = serde_json::to_value(&action).expect("serialize");
    assert_eq!(
        json,
        serde_json::json!({
            "Select": {
                "selection": {
                    "profile": "dev",
                    "agent": "team",
                    "subagent_models": { "tester": "opus" }
                },
                "model": "opus"
            }
        }),
        "the wire shape of UiAction::Select with a model and overrides"
    );
    let back: UiAction = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, action, "the extended frame round-trips");
}

/// T10: `AppState` carries the catalog, the selection and the pin — and the
/// serializer emits `settings_available = !profile_pinned`.
/// ASSUMED API: the `AppState` fields (see header, #9).
#[test]
fn appstate_settings_available_derived_from_pin() {
    let cfg = parse_global(GLOBAL_TEAM);
    let mut state = AppState {
        catalog: cfg.catalog.infos(),
        selection: Selection {
            profile: "dev".to_string(),
            agent: "team".to_string(),
            ..Default::default()
        },
        profile_pinned: true,
        ..AppState::default()
    };

    let json = serde_json::to_value(&state).expect("serialize");
    assert_eq!(
        json["settings_available"], false,
        "under a pin the settings tab is unavailable"
    );
    assert_eq!(
        json["catalog"][0]["name"], "dev",
        "the catalog is the wire projection of the profiles"
    );
    assert_eq!(
        json["selection"],
        serde_json::json!({ "profile": "dev", "agent": "team" }),
        "the selection is on the wire"
    );

    state.profile_pinned = false;
    let json = serde_json::to_value(&state).expect("serialize");
    assert_eq!(
        json["settings_available"], true,
        "unpinned ⇒ settings available"
    );
}

// Keep the (assumed) `ProfileInfo` import honest: the catalog infos feed
// `AppState.catalog` / `StateManager::set_catalog`.
#[allow(dead_code)]
fn catalog_infos(cfg: &Config) -> Vec<ProfileInfo> {
    cfg.catalog.infos()
}

#[allow(dead_code)]
fn resolved_model_of(cfg: &Config, alias: &str) -> Option<ResolvedModel> {
    cfg.registry.resolve(alias).cloned()
}
