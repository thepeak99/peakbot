//! A2 — `Named<T>` RED tests (config-v2 spec §4.2 name rule, §4.3 N1/N2, §5.2).
//!
//! **Status: compile-fail until A2 lands.** `peakbot::config::named` does not
//! exist yet; this file targets the locked interface from spec §5.2
//! (`Named<T>`: ordered, duplicate-rejecting, name-validated map) and fails
//! to compile — the RED state we want.
//!
//! Interface assumed (spec §5.2, verbatim):
//! - `pub struct Named<T>(Vec<(String, T)>)`
//! - manual `Deserialize` (a map visitor) that rejects duplicates (N2) and
//!   invalid names (N1): pattern `^[A-Za-z0-9_ .-]{1,64}$`, no leading or
//!   trailing whitespace
//! - API: `iter()`, `len()`, `is_empty()`, `get(&str) -> Option<&T>`,
//!   `first()`, `Default` (empty)
//! - NOT responsible for non-emptiness (E2/E3 are checked in `parse`)

use peakbot::config::named::Named;

/// The exact N1 message (spec §4.3) with the offending name substituted.
/// NB: "1–64" uses an en dash (U+2013) — copied verbatim from the spec.
fn n1_message(name: &str) -> String {
    format!(
        "name '{name}' must be 1–64 characters of letters, digits, space, '_', '.', '-' with no leading/trailing space"
    )
}

fn parse(yaml: &str) -> Result<Named<u32>, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}

#[test]
fn named_preserves_order() {
    let n = parse("zeta: 1\nalpha: 2\nmid 9: 3\n").expect("valid names must parse");
    let names: Vec<&str> = n.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        names,
        ["zeta", "alpha", "mid 9"],
        "insertion order must be preserved, not sorted"
    );
    assert_eq!(n.len(), 3);
}

#[test]
fn named_rejects_duplicate_key() {
    let err = parse("dev: 1\ndev: 2\n").expect_err("duplicate YAML keys must be rejected (N2)");
    let msg = err.to_string();
    assert!(
        msg.contains("duplicate"),
        "N2 must say duplicate, got: {msg}"
    );
    assert!(
        msg.contains("dev"),
        "N2 must name the duplicate key, got: {msg}"
    );
    // Spec ambiguity (flagged in the TDD report): §4.3 pins the noun per
    // context ("duplicate profile '<n>'" / "duplicate agent '<n>'" /
    // "duplicate subagent '<n>'") but never says how a generic `Named<T>`
    // learns the noun. We therefore do NOT assert the noun here; the
    // per-map noun is the resolve layer's job (A4).
}

#[test]
fn named_rejects_bad_name() {
    let err = parse("bad/name: 1\n").expect_err("slash is not in the §4.2 name pattern");
    assert!(
        err.to_string().contains(&n1_message("bad/name")),
        "N1 exact message expected, got: {err}"
    );
}

#[test]
fn named_rejects_empty_name() {
    let err = parse("\"\": 1\n").expect_err("an empty name is outside {1,64}");
    assert!(
        err.to_string().contains(&n1_message("")),
        "N1 exact message expected, got: {err}"
    );
}

#[test]
fn named_rejects_non_ascii_name() {
    let err = parse("café: 1\n").expect_err("the pattern is [A-Za-z0-9_ .-] only");
    assert!(
        err.to_string().contains(&n1_message("café")),
        "N1 exact message expected, got: {err}"
    );
}

#[test]
fn named_allows_spaces_in_names() {
    let n = parse("my agent: 1\nv1.2 beta-x: 2\n_under: 3\n")
        .expect("spaces, dots and dashes are legal in names");
    assert_eq!(n.len(), 3);
    assert_eq!(n.get("my agent").copied(), Some(1));
    assert_eq!(n.get("v1.2 beta-x").copied(), Some(2));
    assert_eq!(n.get("_under").copied(), Some(3));
}

#[test]
fn named_rejects_leading_whitespace_in_name() {
    let err = parse("\" padded\": 1\n").expect_err("leading whitespace is not allowed (§4.2)");
    assert!(
        err.to_string().contains(&n1_message(" padded")),
        "N1 exact message expected, got: {err}"
    );
}

#[test]
fn named_rejects_trailing_whitespace_in_name() {
    let err = parse("\"padded \": 1\n").expect_err("trailing whitespace is not allowed (§4.2)");
    assert!(
        err.to_string().contains(&n1_message("padded ")),
        "N1 exact message expected, got: {err}"
    );
}

#[test]
fn named_accepts_64_char_name() {
    let name = "a".repeat(64);
    let n = parse(&format!("{name}: 1\n")).expect("64 characters is the inclusive maximum");
    assert_eq!(n.len(), 1);
    assert_eq!(n.get(&name).copied(), Some(1));
}

#[test]
fn named_rejects_65_char_name() {
    let name = "a".repeat(65);
    let err = parse(&format!("{name}: 1\n")).expect_err("65 characters exceeds the 64-char limit");
    assert!(
        err.to_string().contains(&n1_message(&name)),
        "N1 exact message expected, got: {err}"
    );
}

#[test]
fn named_first_returns_first_entry() {
    let n = parse("one: 1\ntwo: 2\nthree: 3\n").expect("valid names must parse");
    let (name, value) = n.first().expect("a non-empty Named has a first entry");
    assert_eq!(
        name.as_str(),
        "one",
        "first() must be positional, not sorted"
    );
    assert_eq!(value, 1u32);
}

#[test]
fn named_first_is_none_when_empty() {
    let n = Named::<u32>::default();
    assert!(n.first().is_none());
}

#[test]
fn named_get_returns_value_for_known_name() {
    let n = parse("alpha: 1\nbeta: 2\n").expect("valid names must parse");
    assert_eq!(n.get("beta").copied(), Some(2));
}

#[test]
fn named_get_returns_none_for_unknown_name() {
    let n = parse("alpha: 1\n").expect("valid names must parse");
    assert!(n.get("beta").is_none());
}

#[test]
fn named_len_and_is_empty() {
    let n = parse("a: 1\nb: 2\n").expect("valid names must parse");
    assert_eq!(n.len(), 2);
    assert!(!n.is_empty());
}

#[test]
fn named_default_is_empty() {
    let n = Named::<u32>::default();
    assert!(n.is_empty());
    assert_eq!(n.len(), 0);
}
