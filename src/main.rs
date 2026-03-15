// Use the library crate (defined in lib.rs)
use anyhow::Result;
use peakbot::{
    AgentRunner, Config, TodoTool, build_system_prompt, create_provider, load_default_skills,
    load_mcp_servers,
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // Setup logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // Load configuration from environment variables
    let config = Config::load()?;

    // Load skills from default locations (~/.agents/skills and ./.agents/skills)
    let skills = load_default_skills()?;

    // Build the system prompt dynamically with environment info and skills
    // This must be done before creating the provider so the agent can use it as preamble
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

    // Create the todo tool - we'll pass it to the provider and store its state
    let todo_tool = TodoTool::new();

    // Create provider (agent) with all tools (built-in + MCP) and system prompt
    // The event_receiver is returned for external processing by AgentRunner
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

    // Run the interactive REPL - the agent includes both built-in and MCP tools
    let mut runner = AgentRunner::new(
        agent,
        config.clone(),
        provider_info,
        skills,
        cost_tracker,
        Some(todo_state),
        event_receiver,
    );
    runner.run().await?;

    Ok(())
}
