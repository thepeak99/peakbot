//! 0.24 port of the `UploadsConfig` tests (file-uploads design §3.1 / §5.6).
//!
//! The top-level `uploads:` block moved under `general:` — the tests drive
//! THE boundary (`Config::parse`) with the 0.24 layout and read
//! `cfg.general.uploads`. The locked contract is unchanged:
//!
//! - absent `uploads:` ⇒ defaults 50 MB / 10 files
//! - a partial block fills the missing field with its default
//! - `max_file_mb` is 1..=1024, `max_files` is 1..=100
//! - `deny_unknown_fields` rejects unknown keys under the block

use std::path::Path;

use peakbot::config::UploadsConfig;
use peakbot::config::resolve::{Config, ParseCtx, Source};

const GLOBAL_PATH: &str = "/home/u/.config/peakbot/config.yaml";

/// The 0.24 skeleton: one provider, one profile `dev`/`main`, and the given
/// `general:` body (empty = no `general:` block at all).
fn parse_general(general_body: &str) -> Config {
    let general = if general_body.is_empty() {
        String::new()
    } else {
        format!("general:\n{general_body}")
    };
    let yaml = format!(
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\nprofiles:\n  dev:\n    agents:\n      main:\n        model: a\n{general}"
    );
    Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text: &yaml,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .unwrap_or_else(|e| panic!("config must parse: {e:?}; yaml was:\n{yaml}"))
}

fn parse_general_err(general_body: &str) -> String {
    let general = if general_body.is_empty() {
        String::new()
    } else {
        format!("general:\n{general_body}")
    };
    let yaml = format!(
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\nprofiles:\n  dev:\n    agents:\n      main:\n        model: a\n{general}"
    );
    let err = Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text: &yaml,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .expect_err("the config must be rejected");
    // `ConfigErrors` has no Display — project the segment paths + messages
    // (the same shape the boot/reload lines use).
    err.0
        .iter()
        .map(|e| format!("{}: {}", e.dotted_path(), e.message))
        .collect::<Vec<_>>()
        .join("; ")
}

#[test]
fn uploads_block_absent_defaults_to_50mb_10_files() {
    let cfg = parse_general("");
    assert_eq!(
        cfg.general.uploads,
        UploadsConfig {
            max_file_mb: 50,
            max_files: 10,
        },
        "absent uploads: block must default to 50 MB / 10 files"
    );
}

#[test]
fn uploads_partial_block_fills_missing_field_with_default() {
    let cfg = parse_general("  uploads:\n    max_file_mb: 25\n");
    assert_eq!(cfg.general.uploads.max_file_mb, 25);
    assert_eq!(
        cfg.general.uploads.max_files, 10,
        "max_files must default when absent from the block"
    );
}

#[test]
fn uploads_explicit_values_are_kept() {
    let cfg = parse_general("  uploads:\n    max_file_mb: 5\n    max_files: 3\n");
    assert_eq!(
        cfg.general.uploads,
        UploadsConfig {
            max_file_mb: 5,
            max_files: 3,
        }
    );
}

#[test]
fn max_file_mb_zero_is_rejected() {
    let msg = parse_general_err("  uploads:\n    max_file_mb: 0\n");
    assert!(
        msg.contains("max_file_mb"),
        "max_file_mb: 0 must be rejected (valid 1..=1024): {msg}"
    );
}

#[test]
fn max_file_mb_above_1024_is_rejected() {
    let msg = parse_general_err("  uploads:\n    max_file_mb: 1025\n");
    assert!(
        msg.contains("max_file_mb"),
        "max_file_mb: 1025 must be rejected (valid 1..=1024): {msg}"
    );
}

#[test]
fn max_files_zero_is_rejected() {
    let msg = parse_general_err("  uploads:\n    max_files: 0\n");
    assert!(
        msg.contains("max_files"),
        "max_files: 0 must be rejected (valid 1..=100): {msg}"
    );
}

#[test]
fn max_files_above_100_is_rejected() {
    let msg = parse_general_err("  uploads:\n    max_files: 101\n");
    assert!(
        msg.contains("max_files"),
        "max_files: 101 must be rejected (valid 1..=100): {msg}"
    );
}

#[test]
fn boundary_values_one_and_max_are_accepted() {
    let cfg = parse_general("  uploads:\n    max_file_mb: 1\n    max_files: 1\n");
    assert_eq!(
        cfg.general.uploads,
        UploadsConfig {
            max_file_mb: 1,
            max_files: 1,
        },
        "max_file_mb: 1 / max_files: 1 are in range"
    );
    let cfg = parse_general("  uploads:\n    max_file_mb: 1024\n    max_files: 100\n");
    assert_eq!(
        cfg.general.uploads,
        UploadsConfig {
            max_file_mb: 1024,
            max_files: 100,
        },
        "max_file_mb: 1024 / max_files: 100 are in range"
    );
}

#[test]
fn uploads_unknown_key_is_rejected() {
    let msg = parse_general_err("  uploads:\n    max_file_mb: 50\n    bogus: 1\n");
    assert!(
        msg.contains("bogus"),
        "the error must name the unknown key; got: {msg}"
    );
}
