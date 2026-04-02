//! PeakBot entry point

use anyhow::Result;
use clap::Parser;
use peakbot::{
    AgentRunner, Config, SubAgentRegistry, TodoTool, UiAction, Ui, build_system_prompt,
    create_provider, load_default_skills, load_mcp_servers,
};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

/// CLI arguments for PeakBot
#[derive(Parser, Debug)]
#[command(name = "peakbot")]
#[command(about = "PeakBot coding agent with TUI and REPL modes")]
struct Args {
    /// Choose the UI mode: 'tui' for rich terminal UI, 'repl' for simple REPL
    #[arg(short, long, default_value = "repl")]
    ui: UiMode,
}

/// UI mode selection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiMode {
    /// Rich terminal UI using ratatui (requires tui feature)
    Tui,
    /// Simple REPL interface (always available)
    Repl,
}

impl std::str::FromStr for UiMode {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "tui" => Ok(UiMode::Tui),
            "repl" => Ok(UiMode::Repl),
            _ => Err(format!("Invalid UI mode '{}'. Use 'tui' or 'repl'", s)),
        }
    }
}

impl std::fmt::Display for UiMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UiMode::Tui => write!(f, "tui"),
            UiMode::Repl => write!(f, "repl"),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    #[cfg(not(feature = "tui"))]
    if args.ui == UiMode::Tui {
        eprintln!("Error: TUI mode requires the 'tui' feature to be enabled.");
        eprintln!("Run with: cargo run --features tui -- --ui tui");
        eprintln!("Or use REPL mode: peakbot --ui repl");
        std::process::exit(1);
    }

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

    let todo_tool = TodoTool::new();

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
    let (agent, provider_info, cost_tracker, todo_state, event_receiver) = create_provider(
        &config.provider,
        mcp_tools,
        &system_prompt,
        searxng_config,
        config.agent_max_turns,
        Some(todo_tool),
        &config.bash,
        pipeline_registry.as_ref(),
    )?;

    tracing::info!(
        "Using provider: {} with model: {}",
        provider_info.name,
        provider_info.model
    );

    // StateManager (Model) — shared by all
    let state_manager = Arc::new(peakbot::ui::StateManager::new());
    if let Ok(list) = todo_state.lock() {
        state_manager.update_todo(&list);
    }
    state_manager.update_stats(&cost_tracker.get_session_stats());

    // Channel: View → Controller
    let (action_sender, action_receiver) = mpsc::unbounded_channel::<UiAction>();

    // Create AgentRunner (Controller)
    let mut runner = AgentRunner::new(
        agent,
        config.clone(),
        provider_info.clone(),
        skills,
        cost_tracker,
        Some(todo_state),
        event_receiver,
        Some(state_manager.clone()),
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

    // ── Run UI ──────────────────────────────────────────────────────
    match args.ui {
        UiMode::Tui => {
            #[cfg(feature = "tui")]
            {
                use peakbot::ui::Tui;

                // Spawn controller task
                let runner_handle = tokio::spawn(async move {
                    runner.run_loop(action_receiver).await;
                });

                // Run TUI (blocking)
                let mut tui = Tui::new(state_manager.clone(), action_sender);
                tui.init()?;
                tui.run()?;
                tui.shutdown()?;

                // Signal controller to exit
                let _ = runner_handle.abort();
                let _ = runner_handle.await;
            }
            #[cfg(not(feature = "tui"))]
            {
                unreachable!();
            }
        }
        UiMode::Repl => {
            use peakbot::ui::ReplUi;

            // Spawn controller task
            let runner_handle = tokio::spawn(async move {
                runner.run_loop(action_receiver).await;
            });

            // Run REPL View (blocking)
            let mut ui = ReplUi::new(state_manager.clone(), action_sender);
            ui.init()?;
            ui.run()?;
            ui.shutdown()?;

            let _ = runner_handle.abort();
            let _ = runner_handle.await;
        }
    }

    Ok(())
}
