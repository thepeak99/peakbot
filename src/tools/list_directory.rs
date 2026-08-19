use crate::tools::file_edit::resolve_against;
use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ListDirectoryError {
    #[error("{0}")]
    Validation(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Deserialize)]
pub struct ListDirectoryArgs {
    path: String,
    recursive: Option<bool>,
}

/// List-directory tool. `session_cwd` is the base for relative path resolution;
/// the `Default` empty path leaves relatives anchored at the process cwd
/// (tests / no state manager).
#[derive(Serialize, Deserialize, Default)]
pub struct ListDirectoryTool {
    #[serde(skip)]
    session_cwd: PathBuf,
}

impl ListDirectoryTool {
    pub fn new(session_cwd: PathBuf) -> Self {
        Self { session_cwd }
    }
}

impl Tool for ListDirectoryTool {
    const NAME: &'static str = "list_directory";
    type Error = ListDirectoryError;
    type Args = ListDirectoryArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "list_directory".to_string(),
            description: "List files and directories at the given path. \
                Returns names with indicators for directories (trailing /). \
                Optionally recurse into subdirectories (max depth 3)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the directory to list (absolute, or relative to the working directory)"
                    },
                    "recursive": {
                        "type": "boolean",
                        "description": "If true, recurse into subdirectories (max depth 3). Default: false."
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
            tool_type = "list_directory",
            path = %args.path,
            recursive = args.recursive.unwrap_or(false),
            "Starting list_directory tool execution"
        );

        let start_time = std::time::Instant::now();
        let resolved = resolve_against(&self.session_cwd, &args.path);
        let path = resolved.as_path();

        if !path.exists() {
            return Err(ListDirectoryError::Validation(format!(
                "Path '{}' does not exist.",
                args.path
            )));
        }
        if !path.is_dir() {
            return Err(ListDirectoryError::Validation(format!(
                "'{}' is not a directory. Use file_read to read files.",
                args.path
            )));
        }

        let recursive = args.recursive.unwrap_or(false);
        let max_depth = if recursive { 3 } else { 1 };

        let mut entries = Vec::new();
        collect_entries(path, path, 0, max_depth, &mut entries)?;
        entries.sort();

        let entry_count = entries.len();
        tracing::info!(
            target: "peakbot",
            tool_type = "list_directory",
            path = %args.path,
            entry_count = entry_count,
            duration_ms = start_time.elapsed().as_millis(),
            "List directory completed successfully"
        );

        let result = if entries.is_empty() {
            "(empty directory)".to_string()
        } else {
            entries.join("\n")
        };
        Ok(result)
    }
}

fn collect_entries(
    base: &Path,
    dir: &Path,
    depth: usize,
    max_depth: usize,
    entries: &mut Vec<String>,
) -> Result<(), ListDirectoryError> {
    if depth >= max_depth {
        return Ok(());
    }

    let mut dir_entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(|e| e.ok()).collect();

    dir_entries.sort_by_key(|e| e.file_name());

    for entry in dir_entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }

        let rel_path = entry
            .path()
            .strip_prefix(base)
            .unwrap_or(&entry.path())
            .to_string_lossy()
            .to_string();

        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            entries.push(format!("{}/", rel_path));
            collect_entries(base, &entry.path(), depth + 1, max_depth, entries)?;
        } else {
            entries.push(rel_path);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // Contract: an empty directory must NOT yield an empty tool result —
    // an empty tool-result string crashes some LLM providers. The exact
    // placeholder wording is left to the implementation, so this test
    // asserts non-emptiness only, never a specific string.
    #[tokio::test]
    async fn empty_directory_returns_non_empty_placeholder() {
        let dir = tempdir().unwrap();

        let tool = ListDirectoryTool::new(dir.path().to_path_buf());
        let args: ListDirectoryArgs = serde_json::from_value(json!({
            "path": dir.path().to_string_lossy().to_string()
        }))
        .unwrap();

        let out = tool
            .call(args)
            .await
            .expect("listing an existing empty directory should succeed");
        assert!(
            !out.is_empty(),
            "list_directory on an empty directory returned an empty string; \
             expected a non-empty placeholder message"
        );
    }
}
