// Use the library crate (defined in lib.rs)
use anyhow::Result;
use peakbot::{
    AgentRunner, Config, build_agent, create_openrouter_client, load_mcp_servers,
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // Setup logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("peakbot=info,envy=debug"))
        .init();

    // Load configuration from environment variables
    let config = Config::load().unwrap_or_default();

    // Create OpenRouter client
    let client = create_openrouter_client(&config)?;

    // Load MCP servers (handles kept alive by McpServers)
    let mcp_servers = load_mcp_servers(&config).await?;

    // Build the agent with all tools
    let agent = build_agent(&client, &config, &mcp_servers).await;

    // Run the interactive REPL
    let mut runner = AgentRunner::new(agent, config.clone());
    runner.run().await?;

    Ok(())
}
