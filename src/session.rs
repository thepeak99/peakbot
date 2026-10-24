//! Session factory — builds one independent agent (Model + Controller).
//!
//! PeakBot was single-session by construction: one `StateManager` (Model) +
//! one `AgentRunner` (Controller), built once in `main` and shared by
//! whichever View ran. The web UI makes **each tab its own agent**, so that
//! "build one session" block is named here and called 1×N.
//!
//! [`SessionDeps`] holds the process-wide inputs (boot config, MCP
//! subprocess handles, skills, storage). It is built once and borrowed by
//! every [`create_session`] call. Everything per-session (a fresh
//! `StateManager`, its `AgentRunner` + spawned `run_loop`, the `UiAction`
//! channel, the `TodoTool`, the session's one `Bound`) is built anew each
//! call.
//!
//! ## Teardown (Option C, ephemeral session-per-connection)
//!
//! A [`Session`] owns its `action_sender` and the `run_loop` task handle.
//! Dropping the `Session` drops the `action_sender`; `run_loop`'s event
//! loop then observes the closed channel, aborts the agent loop, calls
//! `clear_bg()` (kills this session's PTY children), and returns. No
//! `Drop` impl or `AbortHandle` is needed — the channel-close cascade is
//! the teardown (verified `lib.rs` run_loop end).

use crate::config::Config;
use crate::tools::ShellKind;
use crate::ui::app_state::WelcomeState;
use crate::{
    AgentRunner, McpServerHandle, PEAKBOT_VERSION, RebuildContext, SkillRegistry, StateManager,
    TodoTool, UiAction, build_bound_agent, reload_session_config,
};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Process-wide inputs shared by every session. Built once in `main`,
/// borrowed by each [`create_session`] call. Everything else a session
/// needs (registry, profiles, search, vector store, tabs) is derived from
/// `config` — or from the fresh config each session loads.
pub struct SessionDeps {
    /// The **boot** config: the one `boot_only_diff` compares a session's
    /// fresh config against, and the fallback when that load fails.
    pub config: Arc<Config>,
    /// The boot skill scan; each session rescans for its own cwd.
    pub skills: SkillRegistry,
    /// One set of MCP subprocesses shared by all sessions; each session
    /// clones the *tools list*, not the processes.
    pub mcp_handles: Arc<Vec<McpServerHandle>>,
    /// The boot migration notices (spec §7.5). The **first** session takes
    /// them (`std::mem::take`) and shows them as system messages; later
    /// sessions find the vec empty.
    pub boot_notices: Mutex<Vec<String>>,
    pub shell_kind: Option<ShellKind>,
    /// Shared conversation storage (writes distinct files per conversation
    /// id), or `None` when persistence is disabled. Cloned into each
    /// session's `StateManager`.
    pub storage: Option<Arc<dyn crate::ConversationStorage>>,
    /// The boot `--profile` pin. Every session runs under it, refuses
    /// runtime profile selection and ignores repo files.
    pub profile_pin: Option<String>,
    /// The upload store, built once at boot from `general.uploads` (the
    /// limits are boot-only). Cloned into each session's `StateManager`.
    pub uploads: crate::attachments::UploadStore,
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
/// Every session reads a **fresh** config for its cwd, exactly like `/new`
/// (spec §6.7), so a new web tab sees edits saved since boot and a resumed
/// conversation sees its own tree's repo profiles. A failed load keeps the
/// boot config and says so.
///
/// ## Per-session cwd
///
/// This is the **single point** that resolves the per-session `session_cwd`.
/// Resume adopts the saved cwd (persisted on the conversation at mint time);
/// fresh sessions inherit the boot `current_dir()`. `set_session_cwd` runs
/// *before* the agent build — the tools and the prompt snapshot it then.
pub fn create_session(deps: &SessionDeps, resume: Option<Uuid>) -> Result<Session> {
    // Per-session Model. Storage (if any) is shared — it writes distinct
    // files per conversation id.
    let state_manager =
        StateManager::new_arc_for_session(deps.storage.clone(), deps.uploads.clone());

    // ── Per-session cwd ──────────────────────────────────────────────────────
    // Resume adopts the saved cwd iff it's non-empty and still points at a
    // directory. Anything else (no resume, no storage, missing/empty
    // cwd, gone directory) falls through to the boot cwd. A gone cwd is
    // best-effort: the user can `/cd` to a valid tree at runtime.
    let boot_cwd = std::env::current_dir().unwrap_or_default();
    let session_cwd = resume
        .and_then(|id| state_manager.peek_conversation_cwd(id).ok())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or(boot_cwd);
    // Stamp the SM *before* the agent build so the tools and the prompt
    // snapshot the per-session value.
    state_manager.set_session_cwd(session_cwd.clone());

    let todo_tool = TodoTool::new(state_manager.clone());
    let start = deps
        .config
        .catalog
        .start_bound(deps.profile_pin.as_deref())
        .map_err(anyhow::Error::msg)?;
    let mut ctx = RebuildContext {
        config: Arc::clone(&deps.config),
        bound: start,
        mcp_handles: deps.mcp_handles.clone(),
        shell_kind: deps.shell_kind.clone(),
        skills: deps.skills.clone(),
        todo_tool: Some(todo_tool),
        profile_pin: deps.profile_pin.clone(),
    };

    // Fresh config + skills for the session cwd (repo-file migration, boot-
    // only diff warnings and catalog publishing included). The messages are
    // emitted after the conversation exists — a load would replace them.
    let mut messages = reload_session_config(&mut ctx, &state_manager, &session_cwd);

    // Bind: a fresh session starts on the default (or pinned) profile's
    // first agent; a resumed one re-binds to its saved `(profile, agent)`.
    let saved = resume
        .and_then(|id| state_manager.peek_conversation_selection(id).ok())
        .flatten();
    let nonempty = |s: &str| (!s.is_empty()).then(|| s.to_string());
    let saved_profile = saved.as_ref().and_then(|s| nonempty(&s.profile));
    let saved_agent = saved.as_ref().and_then(|s| nonempty(&s.agent));
    let (bound, warnings) = crate::selection::resolve_saved_selection(
        &ctx.config.catalog,
        saved_profile.as_deref(),
        saved_agent.as_deref(),
        deps.profile_pin.as_deref(),
    );
    ctx.bound = bound;
    messages.extend(warnings);
    state_manager.set_selection(ctx.bound.selection());
    state_manager.set_visible_tabs(
        ctx.bound
            .profile
            .tabs
            .iter()
            .map(|tab| tab.to_string())
            .collect(),
    );

    // Boot model: a resumed conversation's saved wire id when it is still
    // configured (the model is free, spec §6.5), else the bound agent's model.
    let saved_wire_id = resume.and_then(|id| state_manager.peek_conversation_wire_id(id).ok());
    let boot = ctx.config.registry.resolve_boot(
        saved_wire_id
            .as_ref()
            .map(|(p, m)| (p.as_str(), m.as_str())),
        &ctx.bound.agent().main.model,
    );
    let model = boot.model.clone();
    let unavailable = boot.unavailable;

    let (agent, provider_info, event_receiver, session_hook) =
        build_bound_agent(&model, &ctx, &state_manager).map_err(anyhow::Error::msg)?;

    // Thread the resolved reasoning gates into the shared StateManager —
    // the boot half of the pair; `AgentRunner::rebuild_agent` is the other.
    state_manager
        .set_wire_reasoning(provider_info.name == "anthropic" && provider_info.preserve_reasoning);
    state_manager.set_display_reasoning(provider_info.display_reasoning);
    state_manager.set_supports_vision(provider_info.supports_vision);

    // Stamp the wire identity `(provider_name, model)` and the display
    // alias for the booted model.
    state_manager.set_model(provider_info.model.clone());
    state_manager.set_provider_name(model.provider_name.clone());
    state_manager.set_model_alias(model.alias.clone());

    // Initialise the conversation synchronously so `conversation_id` is valid
    // before the controller loop spawns. `resume` best-effort adopts a
    // persisted conversation; `ensure_boot_conversation` is idempotent, so it
    // mints a fresh one iff the resume didn't produce a current conversation
    // (unknown id, disabled storage, or `None`). The fresh-mint path
    // persists `session_cwd` and the selection.
    if let Some(id) = resume {
        let _ = state_manager.load_conversation(id);
    }
    state_manager.ensure_boot_conversation(&session_cwd, provider_info.model.as_str());
    let conversation_id = state_manager
        .get_current_conversation_id()
        .expect("ensure_boot_conversation guarantees a current conversation");
    // `load_conversation` restored the *saved* names; re-stamp the bound the
    // session actually runs so a dropped name is not re-persisted.
    state_manager.set_selection(ctx.bound.selection());

    // Boot migration notices reach the first session only.
    let boot_notices = std::mem::take(
        &mut *deps
            .boot_notices
            .lock()
            .expect("boot_notices lock is never poisoned"),
    );
    for message in boot_notices.into_iter().chain(messages) {
        state_manager.add_system_message(message);
    }
    // A resumed conversation whose saved model is gone from the registry
    // boots on the bound agent's model — say so rather than downgrade
    // silently.
    if let Some((provider, model_name)) = unavailable {
        state_manager.add_system_message(format!(
            "⚠ Model '{provider}/{model_name}' from this conversation is no longer available; \
             loaded on '{}' instead.",
            model.alias
        ));
    }

    let (action_sender, action_receiver) = mpsc::unbounded_channel::<UiAction>();

    let general = &ctx.config.general;
    let welcome = WelcomeState {
        provider_name: provider_info.name.clone(),
        model: provider_info.model.clone(),
        max_tokens: model.provider_config.max_tokens() as usize,
        // file_create, file_str_replace, file_insert, file_read, bash,
        // list_directory, fetch_url, fetch_page, think, todo, search
        builtin_tools_count: 11,
        mcp_tools_count: deps.mcp_handles.len(),
        skills_count: ctx.skills.len(),
        searxng_enabled: general.search.is_some(),
        searxng_url: general.search.as_ref().map(|s| s.base_url.clone()),
        cost_tracking_enabled: crate::config::cost_tracking_enabled(general.cost_tracking, &model),
        compaction_enabled: general.context.enabled,
        compaction_threshold: general.context.threshold,
        compaction_keep_recent: general.context.keep_recent,
        conversation_persistence_enabled: deps.storage.is_some(),
        cwd: session_cwd,
        peakbot_version: PEAKBOT_VERSION.to_string(),
    };

    let mut runner = AgentRunner::new(
        agent,
        Arc::clone(&ctx.config),
        provider_info,
        ctx.skills.clone(),
        event_receiver,
        Some(state_manager.clone()),
        session_hook,
        &model,
    )?
    .with_rebuild_context(ctx);

    // Welcome banner's cwd reads from `session_cwd` (the single source of
    // truth), not the live process cwd.
    state_manager.set_welcome(welcome);

    let run_handle = tokio::spawn(async move {
        runner.run_loop(action_receiver).await;
    });

    Ok(Session {
        state_manager,
        action_sender,
        model_alias: model.alias,
        conversation_id,
        _run_handle: run_handle,
    })
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Offline `SessionDeps` fixtures shared by session, registry and web tests.

    use super::*;
    use crate::config::resolve::{Config, ParseCtx, Source};
    use crate::storage::ConversationStorage;
    use std::sync::Arc;

    /// One Ollama model + one profile: the cheapest boot that works offline
    /// (no API key, no network at construction). Compaction is off — these
    /// tests are not about compaction.
    const GLOBAL_YAML: &str = r#"
providers:
  - name: ollama
    type: ollama
    models:
      - { name: llama3, alias: local }
general:
  context:
    enabled: false
profiles:
  dev:
    agents:
      team:
        model: local
"#;

    pub(crate) fn test_config() -> Config {
        Config::parse(
            Source {
                path: std::path::Path::new("/global/config.yaml"),
                text: GLOBAL_YAML,
            },
            None,
            &ParseCtx { known_skills: &[] },
        )
        .expect("fixture config must parse")
    }

    pub(crate) fn test_deps(storage: Option<Arc<dyn ConversationStorage>>) -> SessionDeps {
        SessionDeps {
            config: Arc::new(test_config()),
            skills: crate::skills::SkillRegistry::new(),
            mcp_handles: Arc::new(Vec::new()),
            boot_notices: std::sync::Mutex::new(Vec::new()),
            shell_kind: None,
            storage,
            profile_pin: None,
            uploads: crate::attachments::UploadStore::new(
                crate::attachments::UploadStore::default_root(),
                crate::config::UploadsConfig::default(),
            ),
        }
    }
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

    use super::test_support::test_deps;
    use super::*;
    use crate::Conversation;
    use crate::storage::{ConversationStorage, InMemoryStorage};
    use std::sync::Arc;

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
        let deps = test_deps(Some(storage.clone()));
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
        let deps = test_deps(Some(storage.clone()));
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

        let deps = test_deps(Some(storage.clone()));
        let session = create_session(&deps, Some(saved_id)).expect("create_session");

        assert_eq!(
            session.state_manager.session_cwd(),
            std::env::current_dir().unwrap(),
            "missing saved cwd must fall back to the boot cwd"
        );
    }
}
