use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tokio::process::Command;

const MAX_OUTPUT_CHARS: usize = 10_000;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, thiserror::Error)]
pub enum BashError {
    #[error("{0}")]
    Execution(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Deserialize)]
pub struct BashArgs {
    command: String,
    timeout_seconds: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct BashTool {
    /// Optional environment variables to set for the command
    #[serde(default)]
    env: Option<HashMap<String, String>>,
}

impl Default for BashTool {
    fn default() -> Self {
        Self { env: None }
    }
}

impl BashTool {
    /// Create a new BashTool with the given environment variables
    pub fn new(env: Option<HashMap<String, String>>) -> Self {
        Self { env }
    }
}

impl Tool for BashTool {
    const NAME: &'static str = "bash";
    type Error = BashError;
    type Args = BashArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "bash".to_string(),
            description: "Run a shell command and return stdout and stderr. \
                Use for running builds, tests, git operations, grep, and other CLI tools. \
                Commands run in /bin/sh. Default timeout is 30 seconds."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Optional timeout in seconds (default: 30, max: 120)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let timeout_secs = args
            .timeout_seconds
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS);

        // Log before execution
        tracing::info!(
            target: "peakbot",
            tool_type = "bash",
            command = %args.command,
            timeout_secs = timeout_secs,
            env_vars = ?self.env.as_ref().map(|e| e.keys().collect::<Vec<_>>()),
            "Starting bash tool execution"
        );

        let start_time = std::time::Instant::now();

        // Build the command with optional environment variables
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(&args.command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        // Add configured environment variables if any
        if let Some(ref env_vars) = self.env {
            for (key, value) in env_vars {
                cmd.env(key, value);
            }
        }

        let child = cmd
            .spawn()
            .map_err(|e| BashError::Execution(format!("Failed to spawn shell: {}", e)))?;

        let result =
            tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await;

        match result {
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                let stdout = maybe_truncate(&stdout);
                let stderr = maybe_truncate(&stderr);

                let mut result = format!("Exit code: {}\n", exit_code);
                if !stdout.is_empty() {
                    result.push_str(&format!("\nSTDOUT:\n{}\n", stdout));
                }
                if !stderr.is_empty() {
                    result.push_str(&format!("\nSTDERR:\n{}\n", stderr));
                }

                // Log successful completion
                tracing::info!(
                    target: "peakbot",
                    tool_type = "bash",
                    exit_code = exit_code,
                    duration_ms = start_time.elapsed().as_millis(),
                    "Bash tool completed successfully"
                );

                Ok(result)
            }
            Ok(Err(e)) => {
                let error = format!("Command failed: {}", e);
                tracing::warn!(
                    target: "peakbot",
                    tool_type = "bash",
                    error = %error,
                    "Bash tool execution failed"
                );
                Err(BashError::Execution(error))
            }
            Err(_) => {
                // child is dropped here -> killed automatically due to kill_on_drop
                let error = format!(
                    "Command timed out after {} seconds. Consider increasing timeout_seconds.",
                    timeout_secs
                );
                tracing::warn!(
                    target: "peakbot",
                    tool_type = "bash",
                    timeout_secs = timeout_secs,
                    "Bash tool timed out"
                );
                Err(BashError::Execution(error))
            }
        }
    }
}

fn maybe_truncate(s: &str) -> String {
    if s.len() > MAX_OUTPUT_CHARS {
        // Safely truncate at the nearest character boundary to avoid
        // panicking on multi-byte UTF-8 characters (e.g., '─' = 3 bytes)
        let truncate_at = s
            .get(..MAX_OUTPUT_CHARS)
            .map(|sub| sub.len())
            .unwrap_or_else(|| {
                // Byte index wasn't a char boundary, find the nearest one below
                let mut boundary = MAX_OUTPUT_CHARS;
                while !s.is_char_boundary(boundary) {
                    boundary -= 1;
                }
                boundary
            });
        format!(
            "{}... [truncated, {} total chars]",
            &s[..truncate_at],
            s.len()
        )
    } else {
        s.to_string()
    }
}
