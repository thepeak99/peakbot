//! `file_create`: create a new file (fails if it already exists).

use std::path::Path;

use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::Deserialize;
use serde_json::json;

use super::{FileEditError, write_file};

#[derive(Deserialize)]
pub struct FileCreateArgs {
    path: String,
    file_text: Option<String>,
}

#[derive(Default)]
pub struct FileCreateTool;

impl Tool for FileCreateTool {
    const NAME: &'static str = "file_create";
    type Error = FileEditError;
    type Args = FileCreateArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Create a new file at an absolute path. Fails if the file already exists \
                — use `file_str_replace` to edit existing files.\n\n\
PREFER THIS TOOL OVER BASH for creating files because:\n\
- Validates the path is absolute up front\n\
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
                        "description": "Absolute path to the file to create."
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
        let result = run(&args.path, args.file_text.as_deref());

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

    if !path.is_absolute() {
        return Err(FileEditError::Validation(format!(
            "Path '{}' is not absolute. Use an absolute path starting with '/'.",
            path.display()
        )));
    }

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
