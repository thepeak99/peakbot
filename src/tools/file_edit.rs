use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SNIPPET_CONTEXT_LINES: usize = 4;

/// Level of matching that was used to find the text
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchLevel {
    Exact,
    WhitespaceNormalized,
    FlexibleWhitespace,
}

impl MatchLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            MatchLevel::Exact => "exact",
            MatchLevel::WhitespaceNormalized => "whitespace_normalized",
            MatchLevel::FlexibleWhitespace => "flexible_whitespace",
        }
    }
}

/// Result of a matching operation
#[derive(Debug, Clone)]
pub enum MatchResult {
    NoMatch,
    MultipleMatches {
        count: usize,
        positions: Vec<usize>,
    },
    UniqueMatch {
        position: usize,
        match_level: MatchLevel,
        confidence: f32,
    },
}

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
    #[allow(dead_code)]
    thought: String,
    command: String,
    path: String,
    file_text: Option<String>,
    old_str: Option<String>,
    new_str: Option<String>,
    insert_line: Option<usize>,
    insert_text: Option<String>,
    replace_all: Option<bool>,
}

#[derive(Serialize, Deserialize)]
pub struct FileEditTool {
    /// Per-path undo stack. Each entry is the **pre-edit** snapshot:
    /// - `Some(content)` — the file existed with this content before the edit
    /// - `None` — the file did not exist (used by `create`, so undo deletes)
    #[serde(skip)]
    file_history: Mutex<HashMap<PathBuf, Vec<Option<String>>>>,
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
            description: "Edit files safely with automatic formatting detection.\n\n\
PREFER THIS TOOL OVER BASH/SED for all file modifications because:\n\
- Provides clear diffs for review\n\
- Automatically handles whitespace differences\n\
- Safer: won't accidentally modify wrong files\n\
- Works across all platforms (sed syntax varies)\n\
\n\
Use bash ONLY for: file operations (mv/cp/rm), permissions, bulk operations on many files.\n\
\n\
Commands:\n\
- `create`: Create a new file (fails if file already exists)\n\
- `str_replace`: Replace text in a file (use replace_all:true for global replacement)\n\
- `insert`: Insert text at a specific line number\n\
- `undo_edit`: Revert the most recent file_edit on a path (per-session undo stack)\n\
\n\
If editing fails, read the file first to get exact content, then retry."
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
                        "enum": ["create", "str_replace", "insert", "undo_edit"],
                        "description": "The editing command to execute"
                    },
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the file"
                    },
                    "file_text": {
                        "type": "string",
                        "description": "Optional for `create`: the full content of the new file. If omitted or empty, an empty file is created. Recommended: provide content upfront to avoid creating an empty file and editing it later."
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
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Optional for 'str_replace': if true, replace all occurrences instead of just the first one. Default: false (single match only)."
                    }
                },
                "required": ["thought", "command", "path"]
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
            "undo_edit" => self.cmd_undo_edit(&args),
            other => Err(FileEditError::Validation(format!(
                "Unknown command '{}'. Valid commands: create, str_replace, insert, undo_edit",
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

        let (file_text, recommendation) = match args.file_text.as_deref() {
            None => (String::new(), true),
            Some("") => (String::new(), true),
            Some(text) => (text.to_string(), false),
        };

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

        self.write_file(path, &file_text)?;

        // Record pre-edit state ("file did not exist") so undo deletes it.
        self.push_history_none(path);

        let mut msg = format!("File created successfully at: {}", path.display());
        if recommendation {
            msg.push_str(
                "\n\n💡 Tip: Consider providing the file content upfront with the `file_text` \
                 parameter instead of creating an empty file and editing it later.",
            );
        }

        Ok(msg)
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
        let replace_all = args.replace_all.unwrap_or(false);

        let content = self.read_file(path)?;

        // Use progressive matching
        let match_result = self.progressive_match(&content, old_str);

        // Handle match results
        let (count, position, match_level, confidence) = match match_result {
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
            MatchResult::MultipleMatches { count, positions } => {
                if replace_all {
                    (count, positions[0], MatchLevel::Exact, 1.0)
                } else {
                    let line_nums: Vec<usize> = positions
                        .iter()
                        .map(|&pos| content[..pos].lines().count() + 1)
                        .collect();
                    return Err(FileEditError::Validation(format!(
                        "String appears {} times in '{}' at lines {:?}.\n\n\
To replace all occurrences, use: replace_all: true\n\
To replace a specific occurrence, include more surrounding context in old_str to make it unique.\n\n\
Tip: Read the file first with file_read to see exact formatting.",
                        count,
                        path.display(),
                        line_nums
                    )));
                }
            }
            MatchResult::UniqueMatch {
                position,
                match_level,
                confidence,
            } => (1, position, match_level, confidence),
        };

        // Save undo history
        self.push_history(path, &content);

        // Perform replacement
        let new_content = if replace_all && count > 1 {
            content.replace(old_str, new_str)
        } else {
            content.replacen(old_str, new_str, 1)
        };
        self.write_file(path, &new_content)?;

        // Build success message
        let replacement_msg = if replace_all && count > 1 {
            format!("Replaced all {} occurrences", count)
        } else {
            "Replaced 1 occurrence".to_string()
        };

        // Build context snippet
        let replacement_line = content[..position].lines().count();
        let new_lines: Vec<&str> = new_content.lines().collect();
        let start = replacement_line.saturating_sub(SNIPPET_CONTEXT_LINES);
        let end = (replacement_line + SNIPPET_CONTEXT_LINES + new_str.matches('\n').count() + 1)
            .min(new_lines.len());
        let snippet = format_lines_numbered(&new_lines[start..end], start + 1);

        // Build warnings
        let mut warnings = Vec::new();

        if replace_all && count > 1 {
            warnings.push(format!(
                "⚠️  Global replacement: {} occurrences changed. Review carefully.",
                count
            ));
        }

        if match_level != MatchLevel::Exact {
            warnings.push(format!(
                "ℹ️  Match required {} (confidence: {:.0}%). Consider reading file first for exact match.",
                match_level.as_str(),
                confidence * 100.0
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

    // ── undo_edit ────────────────────────────────────────

    fn cmd_undo_edit(&self, args: &FileEditArgs) -> Result<String, FileEditError> {
        let path = Path::new(&args.path);

        if !path.is_absolute() {
            return Err(FileEditError::Validation(format!(
                "Path '{}' is not absolute. Use an absolute path starting with '/'.",
                path.display()
            )));
        }

        let snapshot = self.pop_history(path).ok_or_else(|| {
            FileEditError::Validation(format!(
                "Nothing to undo for '{}'. The undo stack is empty — only edits made by file_edit in this session can be undone.",
                path.display()
            ))
        })?;

        match snapshot {
            // File existed before the edit — restore its prior content.
            Some(prior) => {
                self.write_file(path, &prior)?;
                Ok(format!(
                    "✅ Undo successful: restored prior content of {} ({} bytes).",
                    path.display(),
                    prior.len()
                ))
            }
            // File did not exist before the edit — undo means delete it.
            None => {
                if path.exists() {
                    std::fs::remove_file(path).map_err(|e| FileEditError::Io {
                        path: path.to_path_buf(),
                        source: e,
                    })?;
                }
                Ok(format!(
                    "✅ Undo successful: removed {} (it did not exist before the create).",
                    path.display()
                ))
            }
        }
    }

    // ── Matching Functions ────────────────────────────────────────

    /// Level 1: Exact match (current implementation)
    fn exact_match(&self, content: &str, old_str: &str) -> MatchResult {
        let count = content.matches(old_str).count();

        if count == 0 {
            MatchResult::NoMatch
        } else if count > 1 {
            let positions: Vec<usize> =
                content.match_indices(old_str).map(|(pos, _)| pos).collect();
            MatchResult::MultipleMatches { count, positions }
        } else if let Some((pos, _)) = content.match_indices(old_str).next() {
            MatchResult::UniqueMatch {
                position: pos,
                match_level: MatchLevel::Exact,
                confidence: 1.0,
            }
        } else {
            MatchResult::NoMatch
        }
    }

    /// Level 2: Whitespace-normalized match
    /// Normalizes whitespace per line while preserving line structure
    fn whitespace_normalized_match(&self, content: &str, old_str: &str) -> MatchResult {
        let normalize_line = |line: &str| -> String {
            // Preserve leading indentation but normalize trailing whitespace
            let trimmed = line.trim_end();
            trimmed.to_string()
        };

        let normalize =
            |s: &str| -> String { s.lines().map(normalize_line).collect::<Vec<_>>().join("\n") };

        let norm_content = normalize(content);
        let norm_old = normalize(old_str);

        // Try to find in normalized content
        let count = norm_content.matches(&norm_old).count();

        if count == 0 {
            MatchResult::NoMatch
        } else if count > 1 {
            // Map positions back to original content
            let positions: Vec<usize> = norm_content
                .match_indices(&norm_old)
                .map(|(pos, _)| pos)
                .collect();
            MatchResult::MultipleMatches { count, positions }
        } else if let Some((norm_pos, _)) = norm_content.match_indices(&norm_old).next() {
            // Map normalized position back to original content
            // We need to find the corresponding line in original content
            let norm_lines: Vec<&str> = norm_content.lines().collect();
            let orig_lines: Vec<&str> = content.lines().collect();

            // Find which line the match starts on
            let match_start_line = norm_lines
                .iter()
                .take_while(|&&line| {
                    norm_pos > line.len() // +1 for newline
                })
                .count();

            // Find the position in original content
            let orig_pos: usize = orig_lines
                .iter()
                .take(match_start_line)
                .map(|l| l.len() + 1) // +1 for newline
                .sum();

            MatchResult::UniqueMatch {
                position: orig_pos,
                match_level: MatchLevel::WhitespaceNormalized,
                confidence: 0.95,
            }
        } else {
            MatchResult::NoMatch
        }
    }

    /// Level 3: Flexible whitespace regex
    /// Allows variable whitespace between tokens
    fn flexible_whitespace_match(&self, content: &str, old_str: &str) -> MatchResult {
        // Tokenize old_str on whitespace, but preserve the tokens
        let tokens: Vec<&str> = old_str
            .split(|c: char| c.is_whitespace() && c != '\n')
            .collect();

        if tokens.is_empty() || tokens.iter().all(|t| t.is_empty()) {
            return MatchResult::NoMatch;
        }

        // Build regex pattern with \s* between tokens
        // Escape regex special characters in each token
        let pattern: String = tokens
            .iter()
            .map(|token| regex::escape(token))
            .collect::<Vec<_>>()
            .join(r"\s*");

        // Add optional whitespace at start and end
        let pattern = format!(r"^\s*{}\s*$", pattern);

        // Try to match line by line first (for multi-line, we'll do a simpler approach)
        if old_str.contains('\n') {
            // For multi-line, use a simpler approach: normalize all whitespace to single space
            let flat_old = old_str.split_whitespace().collect::<Vec<_>>().join(" ");
            let flat_pattern = regex::escape(&flat_old);

            // Search in content with flexible whitespace
            let re = match regex::Regex::new(&format!(r"\s*{}\s*", flat_pattern)) {
                Ok(re) => re,
                Err(_) => return MatchResult::NoMatch,
            };

            let count = re.find_iter(content).count();

            if count == 0 {
                MatchResult::NoMatch
            } else if count > 1 {
                let positions: Vec<usize> = re.find_iter(content).map(|m| m.start()).collect();
                MatchResult::MultipleMatches { count, positions }
            } else if let Some(m) = re.find(content) {
                MatchResult::UniqueMatch {
                    position: m.start(),
                    match_level: MatchLevel::FlexibleWhitespace,
                    confidence: 0.85,
                }
            } else {
                MatchResult::NoMatch
            }
        } else {
            // Single line: use the token-based pattern
            let re = match regex::Regex::new(&pattern) {
                Ok(re) => re,
                Err(_) => return MatchResult::NoMatch,
            };

            let count = re.find_iter(content).count();

            if count == 0 {
                MatchResult::NoMatch
            } else if count > 1 {
                let positions: Vec<usize> = re.find_iter(content).map(|m| m.start()).collect();
                MatchResult::MultipleMatches { count, positions }
            } else if let Some(m) = re.find(content) {
                MatchResult::UniqueMatch {
                    position: m.start(),
                    match_level: MatchLevel::FlexibleWhitespace,
                    confidence: 0.85,
                }
            } else {
                MatchResult::NoMatch
            }
        }
    }

    /// Progressive fallback matching: tries Level 1, then 2, then 3
    fn progressive_match(&self, content: &str, old_str: &str) -> MatchResult {
        // Level 1: Exact match
        let result = self.exact_match(content, old_str);
        if matches!(result, MatchResult::UniqueMatch { .. }) {
            return result;
        }

        // Level 2: Whitespace normalized
        let result = self.whitespace_normalized_match(content, old_str);
        if matches!(result, MatchResult::UniqueMatch { .. }) {
            return result;
        }

        // Level 3: Flexible whitespace
        self.flexible_whitespace_match(content, old_str)
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
            .push(Some(content.to_string()));
    }

    /// Record that the file did not exist before this edit (used by `create`).
    /// Undoing such an entry means deleting the file.
    fn push_history_none(&self, path: &Path) {
        let mut history = self.file_history.lock().unwrap_or_else(|e| e.into_inner());
        history.entry(path.to_path_buf()).or_default().push(None);
    }

    fn pop_history(&self, path: &Path) -> Option<Option<String>> {
        let mut history = self.file_history.lock().unwrap_or_else(|e| e.into_inner());
        history.get_mut(path).and_then(|stack| stack.pop())
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Builder for `FileEditArgs` in tests — every field defaults to `None` /
    /// empty so each test only sets what it cares about.
    fn args(command: &str, path: &str) -> FileEditArgs {
        FileEditArgs {
            thought: "test".into(),
            command: command.into(),
            path: path.into(),
            file_text: None,
            old_str: None,
            new_str: None,
            insert_line: None,
            insert_text: None,
            replace_all: None,
        }
    }

    // ── create ─────────────────────────────────────────────────────────

    #[test]
    fn create_happy_path_writes_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        let tool = FileEditTool::default();

        let mut a = args("create", path.to_str().unwrap());
        a.file_text = Some("hello world\n".into());

        let out = tool.cmd_create(&a).expect("create should succeed");
        assert!(out.contains("File created successfully"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world\n");
    }

    #[test]
    fn create_rejects_relative_path() {
        let tool = FileEditTool::default();
        let mut a = args("create", "relative.txt");
        a.file_text = Some("x".into());

        let err = tool.cmd_create(&a).unwrap_err();
        assert!(
            matches!(err, FileEditError::Validation(ref msg) if msg.contains("not absolute")),
            "expected validation error about absolute path, got: {err}"
        );
    }

    #[test]
    fn create_rejects_existing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("exists.txt");
        std::fs::write(&path, "original").unwrap();

        let tool = FileEditTool::default();
        let mut a = args("create", path.to_str().unwrap());
        a.file_text = Some("new content".into());

        let err = tool.cmd_create(&a).unwrap_err();
        assert!(
            matches!(err, FileEditError::Validation(ref msg) if msg.contains("already exists")),
            "expected validation error about existing file, got: {err}"
        );
        // Original content untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    }

    #[test]
    fn create_without_file_text_creates_empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing-text.txt");
        let tool = FileEditTool::default();
        let a = args("create", path.to_str().unwrap()); // no file_text

        let out = tool
            .cmd_create(&a)
            .expect("create should succeed without file_text");
        assert!(
            out.contains("File created successfully"),
            "should report success, got: {out}"
        );
        assert!(
            out.contains("💡 Tip"),
            "should include recommendation tip, got: {out}"
        );
        assert!(path.exists(), "empty file should be created");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn create_makes_missing_parent_dirs() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a/b/c/deep.txt");
        let tool = FileEditTool::default();

        let mut a = args("create", path.to_str().unwrap());
        a.file_text = Some("deep".into());

        tool.cmd_create(&a)
            .expect("create should auto-create parents");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "deep");
    }

    // NOTE: The `create` command now allows omitted/empty `file_text`,
    // creating an empty file with a soft recommendation to provide content
    // upfront. See `create_without_file_text_creates_empty_file` and
    // `create_with_empty_file_text_creates_empty_file` for the current contract.

    // ── str_replace ────────────────────────────────────────────────────

    #[test]
    fn str_replace_basic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("r.txt");
        std::fs::write(&path, "alpha beta gamma").unwrap();

        let tool = FileEditTool::default();
        let mut a = args("str_replace", path.to_str().unwrap());
        a.old_str = Some("beta".into());
        a.new_str = Some("BETA".into());

        tool.cmd_str_replace(&a).expect("replace should succeed");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha BETA gamma");
    }

    #[test]
    fn str_replace_multiple_without_replace_all_errors() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("dup.txt");
        std::fs::write(&path, "x x x").unwrap();

        let tool = FileEditTool::default();
        let mut a = args("str_replace", path.to_str().unwrap());
        a.old_str = Some("x".into());
        a.new_str = Some("y".into());

        let err = tool.cmd_str_replace(&a).unwrap_err();
        assert!(matches!(err, FileEditError::Validation(_)));
        // File untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x x x");
    }

    // ── insert ────────────────────────────────────────────────────────

    #[test]
    fn insert_at_beginning() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("i.txt");
        std::fs::write(&path, "one\ntwo\n").unwrap();

        let tool = FileEditTool::default();
        let mut a = args("insert", path.to_str().unwrap());
        a.insert_line = Some(0);
        a.insert_text = Some("zero".into());

        tool.cmd_insert(&a).expect("insert should succeed");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "zero\none\ntwo\n");
    }

    // ── undo_edit ─────────────────────────────────────────────────────

    #[test]
    fn undo_after_create_deletes_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("born.txt");
        let tool = FileEditTool::default();

        let mut create = args("create", path.to_str().unwrap());
        create.file_text = Some("hello".into());
        tool.cmd_create(&create).unwrap();
        assert!(path.exists());

        let undo = args("undo_edit", path.to_str().unwrap());
        let out = tool.cmd_undo_edit(&undo).expect("undo should succeed");
        assert!(out.contains("removed"));
        assert!(!path.exists(), "undo of create must delete the file");
    }

    #[test]
    fn undo_after_str_replace_restores_original() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("doc.txt");
        std::fs::write(&path, "alpha beta gamma").unwrap();

        let tool = FileEditTool::default();
        let mut sr = args("str_replace", path.to_str().unwrap());
        sr.old_str = Some("beta".into());
        sr.new_str = Some("BETA".into());
        tool.cmd_str_replace(&sr).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha BETA gamma");

        let undo = args("undo_edit", path.to_str().unwrap());
        let out = tool.cmd_undo_edit(&undo).expect("undo should succeed");
        assert!(out.contains("restored"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha beta gamma");
    }

    #[test]
    fn undo_after_insert_restores_original() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ins.txt");
        std::fs::write(&path, "one\ntwo\n").unwrap();

        let tool = FileEditTool::default();
        let mut ins = args("insert", path.to_str().unwrap());
        ins.insert_line = Some(1);
        ins.insert_text = Some("MIDDLE".into());
        tool.cmd_insert(&ins).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "one\nMIDDLE\ntwo\n"
        );

        let undo = args("undo_edit", path.to_str().unwrap());
        tool.cmd_undo_edit(&undo).expect("undo should succeed");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
    }

    #[test]
    fn undo_with_empty_history_errors() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("never-touched.txt");
        std::fs::write(&path, "untouched").unwrap();

        let tool = FileEditTool::default();
        let undo = args("undo_edit", path.to_str().unwrap());
        let err = tool.cmd_undo_edit(&undo).unwrap_err();
        assert!(
            matches!(err, FileEditError::Validation(ref msg) if msg.contains("Nothing to undo")),
            "expected 'Nothing to undo' error, got: {err}"
        );
        // File untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "untouched");
    }

    #[test]
    fn undo_rejects_relative_path() {
        let tool = FileEditTool::default();
        let undo = args("undo_edit", "rel.txt");
        let err = tool.cmd_undo_edit(&undo).unwrap_err();
        assert!(
            matches!(err, FileEditError::Validation(ref msg) if msg.contains("not absolute")),
            "expected absolute-path error, got: {err}"
        );
    }

    #[test]
    fn undo_is_lifo_across_multiple_edits() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("stack.txt");
        std::fs::write(&path, "v0").unwrap();

        let tool = FileEditTool::default();
        let p = path.to_str().unwrap();

        // Edit 1: v0 → v1
        let mut e1 = args("str_replace", p);
        e1.old_str = Some("v0".into());
        e1.new_str = Some("v1".into());
        tool.cmd_str_replace(&e1).unwrap();
        // Edit 2: v1 → v2
        let mut e2 = args("str_replace", p);
        e2.old_str = Some("v1".into());
        e2.new_str = Some("v2".into());
        tool.cmd_str_replace(&e2).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v2");

        // First undo → back to v1
        let undo = args("undo_edit", p);
        tool.cmd_undo_edit(&undo).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v1");

        // Second undo → back to v0
        tool.cmd_undo_edit(&undo).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v0");

        // Third undo → empty stack, error.
        let err = tool.cmd_undo_edit(&undo).unwrap_err();
        assert!(matches!(err, FileEditError::Validation(_)));
    }

    // ── empty file_text guidance for the model ──────────────────────────
    //
    // When `create` is invoked with `file_text: ""` (or omitted) we
    // create an empty file and include a soft recommendation in the
    // output suggesting the model provide content upfront.

    #[test]
    fn create_with_empty_file_text_creates_empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        let tool = FileEditTool::default();

        let mut a = args("create", path.to_str().unwrap());
        a.file_text = Some(String::new()); // empty, not missing

        let out = tool
            .cmd_create(&a)
            .expect("create should succeed with empty file_text");
        assert!(
            out.contains("File created successfully"),
            "should report success, got: {out}"
        );
        assert!(
            out.contains("💡 Tip"),
            "should include recommendation tip, got: {out}"
        );
        assert!(path.exists(), "empty file should be created");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn create_with_content_no_tip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("content.txt");
        let tool = FileEditTool::default();

        let mut a = args("create", path.to_str().unwrap());
        a.file_text = Some("hello world\n".into());

        let out = tool.cmd_create(&a).expect("create should succeed");
        assert!(out.contains("File created successfully"));
        assert!(!out.contains("💡 Tip"), "no tip when content is provided");
    }
}
