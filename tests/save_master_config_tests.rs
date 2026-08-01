//! T1 — `config::save_master_config` tests (plan §A-Q4, track S task S1).
//!
//! **Status: compile-fail until S1 lands.** This file targets the planned
//! public API `peakbot::config::save_master_config` and the inner pure
//! helper `save_config_at(dir, yaml)` that S1 adds in `src/config/mod.rs`.
//! The function does not exist today; the file will fail to compile and
//! block `cargo test` until S1 lands. That is the RED state we want.
//!
//! Per plan §A-Q4 the writer contract is:
//!
//! 1. `create_dir_all` the parent.
//! 2. If `config.yaml` exists, copy it to `config.yaml.bak` (single slot,
//!    overwritten).
//! 3. Write `config.yaml.tmp` in the same directory, `0600` on Unix, then
//!    `sync_all`.
//! 4. Remove the existing `config.yaml` if present, then `rename` tmp → final.
//!    (remove-then-rename order is for Windows.)
//! 5. Return `{ path, backup: Option<path> }` so the response can state what
//!    happened.
//!
//! The locked signature the tests are pinned to (from the plan):
//!
//! ```ignore
//! pub fn save_master_config(yaml: &str) -> Result<SaveOutcome>;
//! pub fn save_config_at(dir: &Path, yaml: &str) -> Result<SaveOutcome>;
//! ```

use peakbot::config::{save_config_at, save_master_config};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use tempfile::tempdir;

/// Sanity: the saved outcome is printable (so failed `unwrap`s have useful
/// error messages). The Debug bound is satisfied trivially here because
/// SaveOutcome isn't `pub` yet, but Debug is the obvious contract.
fn touch<T: std::fmt::Debug>(_: &T) {}

// ===========================================================================
// S1.1 — first write produces no backup, exact bytes, 0600 on Unix.
// ===========================================================================

#[test]
fn save_master_config_writes_exact_bytes_to_destination() {
    let dir = tempdir().unwrap();
    let yaml = "provider:\n  type: openrouter\n  config:\n    model: x\n";
    let outcome = save_config_at(dir.path(), yaml).expect("first write must succeed");
    touch(&outcome);

    let written = fs::read_to_string(&outcome.path).expect("written file must be readable");
    assert_eq!(written, yaml, "writer must write the input bytes verbatim");

    // Outcome must point inside the dir we passed.
    assert_eq!(
        outcome.path.parent().unwrap(),
        dir.path(),
        "path must be the destination dir's `config.yaml`"
    );
}

#[test]
fn save_master_config_first_write_has_no_backup() {
    let dir = tempdir().unwrap();
    let outcome = save_config_at(dir.path(), "x: 1\n").unwrap();
    assert!(
        outcome.backup.is_none(),
        "first write must report backup = None"
    );
    assert!(
        !dir.path().join("config.yaml.bak").exists(),
        "no .bak file may exist on a first write"
    );
}

#[test]
fn save_master_config_writes_file_with_mode_0600_on_unix() {
    if cfg!(unix) {
        let dir = tempdir().unwrap();
        let outcome = save_config_at(dir.path(), "x: 1\n").unwrap();
        let mode = fs::metadata(&outcome.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "config.yaml must be 0600 on Unix (not world-readable)"
        );
    }
}

// ===========================================================================
// S1.2 — second write produces .bak with the OLD bytes; tmp is gone.
// ===========================================================================

#[test]
fn save_master_config_second_write_copies_old_bytes_to_backup() {
    let dir = tempdir().unwrap();
    let first = "provider:\n  type: openrouter\n  config:\n    model: A\n";
    let second = "provider:\n  type: openrouter\n  config:\n    model: B\n";

    save_config_at(dir.path(), first).expect("first write");
    let outcome = save_config_at(dir.path(), second).expect("second write");

    // New bytes on disk.
    let on_disk = fs::read_to_string(&outcome.path).unwrap();
    assert_eq!(
        on_disk, second,
        "current config.yaml must hold the new bytes"
    );

    // Backup holds the OLD bytes.
    let backup_path: &PathBuf = outcome
        .backup
        .as_ref()
        .expect("second write must report a backup");
    let backup_bytes = fs::read_to_string(backup_path).unwrap();
    assert_eq!(
        backup_bytes, first,
        ".bak must contain the bytes that were on disk before the second write"
    );
    assert_eq!(
        backup_path.file_name().and_then(|s| s.to_str()),
        Some("config.yaml.bak"),
        "backup must be named config.yaml.bak (single slot)"
    );
}

#[test]
fn save_master_config_leaves_no_tmp_file_after_success() {
    let dir = tempdir().unwrap();
    save_config_at(dir.path(), "x: 1\n").unwrap();
    save_config_at(dir.path(), "x: 2\n").unwrap();

    // No *.tmp* survivors in the dir after either write.
    for entry in fs::read_dir(dir.path()).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let s = name.to_string_lossy();
        assert!(
            !s.contains(".tmp"),
            "no tmp file may survive a successful write; found {s:?}"
        );
    }
}

#[test]
fn save_master_config_backup_is_overwritten_on_each_write() {
    // Single-slot backup is the design (plan §A-Q4): predictable, bounded,
    // nothing to garbage-collect. Three writes leave only the second's
    // predecessor as .bak.
    let dir = tempdir().unwrap();
    save_config_at(dir.path(), "x: 1\n").unwrap();
    save_config_at(dir.path(), "x: 2\n").unwrap();
    save_config_at(dir.path(), "x: 3\n").unwrap();

    let bak = fs::read_to_string(dir.path().join("config.yaml.bak")).unwrap();
    assert_eq!(
        bak, "x: 2\n",
        "the .bak from the third write must be the second write's bytes"
    );
    let cur = fs::read_to_string(dir.path().join("config.yaml")).unwrap();
    assert_eq!(cur, "x: 3\n");
}

#[test]
fn save_master_config_creates_missing_parent_directories() {
    // Plan §A-Q4 step 1: `create_dir_all` the parent.
    let dir = tempdir().unwrap();
    let nested = dir.path().join("a/b/c");
    let outcome = save_config_at(&nested, "x: 1\n").expect("nested write must succeed");
    assert!(
        outcome.path.exists(),
        "config.yaml must exist under the nested dir"
    );
    assert!(
        outcome.path.starts_with(&nested),
        "config.yaml must live under the passed dir"
    );
}

// ===========================================================================
// S1.3 — outcome struct shape (compile-time check).
// ===========================================================================

#[test]
fn save_outcome_has_path_and_backup_fields() {
    // Pure compile-time check: the returned struct exposes `path: PathBuf`
    // and `backup: Option<PathBuf>` exactly. The wire type at §B uses the
    // same shape (`{ path, backup }`).
    let dir = tempdir().unwrap();
    let outcome = save_config_at(dir.path(), "x: 1\n").unwrap();
    let _p: &PathBuf = &outcome.path;
    let _b: &Option<PathBuf> = &outcome.backup;
}

// ===========================================================================
// S1.4 — save_master_config (no-arg entry point) is the production seam.
// ===========================================================================

#[test]
fn save_master_config_no_args_compiles() {
    // Pure signature check: `save_master_config(yaml: &str) -> Result<SaveOutcome>`.
    // If S1 wires this up as a different shape, this file fails to compile.
    fn _check_signature(yaml: &str) -> anyhow::Result<()> {
        save_master_config(yaml).map(|_| ())
    }
    let _ = _check_signature;
}
