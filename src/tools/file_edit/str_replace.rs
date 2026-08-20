//! `file_str_replace`: replace exact text in an existing file.

use std::ops::Range;
use std::path::{Path, PathBuf};

use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::Deserialize;
use serde_json::json;

use super::{
    FileEditError, MatchLevel, MatchResult, SNIPPET_CONTEXT_LINES, format_lines_numbered,
    progressive_match, read_file, resolve_against, validate_path_exists, write_file,
};

#[derive(Deserialize)]
pub struct FileStrReplaceArgs {
    path: String,
    old_str: String,
    new_str: Option<String>,
    replace_all: Option<bool>,
}

/// Replace-text tool. `session_cwd` is the base for relative path resolution.
#[derive(Default)]
pub struct FileStrReplaceTool {
    session_cwd: PathBuf,
}

impl FileStrReplaceTool {
    pub fn new(session_cwd: PathBuf) -> Self {
        Self { session_cwd }
    }
}

impl Tool for FileStrReplaceTool {
    const NAME: &'static str = "file_str_replace";
    type Error = FileEditError;
    type Args = FileStrReplaceArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Replace exact text in an existing file.\n\n\
PREFER THIS TOOL OVER BASH/SED for in-place edits because:\n\
- Provides clear diffs for review\n\
- Automatically handles whitespace differences (exact → whitespace-normalized)\n\
- Refuses ambiguous matches unless `replace_all: true` is set\n\
- Works across all platforms (sed syntax varies)\n\n\
`old_str` must appear exactly once in the file unless `replace_all: true`. \
Tip: include 2-3 lines of surrounding context in `old_str` for unique matches. \
If editing fails, read the file first with `file_read` to get exact content, then retry."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file (absolute, or relative to the working directory)."
                    },
                    "old_str": {
                        "type": "string",
                        "description": "The exact string to find. Must appear exactly once in the file (unless replace_all is true)."
                    },
                    "new_str": {
                        "type": "string",
                        "description": "Replacement string. Omit to delete old_str."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "If true, replace all occurrences. Default: false (single match only)."
                    }
                },
                "required": ["path", "old_str"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            target: "peakbot",
            tool_type = "file_str_replace",
            path = %args.path,
            "Starting file_str_replace tool execution"
        );

        let start_time = std::time::Instant::now();
        let resolved = resolve_against(&self.session_cwd, &args.path);
        let result = run(
            &resolved.to_string_lossy(),
            &args.old_str,
            args.new_str.as_deref(),
            args.replace_all.unwrap_or(false),
        );

        match &result {
            Ok(output) => tracing::info!(
                target: "peakbot",
                tool_type = "file_str_replace",
                path = %args.path,
                output_len = output.len(),
                duration_ms = start_time.elapsed().as_millis(),
                "file_str_replace completed successfully"
            ),
            Err(e) => tracing::warn!(
                target: "peakbot",
                tool_type = "file_str_replace",
                path = %args.path,
                error = %e,
                "file_str_replace failed"
            ),
        }

        result
    }
}

/// Splice `new_str` into every range. Ranges must be sorted ascending and
/// pairwise disjoint (guaranteed by the matchers, invariant I3) — one path
/// handles both the single-match and `replace_all` cases, so a match is
/// never re-searched literally and can never silently match nothing.
fn splice(content: &str, ranges: &[Range<usize>], new_str: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut cursor = 0;
    for range in ranges {
        result.push_str(&content[cursor..range.start]);
        result.push_str(new_str);
        cursor = range.end;
    }
    result.push_str(&content[cursor..]);
    result
}

/// Core logic, factored out so the tests can drive it without going through the Rig Tool trait.
pub(crate) fn run(
    path: &str,
    old_str: &str,
    new_str: Option<&str>,
    replace_all: bool,
) -> Result<String, FileEditError> {
    let path = Path::new(path);
    validate_path_exists(path)?;

    if path.is_dir() {
        return Err(FileEditError::Validation(
            "file_str_replace cannot be used on directories".into(),
        ));
    }

    // An empty (or whitespace-only) old_str can only mean "insert at every
    // byte offset" — never a sensible edit, and catastrophic under
    // replace_all (today: `String::replace("", new_str)` corrupts the
    // file). Reject at the boundary, before any read or match attempt.
    if old_str.trim().is_empty() {
        return Err(FileEditError::Validation(
            "old_str must not be empty".into(),
        ));
    }

    let new_str = new_str.unwrap_or("");
    let content = read_file(path)?;

    let match_result = progressive_match(&content, old_str);

    let (ranges, match_level): (Vec<Range<usize>>, MatchLevel) = match match_result {
        MatchResult::NoMatch => {
            return Err(FileEditError::Validation(format!(
                "String not found in file '{}'\n\n\
Searched for:\n  {}\n\n\
Suggestions:\n\
1. The text might have different whitespace or indentation\n   Try: file_read with line ranges to see exact formatting\n   \n\
2. If you need to replace all occurrences of a pattern, use replace_all: true\n\
3. Always read the file first for precise edits to get exact content.\n\n\
Tip: Include 2-3 lines of surrounding context in old_str for better matching.",
                path.display(),
                old_str.lines().take(5).collect::<Vec<_>>().join("\n  ")
            )));
        }
        MatchResult::MultipleMatches { ranges, level } => {
            if replace_all {
                (ranges, level)
            } else {
                let line_nums: Vec<usize> = ranges
                    .iter()
                    .map(|r| content[..r.start].lines().count() + 1)
                    .collect();
                return Err(FileEditError::Validation(format!(
                    "String appears {} times in '{}' at lines {:?}.\n\n\
To replace all occurrences, use: replace_all: true\n\
To replace a specific occurrence, include more surrounding context in old_str to make it unique.\n\n\
Tip: Read the file first with file_read to see exact formatting.",
                    ranges.len(),
                    path.display(),
                    line_nums
                )));
            }
        }
        MatchResult::UniqueMatch { range, level } => (vec![range], level),
    };

    let count = ranges.len();
    let new_content = splice(&content, &ranges, new_str);
    write_file(path, &new_content)?;

    let replacement_msg = if count > 1 {
        format!("Replaced all {} occurrences", count)
    } else {
        "Replaced 1 occurrence".to_string()
    };

    let replacement_line = content[..ranges[0].start].lines().count();
    let new_lines: Vec<&str> = new_content.lines().collect();
    let start = replacement_line.saturating_sub(SNIPPET_CONTEXT_LINES);
    let end = (replacement_line + SNIPPET_CONTEXT_LINES + new_str.matches('\n').count() + 1)
        .min(new_lines.len());
    let snippet = format_lines_numbered(&new_lines[start..end], start + 1);

    let mut warnings = Vec::new();

    if count > 1 {
        warnings.push(format!(
            "⚠️  Global replacement: {} occurrences changed. Review carefully.",
            count
        ));
    }

    if match_level != MatchLevel::Exact {
        warnings.push(format!(
            "ℹ️  Match required {}. Consider reading file first for exact match.",
            match_level.as_str()
        ));
    }

    let warning = if warnings.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", warnings.join("\n"))
    };

    Ok(format!(
        "✅ Successfully edited {}\n\n{}\n\n{}\n{}\n\
Review the changes and edit again if necessary.\n\
Tip: For global replacements, use replace_all: true",
        path.display(),
        replacement_msg,
        snippet,
        warning
    ))
}
