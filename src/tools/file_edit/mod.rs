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
    use rig_core::tool::Tool;
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

        // The match covers the whole last line ("        return final_result;",
        // 28 bytes, indentation included per I5); the splice must actually
        // land on disk, not silently no-op behind a reported success.
        let expected = format!("{}REPLACED", &content[..content.len() - 28]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            expected,
            "Bug B: fuzzy match reported success but did not rewrite the file"
        );
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

    // ── str_replace: splice by range (PR B, file-edit-offset-fix.md §7 cases 14-19) ──
    //
    // Every test in this section reads the file back off disk after the
    // call. A test that only checks the returned success string is exactly
    // the test that let Bug B ship: `str_replace.rs`'s literal
    // `content.replacen(old_str, new_str, 1)` searches for `old_str`
    // verbatim even when the match came from the whitespace-normalized
    // matcher, so it matches nothing, and the file is rewritten
    // byte-identical while the tool reports "Replaced 1 occurrence".

    #[test]
    fn str_replace_normalized_match_writes_new_bytes_to_disk() {
        // §7 case 14 — THE load-bearing proof of Bug B. "TARGET\n" is not
        // literally present (the real line is "TARGET   \n"), so this only
        // matches via whitespace normalization.
        let original = "alpha\nTARGET   \nbeta\n";
        let dir = tempdir().unwrap();
        let path = dir.path().join("case14.txt");
        std::fs::write(&path, original).unwrap();

        let result = str_replace::run(path.to_str().unwrap(), "TARGET\n", Some("REPLACED"), false);
        let msg = result.expect("expected a successful edit");
        assert!(msg.contains("Replaced 1 occurrence"), "got: {msg}");

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_ne!(
            on_disk, original,
            "Bug B: file reported as edited but bytes on disk are unchanged"
        );
        assert!(
            !on_disk.contains("TARGET"),
            "old_str must be gone: {on_disk:?}"
        );
        assert!(
            on_disk.contains("REPLACED"),
            "new_str must be present: {on_disk:?}"
        );
        assert_eq!(on_disk, "alpha\nREPLACED   \nbeta\n");
    }

    #[test]
    fn str_replace_normalized_match_with_differing_orig_norm_lengths_splices_correctly() {
        // §7 case 15 — Bug B + case 11 combined: the matched span is 13
        // original bytes ("foo   \nline_b") but only 10 normalized bytes
        // ("foo\nline_b"). A literal replacen for the 10-byte needle either
        // misses entirely or would (if it coincidentally matched something
        // else) hit the wrong bytes; splicing by the resolved range does
        // not shift regardless of the length difference.
        let original = "prefix\nfoo   \nline_b\nsuffix";
        let dir = tempdir().unwrap();
        let path = dir.path().join("case15.txt");
        std::fs::write(&path, original).unwrap();

        let result = str_replace::run(
            path.to_str().unwrap(),
            "foo\nline_b",
            Some("bar\nqux"),
            false,
        );
        assert!(result.is_ok(), "expected success, got: {:?}", result.err());

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "prefix\nbar\nqux\nsuffix"
        );
    }

    #[test]
    fn str_replace_normalized_match_replace_all_splices_every_range() {
        // §7 case 16 — replace_all across three whitespace-normalized
        // matches, each with a DIFFERENT amount of trailing whitespace, so
        // a shared/incorrect offset could not accidentally pass. Every
        // range must be spliced and the reported count must equal the
        // number of ranges actually spliced.
        let original = "TARGET  \nfiller\nTARGET \nfiller\nTARGET   \n";
        let dir = tempdir().unwrap();
        let path = dir.path().join("case16.txt");
        std::fs::write(&path, original).unwrap();

        let result = str_replace::run(path.to_str().unwrap(), "TARGET\n", Some("X"), true);
        let msg = result.expect("expected a successful edit");
        assert!(msg.contains("Replaced all 3 occurrences"), "got: {msg}");

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "X  \nfiller\nX \nfiller\nX   \n"
        );
    }

    #[test]
    fn str_replace_normalized_match_deletion_removes_matched_range_only() {
        // §7 case 17 — `new_str` omitted (deletion) through a fuzzy match:
        // only the matched "TARGET" bytes vanish; the trailing spaces and
        // newline the matcher did not claim survive verbatim (invariant I5).
        let original = "alpha\nTARGET   \nbeta\n";
        let dir = tempdir().unwrap();
        let path = dir.path().join("case17.txt");
        std::fs::write(&path, original).unwrap();

        let result = str_replace::run(path.to_str().unwrap(), "TARGET\n", None, false);
        assert!(result.is_ok(), "expected success, got: {:?}", result.err());

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha\n   \nbeta\n"
        );
    }

    #[test]
    fn str_replace_exact_match_deleting_large_block_near_end_does_not_panic() {
        // §7 case 18 — Bug C: the snippet window computes `start` from a
        // line number in the OLD content but clamps `end` to the NEW
        // content's length. Deleting the last 10 of 200 lines and replacing
        // them with much shorter text must not panic, and the resulting
        // bytes must be exactly right.
        let lines: Vec<String> = (0..200).map(|i| format!("line{i:04}")).collect();
        let original = lines.join("\n") + "\n";
        let dir = tempdir().unwrap();
        let path = dir.path().join("case18.txt");
        std::fs::write(&path, &original).unwrap();

        let old_str = lines[190..200].join("\n") + "\n";
        assert!(
            original.contains(&old_str),
            "fixture must contain the exact block verbatim"
        );

        let result = str_replace::run(path.to_str().unwrap(), &old_str, Some("SHORT"), false);
        assert!(result.is_ok(), "must not panic, got: {:?}", result.err());

        let expected = format!("{}\nSHORT", lines[..190].join("\n"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    }

    #[test]
    fn str_replace_empty_old_str_is_rejected_and_leaves_file_untouched() {
        // §7 case 19 — an empty `old_str` can only mean "insert at every
        // byte offset", which is never a sensible edit and is catastrophic
        // under `replace_all` (today: `String::replace("", new_str)`
        // inserts `new_str` at every position). Must be rejected at the
        // boundary before any write.
        let original = "abc";
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty_needle.txt");
        std::fs::write(&path, original).unwrap();

        let err = str_replace::run(path.to_str().unwrap(), "", Some("X"), true).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.to_lowercase().contains("empty"),
            "expected a message naming the empty old_str, got: {msg}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "file must be untouched by a rejected edit"
        );
    }

    #[test]
    fn str_replace_exact_match_replace_all_unchanged_behaviour() {
        // Regression guard — literal multi-occurrence replace_all must
        // behave byte-for-byte the same after the splice rewrite. Already
        // correct today (Bug B only affects the fuzzy path); pin it so the
        // splice/replace_all collapse (B2) cannot regress the common case.
        let dir = tempdir().unwrap();
        let path = dir.path().join("case_exact_pin.txt");
        std::fs::write(&path, "x x x").unwrap();

        let msg = str_replace::run(path.to_str().unwrap(), "x", Some("y"), true)
            .expect("exact replace_all should succeed");
        assert!(msg.contains("Replaced all 3 occurrences"), "got: {msg}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "y y y");
    }

    #[test]
    fn str_replace_normalized_match_in_crlf_file_preserves_untouched_line_endings() {
        // §7 case 9/B — a fuzzy match inside a CRLF file must splice by
        // byte range (2-byte separators) without corrupting the CRLF
        // endings on lines outside the replaced range.
        let original = "aaa\r\nbbb\r\nTARGET\r\n";
        let dir = tempdir().unwrap();
        let path = dir.path().join("crlf.txt");
        std::fs::write(&path, original).unwrap();

        let result = str_replace::run(path.to_str().unwrap(), "TARGET\n", Some("REPLACED"), false);
        assert!(result.is_ok(), "expected success, got: {:?}", result.err());

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "aaa\r\nbbb\r\nREPLACED\r\n"
        );
    }

    #[test]
    fn str_replace_normalized_match_near_multibyte_utf8_produces_valid_utf8() {
        // §7 case 10/B — the match sits right after a multi-byte codepoint
        // and trailing whitespace that get stripped by normalization;
        // splicing at the mapped byte range must not land mid-codepoint on
        // either side. `read_to_string` itself would fail on invalid UTF-8.
        let original = "h\u{e9}llo   \nTARGET";
        let dir = tempdir().unwrap();
        let path = dir.path().join("utf8.txt");
        std::fs::write(&path, original).unwrap();

        let result = str_replace::run(path.to_str().unwrap(), "TARGET\n", Some("日本語"), false);
        assert!(result.is_ok(), "expected success, got: {:?}", result.err());

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "h\u{e9}llo   \n日本語");
    }

    #[test]
    fn str_replace_ambiguous_fuzzy_match_writes_nothing() {
        // Regression guard — an ambiguous match, fuzzy or exact, must
        // error and never reach the splice; the file on disk must stay
        // byte-identical. `splice` handles N ranges, so it must not be
        // reachable on this path.
        let (content, old_str) = ambiguous_fuzzy_fixture();
        let dir = tempdir().unwrap();
        let path = dir.path().join("ambiguous_fuzzy.txt");
        std::fs::write(&path, &content).unwrap();

        let err = str_replace::run(path.to_str().unwrap(), &old_str, Some("X"), false).unwrap_err();
        assert!(matches!(err, FileEditError::Validation(_)));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            content,
            "file must be untouched"
        );
    }

    // ── str_replace: required new_str contract (RED — pins the fix) ────
    //
    // Pins the user-approved contract for file_str_replace:
    //   1. schema `required` = {path, old_str, new_str}, and new_str's
    //      description states that an EMPTY STRING deletes old_str;
    //   2. new_str ABSENT  -> Err carrying the exact self-correcting
    //      guidance `new_str is required; pass "" to delete old_str`,
    //      with NO file modification;
    //   3. new_str null    -> same Err, no file modification;
    //   4. new_str ""      -> Ok, wording says "Deleted" (never
    //      "Replaced"), old_str gone from disk;
    //   5. new_str non-empty -> Ok, wording still says "Replaced"
    //      (unchanged behaviour — regression guard);
    //   6. the match-level indicator ("whitespace_normalized") that
    //      today rides along in the success message must survive in the
    //      Deleted wording exactly as it does in the Replaced one.
    //
    // These drive the Rig `Tool` trait boundary (`definition` / `call`)
    // with JSON args strings exactly as rig would deserialize and deliver
    // them — NOT `str_replace::run` directly — because the contract lives
    // at the boundary where model args arrive. `run`'s own
    // `None`-means-delete behaviour stays pinned by
    // `str_replace_normalized_match_deletion_removes_matched_range_only`,
    // so the required-ness guard belongs in `call()`.
    //
    // RED expectation on current code: (1) required is only
    // [path, old_str]; (2)/(3) a missing/null new_str silently DELETES
    // old_str and returns Ok("... Replaced 1 occurrence ..."); (4)/(6)/(7)
    // deletion is reported as "Replaced". (5) and (8) pass today:
    // non-empty replacement is unchanged behaviour.

    /// The exact guidance substring the model must see in the tool error
    /// to self-correct (spec item 2/3 — verbatim).
    const NEW_STR_REQUIRED_GUIDANCE: &str = "new_str is required; pass \"\" to delete old_str";

    /// Deserialize rig-style args JSON the same way rig does before
    /// invoking `Tool::call`.
    fn parse_rig_args(json: &str) -> str_replace::FileStrReplaceArgs {
        serde_json::from_str(json)
            .unwrap_or_else(|e| panic!("test fixture must deserialize like rig's args: {e}"))
    }

    #[tokio::test]
    async fn str_replace_schema_requires_new_str_and_documents_empty_string_deletion() {
        // Spec item 1 — the schema is the first line of defence: a model
        // that reads the schema must see new_str as required and learn
        // that "" is the deletion path.
        let tool = FileStrReplaceTool::new(PathBuf::from("/tmp"));
        let def = tool.definition(String::new()).await;
        let params = &def.parameters;

        let required = params["required"]
            .as_array()
            .expect("schema must keep a `required` array");
        let required_set: std::collections::BTreeSet<&str> = required
            .iter()
            .map(|v| v.as_str().expect("required entries are strings"))
            .collect();
        let expected: std::collections::BTreeSet<&str> =
            ["path", "old_str", "new_str"].into_iter().collect();
        assert_eq!(
            required_set, expected,
            "new_str must be required — a missing new_str today silently \
             deletes old_str and reports a replacement"
        );

        let new_str_prop = &params["properties"]["new_str"];
        assert_eq!(
            new_str_prop["type"].as_str(),
            Some("string"),
            "new_str stays a string property"
        );
        let desc = new_str_prop["description"]
            .as_str()
            .expect("new_str must keep a description");
        assert!(
            desc.to_lowercase().contains("empty string"),
            "new_str description must state that an empty string deletes \
             old_str, got: {desc:?}"
        );
        assert!(
            desc.to_lowercase().contains("delete"),
            "new_str description must state that an empty string deletes \
             old_str, got: {desc:?}"
        );
        assert!(
            !desc.contains("Omit to delete"),
            "the dishonest 'Omit to delete old_str' wording must be gone, \
             got: {desc:?}"
        );
    }

    #[tokio::test]
    async fn str_replace_call_missing_new_str_errors_with_guidance_and_writes_nothing() {
        // Spec item 2 — the exact shape of the production corruption: the
        // model meant to replace, omitted new_str, and the tool deleted
        // old_str while reporting "Replaced 1 occurrence".
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing_new_str.txt");
        let original = "alpha beta gamma\n";
        std::fs::write(&path, original).unwrap();

        let args_json = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_str": "beta"
        })
        .to_string();
        let args = parse_rig_args(&args_json);

        let tool = FileStrReplaceTool::new(dir.path().to_path_buf());
        let result = tool.call(args).await;
        match result {
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains(NEW_STR_REQUIRED_GUIDANCE),
                    "error must carry the exact self-correcting guidance, got: {msg}"
                );
            }
            Ok(out) => panic!(
                "missing new_str must be rejected, not silently deleted — \
                 got Ok: {out}"
            ),
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "rejected call must not touch the file"
        );
    }

    #[tokio::test]
    async fn str_replace_call_null_new_str_errors_with_guidance_and_writes_nothing() {
        // Spec item 3 — an explicit JSON null deserializes to the same
        // `None` as an absent key and must be rejected identically.
        let dir = tempdir().unwrap();
        let path = dir.path().join("null_new_str.txt");
        let original = "alpha beta gamma\n";
        std::fs::write(&path, original).unwrap();

        let args_json = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_str": "beta",
            "new_str": null
        })
        .to_string();
        let args = parse_rig_args(&args_json);

        let tool = FileStrReplaceTool::new(dir.path().to_path_buf());
        let result = tool.call(args).await;
        match result {
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains(NEW_STR_REQUIRED_GUIDANCE),
                    "error must carry the exact self-correcting guidance, got: {msg}"
                );
            }
            Ok(out) => panic!(
                "null new_str must be rejected, not silently deleted — \
                 got Ok: {out}"
            ),
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "rejected call must not touch the file"
        );
    }

    #[tokio::test]
    async fn str_replace_call_empty_new_str_reports_deleted_not_replaced_and_removes_text() {
        // Spec item 4 — the explicit empty string is the ONLY legal
        // deletion path, and the success wording must say DELETED, never
        // Replaced. (Today the deletion mechanics already work; only the
        // wording is wrong — this test is RED on the wording assertions.)
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty_new_str.txt");
        std::fs::write(&path, "alpha beta gamma\n").unwrap();

        let args_json = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_str": "beta",
            "new_str": ""
        })
        .to_string();
        let args = parse_rig_args(&args_json);

        let tool = FileStrReplaceTool::new(dir.path().to_path_buf());
        let out = tool
            .call(args)
            .await
            .expect("explicit empty new_str is the legal deletion path");
        assert!(
            out.contains("Deleted"),
            "deletion must be reported as a deletion, got: {out}"
        );
        assert!(
            !out.contains("Replaced"),
            "deletion must not be reported as a replacement, got: {out}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha  gamma\n",
            "old_str must be removed from disk"
        );
    }

    #[tokio::test]
    async fn str_replace_call_nonempty_new_str_still_reports_replaced() {
        // Spec item 5 — regression guard: a real replacement keeps the
        // existing "Replaced" wording and behaviour. Expected GREEN today.
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonempty_new_str.txt");
        std::fs::write(&path, "alpha beta gamma\n").unwrap();

        let args_json = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_str": "beta",
            "new_str": "BETA"
        })
        .to_string();
        let args = parse_rig_args(&args_json);

        let tool = FileStrReplaceTool::new(dir.path().to_path_buf());
        let out = tool
            .call(args)
            .await
            .expect("normal replacement must succeed");
        assert!(out.contains("Replaced"), "got: {out}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha BETA gamma\n"
        );
    }

    #[tokio::test]
    async fn str_replace_call_end_to_end_json_args_with_thought_key_replaces() {
        // Spec item (f) — full-path proof: the args arrive as ONE JSON
        // string exactly as rig would deliver ThoughtGate's output,
        // including the extra `thought` key, which the Args struct must
        // ignore, not choke on. Expected GREEN today (non-empty
        // replacement is unchanged behaviour).
        let dir = tempdir().unwrap();
        let path = dir.path().join("e2e_thought.txt");
        std::fs::write(&path, "alpha beta gamma\n").unwrap();

        let args_json = serde_json::json!({
            "thought": "swap beta for BETA",
            "path": path.to_string_lossy(),
            "old_str": "beta",
            "new_str": "BETA"
        })
        .to_string();
        let args = parse_rig_args(&args_json);

        let tool = FileStrReplaceTool::new(dir.path().to_path_buf());
        let out = tool
            .call(args)
            .await
            .expect("end-to-end replacement must succeed");
        assert!(out.contains("Replaced"), "got: {out}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha BETA gamma\n"
        );
    }

    #[tokio::test]
    async fn str_replace_call_empty_new_str_normalized_match_keeps_match_level_in_deleted_wording()
    {
        // Spec item 6 — the match-level indicator that today rides along
        // in the success message ("Match required whitespace_normalized")
        // must survive the wording change: Deleted messages keep the same
        // format conventions as Replaced ones.
        let dir = tempdir().unwrap();
        let path = dir.path().join("deleted_normalized.txt");
        std::fs::write(&path, "alpha\nTARGET   \nbeta\n").unwrap();

        let args_json = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_str": "TARGET\n",
            "new_str": ""
        })
        .to_string();
        let args = parse_rig_args(&args_json);

        let tool = FileStrReplaceTool::new(dir.path().to_path_buf());
        let out = tool
            .call(args)
            .await
            .expect("normalized-match deletion must succeed");
        assert!(out.contains("Deleted"), "got: {out}");
        assert!(!out.contains("Replaced"), "got: {out}");
        assert!(
            out.contains("whitespace_normalized"),
            "match-level indicator must survive in the Deleted wording, got: {out}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha\n   \nbeta\n"
        );
    }

    #[tokio::test]
    async fn str_replace_call_empty_new_str_replace_all_reports_deleted_not_replaced() {
        // Spec item 4, plural form — replace_all deletion must also be
        // reported as a deletion. The exact plural wording is NOT pinned
        // beyond "Deleted"/not-"Replaced" (the spec leaves it open).
        let dir = tempdir().unwrap();
        let path = dir.path().join("deleted_all.txt");
        std::fs::write(&path, "x x x\n").unwrap();

        let args_json = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_str": "x",
            "new_str": "",
            "replace_all": true
        })
        .to_string();
        let args = parse_rig_args(&args_json);

        let tool = FileStrReplaceTool::new(dir.path().to_path_buf());
        let out = tool
            .call(args)
            .await
            .expect("replace_all deletion must succeed");
        assert!(out.contains("Deleted"), "got: {out}");
        assert!(!out.contains("Replaced"), "got: {out}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "  \n",
            "every occurrence of old_str must be removed"
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
