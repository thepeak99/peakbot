//! T1 — `UploadsConfig` RED tests (file-uploads design §3.1 / §5.6 / §7 T1).
//!
//! **Status: compile-fail until T1 lands.** `peakbot::config::UploadsConfig`
//! and the `Config.uploads` field do not exist yet; this file targets the
//! locked interface from design §3.1 and fails to compile — the RED state
//! we want.
//!
//! Uses the public config-loading API the same way the in-crate tests do:
//! `serde_yaml::from_str::<Config>(yaml)` followed by `Config::validate()`.

use peakbot::config::{Config, UploadsConfig};

fn parse(yaml: &str) -> Config {
    serde_yaml::from_str::<Config>(yaml).expect("yaml must parse")
}

#[test]
fn uploads_block_absent_defaults_to_50mb_10_files() {
    let cfg = parse("cost_tracking: true\n");
    assert_eq!(
        cfg.uploads,
        UploadsConfig {
            max_file_mb: 50,
            max_files: 10,
        },
        "absent uploads: block must default to 50 MB / 10 files"
    );
    assert!(cfg.validate().is_ok(), "defaults must validate");
}

#[test]
fn uploads_partial_block_fills_missing_field_with_default() {
    let cfg = parse("uploads:\n  max_file_mb: 25\n");
    assert_eq!(cfg.uploads.max_file_mb, 25);
    assert_eq!(
        cfg.uploads.max_files, 10,
        "max_files must default when absent from the block"
    );
    assert!(cfg.validate().is_ok());
}

#[test]
fn uploads_explicit_values_are_kept() {
    let cfg = parse("uploads:\n  max_file_mb: 5\n  max_files: 3\n");
    assert_eq!(
        cfg.uploads,
        UploadsConfig {
            max_file_mb: 5,
            max_files: 3,
        }
    );
    assert!(cfg.validate().is_ok());
}

#[test]
fn max_file_mb_zero_is_rejected_by_validate() {
    let cfg = parse("uploads:\n  max_file_mb: 0\n");
    assert!(
        cfg.validate().is_err(),
        "max_file_mb: 0 must be rejected (valid 1..=1024)"
    );
}

#[test]
fn max_file_mb_above_1024_is_rejected_by_validate() {
    let cfg = parse("uploads:\n  max_file_mb: 1025\n");
    assert!(
        cfg.validate().is_err(),
        "max_file_mb: 1025 must be rejected (valid 1..=1024)"
    );
}

#[test]
fn max_files_zero_is_rejected_by_validate() {
    let cfg = parse("uploads:\n  max_files: 0\n");
    assert!(
        cfg.validate().is_err(),
        "max_files: 0 must be rejected (valid 1..=100)"
    );
}

#[test]
fn max_files_above_100_is_rejected_by_validate() {
    let cfg = parse("uploads:\n  max_files: 101\n");
    assert!(
        cfg.validate().is_err(),
        "max_files: 101 must be rejected (valid 1..=100)"
    );
}

#[test]
fn boundary_values_one_and_max_are_accepted() {
    let cfg = parse("uploads:\n  max_file_mb: 1\n  max_files: 1\n");
    assert!(
        cfg.validate().is_ok(),
        "max_file_mb: 1 / max_files: 1 are in range"
    );
    let cfg = parse("uploads:\n  max_file_mb: 1024\n  max_files: 100\n");
    assert!(
        cfg.validate().is_ok(),
        "max_file_mb: 1024 / max_files: 100 are in range"
    );
}

#[test]
fn uploads_unknown_key_is_rejected() {
    let err = serde_yaml::from_str::<Config>("uploads:\n  max_file_mb: 50\n  bogus: 1\n")
        .expect_err("deny_unknown_fields must reject the unknown key");
    assert!(
        err.to_string().contains("bogus"),
        "error must name the unknown key; got: {err}"
    );
}
