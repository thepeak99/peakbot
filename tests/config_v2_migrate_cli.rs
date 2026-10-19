//! RED tests for task A7: the `migrate_file` IO wrapper (§7.5) and the
//! `peakbot config migrate` subcommand.
//!
//! The subcommand does not exist yet. The binary is invoked through
//! `CARGO_BIN_EXE_peakbot` with `XDG_CONFIG_HOME` pointed at a tempdir, so
//! the tests never touch the real config. (The in-process `migrate_file`
//! tests pass the paths explicitly and need no env swapping.)
//!
//! The in-process `migrate_file` tests (and the assumed `MigrateReport` API)
//! live in `config_v2_migrate_file.rs`.
//!
//! Pinned CLI behaviour (§7.5):
//! - exit 0 on success AND when the file is already current;
//! - the rewritten file starts with the header comment
//!   `# migrated from the pre-0.24 layout by peakbot <version> — previous file: config.yaml.bak`;
//! - `config.yaml.bak` is byte-identical to the original;
//! - a non-empty `web-token` file becomes `general.web.token` and is renamed
//!   to `web-token.bak`;
//! - a legacy per-repo `.peakbot/config.yaml` in `cwd` gets a note and is
//!   NOT touched;
//! - a migration whose result fails `Config::parse` exits 1 and writes
//!   nothing.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Output;

/// A throwaway `XDG_CONFIG_HOME` plus the config dir PeakBot derives from it.
/// `get_config_dir()` is `ProjectDirs("com","peakbot","peakbot")`, which on
/// Linux resolves to `$XDG_CONFIG_HOME/peakbot` — so the files live one level
/// below the tempdir. (Linux-only: other platforms ignore `XDG_CONFIG_HOME`.)
struct Sandbox {
    _xdg: tempfile::TempDir,
    dir: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let xdg = tempfile::tempdir().expect("tempdir");
        let dir = xdg.path().join("peakbot");
        std::fs::create_dir_all(&dir).expect("mkdir config dir");
        Self { _xdg: xdg, dir }
    }

    fn dir(&self) -> &Path {
        &self.dir
    }
}

/// A small pre-0.24 layout config: legacy top-level keys, no `general`/`mcps`.
const LEGACY_CONFIG: &str = "providers:\n  - name: openai\n    type: openai\n    api_key: fake-key\n    models:\n      - name: fake-model\n        alias: solo\ndefault_model: solo\npersona: You are a careful engineer.\n";

/// Legacy input whose migration CANNOT validate: `context.threshold: 5` is
/// copied verbatim to `general.context` and violates G4 (0 < x ≤ 1).
const INVALID_MIGRATION_CONFIG: &str = "providers:\n  - name: openai\n    type: openai\n    api_key: fake-key\n    models:\n      - name: fake-model\n        alias: solo\ndefault_model: solo\ncontext:\n  threshold: 5\n";

/// The per-repo legacy file: has a non-`profiles` top-level key.
const REPO_LEGACY_CONFIG: &str =
    "default_model: solo\nprofiles:\n  web:\n    mcp_servers:\n      only: [alpha]\n";

const TOKEN: &str = "secret-token-123";

fn run_migrate(config_dir: &Path, cwd: &Path) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_peakbot"))
        .args(["config", "migrate"])
        .env(
            "XDG_CONFIG_HOME",
            config_dir.parent().expect("config dir has a parent"),
        )
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("spawn peakbot: {e}"))
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn config_path(config_dir: &Path) -> PathBuf {
    config_dir.join("config.yaml")
}

fn backup_path(config_dir: &Path) -> PathBuf {
    config_dir.join("config.yaml.bak")
}

// ===========================================================================
// CLI: `peakbot config migrate`
// ===========================================================================

/// Happy path: a legacy `config.yaml` plus a `web-token` file are migrated —
/// exit 0, header comment, byte-identical `.bak`, token in `general.web.token`,
/// `web-token` renamed to `web-token.bak`, and the CLI prints path + backup.
#[test]
fn cli_migrate_rewrites_legacy_config_writes_backup_and_moves_token() {
    let cfg = Sandbox::new();
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.dir()), LEGACY_CONFIG).expect("write config");
    std::fs::write(cfg.dir().join("web-token"), TOKEN).expect("write token");

    let out = run_migrate(cfg.dir(), cwd.path());
    assert!(
        out.status.success(),
        "exit 0 expected; stdout: {}\nstderr: {}",
        stdout_of(&out),
        stderr_of(&out)
    );

    let new_text = std::fs::read_to_string(config_path(cfg.dir())).expect("read new config");
    let version = env!("CARGO_PKG_VERSION");
    let header = format!(
        "# migrated from the pre-0.24 layout by peakbot {version} — previous file: config.yaml.bak"
    );
    assert!(
        new_text.starts_with(&header),
        "config.yaml must start with the migration header; first line was: {}",
        new_text.lines().next().unwrap_or("<empty>")
    );

    let bak = std::fs::read(backup_path(cfg.dir())).expect("read backup");
    assert_eq!(
        bak,
        LEGACY_CONFIG.as_bytes(),
        "config.yaml.bak must be byte-identical to the original"
    );

    let token_path = cfg.dir().join("web-token");
    let token_bak = cfg.dir().join("web-token.bak");
    assert!(!token_path.exists(), "web-token must be renamed away");
    assert_eq!(
        std::fs::read(&token_bak).expect("read web-token.bak"),
        TOKEN.as_bytes(),
        "web-token.bak must keep the original token bytes"
    );

    let migrated: serde_yaml::Value = serde_yaml::from_str(&new_text).expect("parse new config");
    assert_eq!(
        migrated["general"]["web"]["token"],
        serde_yaml::Value::String(TOKEN.to_string()),
        "the web-token file must become general.web.token"
    );

    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("config.yaml"),
        "the CLI must print the config path; stdout: {stdout}"
    );
    assert!(
        stdout.contains("config.yaml.bak"),
        "the CLI must print the backup path; stdout: {stdout}"
    );
    assert!(
        stdout.contains("web-token file moved into general.web.token"),
        "the CLI must print the web-token note; stdout: {stdout}"
    );
}

/// Idempotency: a second run exits 0, says the file is already current, and
/// changes nothing.
#[test]
fn cli_migrate_second_run_reports_already_current_and_changes_nothing() {
    let cfg = Sandbox::new();
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.dir()), LEGACY_CONFIG).expect("write config");

    let first = run_migrate(cfg.dir(), cwd.path());
    assert!(
        first.status.success(),
        "first run must succeed; stderr: {}",
        stderr_of(&first)
    );
    let after_first = std::fs::read(config_path(cfg.dir())).expect("read config");

    let second = run_migrate(cfg.dir(), cwd.path());
    assert!(
        second.status.success(),
        "second run must exit 0; stdout: {}\nstderr: {}",
        stdout_of(&second),
        stderr_of(&second)
    );
    let stdout = stdout_of(&second);
    assert!(
        stdout.contains("config.yaml already uses the 0.24 layout — nothing to do."),
        "second run must report the file is already current; stdout: {stdout}"
    );
    assert_eq!(
        std::fs::read(config_path(cfg.dir())).expect("read config again"),
        after_first,
        "the second run must not modify config.yaml"
    );
}

/// A legacy per-repo `.peakbot/config.yaml` in `cwd` is only noted — never
/// migrated.
#[test]
fn cli_migrate_legacy_repo_file_gets_note_and_is_untouched() {
    let cfg = Sandbox::new();
    let repo = tempfile::tempdir().expect("tempdir");
    let repo_dir = repo.path().join(".peakbot");
    std::fs::create_dir_all(&repo_dir).expect("mkdir .peakbot");
    let repo_cfg = repo_dir.join("config.yaml");
    std::fs::write(&repo_cfg, REPO_LEGACY_CONFIG).expect("write repo config");

    std::fs::write(config_path(cfg.dir()), LEGACY_CONFIG).expect("write config");

    let out = run_migrate(cfg.dir(), repo.path());
    assert!(
        out.status.success(),
        "exit 0 expected; stdout: {}\nstderr: {}",
        stdout_of(&out),
        stderr_of(&out)
    );

    assert_eq!(
        std::fs::read(&repo_cfg).expect("read repo config"),
        REPO_LEGACY_CONFIG.as_bytes(),
        "the repo file must be left untouched"
    );

    let stdout = stdout_of(&out);
    assert!(
        stdout.contains(
            "uses the old layout and was NOT migrated — a repo config may now only contain profiles:"
        ),
        "the CLI must note the legacy repo file; stdout: {stdout}"
    );
}

/// A legacy input whose migration would not validate → exit 1, nothing written.
#[test]
fn cli_migrate_invalid_migration_exits_1_and_writes_nothing() {
    let cfg = Sandbox::new();
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.dir()), INVALID_MIGRATION_CONFIG).expect("write config");

    let out = run_migrate(cfg.dir(), cwd.path());
    assert!(
        !out.status.success(),
        "exit 1 expected; stdout: {}",
        stdout_of(&out)
    );
    assert_eq!(
        std::fs::read(config_path(cfg.dir())).expect("read config"),
        INVALID_MIGRATION_CONFIG.as_bytes(),
        "config.yaml must be untouched on a failed migration"
    );
    assert!(
        !backup_path(cfg.dir()).exists(),
        "no config.yaml.bak may be written on a failed migration"
    );
    let combined = format!("{}{}", stdout_of(&out), stderr_of(&out));
    assert!(
        combined.contains("migrated config does not validate"),
        "the failure must say the migrated config does not validate; output: {combined}"
    );
}
