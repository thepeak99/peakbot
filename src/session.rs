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
use crate::tools::ShellKind;
use crate::ui::app_state::WelcomeState;
use crate::{
    AgentRunner, McpServerHandle, RebuildContext, SkillRegistry, StateManager, SubAgentRegistry,
    TodoTool, UiAction, create_provider,
};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Process-wide inputs shared by every session. Built once in `main`,
/// borrowed by each [`create_session`] call. The heavy handles
/// (`mcp_handles`, `vector_store`, `pipeline_registry`, `model_registry`)
/// are `Arc`-backed, so the per-call clones are cheap.
pub struct SessionDeps {
    /// Boot config with `provider` already mirrored to the active provider
    /// (see `Config::resolve_and_mirror_boot_provider`). Cloned per session
    /// so each `AgentRunner` reads a stable provider for its compaction model.
    pub config: Config,
    pub model_registry: Arc<ModelRegistry>,
    pub system_prompt: String,
    pub skills: SkillRegistry,
    /// One set of MCP subprocesses shared by all sessions; each session
    /// clones the *tools list*, not the processes.
    pub mcp_handles: Arc<Vec<McpServerHandle>>,
    pub searxng_config: Option<SearXngConfig>,
    pub pipeline_registry: Option<Arc<SubAgentRegistry>>,
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

    let todo_tool = TodoTool::new(state_manager.clone());

    // Pick the model to boot on. A resumed conversation boots on the model
    // it was saved with (mirrors the REPL `/load` re-activation); a fresh
    // session boots the registry default. The saved wire id is peeked
    // without loading — the same preflight `/load` uses.
    let saved_wire_id = resume.and_then(|id| state_manager.peek_conversation_wire_id(id).ok());
    let boot = deps.model_registry.resolve_boot(
        saved_wire_id
            .as_ref()
            .map(|(p, m)| (p.as_str(), m.as_str())),
    );
    // A resumed conversation boots on its saved model; a fresh session on
    // the registry default. `resolve_boot` guarantees a model (a config-built
    // registry always has a default), so there is no None case to handle.
    let boot_model = boot.model;
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

    let (agent, provider_info, event_receiver, session_hook) = create_provider(
        boot_provider_config,
        mcp_tools,
        &deps.system_prompt,
        deps.searxng_config.as_ref(),
        deps.config.agent_max_turns,
        Some(todo_tool.clone()),
        &deps.config.bash,
        deps.pipeline_registry.as_deref(),
        state_manager.clone(),
        deps.shell_kind.as_ref(),
        deps.vector_store.as_ref(),
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
    // current conversation always exists afterwards.
    if let Some(id) = resume {
        let _ = state_manager.load_conversation(id);
    }
    state_manager.ensure_boot_conversation(provider_info.model.as_str());
    let conversation_id = state_manager
        .get_current_conversation_id()
        .expect("ensure_boot_conversation guarantees a current conversation");

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

    let (action_sender, action_receiver) = mpsc::unbounded_channel::<UiAction>();

    let rebuild_ctx = RebuildContext {
        registry: deps.model_registry.clone(),
        system_prompt: deps.system_prompt.clone(),
        mcp_handles: deps.mcp_handles.clone(),
        searxng_config: deps.searxng_config.clone(),
        max_turns: deps.config.agent_max_turns,
        todo_tool: Some(todo_tool),
        bash_config: deps.config.bash.clone(),
        pipeline_registry: deps.pipeline_registry.clone(),
        shell_kind: deps.shell_kind.clone(),
        skills: deps.skills.clone(),
        vector_store: deps.vector_store.clone(),
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
        cwd: std::env::current_dir().unwrap_or_default(),
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
