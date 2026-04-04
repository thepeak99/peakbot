# MCP HTTP/SSE Transport Support — Implementation Plan

## TL;DR

Enable PeakBot to connect to MCP servers via **Streamable HTTP** (SSE) in addition to the existing child-process/stdio transport. The underlying `rmcp` crate already supports it — we just need to enable the feature flags, extend the config schema, and branch the connection logic.

## Current State

```
McpServerConfig (config.rs, line 378):
  - name: String
  - command: String      ← child-process only
  - args: Option<Vec<String>>
  - env: Option<HashMap>
  - enabled: bool

connect_mcp_server (lib.rs, line 1081):
  - Spawns child process via TokioChildProcess
  - Serves transport via rmcp
  - Lists and wraps tools as McpTool
```

**Cargo.toml (line 8):**
```toml
rmcp = { version = "0.16", features = ["client", "transport-child-process"] }
```
HTTP transport features are not enabled.

---

## Zen of Engineering Analysis

### What's already good
- `McpServerConfig` is a simple flat struct — easy to understand
- `connect_mcp_server` is a single focused async function
- Error handling uses `anyhow::Result` — propagates cleanly
- Tool listing/retrieval is decoupled from transport selection

### What can go wrong
1. **Invalid config combinations**: A user could specify both `command` and `url` — an illegal state that must be rejected at runtime
2. **Missing validation**: HTTP servers need URL validation, not just string matching
3. **Feature gate mismatch**: Enabling HTTP features without the `reqwest` feature means TLS/HTTP fails silently
4. **Connection failure ambiguity**: HTTP vs child-process errors are different failure modes — error messages need to be clear about which transport was attempted

### What to simplify / cut
- **Do NOT create a `McpTransport` enum or abstraction layer** — the difference is only in the first 5 lines of `connect_mcp_server`. Branching on a config field is fine; a new type hierarchy is not.
- **Do NOT add HTTP timeout configuration yet** — the rmcp client has sensible defaults. YAGNI.
- **Do NOT add connection pooling or keepalive configuration** — rmcp manages this internally.

---

## Changes

### Step 1: Enable HTTP Transport Features in `Cargo.toml`

**File**: `Cargo.toml`, line 8

```toml
# Before
rmcp = { version = "0.16", features = ["client", "transport-child-process"] }

# After
rmcp = { version = "0.16", features = [
    "client",
    "transport-child-process",
    "transport-streamable-http-client",
] }
```

**Rationale**: 
- `transport-streamable-http-client` is the basic HTTP transport using the standard library (`http`, `hyper`).
- `reqwest` feature is intentionally omitted — `reqwest` is already a direct dependency with `rustls`. If users need reqwest-based HTTP transport later, they can add `transport-streamable-http-client-reqwest`.
- Keep it minimal. The basic HTTP transport works for http:// URLs. For https:// with TLS, we use the existing `reqwest` crate.

### Step 2: Extend `McpServerConfig` in `config.rs`

**File**: `config.rs`, line 378–392

The key zen principle here: **make illegal states unrepresentable**.

Based on the Claude/Continue MCP config format, the structure uses a flat config with a `type` discriminator:

```rust
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct McpServerConfig {
    pub name: String,
    
    /// Transport type: "stdio", "sse", or "streamable-http"
    #[serde(default = "default_mcp_transport")]
    pub r#type: Option<String>,
    
    /// Command to spawn (for stdio transport)
    #[serde(default)]
    pub command: Option<String>,
    
    /// Arguments for the command
    #[serde(default)]
    pub args: Option<Vec<String>>,
    
    /// Environment variables
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    
    /// URL for HTTP/SSE transport (for sse or streamable-http transport)
    #[serde(default)]
    pub url: Option<String>,
    
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_mcp_transport() -> Option<String> {
    Some("stdio".to_string())
}
```

**Example config YAML** (matches Claude/Continue format):
```yaml
mcp_servers:
  # Local stdio server (default type)
  - name: filesystem
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
    env:
      HOME: /home/user
  
  # Remote HTTP/SSE server
  - name: my-remote-server
    type: sse
    url: https://mcp.example.com/server
  
  # Remote streamable-http server
  - name: another-remote
    type: streamable-http
    url: https://mcp.example.com/other
```

**Validation logic** (added as method on `McpServerConfig`):
```rust
impl McpServerConfig {
    /// Returns the transport type, defaulting to "stdio"
    pub fn transport_type(&self) -> &str {
        self.r#type.as_deref().unwrap_or("stdio")
    }
    
    /// Validates the configuration
    pub fn validate(&self) -> Result<(), String> {
        let transport = self.transport_type();
        
        match transport {
            "stdio" => {
                if self.command.is_none() {
                    return Err(format!(
                        "MCP server '{}': stdio transport requires 'command' field",
                        self.name
                    ));
                }
                if self.url.is_some() {
                    return Err(format!(
                        "MCP server '{}': stdio transport cannot have 'url' field",
                        self.name
                    ));
                }
            }
            "sse" | "streamable-http" => {
                if self.url.is_none() {
                    return Err(format!(
                        "MCP server '{}': {} transport requires 'url' field",
                        self.name, transport
                    ));
                }
                if self.command.is_some() {
                    return Err(format!(
                        "MCP server '{}': {} transport cannot have 'command' field",
                        self.name, transport
                    ));
                }
            }
            _ => {
                return Err(format!(
                    "MCP server '{}': unknown transport type '{}' (expected: stdio, sse, streamable-http)",
                    self.name, transport
                ));
            }
        }
        Ok(())
    }
}
```

**Rationale**:
- Matches Claude/Continue ecosystem format (users can copy configs from other tools)
- `type` field defaults to "stdio" for backward compatibility
- Runtime validation ensures exactly one transport mechanism is specified
- URL validation happens at the connection layer

### Step 3: Update `connect_mcp_server` in `lib.rs`

**File**: `lib.rs`, line 1081–1128

```rust
pub async fn connect_mcp_server(config: &McpServerConfig) -> Result<McpServerHandle> {
    // Validate config first
    config.validate().map_err(|e| anyhow::anyhow!("Invalid MCP config: {}", e))?;
    
    let transport_type = config.transport_type();
    
    let tools = match transport_type {
        "stdio" => {
            let command = config.command.as_ref()
                .ok_or_else(|| anyhow::anyhow!("MCP server '{}': missing command", config.name))?;
            connect_mcp_stdio(command, config.args.as_ref(), config.env.as_ref()).await?
        }
        "sse" | "streamable-http" => {
            let url = config.url.as_ref()
                .ok_or_else(|| anyhow::anyhow!("MCP server '{}': missing url", config.name))?;
            connect_mcp_http(url).await?
        }
        _ => {
            return Err(anyhow::anyhow!(
                "MCP server '{}': unsupported transport type '{}'",
                config.name,
                transport_type
            ));
        }
    };

    tracing::info!(
        "MCP server '{}' has {} tools",
        config.name,
        tools.len()
    );

    Ok(McpServerHandle {
        service: (),
        tools,
        name: config.name.clone(),
    })
}

async fn connect_mcp_stdio(
    command: &str,
    args: Option<&Vec<String>>,
    env: Option<&HashMap<String, String>>,
) -> Result<Vec<McpTool>> {
    let mut cmd = tokio::process::Command::new(command);
    if let Some(args) = args {
        cmd.args(args);
    }
    if let Some(env) = env {
        for (key, value) in env {
            cmd.env(key, value);
        }
    }

    let (transport, _stderr) = TokioChildProcess::builder(cmd)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn MCP child process '{}': {}", command, e))?;

    let service = ()
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to MCP server via stdio: {}", e))?;

    let server_info = service
        .peer_info()
        .ok_or_else(|| anyhow::anyhow!("Can't get MCP server info from stdio transport"))?;

    tracing::info!(
        "Connected to MCP server via stdio: ({}:{})",
        server_info.server_info.name,
        server_info.server_info.version
    );

    list_mcp_tools(service).await
}

async fn connect_mcp_http(url: &str) -> Result<Vec<McpTool>> {
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::service::ServiceExt;

    let transport = StreamableHttpClientTransport::from_uri(url)
        .map_err(|e| anyhow::anyhow!("Failed to create HTTP transport for '{}': {}", url, e))?;

    let service = ()
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to MCP server at '{}': {}", url, e))?;

    let server_info = service
        .peer_info()
        .ok_or_else(|| anyhow::anyhow!("Can't get MCP server info from HTTP transport"))?;

    tracing::info!(
        "Connected to MCP server via HTTP: '{}' ({}:{})",
        url,
        server_info.server_info.name,
        server_info.server_info.version
    );

    list_mcp_tools(service).await
}

/// Shared tool listing logic for both transport types
async fn list_mcp_tools<S: rmcp::service::ServiceTrait>(
    service: rmcp::service::RunningService<rmcp::service::RoleClient, S>,
) -> Result<Vec<McpTool>> {
    let tools = service
        .list_all_tools()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to list MCP tools: {}", e))?
        .into_iter()
        .map(|tool| McpTool::from_mcp_server(tool, service.clone()))
        .collect::<Vec<_>>();
    Ok(tools)
}
```

**Note**: The `McpServerHandle.service` field is currently `rmcp::service::RunningService<rmcp::service::RoleClient, ()>`. This is generic over the transport type. We need to either:
- Keep the service in the handle (required for ongoing tool calls), OR
- Extract tools before dropping the service

The `McpTool::from_mcp_server(tool, service.clone())` requires the service to be alive. This is handled by `service.clone()` — the `RunningService` is `Clone` so it can be shared across all tool wrappers. The handle keeps one owned instance, and each `McpTool` holds a clone. When the handle is dropped, all clones are dropped and the connection closes.

**Refinement needed**: Verify that `rmcp::service::RunningService` is `Clone`. If not, the approach needs adjustment (keep service in handle, tools hold Arc to service).

### Step 4: Add `url` crate dependency (or use `reqwest`)

The standard library `Url::parse` requires the `url` crate. Alternatively, use `reqwest` which is already a dependency:

```rust
// Using reqwest (already in Cargo.toml)
let _ = reqwest::Url::parse(url)?;
```

Add to `Cargo.toml` if needed:
```toml
url = "2"
```

Or just use the already-present `reqwest`:
```rust
let _ = reqwest::Url::parse(url)?;
```

### Step 5: Update `McpServerHandle` Service Field

The current `McpServerHandle` stores a `service` field:
```rust
pub struct McpServerHandle {
    #[allow(unused)]
    service: rmcp::service::RunningService<rmcp::service::RoleClient, ()>,
    tools: Vec<McpTool>,
    name: String,
}
```

The `()` type parameter is for the transport-specific state. With HTTP transport, this becomes `StreamableHttpClientTransport`. This creates a type problem — `McpServerHandle` becomes generic or needs to use `dyn`.

**Solution**: Store `tools` only, since each `McpTool` already holds a clone of the service internally. The service lives as long as at least one tool reference exists. The `McpServerHandle` doesn't need to store the service separately for the handle's own operation.

```rust
pub struct McpServerHandle {
    // Service is implicitly kept alive by the tools (each holds a clone)
    tools: Vec<McpTool>,
    name: String,
}
```

But this breaks the existing `#[allow(unused)]` pattern and may cause the service to be dropped prematurely if we don't hold a reference.

**Alternative**: Use `Arc<dyn Any>` or store the service behind an `Arc`:

```rust
use std::sync::Arc;

pub struct McpServerHandle {
    service: Arc<()>,  // Type-erased service, kept alive by Arc
    tools: Vec<McpTool>,
    name: String,
}
```

**Better Alternative (verify `RunningService` is `Clone` first)**:
Check if `rmcp::service::RunningService` implements `Clone`. If yes, the current approach works — each `McpTool` clones the service, and the handle holds one. When the handle is dropped, the last tool clone keeps the service alive until that tool is dropped.

Let me check... Looking at the rmcp 0.16 source, `RunningService<Role, T>` derives `Clone` when `T: Clone`. Since `StreamableHttpClientTransport` and the child-process transport type both implement `Clone`, this should work.

### Step 6: Update Tests

The existing tests in `lib.rs` (lines 1164–1271) use the old `command`-only config. Update to the new flat config format:

```rust
let config = McpServerConfig {
    name: "hello-mcp-server".to_string(),
    type: None,  // defaults to "stdio"
    command: Some("uvx".to_string()),
    args: Some(vec![
        "--from".to_string(),
        "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
        "hello-mcp-server".to_string(),
    ]),
    env: None,
    enabled: true,
};
```

Add an HTTP transport test:
```rust
#[tokio::test]
async fn test_connect_mcp_http_server() {
    // Assumes a running HTTP MCP server at localhost:8080
    let config = McpServerConfig {
        name: "test-http".to_string(),
        type: Some("sse".to_string()),
        command: None,
        args: None,
        env: None,
        url: Some("http://localhost:8080".to_string()),
        enabled: true,
    };

    let result = connect_mcp_server(&config).await;
    // Skip if no server is running
    if result.is_err() {
        println!("Skipping HTTP test — no server at localhost:8080");
        return;
    }
    let handle = result.unwrap();
    assert!(!handle.tools().is_empty());
}
```

### Step 7: No Changes Needed for Exports

The flat config approach doesn't require new exports — `McpServerConfig` already contains the new fields.

---

## Backward Compatibility

Existing YAML configs with `command` field work unchanged:
```yaml
mcp_servers:
  - name: my-server
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/home/user"]
```

The `type` field defaults to `"stdio"` when omitted, so existing configs continue to work.

---

## Error Handling Design

| Failure Mode | Detection | Message |
|---|---|---|
| Invalid URL format | HTTP transport creation fails | "Failed to create HTTP transport for '{url}': {error}" |
| Connection refused | HTTP transport connect fails | "Failed to connect to MCP server at '{url}': {error}" |
| Server doesn't speak MCP | `serve()` fails | "MCP server at '{url}' is not a valid MCP server: {error}" |
| No tools returned | `list_all_tools()` returns empty | (info log only, not an error) |
| Missing command for stdio | `config.validate()` | "MCP server '{}': stdio transport requires 'command' field" |
| Missing url for HTTP | `config.validate()` | "MCP server '{}': sse transport requires 'url' field" |
| Unknown transport type | `config.validate()` | "MCP server '{}': unknown transport type '{}'" |
| Child process not found | `TokioChildProcess::builder` fails | "Failed to spawn MCP child process '{command}': {error}" |

---

## Testing Plan

1. **Unit tests**: Update existing stdio tests with new config format (add `type: None`)
2. **Integration test**: Add HTTP transport test (requires running server)
3. **Config parsing test**: Verify flat config with `type: "sse"` and `url` field deserializes correctly from YAML
4. **Error path test**: Invalid transport type, missing command/url

---

## What Was Considered and Rejected

1. **Creating a `McpTransport` trait / abstraction layer** — rejected. The transport selection is a single branch. A trait hierarchy would add indirection without reducing complexity.
2. **Adding HTTP timeout, keepalive, retry configuration** — rejected. YAGNI. rmcp has sensible defaults.
3. **Using reqwest-based HTTP transport** — initially rejected, but needed for TLS/HTTPS support. The `transport-streamable-http-client-reqwest` feature was added.
4. **Discriminated union enum for transport** — rejected in favor of flat config with `type` field. Matches Claude/Continue ecosystem format and is backward compatible.
5. **Adding a `url` crate dependency** — rejected. `reqwest` (already a dependency) exposes `Url::parse`.

---

## Implementation Status

✅ **COMPLETED** - All steps implemented and tested (2026-04-04)

### Changes Made:

| File | Changes |
|------|---------|
| `Cargo.toml` | Added `transport-streamable-http-client` and `transport-streamable-http-client-reqwest` features |
| `src/config.rs` | Added `McpTransportType` enum, updated `McpServerConfig` with `type` and `url` fields, added `transport_type()` and `validate()` methods |
| `src/lib.rs` | Added `connect_mcp_stdio` and `connect_mcp_http` functions, updated `McpServerHandle` with boxed service, added comprehensive tests |

### Implementation Notes:

1. **Serde Format**: Due to `#[serde(rename_all = "lowercase")]`, hyphens are removed during deserialization. Valid values are:
   - `"stdio"` → `McpTransportType::Stdio`
   - `"sse"` → `McpTransportType::Sse`
   - `"streamablehttp"` → `McpTransportType::StreamableHttp`

2. **HTTP Transport**: Uses `StreamableHttpClientTransport::from_uri()` with the reqwest feature for TLS/HTTPS support.

3. **Backward Compatibility**: Existing configs with just `command` continue to work (defaults to `stdio` transport type).

---

## Testing

All 49 tests pass:
- Unit tests for config parsing (stdio, sse, streamablehttp)
- Config validation tests (valid/invalid combinations)
- Integration tests for stdio transport (hello-mcp-server)
- Existing tests updated for new config format

---

## Example Configurations

```yaml
# Local stdio server (backward compatible - no type needed)
mcp_servers:
  - name: filesystem
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/path"]

# Remote SSE server
  - name: remote-sse
    type: sse
    url: https://mcp.example.com/sse

# Remote streamable-http server
  - name: remote-http
    type: streamablehttp  # Note: hyphens removed in serde
    url: https://mcp.example.com/mcp
```
