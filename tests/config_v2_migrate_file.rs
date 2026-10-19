//! RED tests for task A7 (unit level): the in-process `migrate_file` IO
//! wrapper (§7.5). Split from `config_v2_migrate_cli.rs` so that file keeps
//! compiling (and fails at runtime on the missing subcommand) while this one
//! fails to compile until `peakbot::config::migrate` exists.
//!
//! **Assumed API** (the spec gives the signature, not the field list):
//!
//! ```rust
//! // peakbot::config::migrate
//! pub struct MigrateReport {
//!     pub path: std::path::PathBuf,   // the config.yaml that was written
//!     pub backup: std::path::PathBuf, // <config_dir>/config.yaml.bak
//!     pub notes: Vec<String>,
//! }
//! pub enum MigrateFileError { AlreadyCurrent, /* … */ } // Debug + Display
//! pub fn migrate_file(
//!     config_path: &std::path::Path,
//!     cwd: &std::path::Path,
//! ) -> Result<MigrateReport, MigrateFileError>;
//! ```
//!
//! `migrate_file` takes the config path explicitly, so these tests need no
//! `XDG_CONFIG_HOME` swapping (and no `CONFIG_ENV_LOCK`). The web-token file
//! is looked up next to `config_path` (`<config_dir>/web-token`).

use std::path::{Path, PathBuf};

use peakbot::config::migrate::migrate_file;

/// A small pre-0.24 layout config: legacy top-level keys, no `general`/`mcps`.
const LEGACY_CONFIG: &str = "providers:\n  - name: openai\n    type: openai\n    api_key: fake-key\n    models:\n      - name: fake-model\n        alias: solo\ndefault_model: solo\npersona: You are a careful engineer.\n";

/// A minimal valid 0.24-layout config (has the new-only `profiles…agents` marker).
const NEW_LAYOUT_CONFIG: &str = "providers:\n  - name: openai\n    type: openai\n    api_key: fake-key\n    models:\n      - name: fake-model\n        alias: solo\nprofiles:\n  default:\n    agents:\n      main:\n        model: solo\n";

/// Legacy input whose migration CANNOT validate: `context.threshold: 5` is
/// copied verbatim to `general.context` and violates G4 (0 < x ≤ 1).
const INVALID_MIGRATION_CONFIG: &str = "providers:\n  - name: openai\n    type: openai\n    api_key: fake-key\n    models:\n      - name: fake-model\n        alias: solo\ndefault_model: solo\ncontext:\n  threshold: 5\n";

const TOKEN: &str = "secret-token-123";

fn config_path(dir: &Path) -> PathBuf {
    dir.join("config.yaml")
}

fn backup_path(dir: &Path) -> PathBuf {
    dir.join("config.yaml.bak")
}

// ===========================================================================
// Unit level: `migrate_file(config_path, cwd) -> Result<MigrateReport, _>`
// ===========================================================================

/// The wrapper performs the same work as the CLI: rewrite, backup, token
/// rename, and report the paths.
#[test]
fn migrate_file_rewrites_config_and_reports_paths() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), LEGACY_CONFIG).expect("write config");
    std::fs::write(cfg.path().join("web-token"), TOKEN).expect("write token");

    let report = migrate_file(config_path(cfg.path()).as_path(), cwd.path())
        .expect("migrate_file must succeed on a legacy config");

    assert_eq!(report.path, config_path(cfg.path()));
    assert_eq!(report.backup, backup_path(cfg.path()));

    let new_text = std::fs::read_to_string(config_path(cfg.path())).expect("read new config");
    let migrated: serde_yaml::Value = serde_yaml::from_str(&new_text).expect("parse new config");
    assert_eq!(
        migrated["general"]["web"]["token"],
        serde_yaml::Value::String(TOKEN.to_string()),
        "the web-token file must become general.web.token"
    );
    assert_eq!(
        std::fs::read(backup_path(cfg.path())).expect("read backup"),
        LEGACY_CONFIG.as_bytes(),
        "config.yaml.bak must be byte-identical to the original"
    );
}

/// A file that already uses the 0.24 layout is refused with `AlreadyCurrent`
/// (Display: the CLI's "nothing to do" sentence) and nothing is written.
#[test]
fn migrate_file_already_current_is_an_error_and_writes_nothing() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), NEW_LAYOUT_CONFIG).expect("write config");

    let err = migrate_file(config_path(cfg.path()).as_path(), cwd.path())
        .expect_err("a 0.24-layout config must be refused");
    assert!(
        err.to_string().contains("already uses the 0.24 layout"),
        "the AlreadyCurrent error must carry the 'nothing to do' sentence; got: {err}"
    );
    assert_eq!(
        std::fs::read(config_path(cfg.path())).expect("read config"),
        NEW_LAYOUT_CONFIG.as_bytes(),
        "the current config must be untouched"
    );
    assert!(
        !backup_path(cfg.path()).exists(),
        "no backup may be written for an already-current config"
    );
}

/// A migration whose result fails `Config::parse` is an error and writes
/// nothing (no config, no backup).
#[test]
fn migrate_file_invalid_result_is_an_error_and_writes_nothing() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), INVALID_MIGRATION_CONFIG).expect("write config");

    let err = migrate_file(config_path(cfg.path()).as_path(), cwd.path())
        .expect_err("an unvalidatable migration must be an error");
    assert!(
        err.to_string()
            .contains("migrated config does not validate"),
        "the error must report the unvalidatable result; got: {err}"
    );
    assert_eq!(
        std::fs::read(config_path(cfg.path())).expect("read config"),
        INVALID_MIGRATION_CONFIG.as_bytes(),
        "config.yaml must be untouched"
    );
    assert!(
        !backup_path(cfg.path()).exists(),
        "no backup may be written on a failed migration"
    );
}

/// No `web-token` file → no `general.web.token` is invented.
#[test]
fn migrate_file_without_web_token_file_emits_no_token() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), LEGACY_CONFIG).expect("write config");

    migrate_file(config_path(cfg.path()).as_path(), cwd.path())
        .expect("migrate_file must succeed on a legacy config");

    let new_text = std::fs::read_to_string(config_path(cfg.path())).expect("read new config");
    let migrated: serde_yaml::Value = serde_yaml::from_str(&new_text).expect("parse new config");
    assert!(
        migrated["general"]["web"]["token"].is_null(),
        "no token file ⇒ no general.web.token; got: {new_text}"
    );
}

/// An EMPTY `web-token` file counts as no token (§7.5 step 3: "readable and
/// non-empty") and is not injected as a blank `general.web.token` (G7 would
/// reject it).
#[test]
fn migrate_file_empty_web_token_file_injects_nothing() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), LEGACY_CONFIG).expect("write config");
    std::fs::write(cfg.path().join("web-token"), "").expect("write empty token");

    migrate_file(config_path(cfg.path()).as_path(), cwd.path())
        .expect("an empty token file must not break the migration");

    let new_text = std::fs::read_to_string(config_path(cfg.path())).expect("read new config");
    let migrated: serde_yaml::Value = serde_yaml::from_str(&new_text).expect("parse new config");
    assert!(
        migrated["general"]["web"]["token"].is_null(),
        "an empty token file must not become general.web.token; got: {new_text}"
    );
}
