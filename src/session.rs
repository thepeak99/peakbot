//! Session factory — builds one independent agent (Model + Controller).
//!
//! PeakBot was single-session by construction: one `StateManager` (Model) +
//! one `AgentRunner` (Controller), built once in `main` and shared by
//! whichever View ran. The web UI makes **each tab its own agent**, so that
//! "build one session" block is named here and called 1×N.
//!
//! [`SessionDeps`] holds the process-wide, immutable inputs (config,
//! registry, MCP subprocess handles, vector store, skills, system prompt).
//! It is built once and borrowed by every [`create_session`] call — the
//! shared column of `webui.md` §10.2. Everything in the per-session column
//! (a fresh `StateManager`, its `AgentRunner` + spawned `run_loop`, the
//! `UiAction` channel, the `TodoTool`) is built anew each call.
//!
//! ## Teardown (Option C, ephemeral session-per-connection)
//!
//! A [`Session`] owns its `action_sender` and the `run_loop` task handle.
//! Dropping the `Session` drops the `action_sender`; `run_loop`'s event
//! loop then observes the closed channel, aborts the agent loop, calls
//! `clear_bg()` (kills this session's PTY children), and returns. No
//! `Drop` impl or `AbortHandle` is needed — the channel-close cascade is
//! the teardown (verified `lib.rs` run_loop end).

use crate::config::{Config, ModelRegistry, SearXngConfig};
use crate::pipeline::PipelineSet;
use crate::tools::ShellKind;
use crate::ui::app_state::WelcomeState;
use crate::{
    AgentRunner, McpServerHandle, PEAKBOT_VERSION, RebuildContext, SkillRegistry, StateManager,
    TodoTool, UiAction, build_system_prompt, create_provider,
};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Process-wide inputs shared by every session. Built once in `main`,
/// borrowed by each [`create_session`] call. The heavy handles
/// (`mcp_handles`, `vector_store`, `pipelines`, `model_registry`)
/// are `Arc`-backed, so the per-call clones are cheap.
pub struct SessionDeps {
    /// Boot config with `provider` already mirrored to the active provider
    /// (see `Config::resolve_and_mirror_boot_provider`). Cloned per session
    /// so each `AgentRunner` reads a stable provider for its compaction model.
    pub config: Config,
    pub model_registry: Arc<ModelRegistry>,
    pub skills: SkillRegistry,
    /// User-facing warnings from the boot skill scan (a skill that failed to
    /// parse, an unreadable dir). Emitted as system messages on session
    /// creation so both the TUI and web UI surface them.
    pub skill_warnings: Vec<String>,
    /// One set of MCP subprocesses shared by all sessions; each session
    /// clones the *tools list*, not the processes.
    pub mcp_handles: Arc<Vec<McpServerHandle>>,
    pub searxng_config: Option<SearXngConfig>,
    /// The named teams this install declares, resolved once at boot. An empty
    /// set is "no pipelines configured" — one fewer nullable than the old
    /// `Option<SubAgentRegistry>`.
    pub pipelines: Arc<PipelineSet>,
    pub vector_store: Option<crate::vector::VectorStore>,
    pub shell_kind: Option<ShellKind>,
    /// Shared conversation storage (writes distinct files per conversation
    /// id), or `None` when persistence is disabled. Cloned into each
    /// session's `StateManager`.
    pub storage: Option<Arc<dyn crate::ConversationStorage>>,
    pub mcp_tools_count: usize,
    pub skills_count: usize,
}

/// One running, independent agent: its `StateManager` (Model), the write
/// channel into its `AgentRunner` (Controller), and the handle to the
/// spawned `run_loop` task. Dropping this tears the session down.
pub struct Session {
    pub state_manager: Arc<StateManager>,
    pub action_sender: UnboundedSender<UiAction>,
    /// The active model alias, bound to this session's conversation.
    pub model_alias: String,
    /// The conversation id this session is bound to — the registry key for
    /// sticky web sessions. Minted (fresh) or adopted (resume) synchronously
    /// in [`create_session`], so it is known the instant the session exists.
    pub conversation_id: Uuid,
    /// The `run_loop` task. Private so only the factory spawns it; kept
    /// alive for the session's lifetime. It exits on its own when
    /// `action_sender` drops.
    _run_handle: JoinHandle<()>,
}

/// Build one independent session and spawn its controller loop.
///
/// `resume`: when `Some(id)`, adopt an existing persisted conversation
/// (sticky-session reconnect); when `None`, mint a fresh one. Either way the
/// conversation is initialised **synchronously** before the controller loop
/// spawns, so [`Session::conversation_id`] is immediately valid.
///
/// Mirrors the historical `main` boot block: fresh `StateManager`,
/// per-session `TodoTool`, `create_provider`, `AgentRunner::new` +
/// `with_rebuild_context`, welcome state, then `tokio::spawn(run_loop)`.
///
/// ## Per-session cwd
///
/// This is the **single point** that resolves the per-session `session_cwd`
/// and threads it into the system prompt, the persisted conversation, the
/// welcome banner, and every path-aware tool. Resume adopts the saved
/// cwd (it was persisted on the conversation at mint time); fresh
/// sessions inherit the boot `current_dir()`. Order matters:
/// 1. resolve `session_cwd`,
/// 2. `state_manager.set_session_cwd(...)` *before* `create_provider` —
///    `add_builtin_tools` snapshots it at agent-build time,
/// 3. build the per-session system prompt from `session_cwd`,
/// 4. pass that prompt into `create_provider` *and* into `RebuildContext`
///    so a later `/model` switch keeps the cwd.
pub fn create_session(deps: &SessionDeps, resume: Option<Uuid>) -> Result<Session> {
    // Per-session Model. Storage (if any) is shared — it writes distinct
    // files per conversation id.
    let state_manager = match &deps.storage {
        Some(storage) => StateManager::new_arc_with_storage(storage.clone()),
        None => StateManager::new_arc(),
    };

    if let Some(sk) = deps.shell_kind.as_ref() {
        state_manager.set_shell(sk.executable().to_string());
    }

    // The pipeline catalogue every View reads (Agents panel roster). Stamped
    // once — it is a projection of the boot-built set, never mutated.
    state_manager.set_pipelines(deps.pipelines.infos());

    // The conversation's pipeline selection. A fresh session has none; a
    // resumed one adopts its saved team, or drops it with a warning when that
    // team is no longer configured. Resolved *before* the agent is built so
    // the boot agent runs on the right orchestrator with the right roster.
    let saved_pipeline = resume
        .and_then(|id| state_manager.peek_conversation_pipeline(id).ok())
        .flatten();
    let (active, pipeline_warning) = deps.pipelines.resolve_saved(saved_pipeline.as_deref());
    state_manager.set_selected_pipeline(active.map(|p| p.name.clone()));

    let todo_tool = TodoTool::new(state_manager.clone());

    // Pick the model to boot on. The selected pipeline OWNS the orchestrator's
    // model, so it beats the conversation's saved wire id (plan §8.6) — and a
    // saved-but-dropped pipeline falls back to the registry default rather than
    // that pipeline's fossilised wire id. Only a conversation that never had a
    // pipeline boots on its saved model; a fresh session boots the registry
    // default. The saved wire id is peeked without loading — the same preflight
    // `/load` uses.
    let saved_wire_id = resume.and_then(|id| state_manager.peek_conversation_wire_id(id).ok());
    let boot = deps.model_registry.resolve_boot(
        saved_wire_id
            .as_ref()
            .filter(|_| saved_pipeline.is_none())
            .map(|(p, m)| (p.as_str(), m.as_str())),
    );
    // `resolve_boot` guarantees a model (a config-built registry always has a
    // default), so there is no None case to handle.
    let boot_model = match active {
        Some(pipeline) => &pipeline.orchestrator,
        None => boot.model,
    };
    let boot_provider_config = &boot_model.provider_config;
    let boot_provider_name = boot_model.provider_name.clone();
    let boot_alias = boot_model.alias.clone();
    // Context size for the booted model, eagerly resolved at registry-build
    // time (config override or auto-detected). Drives ContextManager
    // compaction thresholds.
    let context_size = boot_model.context_size;

    // Each session clones the MCP tools list from the shared handles (not
    // the subprocesses). `McpTool: Clone` makes this cheap.
    let mcp_tools = if deps.mcp_handles.is_empty() {
        None
    } else {
        use rig_core::tool::ToolDyn;
        let mut all = Vec::new();
        for handle in deps.mcp_handles.iter() {
            all.extend(
                handle
                    .tools()
                    .iter()
                    .cloned()
                    .map(|t| Box::new(t) as Box<dyn ToolDyn>),
            );
        }
        Some(all)
    };

    // ── Per-session cwd ──────────────────────────────────────────────────────
    // Resume adopts the saved cwd iff it's non-empty and still points at a
    // directory. Anything else (no resume, no storage, missing/empty
    // cwd, gone directory) falls through to the boot cwd. A gone cwd is
    // best-effort: the user can `/cd` to a valid tree at runtime.
    let boot_cwd = std::env::current_dir().unwrap_or_default();
    let session_cwd: PathBuf = match resume {
        Some(id) => state_manager
            .peek_conversation_cwd(id)
            .ok()
            .filter(|s| !s.is_empty())
            .and_then(|s| {
                let p = PathBuf::from(&s);
                if p.is_dir() { Some(p) } else { None }
            })
            .unwrap_or(boot_cwd),
        None => boot_cwd,
    };

    // Stamp the SM *before* create_provider so the tools snapshot the
    // per-session value at agent-build time. A `set_session_cwd` after
    // build would not reach the already-built tools.
    state_manager.set_session_cwd(session_cwd.clone());

    // Build the per-session system prompt from `session_cwd` — the only
    // place the cwd flows into the prompt. Skills + shell_kind are part
    // of the env block too. With a pipeline selected this drops the
    // crusader persona and appends the pipeline's orchestrator prompt. A
    // pipeline's own `persona:` replaces the global one for its orchestrator
    // (amendment 1); otherwise the global `persona:` applies unchanged.
    let session_prompt = build_system_prompt(
        &deps.skills,
        deps.shell_kind.as_ref(),
        &session_cwd,
        deps.config.memory.enabled,
        active.is_some(),
        active.and_then(|p| p.orchestrator_prompt.as_deref()),
        active
            .and_then(|p| p.orchestrator_persona.as_deref())
            .or_else(|| deps.config.persona()),
    );

    // The `delegate` tool (and thus sub-agents) is registered iff a pipeline is
    // selected — and it exposes exactly that team's roster. A fresh session has
    // no selection, so the boot agent has no delegate until the user selects a
    // pipeline (which rebuilds the agent). `RebuildContext` keeps the whole set
    // so that rebuild can hand over a different team's registry.
    let boot_registry = active.map(|p| p.registry.as_ref());

    let (agent, provider_info, event_receiver, session_hook) = create_provider(
        boot_provider_config,
        mcp_tools,
        &session_prompt,
        deps.searxng_config.as_ref(),
        deps.config.agent_max_turns,
        Some(todo_tool.clone()),
        &deps.config.bash,
        &deps.config.tools,
        boot_registry,
        state_manager.clone(),
        deps.shell_kind.as_ref(),
        deps.vector_store.as_ref(),
        &deps.skills,
        &deps.config.retry,
        &deps.config.timeouts,
    )?;

    // Stamp the wire identity `(provider_name, model)` and the display
    // alias for the booted model (resumed model or registry default).
    state_manager.set_model(provider_info.model.clone());
    state_manager.set_provider_name(boot_provider_name);
    state_manager.set_model_alias(boot_alias.clone());

    // Initialise the conversation synchronously so `conversation_id` is valid
    // before the controller loop spawns. `resume` best-effort adopts a
    // persisted conversation; `ensure_boot_conversation` is idempotent, so it
    // mints a fresh one iff the resume didn't produce a current conversation
    // (unknown id, disabled storage, or `None`). The result is total: a
    // current conversation always exists afterwards. The fresh-mint path
    // persists `session_cwd` so a later `/load` re-adopts the same tree.
    if let Some(id) = resume {
        let _ = state_manager.load_conversation(id);
    }
    state_manager.ensure_boot_conversation(&session_cwd, provider_info.model.as_str());
    let conversation_id = state_manager
        .get_current_conversation_id()
        .expect("ensure_boot_conversation guarantees a current conversation");

    // `load_conversation` restored the *saved* selection; re-stamp the one the
    // session actually booted on so an unconfigured name is dropped from the
    // conversation too (and not re-persisted).
    state_manager.set_selected_pipeline(active.map(|p| p.name.clone()));

    // A resumed conversation whose saved pipeline is gone from config boots
    // without one — say so rather than silently downgrade. Emitted after the
    // load, which replaces the chat with the saved transcript.
    if let Some(warning) = pipeline_warning {
        state_manager.add_system_message(warning);
    }

    // A resumed conversation whose saved model is gone from the registry
    // boots on the default instead — tell the user rather than downgrade
    // silently (the REPL `/load` rejects; a session boot must produce an
    // agent, so it falls back and notes it).
    if let Some((provider, model)) = boot.unavailable {
        state_manager.add_system_message(format!(
            "⚠ Model '{provider}/{model}' from this conversation is no longer available; \
             loaded on '{boot_alias}' instead."
        ));
    }

    // Surface any skill-load failures from the boot scan so the user sees a
    // broken skill instead of it silently going missing (TUI + web both
    // render system messages).
    for warning in &deps.skill_warnings {
        state_manager.add_system_message(warning.clone());
    }

    let (action_sender, action_receiver) = mpsc::unbounded_channel::<UiAction>();

    // `RebuildContext.system_prompt` is the per-session prompt (built from
    // `session_cwd` above). A later `/model` rebuild will hand the same
    // prompt to `create_provider`, so the cwd survives the model switch.
    // A `/cd` rebuild overwrites this with a fresh prompt built from the
    // new cwd.
    let rebuild_ctx = RebuildContext {
        registry: deps.model_registry.clone(),
        system_prompt: session_prompt,
        mcp_handles: deps.mcp_handles.clone(),
        searxng_config: deps.searxng_config.clone(),
        max_turns: deps.config.agent_max_turns,
        todo_tool: Some(todo_tool),
        bash_config: deps.config.bash.clone(),
        pipelines: deps.pipelines.clone(),
        shell_kind: deps.shell_kind.clone(),
        skills: deps.skills.clone(),
        vector_store: deps.vector_store.clone(),
        memory_enabled: deps.config.memory.enabled,
        tools_filter: deps.config.tools.clone(),
        persona: deps.config.persona().map(str::to_string),
    };

    let mut runner = AgentRunner::new(
        agent,
        deps.config.clone(),
        provider_info.clone(),
        deps.skills.clone(),
        event_receiver,
        Some(state_manager.clone()),
        session_hook,
        context_size,
    )?
    .with_rebuild_context(rebuild_ctx);

    // Welcome banner's cwd reads from `session_cwd` (the single source of
    // truth), not the live process cwd. Two web sessions in different trees
    // each see their own cwd here, with no shared global touched.
    state_manager.set_welcome(WelcomeState {
        provider_name: provider_info.name.clone(),
        model: provider_info.model.clone(),
        max_tokens: deps.config.max_tokens() as usize,
        // file_create, file_str_replace, file_insert, file_read, bash,
        // list_directory, fetch_url, fetch_page, think, todo, search
        builtin_tools_count: 11,
        mcp_tools_count: deps.mcp_tools_count,
        skills_count: deps.skills_count,
        searxng_enabled: deps.config.searxng_enabled(),
        searxng_url: deps.config.searxng.as_ref().map(|s| s.base_url.clone()),
        cost_tracking_enabled: deps.config.supports_pricing() && deps.config.cost_tracking,
        compaction_enabled: deps.config.context.enabled,
        compaction_threshold: deps.config.context.threshold,
        compaction_keep_recent: deps.config.context.keep_recent,
        conversation_persistence_enabled: deps.config.conversation_enabled(),
        cwd: session_cwd,
        peakbot_version: PEAKBOT_VERSION.to_string(),
    });

    let run_handle = tokio::spawn(async move {
        runner.run_loop(action_receiver).await;
    });

    Ok(Session {
        state_manager,
        action_sender,
        model_alias: boot_alias,
        conversation_id,
        _run_handle: run_handle,
    })
}

#[cfg(test)]
mod tests {
    //! Integration tests for `create_session`'s per-session cwd flow.
    //!
    //! The session's cwd is the single source of truth that flows into
    //! the system prompt, the persisted conversation, the welcome
    //! banner, and every path-aware tool. These tests pin the two
    //! observable surfaces that the contract has to protect:
    //!
    //! - `state_manager.session_cwd()` on the resume path reflects the saved
    //!   cwd, not the boot cwd.
    //! - `state_manager.session_cwd()` on the fresh-mint path reflects the
    //!   boot cwd (and the freshly minted conversation persists it 1:1).

    use super::*;
    use crate::Conversation;
    use crate::config::{ModelEntry, ModelRegistry, ProviderEntry, ProviderType};
    use crate::storage::{ConversationStorage, InMemoryStorage};
    use std::sync::Arc;

    /// Minimal `ModelRegistry` with one Ollama entry — the cheapest provider
    /// to build offline (no API key, no network at construction time). The
    /// URL is the loopback default; we never actually call it in these
    /// tests — we only assert on `state_manager.session_cwd()`.
    fn ollama_registry() -> Arc<ModelRegistry> {
        let ollama = ProviderEntry {
            name: "ollama".to_string(),
            kind: ProviderType::Ollama,
            api_key: None,
            base_url: None,
            models: vec![ModelEntry {
                name: "llama3".to_string(),
                alias: Some("local".to_string()),
                max_tokens: None,
                temperature: None,
                extra_params: None,
                prompt_caching: None,
                vision: None,
                context_size: None,
                preserve_reasoning: true,
                display_reasoning: false,
            }],
        };
        Arc::new(
            ModelRegistry::build(std::slice::from_ref(&ollama), Some("local"))
                .expect("registry builds"),
        )
    }

    fn test_deps(
        registry: Arc<ModelRegistry>,
        storage: Arc<dyn ConversationStorage>,
    ) -> SessionDeps {
        // `Config::default()` enables compaction, which then tries to build
        // a compaction model from `config.provider` (the legacy OpenRouter
        // default, no API key). Disable it — these tests are not about
        // compaction, and the boot-cwd cwd flow is independent of it.
        let mut config = Config::default();
        config.context.enabled = false;

        SessionDeps {
            config,
            model_registry: registry,
            skills: crate::skills::SkillRegistry::new(),
            skill_warnings: Vec::new(),
            mcp_handles: Arc::new(Vec::new()),
            searxng_config: None,
            pipelines: Arc::new(crate::pipeline::PipelineSet::default()),
            vector_store: None,
            shell_kind: None,
            storage: Some(storage),
            mcp_tools_count: 0,
            skills_count: 0,
        }
    }

    /// The bug-fix headline: a resume adopts the saved cwd as the session
    /// cwd, regardless of where the process happens to be running.
    #[tokio::test]
    async fn create_session_resume_uses_saved_cwd() {
        // Pre-create a conversation in an InMemoryStorage with a known
        // cwd that is *not* the process cwd. The two test trees must be
        // distinct — if the resume silently falls through to the boot
        // cwd, this test catches it.
        let saved_tree = tempfile::tempdir().expect("create saved cwd");
        let other_tree = tempfile::tempdir().expect("create other cwd");
        assert_ne!(saved_tree.path(), other_tree.path());

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::default());
        let saved = Conversation::new(
            "resumed".into(),
            "ollama".into(),
            "llama3".into(),
            saved_tree.path().to_string_lossy().into_owned(),
        );
        let saved_id = saved.id;
        storage.save(&saved).expect("save conversation");

        // Boot deps from `other_tree` (deliberately not the saved cwd).
        // We can't chdir the test process, but we can show the resolve
        // picks the saved cwd, not the SM's seed (= process cwd).
        let deps = test_deps(ollama_registry(), storage.clone());
        let session = create_session(&deps, Some(saved_id)).expect("create_session");

        assert_eq!(
            session.state_manager.session_cwd(),
            saved_tree.path(),
            "resume must adopt the saved cwd, not the process cwd"
        );

        // The freshly-loaded conversation's persisted cwd must match too
        // (load_conversation runs after create_provider in the real boot —
        // if a future refactor moves things, this catches a desync).
        let conv = session
            .state_manager
            .get_current_conversation()
            .expect("current conversation");
        assert_eq!(
            PathBuf::from(&conv.cwd),
            saved_tree.path(),
            "loaded conversation's persisted cwd must match the saved cwd"
        );
    }

    /// Fresh mint (no resume) inherits the boot cwd, and the freshly
    /// persisted conversation carries the same cwd — closing the cycle so
    /// a later `/load` re-adopts it.
    #[tokio::test]
    async fn create_session_fresh_uses_boot_cwd() {
        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::default());
        let deps = test_deps(ollama_registry(), storage.clone());
        let session = create_session(&deps, None).expect("create_session");

        assert_eq!(
            session.state_manager.session_cwd(),
            std::env::current_dir().unwrap(),
            "fresh mint must inherit the boot cwd"
        );

        // The freshly-minted conversation's cwd is the SM's session_cwd —
        // i.e. what `ensure_boot_conversation` was handed — not the
        // process cwd at *mint* time if the two ever diverged. They are
        // equal here (the process cwd *is* the boot cwd), but the
        // assertion is the load-bearing one: `conv.cwd` reflects the
        // caller-supplied value, not an implicit `current_dir()` read.
        let conv = session
            .state_manager
            .get_current_conversation()
            .expect("current conversation");
        assert_eq!(
            PathBuf::from(&conv.cwd),
            session.state_manager.session_cwd(),
            "fresh conversation's cwd must equal the SM's session_cwd"
        );
    }

    /// The resume path falls back to the boot cwd when the saved cwd no
    /// longer points at a directory (e.g. the user deleted the tree
    /// between sessions). The session still boots — the user can `/cd`
    /// to a valid tree at runtime.
    #[tokio::test]
    async fn create_session_resume_falls_back_when_saved_cwd_gone() {
        let gone_tree = tempfile::tempdir().expect("create gone cwd");
        let gone_path = gone_tree.path().to_path_buf();
        // Drop the tempdir so the path no longer points at a directory.
        drop(gone_tree);
        assert!(!gone_path.is_dir(), "sanity: the gone path is gone");

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::default());
        let saved = Conversation::new(
            "stale".into(),
            "ollama".into(),
            "llama3".into(),
            gone_path.to_string_lossy().into_owned(),
        );
        let saved_id = saved.id;
        storage.save(&saved).expect("save conversation");

        let deps = test_deps(ollama_registry(), storage.clone());
        let session = create_session(&deps, Some(saved_id)).expect("create_session");

        assert_eq!(
            session.state_manager.session_cwd(),
            std::env::current_dir().unwrap(),
            "missing saved cwd must fall back to the boot cwd"
        );
    }
    // ── Stage 1.2: pipeline-driven session boot ─────────────────────────────
    //
    // These pin §4 of the multi-pipeline plan: when a conversation is
    // resumed with `pipeline: Some(name)`, the session's `model_alias`
    // comes from the pipeline's orchestrator (NOT the conversation's
    // saved wire id). The pipeline owns the orchestrator — "the
    // selection replaces the state, it does not add to it."
    //
    // SessionDeps' `pipeline_registry` field is renamed `pipelines:
    // Arc<PipelineSet>` per §7 (no longer Optional — an empty set is
    // the "no pipelines configured" mode, one fewer nullable). Until
    // that rename lands, these tests won't compile (RED).

    /// Build a `SessionDeps` with a non-empty `PipelineSet`. Mirrors the
    /// existing `test_deps` helper but exercises the new field shape.
    fn test_deps_with_pipelines(
        registry: Arc<ModelRegistry>,
        storage: Arc<dyn ConversationStorage>,
        pipelines: Arc<crate::pipeline::PipelineSet>,
    ) -> SessionDeps {
        let mut config = Config::default();
        config.context.enabled = false;
        SessionDeps {
            config,
            model_registry: registry,
            skills: crate::skills::SkillRegistry::new(),
            skill_warnings: Vec::new(),
            mcp_handles: Arc::new(Vec::new()),
            searxng_config: None,
            pipelines, // Stage 1.2 field (replaces `pipeline_registry`)
            vector_store: None,
            shell_kind: None,
            storage: Some(storage),
            mcp_tools_count: 0,
            skills_count: 0,
        }
    }

    /// Construct a `PipelineSet` whose `web-team` orchestrator aliases
    /// `sonnet`, with one member `reviewer` aliasing `flash`. Mirrors
    /// the test helper in `src/pipeline/set.rs`.
    fn pipelines_with_web_team() -> Arc<crate::pipeline::PipelineSet> {
        // Same YAML shape as the fixture in `src/pipeline/set.rs::tests`;
        // resolves the same way. We could build via Rust literals but the
        // YAML path keeps this fixture honest about what real config
        // looks like (and would catch an upstream serde regression).
        let yaml = "\
providers:
  - name: openrouter
    type: openrouter
    api_key: sk-test
    models:
      - name: anthropic/claude-3.7-sonnet
        alias: sonnet
      - name: google/gemini-2.0-flash-001
        alias: flash
default_model: sonnet
pipelines:
  - name: web-team
    orchestrator:
      model: sonnet
    agents:
      reviewer:
        model: flash
        prompt: review
";
        let cfg: crate::config::Config = serde_yaml::from_str(yaml).expect("config parses");
        let set = crate::pipeline::PipelineSet::build(&cfg, &registry_two_aliases(), Some(&[]))
            .expect("set builds");
        Arc::new(set)
    }

    /// `registry_two_aliases` mirrors `ollama_registry` but with two
    /// aliases (`sonnet`, `flash`) — needed for the pipeline orchestrator
    /// (sonnet) to differ from the conversation's saved wire id (flash).
    fn registry_two_aliases() -> Arc<ModelRegistry> {
        use crate::config::{ModelEntry, ProviderEntry, ProviderType};
        let openrouter = ProviderEntry {
            name: "openrouter".into(),
            kind: ProviderType::OpenRouter,
            api_key: Some("sk-test".into()),
            base_url: None,
            models: vec![
                ModelEntry {
                    name: "anthropic/claude-3.7-sonnet".into(),
                    alias: Some("sonnet".into()),
                    max_tokens: None,
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    vision: None,
                    context_size: None,
                    preserve_reasoning: true,
                    display_reasoning: false,
                },
                ModelEntry {
                    name: "google/gemini-2.0-flash-001".into(),
                    alias: Some("flash".into()),
                    max_tokens: None,
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    vision: None,
                    context_size: None,
                    preserve_reasoning: true,
                    display_reasoning: false,
                },
            ],
        };
        Arc::new(
            ModelRegistry::build(std::slice::from_ref(&openrouter), Some("sonnet"))
                .expect("registry builds"),
        )
    }

    /// The headline Stage 1.2 contract: when a conversation is resumed
    /// with `pipeline: Some("web-team")`, the session's `model_alias` is
    /// the pipeline's orchestrator alias (`sonnet`), not the
    /// conversation's saved wire id (`flash`). The pipeline owns the
    /// orchestrator — see plan §4 "rebuild seam … first lines derive
    /// active from sm.selected_pipeline() and override resolved".
    #[tokio::test]
    async fn create_session_resume_with_pipeline_uses_orchestrator_alias() {
        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::default());
        let mut saved = Conversation::new(
            "resumed-with-pipe".into(),
            "openrouter".into(),
            // Conversation's saved wire id: FLASH — would boot to flash
            // in the no-pipeline world.
            "google/gemini-2.0-flash-001".into(),
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
        // Resume claims `web-team` is selected. Per §4, the pipeline's
        // orchestrator alias (sonnet) MUST beat the saved wire id.
        saved.pipeline = Some("web-team".into());
        let saved_id = saved.id;
        storage.save(&saved).expect("save");

        let deps = test_deps_with_pipelines(
            registry_two_aliases(),
            storage.clone(),
            pipelines_with_web_team(),
        );
        let session = create_session(&deps, Some(saved_id)).expect("create_session");

        assert_eq!(
            session.model_alias, "sonnet",
            "resumed conversation with `pipeline: web-team` must boot the pipeline's \
             orchestrator alias (`sonnet`), NOT the saved wire id (`flash`)"
        );
        // The selection is mirrored into AppState (plan §3 "One nullable fact").
        assert_eq!(
            session.state_manager.selected_pipeline().as_deref(),
            Some("web-team"),
            "selection must be mirrored into AppState.selected_pipeline on resume"
        );
    }

    /// The inverse case: a conversation saved with `pipeline: Some("ghost")`
    /// where `ghost` is no longer configured. `resolve_saved` returns
    /// `(None, Some(warning))` — the implementer must drop the
    /// selection, boot the registry default, AND surface the warning
    /// to the user. The alias that lands on `session.model_alias` is
    /// the registry default (`sonnet`), not anything stale.
    #[tokio::test]
    async fn create_session_resume_with_unconfigured_pipeline_clears_with_warning() {
        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::default());
        let mut saved = Conversation::new(
            "ghost-pipe".into(),
            "openrouter".into(),
            "google/gemini-2.0-flash-001".into(),
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
        saved.pipeline = Some("ghost".into());
        let saved_id = saved.id;
        storage.save(&saved).expect("save");

        let deps = test_deps_with_pipelines(
            registry_two_aliases(),
            storage.clone(),
            // Only `web-team` is configured — `ghost` is missing.
            pipelines_with_web_team(),
        );
        let session = create_session(&deps, Some(saved_id)).expect("create_session");

        // Selection is dropped (amendment 5: orphan pipeline → none,
        // not the stale name).
        assert_eq!(
            session.state_manager.selected_pipeline().as_deref(),
            None,
            "an unconfigured pipeline name must be dropped on resume"
        );
        // The model falls back to the registry default (no pipeline
        // orchestrator to take over).
        assert_eq!(
            session.model_alias, "sonnet",
            "with no pipeline selected, the session must boot the registry default \
             (`sonnet`), NOT any stale orchestrator alias"
        );

        // A system message must surface the warning. We can't assert on
        // exact wording (that's plan §7's "warning text") but the name
        // `ghost` and the phrase "no longer" / "configured" must be in
        // the message so the user understands what changed.
        let state = session.state_manager.get_state();
        let last_warning = state
            .chat
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, crate::ui::app_state::MessageRole::System))
            .map(|m| m.content.clone())
            .unwrap_or_default();
        assert!(
            last_warning.contains("ghost"),
            "warning must name the dropped pipeline; got: {last_warning:?}"
        );
        assert!(
            last_warning.contains("no longer") || last_warning.contains("configured"),
            "warning must explain why (no longer / configured); got: {last_warning:?}"
        );
    }

    /// Fresh session baseline: no resume, no pipeline selection. The
    /// session boots the registry default; nothing is mirrored into
    /// `AppState.selected_pipeline`. Mirrors plan §4 "fresh session =
    /// None (single agent, default_model)".
    #[tokio::test]
    async fn create_session_fresh_has_no_pipeline_selected() {
        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::default());
        let deps = test_deps_with_pipelines(
            registry_two_aliases(),
            storage.clone(),
            pipelines_with_web_team(),
        );
        let session = create_session(&deps, None).expect("create_session");

        assert_eq!(
            session.state_manager.selected_pipeline().as_deref(),
            None,
            "fresh session must start with selected_pipeline = None even when pipelines are configured"
        );
        assert_eq!(
            session.model_alias, "sonnet",
            "fresh session must boot the registry default"
        );
    }
}
