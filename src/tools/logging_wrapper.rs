use rig::completion::ToolDefinition;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;

/// Wrapper around any ToolDyn that adds structured logging for tool execution.
/// This is useful for MCP tools where we can't modify the internal implementation.
pub struct LoggingToolDyn {
    inner: Box<dyn ToolDyn>,
    server_name: String,
}

impl LoggingToolDyn {
    pub fn new(inner: Box<dyn ToolDyn>, server_name: &str) -> Self {
        Self {
            inner,
            server_name: server_name.to_string(),
        }
    }
}

impl ToolDyn for LoggingToolDyn {
    fn name(&self) -> String {
        self.inner.name()
    }

    fn definition<'a>(&'a self, prompt: String) -> WasmBoxedFuture<'a, ToolDefinition> {
        self.inner.definition(prompt)
    }

    fn call<'a>(&'a self, args: String) -> WasmBoxedFuture<'a, Result<String, ToolError>> {
        let tool_name = self.name();
        let server = self.server_name.clone();
        
        // Log BEFORE the call
        tracing::info!(
            target: "peakbot",
            tool_type = "mcp",
            server = %server,
            tool_name = %tool_name,
            args = %args,
            "Starting MCP tool execution"
        );
        
        let start_time = std::time::Instant::now();
        
        Box::pin(async move {
            let result = self.inner.call(args).await;
            
            // Log AFTER the call
            match &result {
                Ok(output) => {
                    tracing::info!(
                        target: "peakbot",
                        tool_type = "mcp",
                        server = %server,
                        tool_name = %tool_name,
                        duration_ms = start_time.elapsed().as_millis(),
                        output_len = output.len(),
                        "MCP tool completed successfully"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        target: "peakbot",
                        tool_type = "mcp",
                        server = %server,
                        tool_name = %tool_name,
                        error = %e,
                        "MCP tool execution failed"
                    );
                }
            }
            
            result
        })
    }
}