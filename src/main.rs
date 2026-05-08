//! PeakBot entry point

use anyhow::Result;
use peakbot::{
    AgentRunner, Config, FileStorage, SubAgentRegistry, TodoTool, Ui, UiAction,
    build_system_prompt, create_provider, load_default_skills, load_mcp_servers,
};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // ── Shared setup ──────────────────────────────────────────────
    let config = Config::load()?;
    let skills = load_default_skills()?;
    let skills_count = skills.len(); // Keep count before moving skills
    let system_prompt = build_system_prompt(&skills);

    // Build the model registry. Two paths:
    // - `providers:` list declared → multi-model, `/model` enabled.
    // - Legacy `provider:` block → synthesised one-entry registry
    //   with alias `default`. `/model default` is a no-op; the user
    //   gets a "no other models declared" message if they try to
    //   switch. See `multi-model.md`.
    let model_registry = match config.build_model_registry() {
        Ok(reg) => Arc::new(reg),
        Err(e) => {
            anyhow::bail!("Invalid model configuration: {e}");
        }
    };

    // Load MCP servers
    let mcp_handles = load_mcp_servers(&config).await?;
    let mcp_tools_count = mcp_handles.len(); // Keep count before moving handles
    // Wrap the handles in an Arc so the AgentRunner's RebuildContext
    // can re-derive the tools list across `/model` switches without
    // restarting the underlying subprocesses. McpTool: Clone (rig 0.33)
    // makes the per-build tool list cheap.
    let mcp_handles_arc = Arc::new(mcp_handles);
    let mcp_tools = if mcp_handles_arc.is_empty() {
        None
    } else {
        let mut all_tools = Vec::new();
        for handle in mcp_handles_arc.iter() {
            use rig::tool::ToolDyn;
            let tools: Vec<Box<dyn ToolDyn>> = handle
                .tools()
                .iter()
                .cloned()
                .map(|t| Box::new(t) as Box<dyn ToolDyn>)
                .collect();
            all_tools.extend(tools);
        }
        Some(all_tools)
    };

    let searxng_config = config
        .searxng_enabled()
        .then_some(config.searxng.as_ref())
        .flatten();

    // Create conversation storage if enabled
    let state_manager = if config.conversation_enabled() {
        let storage_dir = config.conversation_storage_dir();
        match FileStorage::new(storage_dir.clone()) {
            Ok(storage) => {
                tracing::info!("Conversation storage enabled at: {:?}", storage_dir);
                peakbot::StateManager::new_arc_with_storage(Arc::new(storage))
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to create conversation storage at {:?}: {}. \
                     Continuing without persistence.",
                    storage_dir,
                    e
                );
                peakbot::StateManager::new_arc()
            }
        }
    } else {
        peakbot::StateManager::new_arc()
    };

    let todo_tool = TodoTool::new(state_manager.clone());

    // Pipeline
    let pipeline_registry = if config.pipeline_enabled() {
        let pipeline_config = config.pipeline().unwrap();

        Some(SubAgentRegistry::new(pipeline_config))
    } else {
        None
    };

    // Create provider — context window resolution lives in the registry
    // (per-model `context_size:` OR `auto_detect_context_size` against
    // the wire id). The provider itself doesn't need to know the value;
    // `ContextManager` is the single consumer downstream.
    //
    // Boot path: when a multi-model `providers:` block was declared, the
    // registry's default model owns the live credentials/wire id. The
    // legacy `Config::provider` field is just `OpenRouterConfig::default()`
    // in that case (api_key=None, model="anthropic/claude-3.7-sonnet"),
    // so handing that to `create_provider` reliably surfaces a misleading
    // "OpenRouter API key not configured" — the bug we just fixed.
    // *(reuse the seam that already exists)*
    let boot_provider_config: &peakbot::ProviderConfig = match model_registry.default_alias() {
        Some(alias) => {
            &model_registry
                .resolve(alias)
                .expect("default_alias is guaranteed to resolve by ModelRegistry::build")
                .provider_config
        }
        None => &config.provider,
    };

    let (agent, provider_info, event_receiver, session_hook) = create_provider(
        boot_provider_config,
        mcp_tools,
        &system_prompt,
        searxng_config,
        config.agent_max_turns,
        Some(todo_tool.clone()),
        &config.bash,
        pipeline_registry.as_ref(),
        state_manager.clone(),
    )?;

    tracing::info!(
        "Using provider: {} with model: {}",
        provider_info.name,
        provider_info.model
    );

    // StateManager is already created above and shared with TodoTool.
    // Stamp the wire identity `(provider_name, model)` and the alias
    // (the active entry from the registry) so `/model` / `/load` /
    // status bar all see consistent values from boot. The wire id is
    // what gets persisted on conversations; the alias is display-only.
    state_manager.set_model(provider_info.model.clone());
    let (boot_provider_name, boot_alias) = match model_registry.default_alias() {
        Some(a) => match model_registry.resolve(a) {
            Some(rm) => (rm.provider_name.clone(), rm.alias.clone()),
            None => (String::new(), a.to_string()),
        },
        None => (String::new(), "default".to_string()),
    };
    state_manager.set_provider_name(boot_provider_name);
    state_manager.set_model_alias(boot_alias.clone());

    // Channel: View → Controller
    let (action_sender, action_receiver) = mpsc::unbounded_channel::<UiAction>();

    // Build the rebuild context so /model can rebuild between turns
    // without restarting the process. MCP handles, system prompt,
    // builtins config, and the registry are all kept alive here.
    let rebuild_ctx = peakbot::RebuildContext {
        registry: model_registry.clone(),
        system_prompt: system_prompt.clone(),
        mcp_handles: mcp_handles_arc.clone(),
        searxng_config: config.searxng.clone(),
        max_turns: config.agent_max_turns,
        todo_tool: Some(todo_tool),
        bash_config: config.bash.clone(),
        pipeline_registry: pipeline_registry.clone().map(Arc::new),
    };

    // Resolve the boot model's context_size (from config or auto-detected).
    // This value drives ContextManager compaction thresholds.
    let boot_context_size = model_registry
        .default_alias()
        .and_then(|a| model_registry.resolve(a))
        .map(|rm| rm.context_size)
        .unwrap_or_else(|| {
            tracing::warn!("No default model in registry, using auto-detect for context_size");
            peakbot::auto_detect_context_size(provider_info.model.as_str())
        });

    // Create AgentRunner (Controller)
    let mut runner = AgentRunner::new(
        agent,
        config.clone(),
        provider_info.clone(),
        skills,
        event_receiver,
        Some(state_manager.clone()),
        session_hook,
        boot_context_size,
    )
    .with_rebuild_context(rebuild_ctx);

    // Set up welcome banner state
    state_manager.set_welcome(peakbot::ui::app_state::WelcomeState {
        provider_name: provider_info.name.clone(),
        model: provider_info.model.clone(),
        max_tokens: config.max_tokens() as usize,
        builtin_tools_count: 8, // file_edit, file_read, bash, list_directory, fetch_url, think, todo, search
        mcp_tools_count,
        skills_count,
        searxng_enabled: config.searxng_enabled(),
        searxng_url: config.searxng.as_ref().map(|s| s.base_url.clone()),
        cost_tracking_enabled: config.supports_pricing() && config.cost_tracking,
        compaction_enabled: config.context.enabled,
        compaction_threshold: config.context.threshold,
        compaction_keep_recent: config.context.keep_recent,
        conversation_persistence_enabled: config.conversation_enabled(),
        cwd: std::env::current_dir().unwrap_or_default(),
    });

    // ── Run REPL ──────────────────────────────────────────────────────
    use peakbot::ui::ReplUi;

    // Spawn controller task
    let runner_handle = tokio::spawn(async move {
        runner.run_loop(action_receiver).await;
    });

    // Run REPL View (blocking). Pass the model registry through so
    // `/model <alias>` is intercepted, validated, and confirmed in
    // the View before any UiAction is dispatched.
    let mut ui =
        ReplUi::new_with_registry(state_manager.clone(), action_sender, model_registry.clone());
    ui.init().await?;
    ui.run().await?;
    ui.shutdown().await?;

    runner_handle.abort();
    let _ = runner_handle.await;

    Ok(())
}
