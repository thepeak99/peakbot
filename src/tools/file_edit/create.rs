//! `file_create`: create a new file (fails if it already exists).

use std::path::{Path, PathBuf};

use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::Deserialize;
use serde_json::json;

use super::{FileEditError, resolve_against, write_file};

#[derive(Deserialize)]
pub struct FileCreateArgs {
    path: String,
    file_text: Option<String>,
}

/// Create-a-file tool. `session_cwd` is the directory relative paths resolve
/// against; the `Default` empty path leaves relatives anchored at the process
/// cwd (tests / no state manager).
#[derive(Default)]
pub struct FileCreateTool {
    session_cwd: PathBuf,
}

impl FileCreateTool {
    pub fn new(session_cwd: PathBuf) -> Self {
        Self { session_cwd }
    }
}

impl Tool for FileCreateTool {
    const NAME: &'static str = "file_create";
    type Error = FileEditError;
    type Args = FileCreateArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Create a new file. Fails if the file already exists \
                — use `file_str_replace` to edit existing files.\n\n\
PREFER THIS TOOL OVER BASH for creating files because:\n\
- Path may be absolute or relative to the working directory\n\
- Auto-creates missing parent directories\n\
- Safer: refuses to clobber existing files\n\
- Works across all platforms\n\n\
If `file_text` is omitted or empty, an empty file is created. \
Recommended: provide content upfront with `file_text` rather than creating an empty file \
and editing it later."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to create (absolute, or relative to the working directory)."
                    },
                    "file_text": {
                        "type": "string",
                        "description": "The full content of the new file. If omitted or empty, an empty file is created. Recommended: provide content upfront to avoid creating an empty file and editing it later."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            target: "peakbot",
            tool_type = "file_create",
            path = %args.path,
            "Starting file_create tool execution"
        );

        let start_time = std::time::Instant::now();
        let resolved = resolve_against(&self.session_cwd, &args.path);
        let result = run(&resolved.to_string_lossy(), args.file_text.as_deref());

        match &result {
            Ok(output) => tracing::info!(
                target: "peakbot",
                tool_type = "file_create",
                path = %args.path,
                output_len = output.len(),
                duration_ms = start_time.elapsed().as_millis(),
                "file_create completed successfully"
            ),
            Err(e) => tracing::warn!(
                target: "peakbot",
                tool_type = "file_create",
                path = %args.path,
                error = %e,
                "file_create failed"
            ),
        }

        result
    }
}

/// Core logic, factored out so the tests can drive it without going through the Rig Tool trait.
pub(crate) fn run(path: &str, file_text: Option<&str>) -> Result<String, FileEditError> {
    let path = Path::new(path);

    let (file_text, recommendation) = match file_text {
        None => (String::new(), true),
        Some("") => (String::new(), true),
        Some(text) => (text.to_string(), false),
    };

    if path.exists() {
        return Err(FileEditError::Validation(format!(
            "File already exists at {}. Cannot overwrite with 'file_create'. Use 'file_str_replace' to edit existing files.",
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

    write_file(path, &file_text)?;

    let mut msg = format!("File created successfully at: {}", path.display());
    if recommendation {
        msg.push_str(
            "\n\n💡 Tip: Consider providing the file content upfront with the `file_text` \
             parameter instead of creating an empty file and editing it later.",
        );
    }

    Ok(msg)
}
