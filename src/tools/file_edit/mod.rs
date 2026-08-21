//! File-editing tools: `file_create`, `file_str_replace`, `file_insert`.
//!
//! Each tool is independent and stateless — they share only the helpers
//! in this module (matching, IO, snippet formatting). There is no shared
//! state, no undo stack: the previous `undo_edit` capability was
//! writer-only dead code and was removed in the split refactor.

use std::ops::Range;
use std::path::{Path, PathBuf};

mod create;
mod insert;
mod str_replace;

pub use create::FileCreateTool;
pub use insert::FileInsertTool;
pub use str_replace::FileStrReplaceTool;

pub(crate) const SNIPPET_CONTEXT_LINES: usize = 4;

/// Resolve a raw path argument against the session base directory.
///
/// Absolute paths pass through untouched; a relative path is joined onto
/// `base` (the session cwd). No `canonicalize`: plain `join`, no symlink
/// rewriting.
pub fn resolve_against(base: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    base.join(p)
}

/// Level of matching that was used to find the text
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchLevel {
    Exact,
    WhitespaceNormalized,
}

impl MatchLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            MatchLevel::Exact => "exact",
            MatchLevel::WhitespaceNormalized => "whitespace_normalized",
        }
    }
}

/// Byte ranges always index the ORIGINAL content, never the normalized
/// text — only the matchers in this module construct them, so no caller
/// can recompute an offset and get it wrong.
///
/// Invariants: every range's bounds are char boundaries of `content` with
/// `start < end <= content.len()`; `MultipleMatches.ranges` is sorted,
/// pairwise disjoint, and always has `len() >= 2` (only `from_ranges`,
/// the sole constructor, builds this enum).
#[derive(Debug, Clone)]
pub enum MatchResult {
    NoMatch,
    UniqueMatch {
        range: Range<usize>,
        level: MatchLevel,
    },
    MultipleMatches {
        ranges: Vec<Range<usize>>,
        level: MatchLevel,
    },
}

impl MatchResult {
    /// Sole constructor: arity picks the variant, so a `MultipleMatches`
    /// holding fewer than two ranges can never be built.
    fn from_ranges(ranges: Vec<Range<usize>>, level: MatchLevel) -> Self {
        match ranges.len() {
            0 => MatchResult::NoMatch,
            1 => {
                let range = ranges.into_iter().next().expect("len checked above");
                MatchResult::UniqueMatch { range, level }
            }
            _ => MatchResult::MultipleMatches { ranges, level },
        }
    }
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

/// Level 1: Exact match. Ranges are already true original offsets.
pub(crate) fn exact_match(content: &str, old_str: &str) -> MatchResult {
    let ranges = content
        .match_indices(old_str)
        .map(|(pos, _)| pos..pos + old_str.len())
        .collect();
    MatchResult::from_ranges(ranges, MatchLevel::Exact)
}

/// One source line's trimmed content, located in both texts. Normalization
/// only strips a trailing run, so within a line the two offsets differ by
/// a constant — that is what makes the inverse mapping exact.
struct LineSpan {
    norm_start: usize,
    orig_start: usize,
    len: usize,
}

/// Normalized text plus the spans that map it back to the original.
struct Normalized {
    text: String,
    spans: Vec<LineSpan>,
}

impl Normalized {
    /// Trailing whitespace (incl. the line terminator, CRLF or LF) is the
    /// only thing dropped; leading indentation is preserved verbatim. Each
    /// span's `orig_start` is recorded from the true original offset of its
    /// `split_inclusive('\n')` chunk — never derived from a separator-width
    /// assumption, which is what makes this correct for CRLF too.
    fn of(content: &str) -> Self {
        let mut text = String::with_capacity(content.len());
        let mut spans = Vec::new();
        let mut orig_start = 0usize;
        let mut first = true;

        for chunk in content.split_inclusive('\n') {
            let trimmed = chunk.trim_end();
            if !first {
                text.push('\n');
            }
            first = false;
            let norm_start = text.len();
            text.push_str(trimmed);
            spans.push(LineSpan {
                norm_start,
                orig_start,
                len: trimmed.len(),
            });
            orig_start += chunk.len();
        }

        Normalized { text, spans }
    }

    /// Map a byte offset in `self.text` back into the original content.
    /// Precondition: `offset <= self.text.len()` and `self.spans` is
    /// non-empty — both guaranteed because callers only pass offsets of
    /// real matches found in `self.text`.
    fn to_original(&self, offset: usize) -> usize {
        let idx = self.spans.partition_point(|s| s.norm_start <= offset) - 1;
        let span = &self.spans[idx];
        let mapped = span.orig_start + (offset - span.norm_start);
        debug_assert!(mapped <= span.orig_start + span.len);
        mapped
    }
}

/// Level 2: Whitespace-normalized match. Both ends of a range are mapped
/// through `to_original` in the same expression, so a half-mapped range
/// (start original, end still normalized) can never be constructed.
pub(crate) fn whitespace_normalized_match(content: &str, old_str: &str) -> MatchResult {
    let norm = Normalized::of(content);
    let needle = Normalized::of(old_str).text;
    // A needle with no non-whitespace content normalizes to a string of
    // pure join separators (e.g. a 2-line all-whitespace needle becomes
    // "\n", not ""), which would still match constantly. Guard on the
    // trimmed needle, not the raw one, to catch the multi-line case too.
    if needle.trim().is_empty() {
        return MatchResult::NoMatch;
    }
    let ranges = norm
        .text
        .match_indices(&needle)
        .map(|(p, m)| norm.to_original(p)..norm.to_original(p + m.len()))
        .collect();
    MatchResult::from_ranges(ranges, MatchLevel::WhitespaceNormalized)
}

/// Progressive fallback matching: fuzzy (level 2) is a fallback for NOT
/// FOUND only, never for AMBIGUOUS — an ambiguous exact match must report
/// the true literal count, not a fuzzy superset.
pub(crate) fn progressive_match(content: &str, old_str: &str) -> MatchResult {
    let exact = exact_match(content, old_str);
    if !matches!(exact, MatchResult::NoMatch) {
        return exact;
    }
    whitespace_normalized_match(content, old_str)
}

/// Test-only fixtures shared between the black-box (`mod tests`) and the
/// unit (`range_mapping_tests`) suites — kept here, once, so the two
/// groups can't drift apart on the numbers they assert.
#[cfg(test)]
fn production_fixture_no_trailing_newline() -> (String, String) {
    // §7 case 1 — reproduces the incident's exact shape: 11 lines, 1638
    // bytes, no trailing newline. 10 filler lines of 160 bytes + a 28-byte
    // last line, joined by 10 '\n's: 10*160 + 10 + 28 = 1638.
    let filler = "x".repeat(160);
    let last_line = "        return final_result;".to_string(); // 8 spaces + 20 chars = 28
    assert_eq!(last_line.len(), 28);

    let mut lines: Vec<String> = std::iter::repeat_n(filler, 10).collect();
    lines.push(last_line.clone());
    let content = lines.join("\n");

    // old_str ends in `\n`, which does not exist in the file (no trailing
    // newline) — this is precisely what forces the exact level to miss
    // and the whitespace-normalized level to be reached.
    let old_str = format!("{last_line}\n");
    (content, old_str)
}

#[cfg(test)]
fn ambiguous_fuzzy_fixture() -> (String, String) {
    // §7 case 13 — "TARGET_LINE" appears twice, but only after
    // whitespace normalization (trailing spaces on both hit lines, plus
    // leading trailing-whitespace on line 1 that shifts every later
    // normalized offset away from its true original offset).
    let content = "aaa   \nbbb\nTARGET_LINE   \nccc\nddd\nTARGET_LINE   ".to_string();
    let old_str = "TARGET_LINE\n".to_string();
    (content, old_str)
}

#[cfg(test)]
fn exact_ambiguous_with_fuzzy_superset_fixture() -> (String, String) {
    // §7 case 20 — "TARGET\n" matches exactly on lines 1 and 3, and would
    // *additionally* whitespace-normalize-match line 5's "TARGET   \n".
    // The literal (exact) count is 2; the fuzzy count is 3.
    let content = "TARGET\nfiller\nTARGET\nfiller\nTARGET   \n".to_string();
    let old_str = "TARGET\n".to_string();
    (content, old_str)
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
            resolve_against(base, "/etc/hosts"),
            PathBuf::from("/etc/hosts")
        );
    }

    #[test]
    fn resolve_relative_joins_base() {
        let base = Path::new("/session/dir");
        assert_eq!(
            resolve_against(base, "sub/file.txt"),
            PathBuf::from("/session/dir/sub/file.txt")
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

    // ── matching (black-box regression — PR A, file-edit-offset-fix.md §7) ──
    //
    // Drives `str_replace::run` end to end, exactly like the tests above.
    // §7 case 1 (the production repro) is expected to FAIL RED today with
    // the out-of-bounds panic from mod.rs:167-179 / str_replace.rs:192.
    // Cases 12 and 20 are the design doc's "add at this level too" set
    // (task A1 in the ticket's §8 table). Case 13 is deliberately NOT
    // repeated here — see the GAPS note in the handover report: it is
    // masked at the black-box level by a second, unrelated fall-through
    // defect and is only observable by calling `whitespace_normalized_match`
    // directly (covered in `range_mapping_tests` below).

    #[test]
    fn str_replace_last_line_no_trailing_newline_does_not_panic() {
        // §7 case 1 — THE production incident (pid 1523, 87-minute hang):
        // 11 lines, 1638 bytes, no trailing newline, old_str targets the
        // last line and ends in `\n` so the exact level misses and the
        // whitespace-normalized (level 2) path — where Bug A lives — is
        // reached. today's `take_while` bug computes orig_pos =
        // content.len() + 1 = 1639, which is then sliced at
        // str_replace.rs:192 and panics with "byte index 1639 is out of
        // bounds". This test MUST NOT panic once PR A lands.
        let (content, old_str) = production_fixture_no_trailing_newline();
        assert_eq!(
            content.len(),
            1638,
            "fixture drifted from the incident shape"
        );
        assert_eq!(
            content.lines().count(),
            11,
            "fixture drifted from the incident shape"
        );
        assert!(
            !content.ends_with('\n'),
            "fixture must have no trailing newline"
        );

        let dir = tempdir().unwrap();
        let path = dir.path().join("incident.rs");
        std::fs::write(&path, &content).unwrap();

        // No `catch_unwind`: a panic here is a genuine test failure (RED),
        // which is exactly what today's code produces.
        let result = str_replace::run(path.to_str().unwrap(), &old_str, Some("REPLACED"), false);
        assert!(
            result.is_ok(),
            "expected a successful edit, got: {:?}",
            result.err()
        );
        // Bug B (str_replace.rs's literal `replacen`) is still present by
        // design in this PR, so the fuzzy match here is a silent no-op on
        // disk. Asserting the write actually landed is PR B's job (§7
        // case 14); this test only pins "does not panic and returns Ok".
    }

    #[test]
    fn str_replace_ambiguous_exact_match_reports_correct_line_numbers() {
        // §7 case 12 — ambiguity via plain exact matches, no whitespace
        // divergence anywhere. NOT a pin of correct behaviour: today this
        // is ALSO broken, for a distinct reason discovered while building
        // this fixture — `progressive_match`'s final expression is
        // unconditionally `flexible_whitespace_match(...)`, so an
        // ambiguous (non-Unique) level-1/2 result is always discarded in
        // favour of level 3. Because `old_str` has no '\n' here, level 3
        // takes its single-line branch, whose pattern is `^\s*…\s*$`
        // anchored to the WHOLE file (§2.4 of the design doc) — it can
        // never match a multi-line file, so the final answer today is a
        // false "String not found", not an ambiguity error at all.
        let dir = tempdir().unwrap();
        let path = dir.path().join("ambiguous.txt");
        std::fs::write(&path, "TARGET\nfiller\nTARGET\nfiller\nTARGET\n").unwrap();

        let err = str_replace::run(path.to_str().unwrap(), "TARGET", Some("X"), false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("appears 3 times"),
            "expected literal count of 3, got: {msg}"
        );
        assert!(
            msg.contains("[1, 3, 5]"),
            "expected the true original line numbers [1, 3, 5], got: {msg}"
        );
    }

    #[test]
    fn str_replace_ambiguous_exact_match_does_not_fall_through_to_fuzzy() {
        // §7 case 20 — pins the §5.3 rule: fuzzy matching is a fallback
        // for NOT FOUND only, never for AMBIGUOUS. Today's
        // `progressive_match` only short-circuits on `UniqueMatch`, so an
        // ambiguous *exact* result (2 literal hits) falls through to
        // `whitespace_normalized_match`, which finds a 3rd, fuzzy-only
        // occurrence and reports a count that does not correspond to the
        // literal string the model asked to replace.
        let (content, old_str) = exact_ambiguous_with_fuzzy_superset_fixture();
        let dir = tempdir().unwrap();
        let path = dir.path().join("no_fallthrough.txt");
        std::fs::write(&path, &content).unwrap();

        let err = str_replace::run(path.to_str().unwrap(), &old_str, Some("X"), false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("appears 2 times"),
            "ambiguous exact match must report the literal count (2), not a fuzzy superset, got: {msg}"
        );
        assert!(
            msg.contains("[1, 3]"),
            "expected the two literal-match lines [1, 3], got: {msg}"
        );
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

/// Unit tests for the `Range`-based matcher API from
/// file-edit-offset-fix.md §5.1-§5.3 (`MatchResult::UniqueMatch { range,
/// level }`, `MatchResult::from_ranges`, `Normalized`, `LineSpan`).
#[cfg(test)]
mod range_mapping_tests {
    use super::*;

    // ── MatchResult::from_ranges — sole constructor (I1-I4) ─────────

    #[test]
    fn from_ranges_empty_vec_is_no_match() {
        let r = MatchResult::from_ranges(vec![], MatchLevel::Exact);
        assert!(matches!(r, MatchResult::NoMatch));
    }

    #[test]
    // A `Vec<Range<usize>>` of length 1 is exactly what `from_ranges`
    // expects to turn into a `UniqueMatch` — not a `Vec<usize>` typo.
    #[allow(clippy::single_range_in_vec_init)]
    fn from_ranges_single_range_is_unique_match() {
        let r = MatchResult::from_ranges(vec![3..5], MatchLevel::Exact);
        match r {
            MatchResult::UniqueMatch { range, level } => {
                assert_eq!(range, 3..5);
                assert_eq!(level, MatchLevel::Exact);
            }
            other => panic!("expected UniqueMatch, got {other:?}"),
        }
    }

    #[test]
    fn from_ranges_many_ranges_is_multiple_matches() {
        // Also pins I3: a `MultipleMatches` can never be built with < 2.
        let r = MatchResult::from_ranges(vec![1..2, 5..6], MatchLevel::WhitespaceNormalized);
        match r {
            MatchResult::MultipleMatches { ranges, level } => {
                assert_eq!(ranges, vec![1..2, 5..6]);
                assert_eq!(level, MatchLevel::WhitespaceNormalized);
            }
            other => panic!("expected MultipleMatches, got {other:?}"),
        }
    }

    // ── MatchLevel — level 3 deleted ────────────────────────────────

    #[test]
    fn match_level_has_exactly_two_variants() {
        // A non-exhaustive match (no wildcard arm) fails to COMPILE if a
        // third variant (e.g. a resurrected `FlexibleWhitespace`) is added
        // back — this is the test's real assertion, the runtime checks
        // below are secondary.
        fn as_str_via_exhaustive_match(level: MatchLevel) -> &'static str {
            match level {
                MatchLevel::Exact => "exact",
                MatchLevel::WhitespaceNormalized => "whitespace_normalized",
            }
        }
        assert_eq!(
            as_str_via_exhaustive_match(MatchLevel::Exact),
            MatchLevel::Exact.as_str()
        );
        assert_eq!(
            as_str_via_exhaustive_match(MatchLevel::WhitespaceNormalized),
            MatchLevel::WhitespaceNormalized.as_str()
        );
    }

    // ── Normalized::of / to_original — §7 case 6 (empty file) ───────

    #[test]
    fn normalized_of_empty_content_has_no_spans() {
        let n = Normalized::of("");
        assert_eq!(n.text, "");
        assert!(
            n.spans.is_empty(),
            "an empty file must not produce a phantom span"
        );
    }

    // ── whitespace_normalized_match — §7 cases 1-13, 20 ──────────────

    #[test]
    fn maps_last_line_no_trailing_newline_to_content_end() {
        // §7 case 1 — the production case, at the mapping-unit level: the
        // range's end must land exactly on content.len(), never past it.
        let (content, old_str) = production_fixture_no_trailing_newline();
        let result = whitespace_normalized_match(&content, &old_str);
        match result {
            MatchResult::UniqueMatch { range, level } => {
                assert_eq!(level, MatchLevel::WhitespaceNormalized);
                assert_eq!(range.end, content.len());
                assert_eq!(&content[range], "        return final_result;");
            }
            other => panic!("expected UniqueMatch, got {other:?}"),
        }
    }

    #[test]
    fn maps_last_line_with_trailing_newline_excludes_the_newline() {
        // §7 case 2 — same shape, but the file has a trailing '\n'; the
        // range must stop at the trimmed line's end, not swallow the '\n'.
        let content = "alpha\nbeta\nreturn final_result;  \n".to_string();
        let old_str = "return final_result;\n".to_string();
        let result = whitespace_normalized_match(&content, &old_str);
        match result {
            MatchResult::UniqueMatch { range, level } => {
                assert_eq!(level, MatchLevel::WhitespaceNormalized);
                assert_eq!(range, 11..31);
                assert_eq!(&content[range.clone()], "return final_result;");
                assert!(
                    range.end < content.len(),
                    "must exclude the trailing spaces and '\\n'"
                );
            }
            other => panic!("expected UniqueMatch, got {other:?}"),
        }
    }

    #[test]
    fn maps_match_on_first_line_to_start_zero() {
        // §7 case 3.
        let content = "TARGET  \nsecond\nthird".to_string();
        let old_str = "TARGET\n".to_string();
        let result = whitespace_normalized_match(&content, &old_str);
        match result {
            MatchResult::UniqueMatch { range, .. } => assert_eq!(range.start, 0),
            other => panic!("expected UniqueMatch, got {other:?}"),
        }
    }

    #[test]
    fn maps_whole_file_match_to_full_span() {
        // §7 case 4.
        let content = "one\ntwo  \nthree".to_string();
        let old_str = "one\ntwo\nthree".to_string();
        let result = whitespace_normalized_match(&content, &old_str);
        match result {
            MatchResult::UniqueMatch { range, .. } => assert_eq!(range, 0..content.len()),
            other => panic!("expected UniqueMatch, got {other:?}"),
        }
    }

    #[test]
    fn single_line_file_is_resolved_at_exact_level_not_level_two() {
        // §7 case 5 — progressive_match must short-circuit on the exact
        // hit; level 2 is never even reached for a plain literal match.
        let content = "just one line here".to_string();
        let result = progressive_match(&content, "one line");
        match result {
            MatchResult::UniqueMatch { range, level } => {
                assert_eq!(level, MatchLevel::Exact);
                assert_eq!(range, 5..13);
            }
            other => panic!("expected an exact UniqueMatch, got {other:?}"),
        }
    }

    #[test]
    fn empty_file_is_no_match_and_does_not_panic() {
        // §7 case 6 — no underflow in `to_original` (spans empty).
        let result = whitespace_normalized_match("", "needle");
        assert!(matches!(result, MatchResult::NoMatch));
    }

    #[test]
    fn file_of_only_newlines_is_no_match_and_does_not_panic() {
        // §7 case 7.
        let result = whitespace_normalized_match("\n\n\n", "needle");
        assert!(matches!(result, MatchResult::NoMatch));
    }

    #[test]
    fn whitespace_only_needle_is_no_match() {
        // §7 case 8 — the `needle.is_empty()` guard: an all-whitespace
        // needle normalizes to "" and must not match every position.
        let result = whitespace_normalized_match("some content\nhere", "   \n  ");
        assert!(matches!(result, MatchResult::NoMatch));
    }

    #[test]
    fn crlf_separators_are_spanned_correctly() {
        // §7 case 9 — LOAD-BEARING: a naive cumulative "+1 per newline"
        // walk (the tempting "fix" rejected in §4.3) is still wrong here,
        // because a CRLF separator is 2 bytes, not 1. Only a mapping that
        // records true original byte offsets (never assumes a separator
        // width) gets this right.
        let content = "aaa\r\nbbb\r\nTARGET\r\n".to_string();
        let old_str = "TARGET\n".to_string();
        let result = whitespace_normalized_match(&content, &old_str);
        match result {
            MatchResult::UniqueMatch { range, .. } => {
                assert_eq!(range, 10..16);
                assert_eq!(&content[range.clone()], "TARGET");
                assert!(
                    range.end < content.len(),
                    "must exclude the trailing \\r\\n, not just \\n"
                );
            }
            other => panic!("expected UniqueMatch, got {other:?}"),
        }
    }

    #[test]
    fn multibyte_utf8_before_match_yields_char_boundary_range() {
        // §7 case 10 — the regression test for the latent UTF-8 hazard:
        // an earlier line has a multi-byte char AND trailing whitespace
        // that gets stripped, so the normalized and original offsets of
        // the match diverge across a codepoint boundary. An impossible
        // mid-codepoint offset must never be constructible.
        let content = "h\u{e9}llo   \nTARGET".to_string(); // "héllo   \nTARGET"
        let old_str = "TARGET\n".to_string();
        let result = whitespace_normalized_match(&content, &old_str);
        match result {
            MatchResult::UniqueMatch { range, .. } => {
                assert!(content.is_char_boundary(range.start));
                assert!(content.is_char_boundary(range.end));
                assert_eq!(range, 10..16);
                assert_eq!(&content[range], "TARGET");
            }
            other => panic!("expected UniqueMatch, got {other:?}"),
        }
    }

    #[test]
    fn range_reflects_true_original_bytes_when_orig_and_norm_lengths_differ() {
        // §7 case 11 — THE test that proves the range is correct rather
        // than accidentally correct: the match spans two physical lines,
        // the first of which has trailing whitespace inside the matched
        // span. Do not drop this one (ticket's own words).
        let content = "prefix\nfoo   \nline_b\nsuffix".to_string();
        let old_str = "foo\nline_b".to_string(); // no trailing ws on this needle's first line
        let result = whitespace_normalized_match(&content, &old_str);
        match result {
            MatchResult::UniqueMatch { range, .. } => {
                // Original span is 13 bytes ("foo   \nline_b"); the
                // normalized needle that found it is only 10 ("foo\nline_b").
                assert_ne!(range.end - range.start, old_str.len());
                assert_eq!(range, 7..20);
                assert_eq!(
                    &content[range], "foo   \nline_b",
                    "trailing whitespace INSIDE the matched span must survive verbatim"
                );
            }
            other => panic!("expected UniqueMatch, got {other:?}"),
        }
    }

    #[test]
    fn ambiguous_fuzzy_match_reports_all_original_ranges() {
        // §7 case 12/13 at the mapping-unit level — ranges must be
        // original offsets, not the raw normalized offsets today's code
        // stores in `MultipleMatches.positions`.
        let (content, old_str) = ambiguous_fuzzy_fixture();
        let result = whitespace_normalized_match(&content, &old_str);
        match result {
            MatchResult::MultipleMatches { ranges, level } => {
                assert_eq!(level, MatchLevel::WhitespaceNormalized);
                assert_eq!(ranges, vec![11..22, 34..45]);
                for r in &ranges {
                    assert_eq!(&content[r.clone()], "TARGET_LINE");
                }
            }
            other => panic!("expected MultipleMatches, got {other:?}"),
        }
    }

    #[test]
    fn progressive_match_does_not_fall_through_on_ambiguous_exact_match() {
        // §7 case 20 — the §5.3 rule change, pinned directly against
        // `progressive_match` rather than through the tool's error text.
        let (content, old_str) = exact_ambiguous_with_fuzzy_superset_fixture();
        let result = progressive_match(&content, &old_str);
        match result {
            MatchResult::MultipleMatches { ranges, level } => {
                assert_eq!(level, MatchLevel::Exact);
                assert_eq!(
                    ranges.len(),
                    2,
                    "must report the literal count, not the fuzzy superset of 3"
                );
            }
            other => panic!("expected an exact MultipleMatches, got {other:?}"),
        }
    }
}
