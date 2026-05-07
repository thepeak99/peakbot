use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

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
    /// Optional reasoning narration. See `FileEditArgs.thought`.
    #[serde(default)]
    #[allow(dead_code)]
    thought: Option<String>,
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

#[derive(Serialize, Deserialize)]
pub struct FileReadTool;

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
                    "thought": {
                        "type": "string",
                        "description": "Optional: briefly explain what you're about to do and why, for the user's logs. Safe to omit on long payloads."
                    },
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the file to read"
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
        let path = Path::new(&args.path);

        if !path.is_absolute() {
            return Err(FileReadError::Validation(format!(
                "Path '{}' is not absolute. Use an absolute path starting with '/'.",
                args.path
            )));
        }
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
        format!("{}{}", &s[..MAX_OUTPUT_CHARS], TRUNCATION_NOTICE)
    } else {
        s.to_string()
    }
}
