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
\n\
If editing fails, read the file first to get exact content, then retry."
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
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Optional for 'str_replace': if true, replace all occurrences instead of just the first one. Default: false (single match only)."
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
        } else {
            if let Some((pos, _)) = content.match_indices(old_str).next() {
                MatchResult::UniqueMatch {
                    position: pos,
                    match_level: MatchLevel::Exact,
                    confidence: 1.0,
                }
            } else {
                MatchResult::NoMatch
            }
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
        } else {
            if let Some((norm_pos, _)) = norm_content.match_indices(&norm_old).next() {
                // Map normalized position back to original content
                // We need to find the corresponding line in original content
                let norm_lines: Vec<&str> = norm_content.lines().collect();
                let orig_lines: Vec<&str> = content.lines().collect();

                // Find which line the match starts on
                let match_start_line = norm_lines
                    .iter()
                    .take_while(|&&line| {
                        norm_pos >= line.len() + 1 // +1 for newline
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
            } else {
                if let Some(m) = re.find(content) {
                    MatchResult::UniqueMatch {
                        position: m.start(),
                        match_level: MatchLevel::FlexibleWhitespace,
                        confidence: 0.85,
                    }
                } else {
                    MatchResult::NoMatch
                }
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
            } else {
                if let Some(m) = re.find(content) {
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
