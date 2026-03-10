// Use the library crate (defined in lib.rs)
use anyhow::Result;
use peakbot::{
    AgentRunner, Config, build_agent, create_openrouter_client, load_default_skills,
    load_mcp_servers,
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // Setup logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("peakbot=debug"))
        .init();

    // Load configuration from environment variables
    let config = Config::load().unwrap_or_default();

    // Create OpenRouter client
    let client = create_openrouter_client(&config)?;

    // Load skills from default locations (~/.agents/skills and ./.agents/skills)
    let skills = load_default_skills()?;

    // Load MCP servers (handles kept alive by McpServers)
    let mcp_servers = load_mcp_servers(&config).await?;

    // Build the agent with all tools and skills
    // Returns agent and stats reference
    let (agent, stats) = build_agent(&client, &config, &mcp_servers, &skills).await?;

    // Run the interactive REPL
    let mut runner = AgentRunner::new(agent, config.clone(), skills, stats);
    runner.run().await?;

    Ok(())
}
