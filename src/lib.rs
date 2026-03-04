//! PeakBot library - Core functionality for connecting to MCP servers and managing tools.

mod config;
mod tools;

pub use config::{Config, McpServerConfig};
pub use tools::{BashTool, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool, LoggingToolDyn};

use anyhow::Result;
use rig::tool::rmcp::McpTool;
use rig::tool::ToolDyn;
use rmcp::service::ServiceExt;

/// A handle to an MCP server connection that keeps the service alive
/// while tools are being used.
pub struct McpServerHandle {
    /// The service - kept alive to maintain the connection
    /// Using type alias for the running service
    _service: rmcp::service::RunningService<rmcp::service::RoleClient, ()>,
    tools: Vec<Box<dyn ToolDyn>>,
}

impl McpServerHandle {
    /// Get the tools from this MCP server
    pub fn tools(&self) -> &[Box<dyn ToolDyn>] {
        &self.tools
    }
    
    /// Consume the handle and return the tools
    /// Use this when you want to move the tools out while keeping the service alive
    /// until the tools are dropped.
    pub fn into_tools(self) -> Vec<Box<dyn ToolDyn>> {
        self.tools
    }
}

/// Connect to an MCP server and return its tools (wrapped with LoggingToolDyn)
pub async fn connect_mcp_server(config: &McpServerConfig) -> Result<McpServerHandle> {
    // Only stdio transport is supported for now
    let command = &config.command;
    
    let mut cmd = tokio::process::Command::new(command);
    if let Some(args) = &config.args {
        cmd.args(args);
    }
    if let Some(env) = &config.env {
        for (key, value) in env {
            cmd.env(key, value);
        }
    }
    
    // Use TokioChildProcess transport
    let transport = rmcp::transport::TokioChildProcess::new(cmd)
        .map_err(|e| anyhow::anyhow!("Failed to create child process: {}", e))?;
    
    // Connect to the MCP server
    let service = ().serve(transport).await
        .map_err(|e| anyhow::anyhow!("Failed to connect to MCP server: {}", e))?;
    
    // Get server info
    let server_info = service.peer_info();
    tracing::info!("Connected to MCP server '{}': {:?}", config.name, server_info);
    
    // List available tools (use None for paginated request)
    let tools = service.list_tools(None).await
        .map_err(|e| anyhow::anyhow!("Failed to list tools: {}", e))?;
    
    tracing::info!("MCP server '{}' has {} tools", config.name, tools.tools.len());
    
    // Create wrapped McpTool for each tool with logging
    let mcp_tools: Vec<Box<dyn ToolDyn>> = tools.tools.into_iter().map(|tool| {
        let inner = Box::new(McpTool::from_mcp_server(tool, service.clone())) as Box<dyn ToolDyn>;
        Box::new(LoggingToolDyn::new(inner, &config.name)) as Box<dyn ToolDyn>
    }).collect();
    
    Ok(McpServerHandle {
        _service: service,
        tools: mcp_tools,
    })
}

/// Load and connect to all configured MCP servers
pub async fn load_mcp_servers(config: &Config) -> Result<Vec<Box<dyn ToolDyn>>> {
    let mut all_tools = Vec::new();
    
    let servers = match &config.mcp_servers {
        Some(servers) => servers,
        None => {
            tracing::info!("No MCP servers configured");
            return Ok(all_tools);
        }
    };
    
    for server_config in servers {
        tracing::info!("Connecting to MCP server: {}", server_config.name);
        match connect_mcp_server(server_config).await {
            Ok(handle) => {
                let tool_count = handle.tools().len();
                tracing::info!("Loaded {} tools from MCP server '{}'", tool_count, server_config.name);
                // Take ownership of the tools from the handle
                all_tools.extend(handle.into_tools());
            }
            Err(e) => {
                tracing::error!("Failed to connect to MCP server '{}': {}", server_config.name, e);
            }
        }
    }
    
    Ok(all_tools)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Test that connect_mcp_server handles an invalid command gracefully
    #[tokio::test]
    async fn test_connect_mcp_server_invalid_command() {
        let mut env = HashMap::new();
        env.insert("TEST_VAR".to_string(), "test_value".to_string());
        
        let config = McpServerConfig {
            name: "test_invalid".to_string(),
            command: "nonexistent_command_xyz123".to_string(),
            args: None,
            env: Some(env),
        };
        
        let result = connect_mcp_server(&config).await;
        assert!(result.is_err(), "Expected error for invalid command");
    }

    /// Test that connect_mcp_server works with a real MCP server
    #[tokio::test]
    async fn test_connect_mcp_server_hello() {
        let config = McpServerConfig {
            name: "hello-mcp-server".to_string(),
            command: "uvx".to_string(),
            args: Some(vec![
                "--from".to_string(),
                "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
                "hello-mcp-server".to_string(),
            ]),
            env: None,
        };
        
        let result = connect_mcp_server(&config).await;
        
        // This should succeed and return a handle with tools
        let handle = result.expect("Failed to connect to hello-mcp-server");
        let tools = handle.tools();
        assert!(!tools.is_empty(), "Expected at least one tool");
        
        println!("Connected to hello-mcp-server with {} tools", tools.len());
    }

    /// Test that connect_mcp_server works with environment variables
    #[tokio::test]
    async fn test_connect_mcp_server_with_env() {
        let mut env = HashMap::new();
        env.insert("TEST_ENV_VAR".to_string(), "test_value".to_string());
        
        let config = McpServerConfig {
            name: "hello-mcp-server-with-env".to_string(),
            command: "uvx".to_string(),
            args: Some(vec![
                "--from".to_string(),
                "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
                "hello-mcp-server".to_string(),
            ]),
            env: Some(env),
        };
        
        let result = connect_mcp_server(&config).await;
        let handle = result.expect("Failed to connect to hello-mcp-server with env vars");
        let tools = handle.tools();
        
        assert!(!tools.is_empty(), "Expected at least one tool with custom env");
    }
    
    /// Test that we can actually call a tool on the MCP server
    /// This is the key test - keeping the service alive while calling tools
    #[tokio::test]
    async fn test_connect_mcp_server_call_tool() {
        let config = McpServerConfig {
            name: "hello-mcp-server".to_string(),
            command: "uvx".to_string(),
            args: Some(vec![
                "--from".to_string(),
                "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
                "hello-mcp-server".to_string(),
            ]),
            env: None,
        };
        
        // Connect and get the handle (which keeps the service alive)
        let handle = connect_mcp_server(&config).await
            .expect("Failed to connect to hello-mcp-server");
        
        // Get tools from the handle - the service is kept alive by the handle
        let tools = handle.tools();
        assert!(!tools.is_empty(), "Expected at least one tool");
        
        // Call the first tool
        let first_tool = &tools[0];
        println!("Calling tool: {}", first_tool.name());
        
        let result = first_tool.call("{}".to_string())
            .await
            .expect("Failed to call tool");
        
        println!("Tool call result: {:?}", result);
        
        // Verify we got a response
        assert!(!result.is_empty(), "Expected non-empty result from tool call");
    }
}
