//! Integration tests for MCP server connections.
//!
//! These tests verify that the MCP client can connect to external MCP servers,
//! list their tools, and execute tool calls successfully.

use peakbot::{connect_mcp_server, McpServerConfig};
use std::collections::HashMap;

/// Test that we can connect to the hello-mcp-server and list its tools.
///
/// This test verifies:
/// 1. The MCP server starts successfully
/// 2. We can connect to it via stdio transport
/// 3. We can list available tools
#[tokio::test]
async fn test_hello_mcp_server_connection() {
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
    
    // Connect to the MCP server using our library function
    // The handle keeps the service alive while we use the tools
    let handle = connect_mcp_server(&config)
        .await
        .expect("Failed to connect to MCP server");
    
    let tools = handle.tools();
    
    println!("Connected to MCP server, found {} tools", tools.len());
    
    // Verify we have at least one tool
    assert!(
        !tools.is_empty(),
        "Expected at least one tool from hello-mcp-server"
    );

    // Print tool names for debugging
    for tool in tools {
        println!("  - {}", tool.name());
    }
}

/// Test that we can call a tool on the hello-mcp-server.
///
/// This test verifies that we can actually execute a tool and get a result.
/// The key is that we use the McpServerHandle which keeps the service alive.
#[tokio::test]
async fn test_hello_mcp_server_tool_call() {
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
    
    // Connect to the MCP server - get the handle which keeps service alive
    let handle = connect_mcp_server(&config)
        .await
        .expect("Failed to connect to MCP server");
    
    // Get tools from the handle (service is kept alive!)
    let tools = handle.tools();
    
    // Get the first tool
    let first_tool = tools.first().expect("Expected at least one tool");
    
    println!("Calling tool: {}", first_tool.name());
    
    // Call the tool - service is alive because handle keeps it alive
    let result = first_tool.call("{}".to_string())
        .await
        .expect("Failed to call tool");
    
    println!("Tool call result: {:?}", result);
    
    // Verify we got a response
    assert!(
        !result.is_empty(),
        "Expected non-empty result from tool call"
    );
}

/// Test connecting with custom environment variables.
///
/// This test verifies that we can pass environment variables to the MCP server.
#[tokio::test]
async fn test_mcp_server_with_env_vars() {
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
    
    // Connect to the MCP server with custom env vars
    let handle = connect_mcp_server(&config)
        .await
        .expect("Failed to connect to MCP server with custom env vars");
    
    let tools = handle.tools();
    
    println!("Connected with custom env vars, found {} tools", tools.len());
    
    // The server should still work with custom env vars
    assert!(
        !tools.is_empty(),
        "Expected tools even with custom env vars"
    );
}
