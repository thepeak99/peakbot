use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SNIPPET_CONTEXT_LINES: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum FileEditError {
    #[error("{0}")]
    Validation(String),
    #[error("IO error on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Deserialize)]
pub struct FileEditArgs {
    command: String,
    path: String,
    file_text: Option<String>,
    old_str: Option<String>,
    new_str: Option<String>,
    insert_line: Option<usize>,
    insert_text: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct FileEditTool {
    #[serde(skip)]
    file_history: Mutex<HashMap<PathBuf, Vec<String>>>,
}

impl Default for FileEditTool {
    fn default() -> Self {
        Self {
            file_history: Mutex::new(HashMap::new()),
        }
    }
}

impl Tool for FileEditTool {
    const NAME: &'static str = "file_edit";
    type Error = FileEditError;
    type Args = FileEditArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "file_edit".to_string(),
            description: "A filesystem editor tool. Supports three commands:\n\
                - `create`: Create a new file (fails if file already exists)\n\
                - `str_replace`: Replace an exact unique string in a file\n\
                - `insert`: Insert text at a specific line number"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "enum": ["create", "str_replace", "insert"],
                        "description": "The editing command to execute"
                    },
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the file"
                    },
                    "file_text": {
                        "type": "string",
                        "description": "Required for 'create': the full content of the new file"
                    },
                    "old_str": {
                        "type": "string",
                        "description": "Required for 'str_replace': the exact string to find. Must appear exactly once in the file."
                    },
                    "new_str": {
                        "type": "string",
                        "description": "Optional for 'str_replace': replacement string. Omit to delete old_str."
                    },
                    "insert_line": {
                        "type": "integer",
                        "description": "Required for 'insert': line number to insert after. 0 = insert at beginning."
                    },
                    "insert_text": {
                        "type": "string",
                        "description": "Required for 'insert': the text to insert"
                    }
                },
                "required": ["command", "path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Log before execution
        tracing::info!(
            target: "peakbot",
            tool_type = "file_edit",
            command = %args.command,
            path = %args.path,
            "Starting file_edit tool execution"
        );

        let start_time = std::time::Instant::now();
        let result = match args.command.as_str() {
            "create" => self.cmd_create(&args),
            "str_replace" => self.cmd_str_replace(&args),
            "insert" => self.cmd_insert(&args),
            other => Err(FileEditError::Validation(format!(
                "Unknown command '{}'. Valid commands: create, str_replace, insert",
                other
            ))),
        };

        // Log after execution
        match &result {
            Ok(output) => {
                tracing::info!(
                    target: "peakbot",
                    tool_type = "file_edit",
                    command = %args.command,
                    path = %args.path,
                    output_len = output.len(),
                    duration_ms = start_time.elapsed().as_millis(),
                    "File edit completed successfully"
                );
            }
            Err(e) => {
                tracing::warn!(
                    target: "peakbot",
                    tool_type = "file_edit",
                    command = %args.command,
                    path = %args.path,
                    error = %e,
                    "File edit failed"
                );
            }
        }

        result
    }
}

impl FileEditTool {
    fn cmd_create(&self, args: &FileEditArgs) -> Result<String, FileEditError> {
        let path = Path::new(&args.path);

        if !path.is_absolute() {
            return Err(FileEditError::Validation(format!(
                "Path '{}' is not absolute. Use an absolute path starting with '/'.",
                path.display()
            )));
        }

        let file_text = args.file_text.as_deref().ok_or_else(|| {
            FileEditError::Validation("'file_text' is required for create command".into())
        })?;

        if path.exists() {
            return Err(FileEditError::Validation(format!(
                "File already exists at {}. Cannot overwrite with 'create'. Use 'str_replace' to edit existing files.",
                path.display()
            )));
        }

        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent).map_err(|e| FileEditError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        self.write_file(path, file_text)?;

        // Store in history
        self.push_history(path, file_text);

        Ok(format!("File created successfully at: {}", path.display()))
    }

    fn cmd_str_replace(&self, args: &FileEditArgs) -> Result<String, FileEditError> {
        let path = Path::new(&args.path);
        self.validate_path_exists(path)?;

        if path.is_dir() {
            return Err(FileEditError::Validation(
                "str_replace cannot be used on directories".into(),
            ));
        }

        let old_str = args.old_str.as_deref().ok_or_else(|| {
            FileEditError::Validation("'old_str' is required for str_replace command".into())
        })?;
        let new_str = args.new_str.as_deref().unwrap_or("");

        let content = self.read_file(path)?;

        // Count occurrences
        let count = content.matches(old_str).count();
        if count == 0 {
            return Err(FileEditError::Validation(format!(
                "old_str not found verbatim in {}. Ensure you're matching the exact text including whitespace and indentation.",
                path.display()
            )));
        }
        if count > 1 {
            let line_nums: Vec<usize> = content
                .lines()
                .enumerate()
                .filter(|(_, line)| line.contains(old_str))
                .map(|(i, _)| i + 1)
                .collect();
            return Err(FileEditError::Validation(format!(
                "old_str appears {} times in {} at lines {:?}. Include more surrounding context to make the match unique.",
                count,
                path.display(),
                line_nums
            )));
        }

        // Save undo history
        self.push_history(path, &content);

        // Perform replacement
        let new_content = content.replacen(old_str, new_str, 1);
        self.write_file(path, &new_content)?;

        // Build context snippet
        let replacement_line = content
            .split(old_str)
            .next()
            .unwrap_or("")
            .matches('\n')
            .count();
        let new_lines: Vec<&str> = new_content.lines().collect();
        let start = replacement_line.saturating_sub(SNIPPET_CONTEXT_LINES);
        let end = (replacement_line + SNIPPET_CONTEXT_LINES + new_str.matches('\n').count() + 1)
            .min(new_lines.len());
        let snippet = format_lines_numbered(&new_lines[start..end], start + 1);

        Ok(format!(
            "File {} has been edited. Here's the result around the edit:\n{}\nReview the changes and edit again if necessary.",
            path.display(),
            snippet
        ))
    }

    fn cmd_insert(&self, args: &FileEditArgs) -> Result<String, FileEditError> {
        let path = Path::new(&args.path);
        self.validate_path_exists(path)?;

        if path.is_dir() {
            return Err(FileEditError::Validation(
                "insert cannot be used on directories".into(),
            ));
        }

        let insert_line = args.insert_line.ok_or_else(|| {
            FileEditError::Validation("'insert_line' is required for insert command".into())
        })?;
        let insert_text = args.insert_text.as_deref().ok_or_else(|| {
            FileEditError::Validation("'insert_text' is required for insert command".into())
        })?;

        let content = self.read_file(path)?;
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();

        if insert_line > total {
            return Err(FileEditError::Validation(format!(
                "insert_line {} is out of range. File has {} lines. Valid range: [0, {}].",
                insert_line, total, total
            )));
        }

        // Save undo history
        self.push_history(path, &content);

        // Insert new lines
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
        self.write_file(path, &new_content)?;

        // Build context snippet
        let start = insert_line.saturating_sub(SNIPPET_CONTEXT_LINES);
        let end =
            (insert_line + new_text_lines.len() + SNIPPET_CONTEXT_LINES).min(result_lines.len());
        let snippet = format_lines_numbered(&result_lines[start..end], start + 1);

        Ok(format!(
            "File {} has been edited. Here's the result around the insertion:\n{}\nReview the changes and edit again if necessary.",
            path.display(),
            snippet
        ))
    }

    // ── Helpers ────────────────────────────────────────

    fn validate_path_exists(&self, path: &Path) -> Result<(), FileEditError> {
        if !path.is_absolute() {
            return Err(FileEditError::Validation(format!(
                "Path '{}' is not absolute. Use an absolute path starting with '/'.",
                path.display()
            )));
        }
        if !path.exists() {
            return Err(FileEditError::Validation(format!(
                "Path '{}' does not exist.",
                path.display()
            )));
        }
        Ok(())
    }

    fn read_file(&self, path: &Path) -> Result<String, FileEditError> {
        std::fs::read_to_string(path).map_err(|e| FileEditError::Io {
            path: path.to_path_buf(),
            source: e,
        })
    }

    fn write_file(&self, path: &Path, content: &str) -> Result<(), FileEditError> {
        std::fs::write(path, content).map_err(|e| FileEditError::Io {
            path: path.to_path_buf(),
            source: e,
        })
    }

    fn push_history(&self, path: &Path, content: &str) {
        let mut history = self.file_history.lock().unwrap_or_else(|e| e.into_inner());
        history
            .entry(path.to_path_buf())
            .or_default()
            .push(content.to_string());
    }
}

fn format_lines_numbered(lines: &[&str], start_num: usize) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>6}\t{}", start_num + i, line))
        .collect::<Vec<_>>()
        .join("\n")
}
