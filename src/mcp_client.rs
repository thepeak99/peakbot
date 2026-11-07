//! The MCP client: connects the configured `mcps:` servers (stdio child
//! processes or streamable HTTP, with optional OAuth) and hands their tools
//! to the agent. Host edition only — the Local edition has no MCP.

use std::process::Stdio;

use anyhow::{Result, anyhow};
use rig_core::tool::ToolDyn;
use rig_core::tool::rmcp::McpTool;
use rmcp::service::{RoleClient, RunningService, ServiceExt};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{TokioChildProcess, streamable_http_client::StreamableHttpClientTransport};

use crate::config::{Config, McpServerConfig, McpTransportType};

/// Handle for a connected MCP server.
///
/// Holds both the tools and the service connection. The service connection
/// must be kept alive for as long as the tools are used, and should be
/// properly closed on drop to avoid the "RunningService dropped without
/// explicit close()" warning.
pub struct McpServerHandle {
    name: String,
    tools: Vec<McpTool>,
    /// The running service connection. Must be closed on drop for clean shutdown.
    /// This uses a wrapper enum since stdio and HTTP transports return different
    /// service types internally.
    service: McpService,
}

/// Wrapper enum for MCP service connections.
///
/// Both stdio and HTTP transports use `RunningService<RoleClient, ()>` when acting
/// as a client, but the inner service type differs (child process vs HTTP client).
/// This enum allows us to store the correct type while providing a uniform interface.
enum McpService {
    Stdio(Option<RunningService<RoleClient, ()>>),
    Http(Option<RunningService<RoleClient, ()>>),
}

impl Drop for McpServerHandle {
    fn drop(&mut self) {
        // Take the service out so we don't double-close
        let service = match &mut self.service {
            McpService::Stdio(s) => s.take(),
            McpService::Http(s) => s.take(),
        };

        if let Some(mut s) = service {
            // Spawn a task to close the service properly
            // This avoids blocking in Drop while ensuring clean shutdown
            tokio::spawn(async move {
                match s.close().await {
                    Ok(reason) => {
                        tracing::debug!("MCP service closed: {:?}", reason);
                    }
                    Err(e) => {
                        tracing::warn!("MCP service close error: {:?}", e);
                    }
                }
            });
        }
    }
}

impl McpServerHandle {
    /// The server's configured name — the key a profile's `mcps:`
    /// gate addresses.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// The tools as `ToolDyn` boxes — the view `ToolEnv` reads, which the
    /// Local edition's handle stand-in offers too.
    pub fn tool_boxes(&self) -> Vec<Box<dyn ToolDyn>> {
        self.tools
            .iter()
            .cloned()
            .map(|tool| Box::new(tool) as Box<dyn ToolDyn>)
            .collect()
    }

    /// Take ownership of the tools, consuming this handle.
    ///
    /// Note: The MCP service connection will be closed when this handle is dropped.
    /// For explicit control over service shutdown, use `close_and_take_tools()` instead.
    pub fn into_tools(self) -> Vec<Box<dyn ToolDyn>> {
        // Use ManuallyDrop to prevent our Drop impl from running
        // since we're consuming the handle intentionally
        let this = std::mem::ManuallyDrop::new(self);

        // Access fields through the ManuallyDrop'd reference
        this.tools
            .iter()
            .cloned()
            .map(|tool| Box::new(tool) as Box<dyn ToolDyn>)
            .collect()
    }

    /// Close the MCP service connection and get the tools.
    ///
    /// This properly closes the service before extracting tools.
    pub async fn close_and_take_tools(mut self) -> Vec<Box<dyn ToolDyn>> {
        // Close the service first (extract from Option via take on &mut self)
        // We need to do this carefully since we can't move out of self.service
        // Take the service out by matching on &mut self
        let service = match &mut self.service {
            McpService::Stdio(s) => s.take(),
            McpService::Http(s) => s.take(),
        };

        if let Some(mut s) = service {
            s.close().await.ok();
        }

        self.into_tools()
    }
}

pub async fn connect_mcp_server(config: &McpServerConfig) -> Result<McpServerHandle> {
    // Validate configuration
    if let Err(e) = config.validate() {
        return Err(anyhow::anyhow!("Invalid MCP server config: {}", e));
    }

    match config.transport_type() {
        McpTransportType::Stdio => connect_mcp_stdio(config).await,
        McpTransportType::Sse => {
            // rmcp 0.16 dropped the dedicated SSE client transport. The MCP
            // spec (2025-03-26) replaced SSE with Streamable HTTP, which is
            // wire-compatible with most existing "sse" servers. We route
            // there and warn loudly so users aren't surprised.
            tracing::warn!(
                "MCP server '{}': transport_type 'sse' is deprecated; \
                 routing through streamable-http. Set 'type: streamable-http' \
                 in your config to silence this warning.",
                config.name
            );
            connect_mcp_http(config).await
        }
        McpTransportType::StreamableHttp => connect_mcp_http(config).await,
    }
}

/// Connect to an MCP server using stdio transport (spawns a local process)
async fn connect_mcp_stdio(config: &McpServerConfig) -> Result<McpServerHandle> {
    let command = config
        .command
        .as_ref()
        .ok_or_else(|| anyhow!("stdio transport requires 'command'"))?;

    let mut cmd = tokio::process::Command::new(command);
    if let Some(args) = &config.args {
        cmd.args(args);
    }
    if let Some(env) = &config.env {
        for (key, value) in env {
            cmd.env(key, value);
        }
    }

    let (transport, _stderr) = TokioChildProcess::builder(cmd)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to create child process: {}", e))?;

    let service = ()
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to MCP server: {}", e))?;

    let server_info = service
        .peer_info()
        .ok_or_else(|| anyhow!("Can't get MCP info"))?;
    tracing::info!(
        "Connected to MCP server '{}': ({}:{})",
        config.name,
        server_info.server_info.name,
        server_info.server_info.version
    );

    let tools = service
        .list_all_tools()
        .await?
        .into_iter()
        .map(|tool| McpTool::from_mcp_server(tool, service.peer().clone()))
        .collect::<Vec<_>>();

    tracing::info!("MCP server '{}' has {} tools", config.name, tools.len());

    // Store the service in our wrapper enum, keeping a clone for the tools
    Ok(McpServerHandle {
        name: config.name.clone(),
        tools,
        service: McpService::Stdio(Some(service)),
    })
}

/// Connect to an MCP server using HTTP transport (remote server)
async fn connect_mcp_http(config: &McpServerConfig) -> Result<McpServerHandle> {
    let url = config
        .url
        .as_ref()
        .ok_or_else(|| anyhow!("HTTP transport requires 'url'"))?;

    tracing::info!("Connecting to MCP server '{}' at {}", config.name, url);

    // Surface deprecation warnings (e.g. legacy `auth_token`) at connect
    // time. `validate()` already rejected the "both set" footgun, so this
    // is purely informational.
    for warning in config.deprecation_warnings() {
        tracing::warn!("{warning}");
    }

    // Build the streamable-http config with optional bearer token + custom headers.
    let mut transport_config = StreamableHttpClientTransportConfig::with_uri(url.clone());
    // Lazily holds the OAuth-aware HTTP client when the server uses oauth.
    // `None` means the default reqwest client baked into `from_config` will be used.
    let mut auth_client: Option<crate::mcp_auth::AuthorizedClient> = None;

    match config.auth_resolved() {
        Some(crate::config::ResolvedAuth::Bearer { token }) => {
            transport_config = transport_config.auth_header(token);
        }
        Some(crate::config::ResolvedAuth::Oauth {
            client_id,
            client_secret,
            scopes,
        }) => {
            // Slice 2 + Slice 3a: full OAuth 2.1 + PKCE flow.
            // - `client_id` absent → DCR (Linear shape)
            // - `client_id` present → static credentials (Google shape)
            // `authorize` returns an `AuthClient<reqwest::Client>` that
            // implements `StreamableHttpClient`, so we feed it into the
            // transport via `with_client` below.
            let params = crate::mcp_auth::OauthParams {
                client_id,
                client_secret,
                scopes,
            };
            auth_client = Some(
                crate::mcp_auth::authorize(&config.name, url, params)
                    .await
                    .map_err(|e| anyhow!("MCP server '{}': {e}", config.name))?,
            );
        }
        None => {}
    }

    if let Some(headers) = config.headers.as_ref() {
        let mut parsed: std::collections::HashMap<::http::HeaderName, ::http::HeaderValue> =
            std::collections::HashMap::with_capacity(headers.len());
        for (k, v) in headers {
            match (
                ::http::HeaderName::try_from(k.as_str()),
                ::http::HeaderValue::try_from(v.as_str()),
            ) {
                (Ok(name), Ok(value)) => {
                    parsed.insert(name, value);
                }
                _ => {
                    tracing::warn!(
                        "MCP server '{}': skipping invalid header '{}: {}'",
                        config.name,
                        k,
                        v
                    );
                }
            }
        }
        if !parsed.is_empty() {
            transport_config = transport_config.custom_headers(parsed);
        }
    }

    // `StreamableHttpClientTransport<C>` is generic over the inner HTTP
    // client, so the two branches produce different concrete types and
    // can't be unified by a `let transport = if ...`. Drive `.serve`
    // from inside each arm — the resulting `service` (`RunningService<…>`)
    // has the same type regardless because the transport type-parameter
    // is erased by the worker boundary.
    let service = if let Some(client) = auth_client {
        // OAuth-aware client: signs every request and silently refreshes
        // access tokens.
        ().serve(StreamableHttpClientTransport::with_client(
            client,
            transport_config,
        ))
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to MCP server: {}", e))?
    } else {
        ().serve(StreamableHttpClientTransport::with_client(
            crate::http::client(),
            transport_config,
        ))
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to MCP server: {}", e))?
    };

    let server_info = service
        .peer_info()
        .ok_or_else(|| anyhow!("Can't get MCP info"))?;
    tracing::info!(
        "Connected to MCP server '{}': ({}:{})",
        config.name,
        server_info.server_info.name,
        server_info.server_info.version
    );

    let tools = service
        .list_all_tools()
        .await?
        .into_iter()
        .map(|tool| McpTool::from_mcp_server(tool, service.peer().clone()))
        .collect::<Vec<_>>();

    tracing::info!("MCP server '{}' has {} tools", config.name, tools.len());

    // Store the service in our wrapper enum, keeping a clone for the tools
    Ok(McpServerHandle {
        name: config.name.clone(),
        tools,
        service: McpService::Http(Some(service)),
    })
}

pub async fn load_mcp_servers(config: &Config) -> Result<Vec<McpServerHandle>> {
    let mut handles = Vec::new();

    let servers = &config.mcps;
    if servers.is_empty() {
        tracing::info!("No MCP servers configured");
        return Ok(Vec::new());
    }

    for server_config in servers {
        if !server_config.enabled {
            continue;
        }

        tracing::info!("Connecting to MCP server: {}", server_config.name);
        match connect_mcp_server(server_config).await {
            Ok(handle) => {
                handles.push(handle);
            }
            Err(e) => {
                tracing::error!(
                    "Failed to connect to MCP server '{}': {}",
                    server_config.name,
                    e
                );
            }
        }
    }

    Ok(handles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::tool::ToolDyn;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_connect_mcp_server_invalid_command() {
        let mut env = HashMap::new();
        env.insert("TEST_VAR".to_string(), "test_value".to_string());

        let config = McpServerConfig {
            name: "test_invalid".to_string(),
            transport_type: McpTransportType::Stdio,
            command: Some("nonexistent_command_xyz123".to_string()),
            args: None,
            env: Some(env),
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };

        let result = connect_mcp_server(&config).await;
        assert!(result.is_err(), "Expected error for invalid command");
    }

    #[tokio::test]
    #[ignore = "needs network + uvx (Python); run locally with --ignored"]
    async fn test_connect_mcp_server_hello() {
        let config = McpServerConfig {
            name: "hello-mcp-server".to_string(),
            transport_type: McpTransportType::Stdio,
            command: Some("uvx".to_string()),
            args: Some(vec![
                "--from".to_string(),
                "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
                "hello-mcp-server".to_string(),
            ]),
            env: None,
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };

        let result = connect_mcp_server(&config).await;
        let handle = result.expect("Failed to connect to hello-mcp-server");
        let tools = handle.tools();
        assert!(!tools.is_empty(), "Expected at least one tool");

        println!("Connected to hello-mcp-server with {} tools", tools.len());
    }

    #[tokio::test]
    #[ignore = "needs network + uvx (Python); run locally with --ignored"]
    async fn test_connect_mcp_server_with_env() {
        let mut env = HashMap::new();
        env.insert("TEST_ENV_VAR".to_string(), "test_value".to_string());

        let config = McpServerConfig {
            name: "hello-mcp-server-with-env".to_string(),
            transport_type: McpTransportType::Stdio,
            command: Some("uvx".to_string()),
            args: Some(vec![
                "--from".to_string(),
                "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
                "hello-mcp-server".to_string(),
            ]),
            env: Some(env),
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };

        let result = connect_mcp_server(&config).await;
        let handle = result.expect("Failed to connect to hello-mcp-server with env vars");
        let tools = handle.tools();

        assert!(
            !tools.is_empty(),
            "Expected at least one tool with custom env"
        );
    }

    #[tokio::test]
    #[ignore = "needs network + uvx (Python); run locally with --ignored"]
    async fn test_connect_mcp_server_call_tool() {
        let config = McpServerConfig {
            name: "hello-mcp-server".to_string(),
            transport_type: McpTransportType::Stdio,
            command: Some("uvx".to_string()),
            args: Some(vec![
                "--from".to_string(),
                "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
                "hello-mcp-server".to_string(),
            ]),
            env: None,
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };

        let handle = connect_mcp_server(&config)
            .await
            .expect("Failed to connect to hello-mcp-server");

        let tools = handle.tools();
        assert!(!tools.is_empty(), "Expected at least one tool");

        let first_tool = &tools[0];
        println!("Calling tool: {}", first_tool.name());

        let result = first_tool
            .call("{}".to_string())
            .await
            .expect("Failed to call tool");

        println!("Tool call result: {:?}", result);

        assert!(
            !result.is_empty(),
            "Expected non-empty result from tool call"
        );
    }
}
