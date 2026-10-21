//! RED tests for task A7′: `peakbot::config::migrate::auto_migrate` /
//! `auto_migrate_repo` — the automatic migration IO layer (spec §7.5).
//!
//! The API does not exist yet. These tests are written against the
//! interface the spec locks in §7.5:
//!
//! ```rust
//! // peakbot::config::migrate
//! pub fn auto_migrate(config_dir: &Path, repo_cwd: Option<&Path>)
//!     -> Result<AutoMigrateOutcome, AutoMigrateError>;
//! pub fn auto_migrate_repo(config_dir: &Path, cwd: &Path)
//!     -> Result<Option<Migrated>, AutoMigrateError>;
//!
//! pub struct AutoMigrateOutcome { pub global: Option<Migrated>, pub repo: Option<Migrated> }
//! impl AutoMigrateOutcome {
//!     pub fn is_noop(&self) -> bool;
//!     pub fn notices(&self) -> Vec<String>;
//! }
//! pub struct Migrated { pub path: PathBuf, pub original: Original, pub notes: Vec<String> }
//! impl Migrated { pub fn notices(&self) -> Vec<String>; }
//! pub enum Original { Created(PathBuf), Kept(PathBuf) }
//! pub struct AutoMigrateError { pub file: ConfigFileKind, pub path: PathBuf, pub reason: AutoMigrateFailure }
//! pub enum AutoMigrateFailure {
//!     Read(std::io::Error),
//!     Migrate(MigrateError),
//!     Invalid(String),
//!     GlobalNotUsable(String),
//!     Write(PathBuf, String),
//! }
//! ```
//!
//! This replaces the old file-migration tests; their web-token cases
//! reappear here as
//! `auto_migrate_moves_web_token_into_config` and
//! `auto_migrate_empty_web_token_injects_nothing`.
//!
//! All tests use tempdirs and explicit paths — no env swapping, no
//! `CONFIG_ENV_LOCK`, deterministic.

use std::path::{Path, PathBuf};

use peakbot::config::errors::ConfigFileKind;
use peakbot::config::migrate::{AutoMigrateFailure, Original, auto_migrate, auto_migrate_repo};
use peakbot::config::resolve::{Config, ParseCtx, Source};
use peakbot::config::save_config_at;

// ── fixture texts ───────────────────────────────────────────────────────────

/// A small pre-0.24 layout config; its migration produces NO notes.
const LEGACY_CONFIG: &str = "providers:\n  - name: openai\n    type: openai\n    api_key: fake-key\n    models:\n      - name: fake-model\n        alias: solo\ndefault_model: solo\npersona: You are a careful engineer.\n";

/// A legacy config with exactly one note (the delegate strip), so the header
/// gets a `# - ` line.
const NOTED_LEGACY_CONFIG: &str = "providers:\n  - name: openai\n    type: openai\n    api_key: fake-key\n    models:\n      - name: fake-model\n        alias: solo\ndefault_model: solo\ntools:\n  disabled: [delegate]\n";

const DELEGATE_NOTE: &str =
    "tools: 'delegate' was blocked; delegation is now on whenever an agent has subagents.";

/// A minimal valid 0.24-layout config (the new-only `profiles…agents` marker).
const NEW_LAYOUT_CONFIG: &str = "providers:\n  - name: openai\n    type: openai\n    api_key: fake-key\n    models:\n      - name: fake-model\n        alias: solo\nprofiles:\n  default:\n    agents:\n      main:\n        model: solo\n";

/// A legacy input that migrates fine but whose result fails `Config::parse`:
/// the pipeline's `model: missing-alias` has no provider alias (M2).
const INVALID_MIGRATION_CONFIG: &str = "providers:\n  - name: openai\n    type: openai\n    api_key: fake-key\n    models:\n      - name: fake-model\n        alias: solo\npipelines:\n  - name: dev\n    orchestrator:\n      model: missing-alias\n      prompt: p\n";

/// A legacy input `migrate` itself rejects (A6′: non-string profile key).
const MIGRATE_ERROR_CONFIG: &str = "profiles:\n  42:\n    tools:\n      only: [think]\n";

/// A legacy REPO file (0.23 top-level keys).
const LEGACY_REPO_CONFIG: &str = "default_model: solo\n";

/// A current repo file (the 0.24 repo layout: `profiles:` only).
const NEW_REPO_CONFIG: &str =
    "profiles:\n  default:\n    agents:\n      main:\n        model: solo\n";

/// Not YAML at all: the IO layer must leave it alone (`Config::load` reports).
const NOT_YAML: &str = "providers: [unclosed\n  - oops";

const TOKEN: &str = "secret-token-123";

// ── helpers ─────────────────────────────────────────────────────────────────

fn config_path(dir: &Path) -> PathBuf {
    dir.join("config.yaml")
}

fn pre024(dir: &Path) -> PathBuf {
    dir.join("config.yaml.pre-0.24")
}

fn backup_path(dir: &Path) -> PathBuf {
    dir.join("config.yaml.bak")
}

fn repo_dir(cwd: &Path) -> PathBuf {
    cwd.join(".peakbot")
}

fn repo_config(cwd: &Path) -> PathBuf {
    repo_dir(cwd).join("config.yaml")
}

fn read_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o777
}

/// The §7.5 header for a freshly created `.pre-0.24` (`name` = `config.yaml`).
fn created_header() -> String {
    format!(
        "# Migrated from the pre-0.24 layout by PeakBot {}.\n\
         # The previous file is kept as config.yaml.pre-0.24 — restore it to go back to PeakBot 0.23.\n",
        peakbot::PEAKBOT_VERSION
    )
}

/// The §7.5 header line 2 when `.pre-0.24` already existed.
fn kept_header() -> String {
    "# config.yaml.pre-0.24 already existed and was kept; the replaced file is config.yaml.bak.\n"
        .to_string()
}

/// The §7.5 display line 1 for `Original::Created` (`path` is absolute).
fn created_notice(path: &Path) -> String {
    format!(
        "ℹ Migrated {} to the 0.24 layout. The previous file is kept as {}.pre-0.24 — restore it to go back to PeakBot 0.23.",
        path.display(),
        path.display()
    )
}

/// The §7.5 display line 1 for `Original::Kept`.
fn kept_notice(path: &Path) -> String {
    format!(
        "ℹ Migrated {} to the 0.24 layout. {}.pre-0.24 already existed and was kept; the replaced file is {}.bak.",
        path.display(),
        path.display(),
        path.display()
    )
}

/// The §7.6 note E (applied-keys notice) for a profile named `name`.
fn note_e(name: &str) -> String {
    format!(
        "profile '{name}' in this repo is now a copy of the global profile '{name}' with this file's settings applied — later edits to the global '{name}' no longer reach this repo, and the other global profiles do not include this file's settings."
    )
}

fn parse_global(yaml: &str) -> Result<Config, peakbot::config::errors::ConfigErrors> {
    Config::parse(
        Source {
            path: Path::new("/home/u/.config/peakbot/config.yaml"),
            text: yaml,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
}

fn assert_no_migrate_side_effects(dir: &Path) {
    assert!(!pre024(dir).exists(), "no .pre-0.24 may be created");
    assert!(!backup_path(dir).exists(), "no .bak may be created");
}

// ===========================================================================
// Global file
// ===========================================================================

/// §11.2.1 / deliverable 1: a current global is a no-op; nothing is written.
#[test]
fn auto_migrate_current_global_is_noop_and_writes_nothing() {
    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), NEW_LAYOUT_CONFIG).expect("write config");

    let outcome = auto_migrate(cfg.path(), None).expect("current config must not error");
    assert!(outcome.is_noop(), "a current global must be a no-op");
    assert!(
        outcome.notices().is_empty(),
        "a no-op must produce no notices"
    );
    assert_eq!(
        read_bytes(&config_path(cfg.path())),
        NEW_LAYOUT_CONFIG.as_bytes(),
        "the current config must be byte-identical"
    );
    assert_no_migrate_side_effects(cfg.path());
}

/// §7.5: a missing global is `Ok(None)` — first run, not an error.
#[test]
fn auto_migrate_missing_global_is_noop() {
    let cfg = tempfile::tempdir().expect("tempdir");

    let outcome = auto_migrate(cfg.path(), None).expect("a missing config must not error");
    assert!(outcome.is_noop());
    assert_no_migrate_side_effects(cfg.path());
}

/// §11.2.9 / deliverable: a global that is not even YAML is a no-op
/// (`Config::load` reports it later).
#[test]
fn auto_migrate_unparsable_global_is_noop() {
    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), NOT_YAML).expect("write config");

    let outcome = auto_migrate(cfg.path(), None).expect("an unparsable config must not error");
    assert!(outcome.is_noop());
    assert_eq!(
        read_bytes(&config_path(cfg.path())),
        NOT_YAML.as_bytes(),
        "the unparsable config must be untouched"
    );
    assert_no_migrate_side_effects(cfg.path());
}

/// §11.2.1 / deliverable 2: a legacy global is rewritten with the EXACT
/// §7.5 header, a byte-equal 0600 `.pre-0.24`, `Original::Created` and the
/// exact `notices()` lines.
#[test]
fn auto_migrate_legacy_global_rewrites_with_exact_header_and_created_original() {
    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), NOTED_LEGACY_CONFIG).expect("write config");

    let outcome = auto_migrate(cfg.path(), None).expect("legacy config must migrate");
    assert!(!outcome.is_noop());
    let migrated = outcome
        .global
        .expect("the global must be reported as migrated");

    let path = config_path(cfg.path());
    assert_eq!(
        migrated.path, path,
        "Migrated.path must be the rewritten file"
    );
    match &migrated.original {
        Original::Created(kept) => assert_eq!(kept, &pre024(cfg.path())),
        other => panic!("a fresh migration must report Original::Created; got {other:?}"),
    }

    // The original is kept byte-equal, mode 0600.
    assert_eq!(
        read_bytes(&pre024(cfg.path())),
        NOTED_LEGACY_CONFIG.as_bytes(),
        "config.yaml.pre-0.24 must be byte-identical to the original"
    );
    #[cfg(unix)]
    assert_eq!(mode(&pre024(cfg.path())), 0o600, ".pre-0.24 must be 0600");

    // The header is exact, note lines included.
    let new_text = std::fs::read_to_string(&path).expect("read new config");
    let expected_prefix = format!("{}# - {DELEGATE_NOTE}\n", created_header());
    assert!(
        new_text.starts_with(&expected_prefix),
        "the rewritten file must start with the exact §7.5 header;\ngot:\n{new_text}"
    );
    parse_global(&new_text).expect("the migrated global must parse as a valid 0.24 config");

    // The notes: the migrate note, in order.
    assert_eq!(migrated.notes, vec![DELEGATE_NOTE.to_string()]);

    // The display lines are exact: the header line, then one bullet per note.
    assert_eq!(
        migrated.notices(),
        vec![created_notice(&path), format!("  • {DELEGATE_NOTE}")],
        "notices() must be the exact §7.5 display lines"
    );
}

/// §7.5 step 3 / ported from the old file-migration tests: the `web-token`
/// file is injected into `general.web.token` and renamed to `web-token.bak`.
#[test]
fn auto_migrate_moves_web_token_into_config() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let token_file = cfg.path().join("web-token");
    std::fs::write(config_path(cfg.path()), LEGACY_CONFIG).expect("write config");
    std::fs::write(&token_file, "  \n").expect("write token");
    std::fs::write(&token_file, format!("{TOKEN}\n")).expect("overwrite token");

    let outcome = auto_migrate(cfg.path(), None).expect("legacy config must migrate");
    let migrated = outcome.global.expect("the global must be migrated");

    let path = config_path(cfg.path());
    let new_text = std::fs::read_to_string(&path).expect("read new config");
    let migrated_doc: serde_yaml::Value =
        serde_yaml::from_str(&new_text).expect("parse new config");
    assert_eq!(
        migrated_doc["general"]["web"]["token"],
        serde_yaml::Value::String(TOKEN.to_string()),
        "the web-token file must become general.web.token"
    );
    assert!(
        !token_file.exists(),
        "the web-token file must be renamed away"
    );
    let token_bak = cfg.path().join("web-token.bak");
    assert_eq!(
        read_bytes(&token_bak),
        format!("{TOKEN}\n").as_bytes(),
        "web-token.bak must hold the original token file bytes"
    );
    // The IO note is appended after the migrate notes.
    assert_eq!(
        migrated.notes,
        vec![format!(
            "web-token file moved into general.web.token; renamed {}/web-token → web-token.bak.",
            cfg.path().display()
        )],
        "the token move must be reported as a note"
    );
    // And it is in the display lines.
    let notices = migrated.notices();
    assert_eq!(
        notices.len(),
        2,
        "one header line + one note line; got {notices:?}"
    );
    assert_eq!(notices[0], created_notice(&path));
    assert_eq!(
        notices[1],
        format!("  • {}", migrated.notes[0]),
        "note lines are '  • <note>'"
    );
}

/// Ported from the old file-migration tests: an EMPTY token file injects
/// nothing (a blank token would fail G7).
#[test]
fn auto_migrate_empty_web_token_injects_nothing() {
    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), LEGACY_CONFIG).expect("write config");
    std::fs::write(cfg.path().join("web-token"), "").expect("write empty token");

    auto_migrate(cfg.path(), None).expect("an empty token file must not break the migration");

    let new_text = std::fs::read_to_string(config_path(cfg.path())).expect("read new config");
    let migrated_doc: serde_yaml::Value =
        serde_yaml::from_str(&new_text).expect("parse new config");
    assert!(
        migrated_doc["general"]["web"].is_null()
            || migrated_doc["general"]["web"]["token"].is_null(),
        "an empty token file must not become general.web.token; got: {new_text}"
    );
    assert!(
        !cfg.path().join("web-token.bak").exists(),
        "no token file was moved, so no web-token.bak may exist"
    );
}

/// §11.2.2 / deliverable 3: a second call is a no-op and the three files are
/// byte-identical (mtime untouched is implied by the byte check: a rewrite
/// would change the bytes).
#[test]
fn auto_migrate_second_call_is_noop_and_bytes_unchanged() {
    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), LEGACY_CONFIG).expect("write config");

    auto_migrate(cfg.path(), None).expect("first migration");
    let (config_bytes, pre_bytes, bak_bytes) = (
        read_bytes(&config_path(cfg.path())),
        read_bytes(&pre024(cfg.path())),
        read_bytes(&backup_path(cfg.path())),
    );

    let outcome = auto_migrate(cfg.path(), None).expect("second call must not error");
    assert!(outcome.is_noop(), "a second call must be a no-op");
    assert!(outcome.notices().is_empty());
    assert_eq!(read_bytes(&config_path(cfg.path())), config_bytes);
    assert_eq!(read_bytes(&pre024(cfg.path())), pre_bytes);
    assert_eq!(read_bytes(&backup_path(cfg.path())), bak_bytes);
}

/// §7.5 `save_config_at` fixes / deliverable 4: a later form save leaves
/// `.pre-0.24` untouched and writes a 0600 `.bak` even from a 0644 source.
#[test]
#[cfg(unix)]
fn save_config_at_keeps_pre_0_24_and_bak_is_0600_from_0644_source() {
    use std::os::unix::fs::PermissionsExt;

    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), LEGACY_CONFIG).expect("write config");
    auto_migrate(cfg.path(), None).expect("first migration");
    let pre_bytes = read_bytes(&pre024(cfg.path()));

    // A 0644 source file (a form save over the migrated config).
    let migrated_bytes = read_bytes(&config_path(cfg.path()));
    std::fs::set_permissions(
        config_path(cfg.path()),
        std::fs::Permissions::from_mode(0o644),
    )
    .expect("chmod 0644");
    save_config_at(cfg.path(), NEW_LAYOUT_CONFIG).expect("save_config_at");

    assert_eq!(
        read_bytes(&pre024(cfg.path())),
        pre_bytes,
        "a form save must never touch config.yaml.pre-0.24"
    );
    let bak = backup_path(cfg.path());
    assert_eq!(
        read_bytes(&bak),
        migrated_bytes,
        "the .bak must hold the replaced (0644) file"
    );
    assert_eq!(
        mode(&bak),
        0o600,
        "the .bak must be 0600 whatever the source mode"
    );
}

/// §11.2.3 / deliverable 5: a pre-existing `.pre-0.24` is kept byte-for-byte,
/// reported as `Original::Kept`, and the header/notice use the "already
/// existed" variant. The replaced file is in `.bak`.
#[test]
fn auto_migrate_keeps_existing_pre_0_24_and_reports_kept() {
    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), LEGACY_CONFIG).expect("write config");
    let sentinel = b"the TRUE original from a previous 0.24 boot\n";
    std::fs::write(pre024(cfg.path()), sentinel).expect("pre-create .pre-0.24");

    let outcome = auto_migrate(cfg.path(), None).expect("second migration");
    let migrated = outcome.global.expect("the global must be migrated");

    let path = config_path(cfg.path());
    match &migrated.original {
        Original::Kept(kept) => assert_eq!(kept, &pre024(cfg.path())),
        other => panic!("an existing .pre-0.24 must report Original::Kept; got {other:?}"),
    }
    assert_eq!(
        read_bytes(&pre024(cfg.path())),
        sentinel,
        "the pre-existing .pre-0.24 must be untouched"
    );
    assert_eq!(
        read_bytes(&backup_path(cfg.path())),
        LEGACY_CONFIG.as_bytes(),
        "the replaced legacy file must be in config.yaml.bak"
    );

    let new_text = std::fs::read_to_string(&path).expect("read new config");
    let expected_prefix = format!(
        "# Migrated from the pre-0.24 layout by PeakBot {}.\n{}",
        peakbot::PEAKBOT_VERSION,
        kept_header()
    );
    assert!(
        new_text.starts_with(&expected_prefix),
        "the header must use the 'already existed' line 2;\ngot:\n{new_text}"
    );
    assert_eq!(
        migrated.notices(),
        vec![kept_notice(&path)],
        "notices() must use the 'already existed' display line"
    );
}

/// §11.2.4 / deliverable 6: a migration whose result fails `Config::parse`
/// is `Err(Global, Invalid)` and writes NOTHING (validate before any write).
#[test]
fn auto_migrate_invalid_migration_is_global_invalid_and_writes_nothing() {
    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), INVALID_MIGRATION_CONFIG).expect("write config");

    let err =
        auto_migrate(cfg.path(), None).expect_err("an unvalidatable migration must be an error");
    assert_eq!(
        err.file,
        ConfigFileKind::Global,
        "a failed global migration must carry file == Global"
    );
    assert_eq!(
        err.path,
        config_path(cfg.path()),
        "the error must name the file"
    );
    assert!(
        matches!(err.reason, AutoMigrateFailure::Invalid(_)),
        "a parse failure of the migrated file must be Invalid; got {:?}",
        err.reason
    );
    assert!(
        err.to_string().starts_with(&format!(
            "could not migrate {} to the 0.24 layout: the migrated file does not validate:",
            config_path(cfg.path()).display()
        )),
        "the error Display must be the §7.5 sentence; got: {err}"
    );
    assert_eq!(
        read_bytes(&config_path(cfg.path())),
        INVALID_MIGRATION_CONFIG.as_bytes(),
        "config.yaml must be untouched"
    );
    assert_no_migrate_side_effects(cfg.path());
}

/// §11.2.4: a `MigrateError` (not an `Invalid`) is also `Err(Global, …)` and
/// writes nothing.
#[test]
fn auto_migrate_migrate_error_is_global_and_writes_nothing() {
    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), MIGRATE_ERROR_CONFIG).expect("write config");

    let err = auto_migrate(cfg.path(), None).expect_err("a MigrateError must be an error");
    assert_eq!(err.file, ConfigFileKind::Global);
    assert!(
        matches!(err.reason, AutoMigrateFailure::Migrate(_)),
        "a migrate() failure must be Migrate; got {:?}",
        err.reason
    );
    assert_eq!(
        read_bytes(&config_path(cfg.path())),
        MIGRATE_ERROR_CONFIG.as_bytes(),
        "config.yaml must be untouched"
    );
    assert_no_migrate_side_effects(cfg.path());
}

// ===========================================================================
// Repo file (boot step, `auto_migrate` with `repo_cwd`)
// ===========================================================================

/// §11.2.5 / deliverable 7: a legacy global AND a legacy repo file are both
/// migrated in one call; the repo is validated against the MIGRATED global;
/// the repo gets its own `.pre-0.24`.
#[test]
fn auto_migrate_migrates_legacy_global_and_repo_together() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), LEGACY_CONFIG).expect("write global");
    std::fs::create_dir_all(repo_dir(cwd.path())).expect("create .peakbot");
    std::fs::write(repo_config(cwd.path()), LEGACY_REPO_CONFIG).expect("write repo");

    let outcome = auto_migrate(cfg.path(), Some(cwd.path())).expect("both files must migrate");
    assert!(!outcome.is_noop());
    assert!(outcome.global.is_some(), "the global must be migrated");
    let repo = outcome
        .repo
        .as_ref()
        .expect("the repo file must be migrated");

    let repo_path = repo_config(cwd.path());
    assert_eq!(repo.path, repo_path);
    match &repo.original {
        Original::Created(kept) => {
            assert_eq!(kept, &repo_dir(cwd.path()).join("config.yaml.pre-0.24"))
        }
        other => panic!("a fresh repo migration must report Original::Created; got {other:?}"),
    }
    assert_eq!(
        read_bytes(&repo_dir(cwd.path()).join("config.yaml.pre-0.24")),
        LEGACY_REPO_CONFIG.as_bytes(),
        "the repo .pre-0.24 must be byte-identical to the original repo file"
    );

    // The repo profile shadows the first global profile, named `default`.
    let repo_text = std::fs::read_to_string(&repo_path).expect("read new repo");
    let repo_doc: serde_yaml::Value = serde_yaml::from_str(&repo_text).expect("parse repo");
    assert_eq!(
        repo_doc["profiles"]["default"]["agents"]["main"]["model"],
        serde_yaml::Value::String("solo".into()),
        "the repo default_model must be applied to the first global profile's main agent"
    );
    // Validated against the MIGRATED global.
    let global_text = std::fs::read_to_string(config_path(cfg.path())).expect("read global");
    Config::parse(
        Source {
            path: Path::new("/home/u/.config/peakbot/config.yaml"),
            text: &global_text,
        },
        Some(Source {
            path: Path::new("/home/u/repo/.peakbot/config.yaml"),
            text: &repo_text,
        }),
        &ParseCtx { known_skills: &[] },
    )
    .expect("the migrated repo must validate against the migrated global");

    // The repo notes: note E only (default_model applied, single agent).
    assert_eq!(repo.notes, vec![note_e("default")]);
    // The outcome notices: global's lines, then the repo's.
    let notices = outcome.notices();
    assert_eq!(
        notices,
        vec![
            created_notice(&config_path(cfg.path())),
            created_notice(&repo_path),
            format!("  • {}", note_e("default")),
        ],
        "notices() must be the global lines then the repo lines, exact"
    );
}

/// §11.2.5: `repo_cwd = None` (a `--profile` pin) leaves a legacy repo file
/// untouched.
#[test]
fn auto_migrate_repo_cwd_none_leaves_repo_untouched() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), LEGACY_CONFIG).expect("write global");
    std::fs::create_dir_all(repo_dir(cwd.path())).expect("create .peakbot");
    std::fs::write(repo_config(cwd.path()), LEGACY_REPO_CONFIG).expect("write repo");

    let outcome = auto_migrate(cfg.path(), None).expect("global must migrate");
    assert!(outcome.global.is_some());
    assert!(outcome.repo.is_none(), "no repo step was asked for");
    assert_eq!(
        read_bytes(&repo_config(cwd.path())),
        LEGACY_REPO_CONFIG.as_bytes(),
        "the repo file must be untouched when repo_cwd is None"
    );
    assert!(!repo_dir(cwd.path()).join("config.yaml.pre-0.24").exists());
}

/// §11.2.6 / deliverable 8: a MISSING global skips the repo step — boot falls
/// through to the normal missing-config handling.
#[test]
fn auto_migrate_missing_global_skips_repo_step() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(repo_dir(cwd.path())).expect("create .peakbot");
    std::fs::write(repo_config(cwd.path()), LEGACY_REPO_CONFIG).expect("write repo");

    let outcome =
        auto_migrate(cfg.path(), Some(cwd.path())).expect("a missing global must not error");
    assert!(
        outcome.is_noop(),
        "missing global ⇒ global None and repo None"
    );
    assert_eq!(
        read_bytes(&repo_config(cwd.path())),
        LEGACY_REPO_CONFIG.as_bytes(),
        "the repo file must be untouched without a global"
    );
    assert!(!repo_dir(cwd.path()).join("config.yaml.pre-0.24").exists());
}

/// §11.2.6 / deliverable 8: `auto_migrate_repo` with a missing global is
/// `GlobalNotUsable` and touches nothing.
#[test]
fn auto_migrate_repo_missing_global_is_global_not_usable() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(repo_dir(cwd.path())).expect("create .peakbot");
    std::fs::write(repo_config(cwd.path()), LEGACY_REPO_CONFIG).expect("write repo");

    let err = auto_migrate_repo(cfg.path(), cwd.path())
        .expect_err("a missing global must refuse the repo migration");
    assert_eq!(err.file, ConfigFileKind::Repo);
    assert!(
        matches!(err.reason, AutoMigrateFailure::GlobalNotUsable(ref msg) if msg.contains("does not exist")),
        "a missing global must be GlobalNotUsable('<path> does not exist'); got {:?}",
        err.reason
    );
    assert_eq!(
        read_bytes(&repo_config(cwd.path())),
        LEGACY_REPO_CONFIG.as_bytes(),
        "the repo file must be untouched"
    );
}

/// §11.2.7: a legacy GLOBAL at reload is `GlobalNotUsable` (the L1 failure
/// text) and the global is NEVER migrated by reload.
#[test]
fn auto_migrate_repo_with_legacy_global_is_global_not_usable_and_touches_nothing() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), LEGACY_CONFIG).expect("write global");
    std::fs::create_dir_all(repo_dir(cwd.path())).expect("create .peakbot");
    std::fs::write(repo_config(cwd.path()), LEGACY_REPO_CONFIG).expect("write repo");

    let err = auto_migrate_repo(cfg.path(), cwd.path())
        .expect_err("a legacy global is not usable for a repo migration");
    assert_eq!(err.file, ConfigFileKind::Repo);
    assert!(
        matches!(err.reason, AutoMigrateFailure::GlobalNotUsable(ref msg)
            if msg.contains("config.yaml uses the pre-0.24 layout")),
        "the reason must carry the global's L1 failure; got {:?}",
        err.reason
    );
    assert_eq!(
        read_bytes(&config_path(cfg.path())),
        LEGACY_CONFIG.as_bytes(),
        "reload must never migrate the global file"
    );
    assert_eq!(
        read_bytes(&repo_config(cwd.path())),
        LEGACY_REPO_CONFIG.as_bytes(),
        "the repo file must be untouched"
    );
}

// ===========================================================================
// `auto_migrate_repo` (reload step)
// ===========================================================================

/// Deliverable 9: a current repo file is a no-op.
#[test]
fn auto_migrate_repo_current_repo_is_noop() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), NEW_LAYOUT_CONFIG).expect("write global");
    std::fs::create_dir_all(repo_dir(cwd.path())).expect("create .peakbot");
    std::fs::write(repo_config(cwd.path()), NEW_REPO_CONFIG).expect("write repo");

    let outcome = auto_migrate_repo(cfg.path(), cwd.path()).expect("current repo must not error");
    assert!(outcome.is_none(), "a current repo file must be a no-op");
    assert_eq!(
        read_bytes(&repo_config(cwd.path())),
        NEW_REPO_CONFIG.as_bytes(),
        "the repo file must be untouched"
    );
    assert!(!repo_dir(cwd.path()).join("config.yaml.pre-0.24").exists());
    assert_eq!(
        read_bytes(&config_path(cfg.path())),
        NEW_LAYOUT_CONFIG.as_bytes(),
        "reload must never touch the global file"
    );
}

/// A legacy repo file with a current global is migrated by reload, with its
/// own header and `.pre-0.24`.
#[test]
fn auto_migrate_repo_migrates_legacy_repo() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::fs::write(config_path(cfg.path()), NEW_LAYOUT_CONFIG).expect("write global");
    std::fs::create_dir_all(repo_dir(cwd.path())).expect("create .peakbot");
    std::fs::write(repo_config(cwd.path()), LEGACY_REPO_CONFIG).expect("write repo");

    let repo = auto_migrate_repo(cfg.path(), cwd.path())
        .expect("a legacy repo must migrate")
        .expect("the repo must be reported as migrated");

    let repo_path = repo_config(cwd.path());
    assert_eq!(repo.path, repo_path);
    assert_eq!(
        read_bytes(&repo_dir(cwd.path()).join("config.yaml.pre-0.24")),
        LEGACY_REPO_CONFIG.as_bytes(),
        "the repo .pre-0.24 must be byte-identical to the original"
    );
    let repo_text = std::fs::read_to_string(&repo_path).expect("read new repo");
    assert!(
        repo_text.starts_with(&created_header()),
        "the repo file must carry the same §7.5 header;\ngot:\n{repo_text}"
    );
    assert_eq!(
        repo.notices(),
        vec![
            created_notice(&repo_path),
            format!("  • {}", note_e("default"))
        ]
    );
}
