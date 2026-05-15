use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::process::Command;

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 7200; // 2 hours
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
    #[allow(dead_code)]
    thought: String,
    command: String,
    timeout_seconds: Option<u64>,
    /// Show first N lines of output (optional)
    head: Option<usize>,
    /// Show last N lines of output (default: 100, use 0 for all)
    tail: Option<usize>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct BashTool {
    /// Optional environment variables to set for the command
    #[serde(default)]
    env: Option<HashMap<String, String>>,
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
            return Some(
                self.file_edit_warning("sed -i for in-place file editing", "file_str_replace"),
            );
        }

        // Check for awk with output redirection (awk ... > file)
        if command_lower.contains("awk") && command.contains(">") {
            return Some(self.file_edit_warning("awk for file modification", "file_str_replace"));
        }

        if command_lower.contains("perl -pi") {
            return Some(
                self.file_edit_warning("perl for in-place file editing", "file_str_replace"),
            );
        }

        if command_lower.contains("ex +") && command.contains("%") {
            return Some(self.file_edit_warning(
                "vim/ex for file editing",
                "the file editing tools (file_create / file_str_replace / file_insert)",
            ));
        }

        if command_lower.contains("vi -c") {
            return Some(self.file_edit_warning(
                "vi for file editing",
                "the file editing tools (file_create / file_str_replace / file_insert)",
            ));
        }

        None
    }

    /// Generate a standardized warning message for file-editing bash commands
    fn file_edit_warning(&self, description: &str, tool_suggestion: &str) -> String {
        format!(
            "⚠️  Consider using {tool} instead of {description} for file modifications.\n\
            \nThe file editing tools provide:\n\
            - Safe diffs for review\n\
            - Cross-platform compatibility\n\
            - Automatic whitespace handling\n\
            \nThis command will execute, but {tool} is recommended for file content modifications.\n\
            Use bash ONLY for: file operations (mv/cp/rm), permissions, bulk operations on many files.",
            tool = tool_suggestion,
            description = description
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

/// Apply head/tail line truncation to output
/// Returns (displayed_output, was_modified)
fn apply_head_tail(s: &str, head: Option<usize>, tail: Option<usize>) -> (String, bool) {
    let lines: Vec<&str> = s.lines().collect();
    let total_lines = lines.len();

    // No truncation needed
    if head.is_none() && tail.is_none() {
        return (s.to_string(), false);
    }

    // Only head specified
    if let Some(h) = head
        && tail.is_none()
    {
        if total_lines <= h {
            return (s.to_string(), false);
        }
        return (lines[..h].join("\n"), true);
    }

    // Only tail specified (default 100)
    if head.is_none() {
        let t = tail.unwrap_or(100);
        if t == 0 || total_lines <= t {
            return (s.to_string(), false);
        }
        return (lines[total_lines - t..].join("\n"), true);
    }

    // Both head and tail specified
    let h = head.unwrap_or(usize::MAX);
    let t = tail.unwrap_or(100);

    // If lines fit in head + tail, show all
    if total_lines <= h + t {
        return (s.to_string(), false);
    }

    // Need to truncate: head at top, tail at bottom
    let head_lines = &lines[..h];
    let tail_lines = &lines[total_lines - t..];
    let middle_count = total_lines - h - t;

    (
        format!(
            "{}\n... {} lines in between ...\n{}",
            head_lines.join("\n"),
            middle_count,
            tail_lines.join("\n")
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
                Use `head` to show first N lines, `tail` to show last N lines (default: 100). \
                Full output is always saved to /tmp/peakbot/ and accessible via file_read. \
                Commands run in /bin/sh. Default timeout is 30 seconds."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "thought": {
                        "type": "string",
                        "description": "Briefly explain what you're about to do and why, before acting."
                    },
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Optional timeout in seconds (default: 30, max: 7200 = 2 hours)"
                    },
                    "head": {
                        "type": "integer",
                        "description": "Show first N lines of output (optional)"
                    },
                    "tail": {
                        "type": "integer",
                        "description": "Show last N lines of output (default: 100, use 0 for all)"
                    }
                },
                "required": ["thought", "command"]
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

        // Build the command with optional environment variables.
        // stdin is explicitly detached: agent tools are non-interactive, and
        // inheriting the parent's TTY lets the child (e.g. `sudo`, `ssh`,
        // `$EDITOR`) fight ratatui for input and corrupt termios state.
        // See `better-tty.md` for the full rationale.
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(&args.command)
            .stdin(std::process::Stdio::null())
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

                // Save full output to temp files (always saved)
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

                // Apply head/tail truncation (defaults to tail: 100)
                let default_tail = Some(100);
                let (stdout, stdout_modified) =
                    apply_head_tail(&stdout_raw, args.head, args.tail.or(default_tail));
                let (stderr, stderr_modified) =
                    apply_head_tail(&stderr_raw, args.head, args.tail.or(default_tail));

                let mut result = format!("Exit code: {}\n", exit_code);
                if !stdout_raw.is_empty() {
                    result.push_str(&format!("\nSTDOUT:\n{}\n", stdout));
                }
                if !stderr_raw.is_empty() {
                    result.push_str(&format!("\nSTDERR:\n{}\n", stderr));
                }

                // Always show full output location (it's always saved)
                if !stdout_path.as_os_str().is_empty() || !stderr_path.as_os_str().is_empty() {
                    let divider = "\n─────────────────────────────────────────────────\n";
                    result.push_str(divider);
                    result.push_str("Full output saved to:\n");
                    if !stdout_path.as_os_str().is_empty() {
                        result.push_str(&format!("  {}\n", stdout_path.display()));
                    }
                    if !stderr_path.as_os_str().is_empty() {
                        result.push_str(&format!("  {}\n", stderr_path.display()));
                    }
                    if !stdout_modified && !stderr_modified {
                        result.push_str("(output was not truncated)\n");
                    } else {
                        result.push_str("Use file_read tool to access the complete output.\n");
                    }
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
                    stdout_modified = stdout_modified,
                    stderr_modified = stderr_modified,
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
