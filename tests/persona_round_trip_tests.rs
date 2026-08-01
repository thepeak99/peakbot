//! P1 — `Config::persona()` round-trip tests (plan §A-Q7).
//!
//! **Status: compile-fail until P1 lands.** This file targets the planned
//! public API (`Config::persona() -> Option<&str>`) and the planned
//! `persona: Option<String>` field. Both are absent today; the file will
//! fail to compile and block `cargo test` until P1 adds them. That is the
//! RED state we want — see `setup-backend-plan.md` track P, task P1.
//!
//! The expectation at every assertion is the **exact** post-impl round-trip
//! behaviour locked in plan §A-Q7:
//!
//! - the explicit `|2-` indent indicator preserves a leading-space first line
//! - blank interior lines survive
//! - trailing whitespace on a line is content, not stripped
//! - the `Config::persona()` accessor trims surrounding whitespace and
//!   returns `None` for absent / whitespace-only values
//!
//! When the implementer wires up `pub persona: Option<String>` and
//! `pub fn persona(&self) -> Option<&str>`, this file will compile and
//! every assertion below should go GREEN without further edits.

use peakbot::Config;

/// Helper: load a persona-bearing YAML and unwrap its accessor. Centralises
/// the planned API so the test bodies read like the spec.
fn parsed_persona(yaml: &str) -> Option<String> {
    let cfg: Config = serde_yaml::from_str(yaml)
        .unwrap_or_else(|e| panic!("yaml failed to parse: {e}; yaml was:\n{yaml}"));
    cfg.persona().map(str::to_string)
}

#[test]
fn persona_absent_returns_none() {
    assert_eq!(parsed_persona(""), None);
}

#[test]
fn persona_whitespace_only_returns_none() {
    // Accessor must trim + filter — the "absent" representation (§A-Q7).
    assert_eq!(parsed_persona("persona: \"   \"\n"), None);
    assert_eq!(parsed_persona("persona: |2-\n   \n   \n"), None);
}

#[test]
fn persona_block_with_explicit_indent_indicator_preserves_leading_space() {
    // The load-bearing case: the first non-empty line starts with a space,
    // and `|2-` is the only thing keeping it. Without `2`, YAML infers the
    // indent from the first non-empty line and strips every leading space.
    let yaml = "persona: |2-\n  text starting with space\n  second\n";
    assert_eq!(
        parsed_persona(yaml).as_deref(),
        Some("text starting with space\nsecond")
    );
}

#[test]
fn persona_block_preserves_blank_interior_line() {
    let yaml = "persona: |2-\n  first paragraph\n\n  second paragraph\n";
    assert_eq!(
        parsed_persona(yaml).as_deref(),
        Some("first paragraph\n\nsecond paragraph")
    );
}

#[test]
fn persona_block_preserves_trailing_space_on_a_line() {
    let yaml = "persona: |2-\n  ends-with-space \n  next\n";
    assert_eq!(
        parsed_persona(yaml).as_deref(),
        Some("ends-with-space \nnext")
    );
}

#[test]
fn persona_block_handles_hash_and_colon_in_content() {
    // Lines that start with `#` (comment marker) or contain `:` (mapping
    // marker) must NOT be re-interpreted by the parser. Block scalars
    // protect against both; this test pins that.
    let yaml = "persona: |2-\n  uses #: not a comment\n  has: colon\n";
    assert_eq!(
        parsed_persona(yaml).as_deref(),
        Some("uses #: not a comment\nhas: colon")
    );
}

#[test]
fn persona_block_strip_chomping_strips_only_trailing_newlines() {
    // `-` chomping means the parsed value has no trailing newline. This
    // matches the `push('\n')` join rule in build_system_prompt (§A-Q7).
    let yaml = "persona: |2-\n  hello\n  world\n\n\n";
    assert_eq!(parsed_persona(yaml).as_deref(), Some("hello\nworld"));
}

#[test]
fn persona_coexists_with_other_top_level_keys() {
    // The persona is a peer of every other Config key. It must not steal
    // any fields, and a config carrying unrelated keys still parses.
    let yaml = "\
cost_tracking: false
context:
  threshold: 0.9
  keep_recent: 3
persona: |2-
  short persona
";
    let cfg: Config = serde_yaml::from_str(yaml).expect("persona + other keys must parse together");
    assert_eq!(cfg.persona(), Some("short persona"));
    assert!(!cfg.cost_tracking);
}
