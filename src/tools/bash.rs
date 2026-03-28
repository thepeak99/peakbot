use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::process::Command;

const MAX_OUTPUT_CHARS: usize = 50_000;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 600;
const TEMP_DIR_NAME: &str = "peakbot";

/// Session-unique counter for generating output filenames
static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

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

    /// Detect if the command appears to be doing file editing
    /// Returns a warning message if file-editing patterns are detected
    fn check_file_edit_patterns(&self, command: &str) -> Option<String> {
        let command_lower = command.to_lowercase();

        // Check for common file-editing bash patterns
        if command_lower.contains("sed -i") {
            return Some(self.file_edit_warning("sed -i for in-place file editing"));
        }

        // Check for awk with output redirection (awk ... > file)
        if command_lower.contains("awk") && command.contains(">") {
            return Some(self.file_edit_warning("awk for file modification"));
        }

        if command_lower.contains("perl -pi") {
            return Some(self.file_edit_warning("perl for in-place file editing"));
        }

        if command_lower.contains("ex +") && command.contains("%") {
            return Some(self.file_edit_warning("vim/ex for file editing"));
        }

        if command_lower.contains("vi -c") {
            return Some(self.file_edit_warning("vi for file editing"));
        }

        None
    }

    /// Generate a standardized warning message for file-editing bash commands
    fn file_edit_warning(&self, description: &str) -> String {
        format!(
            "⚠️  Consider using file_edit tool instead of {} for file modifications.\n\
            \nfile_edit provides:\n\
            - Safe diffs for review\n\
            - Cross-platform compatibility\n\
            - Automatic whitespace handling\n\
            \nThis command will execute, but file_edit is recommended for file content modifications.\n\
            Use bash ONLY for: file operations (mv/cp/rm), permissions, bulk operations on many files.",
            description
        )
    }
}

/// Save full output to temp files and return the paths
fn save_full_output(stdout: &str, stderr: &str) -> std::io::Result<(PathBuf, PathBuf)> {
    let temp_dir = std::env::temp_dir().join(TEMP_DIR_NAME);
    std::fs::create_dir_all(&temp_dir)?;

    let counter = SESSION_COUNTER.fetch_add(1, Ordering::SeqCst);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let session_id = format!("{}_{}", timestamp, counter);
    let base = temp_dir.join(format!("bash_{}", session_id));

    let stdout_path = base.with_extension("stdout.txt");
    let stderr_path = base.with_extension("stderr.txt");

    std::fs::write(&stdout_path, stdout)?;
    std::fs::write(&stderr_path, stderr)?;

    Ok((stdout_path, stderr_path))
}

/// Truncate string from the beginning, keeping the end (like `tail -c N`)
/// Returns (truncated_string, was_truncated)
fn truncate_from_beginning(s: &str, max_chars: usize) -> (String, bool) {
    if s.len() <= max_chars {
        return (s.to_string(), false);
    }

    // Calculate the starting position for the last max_chars
    let start_byte = s.len() - max_chars;

    // Find the nearest char boundary from the end
    let mut end_byte = s.len();
    while !s.is_char_boundary(end_byte) {
        end_byte -= 1;
    }

    // Find the nearest char boundary from the start
    let mut start_byte = start_byte;
    while !s.is_char_boundary(start_byte) {
        start_byte += 1;
    }

    let truncated = &s[start_byte..end_byte];
    let chars_truncated = s.len() - truncated.len();

    (
        format!(
            "[... {} chars truncated from beginning ...]\n{}",
            chars_truncated, truncated
        ),
        true,
    )
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
                Output is truncated to ~50k chars (keeping the end, like `tail`). \
                Full output is saved to /tmp/peakbot/ and can be accessed via file_read. \
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
        // Check for file-editing patterns and add warning if detected
        let warning = self.check_file_edit_patterns(&args.command);

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
                let stdout_raw = String::from_utf8_lossy(&output.stdout);
                let stderr_raw = String::from_utf8_lossy(&output.stderr);

                // Save full output to temp files
                let (stdout_path, stderr_path) = match save_full_output(&stdout_raw, &stderr_raw) {
                    Ok(paths) => paths,
                    Err(e) => {
                        tracing::warn!(
                            target: "peakbot",
                            tool_type = "bash",
                            error = %e,
                            "Failed to save full output to temp file"
                        );
                        (PathBuf::new(), PathBuf::new())
                    }
                };

                // Truncate from beginning, keeping the end (like `tail`)
                let (stdout, stdout_truncated) = truncate_from_beginning(&stdout_raw, MAX_OUTPUT_CHARS);
                let (stderr, stderr_truncated) = truncate_from_beginning(&stderr_raw, MAX_OUTPUT_CHARS);

                let mut result = format!("Exit code: {}\n", exit_code);
                if !stdout_raw.is_empty() {
                    result.push_str(&format!("\nSTDOUT:\n{}\n", stdout));
                }
                if !stderr_raw.is_empty() {
                    result.push_str(&format!("\nSTDERR:\n{}\n", stderr));
                }

                // Add full output location if anything was truncated
                if stdout_truncated || stderr_truncated {
                    let divider = "\n─────────────────────────────────────────────────\n";
                    result.push_str(divider);
                    result.push_str("Full output saved to:\n");
                    if stdout_truncated {
                        result.push_str(&format!("  {}\n", stdout_path.display()));
                    }
                    if stderr_truncated {
                        result.push_str(&format!("  {}\n", stderr_path.display()));
                    }
                    result.push_str("\nUse file_read tool to access the complete output.\n");
                    result.push_str(divider);
                }

                // Add warning if file-editing pattern was detected
                if let Some(warning_msg) = warning {
                    result.push_str(&format!("\n\n{}", warning_msg));
                }

                // Log successful completion
                tracing::info!(
                    target: "peakbot",
                    tool_type = "bash",
                    exit_code = exit_code,
                    duration_ms = start_time.elapsed().as_millis(),
                    stdout_truncated = stdout_truncated,
                    stderr_truncated = stderr_truncated,
                    stdout_path = %stdout_path.display(),
                    stderr_path = %stderr_path.display(),
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
