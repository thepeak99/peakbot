use crate::tools::file_edit::resolve_against;
use crate::utils::strings::truncate_with_suffix;
use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

const MAX_OUTPUT_CHARS: usize = 50_000;
const TRUNCATION_NOTICE: &str =
    "\n... [output truncated] Use start_line/end_line to read specific sections.";

#[derive(Debug, thiserror::Error)]
pub enum FileReadError {
    #[error("{0}")]
    Validation(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Deserialize)]
pub struct FileReadArgs {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

/// Read-a-file tool. `session_cwd` is the base for relative path resolution;
/// the `Default` empty path leaves relatives anchored at the process cwd
/// (tests / no state manager).
#[derive(Serialize, Deserialize, Default)]
pub struct FileReadTool {
    #[serde(skip)]
    session_cwd: PathBuf,
}

impl FileReadTool {
    pub fn new(session_cwd: PathBuf) -> Self {
        Self { session_cwd }
    }
}

impl Tool for FileReadTool {
    const NAME: &'static str = "file_read";
    type Error = FileReadError;
    type Args = FileReadArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "file_read".to_string(),
            description: "Read the contents of a file. Returns the file content with line numbers."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read (absolute, or relative to the working directory)"
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "Optional: start reading from this line (1-indexed)"
                    },
                    "end_line": {
                        "type": "integer",
                        "description": "Optional: stop reading at this line (1-indexed, inclusive)"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Log before execution
        tracing::info!(
            target: "peakbot",
            tool_type = "file_read",
            path = %args.path,
            start_line = args.start_line,
            end_line = args.end_line,
            "Starting file_read tool execution"
        );

        let start_time = std::time::Instant::now();
        let path = resolve_against(&self.session_cwd, &args.path);
        let path = path.as_path();

        if !path.exists() {
            return Err(FileReadError::Validation(format!(
                "File '{}' does not exist.",
                args.path
            )));
        }
        if path.is_dir() {
            return Err(FileReadError::Validation(format!(
                "'{}' is a directory. Use list_directory instead.",
                args.path
            )));
        }

        let content = std::fs::read_to_string(path)?;
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();

        let start = args.start_line.map(|s| s.saturating_sub(1)).unwrap_or(0);
        let end = args.end_line.unwrap_or(total).min(total);

        if start >= total {
            return Err(FileReadError::Validation(format!(
                "start_line {} exceeds file length of {} lines",
                start + 1,
                total
            )));
        }
        if end <= start {
            return Err(FileReadError::Validation(format!(
                "end_line {} must be greater than start_line {}",
                end,
                start + 1
            )));
        }

        let output: String = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6}\t{}", start + i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        let output = maybe_truncate(&output);

        tracing::info!(
            target: "peakbot",
            tool_type = "file_read",
            path = %args.path,
            total_lines = total,
            lines_read = end - start,
            duration_ms = start_time.elapsed().as_millis(),
            "File read completed successfully"
        );

        Ok(output)
    }
}

fn maybe_truncate(s: &str) -> String {
    if s.len() > MAX_OUTPUT_CHARS {
        truncate_with_suffix(s, MAX_OUTPUT_CHARS, TRUNCATION_NOTICE)
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // A relative path resolves against the injected session_cwd, NOT the
    // process cwd — the core Phase-1 guarantee.
    #[tokio::test]
    async fn resolves_relative_against_session_cwd() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("note.txt"), "hello from session dir").unwrap();

        let tool = FileReadTool::new(dir.path().to_path_buf());
        let args: FileReadArgs = serde_json::from_value(serde_json::json!({
            "path": "note.txt"
        }))
        .unwrap();

        let out = tool.call(args).await.expect("relative read should resolve");
        assert!(out.contains("hello from session dir"), "got: {out}");
    }

    // With the default empty base, a relative path resolves against the
    // process cwd (std default) — the test/no-state-manager fallback.
    #[tokio::test]
    async fn default_base_uses_process_cwd() {
        let tool = FileReadTool::default();
        let args: FileReadArgs = serde_json::from_value(serde_json::json!({
            "path": "definitely-not-a-real-file-xyz.txt"
        }))
        .unwrap();
        // Resolves against the process cwd and simply doesn't exist — proves
        // no "not absolute" rejection fires anymore.
        let err = tool.call(args).await.unwrap_err();
        assert!(
            matches!(err, FileReadError::Validation(ref m) if m.contains("does not exist")),
            "expected does-not-exist, got: {err}"
        );
    }
}
