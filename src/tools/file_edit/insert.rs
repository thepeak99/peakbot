//! `file_insert`: insert text at a specific line in an existing file.

use std::path::{Path, PathBuf};

use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::Deserialize;
use serde_json::json;

use super::{
    FileEditError, SNIPPET_CONTEXT_LINES, format_lines_numbered, read_file, resolve_against,
    validate_path_exists, write_file,
};

#[derive(Deserialize)]
pub struct FileInsertArgs {
    path: String,
    insert_line: usize,
    insert_text: String,
}

/// Insert-at-line tool. `session_cwd` is the base for relative path resolution.
#[derive(Default)]
pub struct FileInsertTool {
    session_cwd: Option<PathBuf>,
}

impl FileInsertTool {
    pub fn new(session_cwd: Option<PathBuf>) -> Self {
        Self { session_cwd }
    }
}

impl Tool for FileInsertTool {
    const NAME: &'static str = "file_insert";
    type Error = FileEditError;
    type Args = FileInsertArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Insert text at a specific line in an existing file.\n\n\
PREFER THIS TOOL OVER BASH for inserting lines because:\n\
- Line-number addressing is unambiguous and platform-independent\n\
- Preserves trailing newline if the original had one\n\
- Returns a numbered snippet around the insertion for review\n\n\
`insert_line: 0` inserts at the beginning of the file; `insert_line: N` inserts \
after line N. Returns an error if `insert_line` exceeds the file's line count."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file (absolute, or relative to the working directory)."
                    },
                    "insert_line": {
                        "type": "integer",
                        "description": "Line number to insert after. 0 = insert at beginning."
                    },
                    "insert_text": {
                        "type": "string",
                        "description": "The text to insert."
                    }
                },
                "required": ["path", "insert_line", "insert_text"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            target: "peakbot",
            tool_type = "file_insert",
            path = %args.path,
            insert_line = args.insert_line,
            "Starting file_insert tool execution"
        );

        let start_time = std::time::Instant::now();
        let resolved = resolve_against(self.session_cwd.as_deref(), &args.path);
        let result = run(
            &resolved.to_string_lossy(),
            args.insert_line,
            &args.insert_text,
        );

        match &result {
            Ok(output) => tracing::info!(
                target: "peakbot",
                tool_type = "file_insert",
                path = %args.path,
                output_len = output.len(),
                duration_ms = start_time.elapsed().as_millis(),
                "file_insert completed successfully"
            ),
            Err(e) => tracing::warn!(
                target: "peakbot",
                tool_type = "file_insert",
                path = %args.path,
                error = %e,
                "file_insert failed"
            ),
        }

        result
    }
}

/// Core logic, factored out so the tests can drive it without going through the Rig Tool trait.
pub(crate) fn run(
    path: &str,
    insert_line: usize,
    insert_text: &str,
) -> Result<String, FileEditError> {
    let path = Path::new(path);
    validate_path_exists(path)?;

    if path.is_dir() {
        return Err(FileEditError::Validation(
            "file_insert cannot be used on directories".into(),
        ));
    }

    let content = read_file(path)?;
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    if insert_line > total {
        return Err(FileEditError::Validation(format!(
            "insert_line {} is out of range. File has {} lines. Valid range: [0, {}].",
            insert_line, total, total
        )));
    }

    let new_text_lines: Vec<&str> = insert_text.lines().collect();
    let mut result_lines = Vec::with_capacity(total + new_text_lines.len());
    result_lines.extend_from_slice(&lines[..insert_line]);
    result_lines.extend_from_slice(&new_text_lines);
    result_lines.extend_from_slice(&lines[insert_line..]);

    let new_content = result_lines.join("\n");
    // Preserve trailing newline if original had one
    let new_content = if content.ends_with('\n') && !new_content.ends_with('\n') {
        new_content + "\n"
    } else {
        new_content
    };
    write_file(path, &new_content)?;

    let start = insert_line.saturating_sub(SNIPPET_CONTEXT_LINES);
    let end = (insert_line + new_text_lines.len() + SNIPPET_CONTEXT_LINES).min(result_lines.len());
    let snippet = format_lines_numbered(&result_lines[start..end], start + 1);

    Ok(format!(
        "File {} has been edited. Here's the result around the insertion:\n{}\nReview the changes and edit again if necessary.",
        path.display(),
        snippet
    ))
}
