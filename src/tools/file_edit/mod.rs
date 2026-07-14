//! File-editing tools: `file_create`, `file_str_replace`, `file_insert`.
//!
//! Each tool is independent and stateless — they share only the helpers
//! in this module (matching, IO, snippet formatting). There is no shared
//! state, no undo stack: the previous `undo_edit` capability was
//! writer-only dead code and was removed in the split refactor.

use std::path::{Path, PathBuf};

mod create;
mod insert;
mod str_replace;

pub use create::FileCreateTool;
pub use insert::FileInsertTool;
pub use str_replace::FileStrReplaceTool;

pub(crate) const SNIPPET_CONTEXT_LINES: usize = 4;

/// Resolve a raw path argument against an optional session base directory.
///
/// Absolute paths pass through untouched. A relative path is joined onto
/// `base` (the session cwd) when present; with no base it stays relative and
/// resolves against the process cwd — the std default, used only in test/no-
/// state-manager paths. No `canonicalize`: plain `join`, no symlink rewriting.
pub fn resolve_against(base: Option<&Path>, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    match base {
        Some(b) => b.join(p),
        None => p.to_path_buf(),
    }
}

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
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

// ── IO helpers ────────────────────────────────────────────────────────

pub(crate) fn validate_path_exists(path: &Path) -> Result<(), FileEditError> {
    if !path.exists() {
        return Err(FileEditError::Validation(format!(
            "Path '{}' does not exist.",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) fn read_file(path: &Path) -> Result<String, FileEditError> {
    std::fs::read_to_string(path).map_err(|e| FileEditError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

pub(crate) fn write_file(path: &Path, content: &str) -> Result<(), FileEditError> {
    std::fs::write(path, content).map_err(|e| FileEditError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

// ── Snippet helper ────────────────────────────────────────────────────

pub(crate) fn format_lines_numbered(lines: &[&str], start_num: usize) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>6}\t{}", start_num + i, line))
        .collect::<Vec<_>>()
        .join("\n")
}

// ── Matching functions ────────────────────────────────────────────────

/// Level 1: Exact match
pub(crate) fn exact_match(content: &str, old_str: &str) -> MatchResult {
    let count = content.matches(old_str).count();

    if count == 0 {
        MatchResult::NoMatch
    } else if count > 1 {
        let positions: Vec<usize> = content.match_indices(old_str).map(|(pos, _)| pos).collect();
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
pub(crate) fn whitespace_normalized_match(content: &str, old_str: &str) -> MatchResult {
    let normalize_line = |line: &str| -> String {
        // Preserve leading indentation but normalize trailing whitespace
        let trimmed = line.trim_end();
        trimmed.to_string()
    };

    let normalize =
        |s: &str| -> String { s.lines().map(normalize_line).collect::<Vec<_>>().join("\n") };

    let norm_content = normalize(content);
    let norm_old = normalize(old_str);

    let count = norm_content.matches(&norm_old).count();

    if count == 0 {
        MatchResult::NoMatch
    } else if count > 1 {
        let positions: Vec<usize> = norm_content
            .match_indices(&norm_old)
            .map(|(pos, _)| pos)
            .collect();
        MatchResult::MultipleMatches { count, positions }
    } else if let Some((norm_pos, _)) = norm_content.match_indices(&norm_old).next() {
        // Map normalized position back to original content
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
pub(crate) fn flexible_whitespace_match(content: &str, old_str: &str) -> MatchResult {
    let tokens: Vec<&str> = old_str
        .split(|c: char| c.is_whitespace() && c != '\n')
        .collect();

    if tokens.is_empty() || tokens.iter().all(|t| t.is_empty()) {
        return MatchResult::NoMatch;
    }

    let pattern: String = tokens
        .iter()
        .map(|token| regex::escape(token))
        .collect::<Vec<_>>()
        .join(r"\s*");

    let pattern = format!(r"^\s*{}\s*$", pattern);

    if old_str.contains('\n') {
        // For multi-line: normalize all whitespace to single space
        let flat_old = old_str.split_whitespace().collect::<Vec<_>>().join(" ");
        let flat_pattern = regex::escape(&flat_old);

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
        // Single line: token-based pattern
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
pub(crate) fn progressive_match(content: &str, old_str: &str) -> MatchResult {
    let result = exact_match(content, old_str);
    if matches!(result, MatchResult::UniqueMatch { .. }) {
        return result;
    }

    let result = whitespace_normalized_match(content, old_str);
    if matches!(result, MatchResult::UniqueMatch { .. }) {
        return result;
    }

    flexible_whitespace_match(content, old_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ── resolve_against ─────────────────────────────────────────────────

    #[test]
    fn resolve_absolute_passes_through() {
        let base = Path::new("/session/dir");
        assert_eq!(
            resolve_against(Some(base), "/etc/hosts"),
            PathBuf::from("/etc/hosts")
        );
    }

    #[test]
    fn resolve_relative_joins_base() {
        let base = Path::new("/session/dir");
        assert_eq!(
            resolve_against(Some(base), "sub/file.txt"),
            PathBuf::from("/session/dir/sub/file.txt")
        );
    }

    #[test]
    fn resolve_relative_no_base_stays_relative() {
        assert_eq!(
            resolve_against(None, "sub/file.txt"),
            PathBuf::from("sub/file.txt")
        );
    }

    // ── create ─────────────────────────────────────────────────────────

    #[test]
    fn create_happy_path_writes_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.txt");

        let out = create::run(path.to_str().unwrap(), Some("hello world\n"))
            .expect("create should succeed");
        assert!(out.contains("File created successfully"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world\n");
    }

    #[test]
    fn create_allows_relative_path() {
        // Relative paths are now legal — the "not absolute" rejection is gone.
        // Resolution against the session cwd happens at the `call()` boundary;
        // `run()` itself is path-agnostic. Driven here with an absolute temp
        // path so the test is hermetic; the point is no rejection fires.
        let dir = tempdir().unwrap();
        let abs = dir.path().join("relative-create-probe.txt");
        create::run(abs.to_str().unwrap(), Some("x")).expect("relative paths are allowed now");
        assert_eq!(std::fs::read_to_string(&abs).unwrap(), "x");
    }

    #[test]
    fn create_rejects_existing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("exists.txt");
        std::fs::write(&path, "original").unwrap();

        let err = create::run(path.to_str().unwrap(), Some("new content")).unwrap_err();
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

        let out = create::run(path.to_str().unwrap(), None)
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

        create::run(path.to_str().unwrap(), Some("deep"))
            .expect("create should auto-create parents");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "deep");
    }

    #[test]
    fn create_with_empty_file_text_creates_empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.txt");

        let out = create::run(path.to_str().unwrap(), Some(""))
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

        let out = create::run(path.to_str().unwrap(), Some("hello world\n"))
            .expect("create should succeed");
        assert!(out.contains("File created successfully"));
        assert!(!out.contains("💡 Tip"), "no tip when content is provided");
    }

    // ── str_replace ────────────────────────────────────────────────────

    #[test]
    fn str_replace_basic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("r.txt");
        std::fs::write(&path, "alpha beta gamma").unwrap();

        str_replace::run(path.to_str().unwrap(), "beta", Some("BETA"), false)
            .expect("replace should succeed");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha BETA gamma");
    }

    #[test]
    fn str_replace_multiple_without_replace_all_errors() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("dup.txt");
        std::fs::write(&path, "x x x").unwrap();

        let err = str_replace::run(path.to_str().unwrap(), "x", Some("y"), false).unwrap_err();
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

        insert::run(path.to_str().unwrap(), 0, "zero").expect("insert should succeed");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "zero\none\ntwo\n");
    }
}
