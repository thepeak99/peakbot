//! Integration tests for MCP server connections.
//!
//! These tests verify that the MCP client can connect to external MCP servers,
//! list their tools, and execute tool calls successfully.

use rmcp::model::CallToolRequestParam;
use rmcp::service::ServiceExt;
use rmcp::transport::TokioChildProcess;
use tokio::process::Command;

/// Test that we can connect to the hello-mcp-server and list its tools.
///
/// This test verifies:
/// 1. The MCP server starts successfully
/// 2. We can connect to it via stdio transport
/// 3. We can list available tools
#[tokio::test]
async fn test_hello_mcp_server_connection() {
    // Start the hello-mcp-server using uvx
    let mut cmd = Command::new("uvx");
    cmd.arg("--from")
        .arg("git+https://github.com/macsymwang/hello-mcp-server.git")
        .arg("hello-mcp-server");

    // Create the TokioChildProcess transport
    let transport = TokioChildProcess::new(cmd)
        .expect("Failed to create child process transport");

    // Connect to the MCP server
    let service = ()
        .serve(transport)
        .await
        .expect("Failed to connect to MCP server");

    // Get server info
    let server_info = service.peer_info();
    println!("Connected to MCP server: {:?}", server_info);

    // List available tools
    let tools_response = service
        .list_tools(None)
        .await
        .expect("Failed to list tools");

    println!("Found {} tools", tools_response.tools.len());
    
    // Verify we have at least one tool
    assert!(
        !tools_response.tools.is_empty(),
        "Expected at least one tool from hello-mcp-server"
    );

    // Print tool names for debugging
    for tool in &tools_response.tools {
        println!("  - {}: {}", tool.name, tool.description.as_deref().unwrap_or("no description"));
    }
}

/// Test that we can call a tool on the hello-mcp-server.
///
/// This test verifies that we can actually execute a tool and get a result.
#[tokio::test]
async fn test_hello_mcp_server_tool_call() {
    // Start the hello-mcp-server using uvx
    let mut cmd = Command::new("uvx");
    cmd.arg("--from")
        .arg("git+https://github.com/macsymwang/hello-mcp-server.git")
        .arg("hello-mcp-server");

    // Create the TokioChildProcess transport
    let transport = TokioChildProcess::new(cmd)
        .expect("Failed to create child process transport");

    // Connect to the MCP server
    let service = ()
        .serve(transport)
        .await
        .expect("Failed to connect to MCP server");

    // List available tools
    let tools_response = service
        .list_tools(None)
        .await
        .expect("Failed to list tools");

    // Get the first tool
    let first_tool = tools_response
        .tools
        .first()
        .expect("Expected at least one tool");

    println!("Calling tool: {}", first_tool.name);

    // Call the first tool with empty arguments
    // (assuming hello-mcp-server has a simple tool that works without args)
    let request = CallToolRequestParam {
        name: first_tool.name.clone(),
        arguments: Some(serde_json::Map::new()),
        task: None,
    };
    let result = service
        .call_tool(request)
        .await
        .expect("Failed to call tool");

    println!("Tool call result: {:?}", result);

    // Verify we got a response (content is a vector of content blocks)
    assert!(
        !result.content.is_empty(),
        "Expected non-empty result from tool call"
    );
}

/// Test connecting with custom environment variables.
///
/// This test verifies that we can pass environment variables to the MCP server.
#[tokio::test]
async fn test_mcp_server_with_env_vars() {
    // Start the hello-mcp-server with custom environment
    let mut cmd = Command::new("uvx");
    cmd.arg("--from")
        .arg("git+https://github.com/macsymwang/hello-mcp-server.git")
        .arg("hello-mcp-server")
        .env("TEST_ENV_VAR", "test_value");

    // Create the TokioChildProcess transport
    let transport = TokioChildProcess::new(cmd)
        .expect("Failed to create child process transport");

    // Connect to the MCP server - this should succeed even with custom env
    let service = ()
        .serve(transport)
        .await
        .expect("Failed to connect to MCP server with custom env vars");

    // Verify we can list tools
    let tools_response = service
        .list_tools(None)
        .await
        .expect("Failed to list tools with custom env vars");

    println!("Connected with custom env vars, found {} tools", tools_response.tools.len());
    
    // The server should still work with custom env vars
    assert!(
        !tools_response.tools.is_empty(),
        "Expected tools even with custom env vars"
    );
}
