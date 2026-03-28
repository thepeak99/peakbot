// Use the library crate (defined in lib.rs)
use anyhow::Result;
use clap::Parser;
use peakbot::{
    AgentRunner, Config, TodoTool, build_system_prompt, create_provider, load_default_skills,
    load_mcp_servers,
};
#[cfg(feature = "tui")]
use peakbot::ui::{Tui, Ui, UiAction};
use peakbot::ui::StateManager;
use std::sync::Arc;
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
    // Setup logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // Parse CLI arguments
    let args = Args::parse();
    
    // Validate TUI mode availability
    #[cfg(not(feature = "tui"))]
    if args.ui == UiMode::Tui {
        eprintln!("Error: TUI mode requires the 'tui' feature to be enabled.");
        eprintln!("Run with: cargo run --features tui -- --ui tui");
        eprintln!("Or use REPL mode: peakbot --ui repl");
        std::process::exit(1);
    }

    // Load configuration from environment variables
    let config = Config::load()?;

    // Load skills from default locations (~/.agents/skills and ./.agents/skills)
    let skills = load_default_skills()?;

    // Build the system prompt dynamically with environment info and skills
    let system_prompt = build_system_prompt(&skills);

    // Load MCP servers first (async operation) - this gives us MCP tools
    let mcp_handles = load_mcp_servers(&config).await?;

    // Extract MCP tools from the handles (if any)
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

    // Get SearXNG config if enabled
    let searxng_config = if config.searxng_enabled() {
        config.searxng.as_ref()
    } else {
        None
    };

    // Create the todo tool
    let todo_tool = TodoTool::new();

    // Create provider (agent) with all tools
    let (agent, provider_info, cost_tracker, todo_state, event_receiver) = create_provider(
        &config.provider,
        mcp_tools,
        &system_prompt,
        searxng_config,
        config.agent_max_turns,
        Some(todo_tool),
        &config.bash,
    )?;
    tracing::info!(
        "Using provider: {} with model: {}",
        provider_info.name,
        provider_info.model
    );

    // Create StateManager for UI state
    let state_manager = Arc::new(StateManager::new());
    
    // Initialize state with current data
    if let Ok(list) = todo_state.lock() {
        state_manager.update_todo(&list);
    }
    let stats = cost_tracker.get_session_stats();
    state_manager.update_stats(&stats);

    // Run the appropriate UI based on CLI argument
    match args.ui {
        UiMode::Tui => {
            #[cfg(feature = "tui")]
            {
                use tokio::sync::mpsc;
                use peakbot::ui::tui::{TuiAgentRunner, RunnerEvent};
                
                // Create channel for TUI -> Agent communication
                let (action_sender, mut action_receiver) = mpsc::unbounded_channel::<UiAction>();
                
                // Create channel for Runner -> TUI events
                let (event_sender, mut event_receiver) = mpsc::unbounded_channel::<RunnerEvent>();
                
                let mut tui = Tui::new(state_manager.clone(), Some(action_sender.clone()));
                tui.init()?;
                
                // Create TuiAgentRunner (takes ownership of components)
                let mut runner = TuiAgentRunner::new(
                    agent,
                    config.clone(),
                    provider_info.clone(),
                    skills,
                    cost_tracker,
                    state_manager.clone(),
                    Some(event_sender),
                );
                
                // Spawn the agent runner in a separate task that processes UI actions
                let agent_handle = tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            Some(action) = action_receiver.recv() => {
                                match action {
                                    UiAction::Exit => {
                                        break;
                                    }
                                    _ => {
                                        // Process action through runner
                                        if let Err(e) = runner.process_action(action).await {
                                            eprintln!("Error processing action: {}", e);
                                        }
                                    }
                                }
                            }
                            Some(event) = event_receiver.recv() => {
                                match event {
                                    RunnerEvent::Exit => {
                                        // Signal TUI to exit
                                        break;
                                    }
                                    RunnerEvent::AgentBusy => {
                                        tracing::debug!("Agent is processing a request");
                                    }
                                    RunnerEvent::AgentIdle => {
                                        tracing::debug!("Agent is ready for next input");
                                    }
                                    RunnerEvent::Error(e) => {
                                        eprintln!("Agent error: {}", e);
                                    }
                                    RunnerEvent::StatsUpdated => {
                                        // Stats updated in UI
                                    }
                                }
                            }
                        }
                    }
                });
                
                // Run TUI loop (blocking)
                tui.run()?;
                tui.shutdown()?;
                
                // Signal agent to exit
                let _ = action_sender.send(UiAction::Exit);
                
                // Wait for agent to finish
                let _ = agent_handle.await;
            }
            #[cfg(not(feature = "tui"))]
            {
                // TUI mode is not available - this branch should never be reached
                // since we check and exit earlier if TUI is requested without the feature
            }
        }
        UiMode::Repl => {
            // Create AgentRunner for REPL mode
            let mut agent_runner = AgentRunner::new(
                agent,
                config,
                provider_info,
                skills,
                cost_tracker,
                Some(todo_state),
                event_receiver,
                Some(state_manager),
            );
            
            // Run REPL - simple stdin/stdout interface
            agent_runner.run().await?;
        }
    }

    Ok(())
}
