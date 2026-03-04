use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

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

#[derive(Serialize, Deserialize)]
pub struct ListDirectoryTool;

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
                        "description": "Absolute path to the directory to list"
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
        let path = Path::new(&args.path);

        if !path.is_absolute() {
            return Err(ListDirectoryError::Validation(format!(
                "Path '{}' is not absolute. Use an absolute path starting with '/'.",
                args.path
            )));
        }
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

        Ok(entries.join("\n"))
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

    let mut dir_entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .collect();

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
