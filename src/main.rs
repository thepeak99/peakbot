//! PeakBot entry point

use anyhow::Result;
use peakbot::{
    AgentRunner, Config, SubAgentRegistry, TodoTool, UiAction, Ui, build_system_prompt,
    create_provider, load_default_skills, load_mcp_servers,
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

    // Load MCP servers
    let mcp_handles = load_mcp_servers(&config).await?;
    let mcp_tools_count = mcp_handles.len(); // Keep count before moving handles
    let mcp_tools = if mcp_handles.is_empty() {
        None
    } else {
        let mut all_tools = Vec::new();
        for handle in mcp_handles {
            use rig::tool::ToolDyn;
            let tools: Vec<Box<dyn ToolDyn>> = handle.take_tools();
            all_tools.extend(tools);
        }
        Some(all_tools)
    };

    let searxng_config = config.searxng_enabled().then(|| config.searxng.as_ref()).flatten();

    // StateManager must be created before TodoTool so we can pass it in
    let state_manager = Arc::new(peakbot::StateManager::new());

    let todo_tool = TodoTool::new(state_manager.clone());

    // Pipeline
    let pipeline_registry = if config.pipeline_enabled() {
        let pipeline_config = config.pipeline().unwrap();
        let openrouter_key = match &config.provider {
            peakbot::ProviderConfig::OpenRouter(c) => c.api_key.clone(),
            _ => std::env::var("OPENROUTER_API_KEY").ok(),
        };
        let openai_key = match &config.provider {
            peakbot::ProviderConfig::OpenAI(c) => c.api_key.clone(),
            _ => std::env::var("OPENAI_API_KEY").ok(),
        };
        let llamacpp_key = std::env::var("LLAMACPP_API_KEY").ok();
        let llamacpp_url = std::env::var("LLAMACPP_BASE_URL").ok();
        let ollama_url = std::env::var("OLLAMA_BASE_URL").ok();

        Some(SubAgentRegistry::new(
            pipeline_config,
            openrouter_key,
            openai_key,
            llamacpp_key,
            llamacpp_url,
            ollama_url,
        ))
    } else {
        None
    };

    // Create provider
    // Compute context window from model name (same logic as ContextManager)
    let context_window = match config.model().to_lowercase().as_str() {
        m if m.contains("claude-3.7-sonnet") => 200_000,
        m if m.contains("claude-3.5-sonnet") => 200_000,
        m if m.contains("claude-3-opus") => 200_000,
        m if m.contains("claude-3-sonnet") => 200_000,
        m if m.contains("claude-3-haiku") => 200_000,
        m if m.contains("gpt-4o") => 128_000,
        m if m.contains("gpt-4-turbo") => 128_000,
        m if m.contains("gpt-4-32k") => 32_768,
        m if m.contains("gpt-4") => 8_192,
        m if m.contains("gpt-3.5-turbo") => 16_385,
        m if m.contains("gemini-2.0") => 1_000_000,
        m if m.contains("gemini-1.5-pro") => 2_000_000,
        m if m.contains("gemini-1.5-flash") => 1_000_000,
        _ => 128_000, // default
    };

    let (agent, provider_info, event_receiver, session_hook) = create_provider(
        &config.provider,
        &config.context,
        context_window,
        mcp_tools,
        &system_prompt,
        searxng_config,
        config.agent_max_turns,
        Some(todo_tool),
        &config.bash,
        pipeline_registry.as_ref(),
        state_manager.clone(),
    )?;

    tracing::info!(
        "Using provider: {} with model: {}",
        provider_info.name,
        provider_info.model
    );

    // StateManager is already created above and shared with TodoTool
    state_manager.set_model(provider_info.model.clone());

    // Channel: View → Controller
    let (action_sender, action_receiver) = mpsc::unbounded_channel::<UiAction>();

    // Create AgentRunner (Controller)
    let mut runner = AgentRunner::new(
        agent,
        config.clone(),
        provider_info.clone(),
        skills,
        event_receiver,
        Some(state_manager.clone()),
        session_hook,
    );

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

    // Run REPL View (blocking)
    let mut ui = ReplUi::new(state_manager.clone(), action_sender);
    ui.init().await?;
    ui.run().await?;
    ui.shutdown().await?;

    let _ = runner_handle.abort();
    let _ = runner_handle.await;

    Ok(())
}
