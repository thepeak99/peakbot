//! RED integration tests for the 0.24 config runtime cutover — the BOOT /
//! migration / token items of spec §11.3 (plan task T6T; written before the
//! cutover, M1 only).
//!
//! Item → test map:
//! ```text
//!  - §11.3 item 16 (boot auto-migration, B2):
//!      `boot_auto_migrates_legacy_global`            legacy global → `.pre-0.24`
//!                                                     backup + web-token →
//!                                                     `general.web.token` +
//!                                                     `web-token.bak`, then
//!                                                     `Config::load` succeeds
//!      `boot_migration_notice_lines_match_stderr_contract`
//!                                                     the `ℹ Migrated …` line
//!                                                     + `•` notes, verbatim
//!      `boot_notice_lines_reach_first_session_only`  first session shows them,
//!                                                     second does not
//!                                                     (ASSUMED API)
//!      `boot_migrates_legacy_repo_file_in_boot_cwd`  a legacy repo file in the
//!                                                     boot cwd migrates in the
//!                                                     same boot
//!      `boot_pin_skips_repo_migration`               under `--profile` the repo
//!                                                     file is untouched
//!      `boot_failed_migration_keeps_file_and_reports_could_not_migrate`
//!                                                     failed migration →
//!                                                     `could not migrate …`,
//!                                                     file unchanged
//!      `pin_unknown_profile_message`                 `--profile` with an unknown
//!                                                     profile: exact message
//!  - §11.3 item 17 (reload repo migration, B7 — the migration step):
//!      `cd_migrates_legacy_repo_file`                `/cd` target's legacy repo
//!                                                     file migrates; its profile
//!                                                     is the default (zero turns)
//!      `cd_failed_repo_migration_reports_could_not_migrate`
//!  - §11.3 item 10 (web token chain):
//!      `token_chain_cli_env_config`                  cli > env > config
//!                                                     (ASSUMED API)
//!      `service_install_token_migrates_legacy_file_first`
//!                                                     `service install --token`
//!                                                     on a legacy file: migrate
//!                                                     first (`.pre-0.24` holds
//!                                                     the true legacy bytes),
//!                                                     then the token is set
//!  - §11.3 item 14 (cost tracking):
//!      `cost_tracking_only_openrouter`               on for OpenRouter, off for
//!                                                     any other provider even
//!                                                     with `general.cost_tracking: true`
//!                                                     (ASSUMED API)
//!  - §11.3 item 3 (persistence, load part):
//!      `old_convo_pipeline_field_loads_as_agent`     old JSON `"pipeline"`
//!                                                     loads as `agent`; a new
//!                                                     save writes `"agent"`
//!                                                     (ASSUMED API)
//! ```
//!
//! ASSUMED API — written against the config-v2-plan.md contract; this file
//! will not compile until the cutover lands (plan: "Won't compile until T12"):
//!  1. `peakbot::config::resolve_token(cli: Option<&str>, env: Option<&str>,
//!     config: Option<&str>) -> Option<String>` — T9b: `resolve_token(cli,
//!     env, general.web.token)`; the env value is `$PEAKBOT_WEB_TOKEN` (see
//!     the `WebConfig.token` doc). Chain: cli > env > config.
//!  2. `peakbot::Conversation.agent: Option<String>` — T9a: "conversation.rs
//!     `agent` field with serde alias=\"pipeline\""; the old `pipeline` field
//!     is deleted, old files load through the alias.
//!  3. `peakbot::session::SessionDeps.boot_notices: std::sync::Mutex<Vec<String>>`
//!     — T9a: boot notices go to stderr + `SessionDeps.boot_notices`, taken
//!     by the first session (`mem::take`). Pinned here as a compile-time
//!     contract (`boot_notices_take`); the runtime take-once behaviour needs
//!     a real `SessionDeps` and is a gap until T9a.
//!  4. `peakbot::config::cost_tracking_enabled(cost_tracking: bool,
//!     model: &ResolvedModel) -> bool` — T7: "cost tracking =
//!     general.cost_tracking && OpenRouter". The plan does not name the seam;
//!     assumed to be a pure helper in `peakbot::config`. The contract is the
//!     assertion; rename freely.
//!
//! Gaps (not testable from a lib integration test at M1):
//!  - The boot exit copy `PeakBot cannot start with this file. …` + exit 1
//!    (main.rs binary seam). The `AutoMigrateError` Display asserted here is
//!    the `could not migrate …` text boot must print before it.
//!  - Windowed web `needs_setup` + `legacy_layout: true` + `migrate_error`
//!    (item 16 web part; C6, needs the web server).
//!  - The `⚠ could not migrate … — ignoring this repo's profiles.` reload
//!    copy + global-only catalog (item 17; B7 `reload_session_config`).
//!  - TLS = `--tls` || `general.web.tls` and the LAN-bind boot refusal naming
//!    `general.web.token` (item 10 web part; T9b, no named lib seam).
//!  - `service install` unit rendering without `--bind`/`--tls` (item 10;
//!    lives in `tests/install_render_tests.rs`, not the boot path).
//!
//! No network. Env isolation: `XDG_CONFIG_HOME` under a file-local lock
//! (`CONFIG_ENV_LOCK` is `pub(crate)`, so this crate keeps its own).

use std::path::{Path, PathBuf};

use peakbot::Conversation;
use peakbot::config::errors::ConfigFileKind;
use peakbot::config::migrate::{Original, auto_migrate, auto_migrate_repo, is_legacy};
use peakbot::config::resolve::{Config, LoadError, ParseCtx, RepoMode, Source};
use peakbot::config::{get_config_dir, save_config_at};

// ── env isolation (same pattern as the T1 `TempHome` in resolve.rs) ─────────

static ENV_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

/// Restores `XDG_CONFIG_HOME` on drop, so a panic cannot leak env state.
struct XdgGuard(Option<String>);

impl XdgGuard {
    fn new(dir: &Path) -> Self {
        let original = std::env::var("XDG_CONFIG_HOME").ok();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir) };
        Self(original)
    }
}

impl Drop for XdgGuard {
    fn drop(&mut self) {
        match &self.0 {
            Some(value) => unsafe { std::env::set_var("XDG_CONFIG_HOME", value) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
    }
}

/// `XDG_CONFIG_HOME` pointed at a fresh tempdir, plus a session cwd tempdir.
/// Field order is drop order: restore the env, delete the dirs, release the
/// lock. A failing test must not poison the lock for the others.
struct TempHome {
    _guard: XdgGuard,
    _xdg: tempfile::TempDir,
    _cwd: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl TempHome {
    fn new() -> Self {
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _xdg = tempfile::tempdir().expect("xdg tempdir");
        let _guard = XdgGuard::new(_xdg.path());
        let _cwd = tempfile::tempdir().expect("cwd tempdir");
        Self {
            _guard,
            _xdg,
            _cwd,
            _lock,
        }
    }

    fn config_dir(&self) -> PathBuf {
        get_config_dir().expect("config dir under XDG_CONFIG_HOME")
    }

    fn cwd(&self) -> &Path {
        self._cwd.path()
    }

    fn global_path(&self) -> PathBuf {
        self.config_dir().join("config.yaml")
    }

    fn repo_path(&self) -> PathBuf {
        self.cwd().join(".peakbot").join("config.yaml")
    }

    fn write_global(&self, text: &str) -> Vec<u8> {
        let dir = self.config_dir();
        std::fs::create_dir_all(&dir).expect("config dir");
        std::fs::write(self.global_path(), text).expect("write global config");
        text.as_bytes().to_vec()
    }

    fn write_web_token(&self, token: &str) {
        std::fs::write(self.config_dir().join("web-token"), token).expect("write web-token");
    }

    fn write_repo(&self, text: &str) -> Vec<u8> {
        let dir = self.cwd().join(".peakbot");
        std::fs::create_dir_all(&dir).expect(".peakbot dir");
        std::fs::write(self.repo_path(), text).expect("write repo config");
        text.as_bytes().to_vec()
    }
}

fn ctx() -> ParseCtx<'static> {
    ParseCtx { known_skills: &[] }
}

// ── fixture texts ───────────────────────────────────────────────────────────

/// A pre-0.24 global config: the 0.23 single-provider shape, a `persona`,
/// no legacy-only oddities — its migration produces no notes of its own.
/// `default_model` names the ALIAS (0.23 required an alias, not a model
/// name) — a faithful 0.23 file.
const LEGACY_GLOBAL: &str = "providers:\n  - name: openrouter\n    type: openrouter\n    api_key: fake-key\n    models:\n      - name: anthropic/claude-3.7-sonnet\n        alias: default\ndefault_model: default\npersona: You are a careful engineer.\n";

/// A pre-0.24 global whose migration MUST fail: a `pipelines` entry without
/// a `name` (`MigrateError::PipelineWithoutName(0)`).
const LEGACY_GLOBAL_BAD: &str = "providers:\n  - name: openrouter\n    type: openrouter\n    api_key: fake-key\n    models:\n      - name: anthropic/claude-3.7-sonnet\n        alias: default\ndefault_model: default\npipelines: [{}]\n";

/// A valid 0.24 global: two profiles (`dev` first), two models.
const GLOBAL_024: &str = "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - { name: anthropic/claude-sonnet-4.5, alias: sonnet }\nprofiles:\n  dev:\n    agents:\n      main:\n        model: sonnet\n  research:\n    agents:\n      main:\n        model: sonnet\n";

/// A pre-0.24 repo overlay on the global `dev` profile. 0.23 repo files
/// carried TOP-LEVEL keys (a `profiles:` block was a no-op there — the file
/// never had one), so the overlay is a bare `system_prompt:`.
const LEGACY_REPO: &str = "system_prompt: Repo persona text.\n";

/// A pre-0.24 repo overlay whose migration MUST fail (`pipelines: [{}]`).
const LEGACY_REPO_BAD: &str = "system_prompt: Repo persona text.\npipelines: [{}]\n";

/// 0.24 global with an OpenRouter model and a non-OpenRouter model.
const COST_YAML: &str = "providers:\n  - name: openrouter\n    type: openrouter\n    api_key: fake-key\n    models:\n      - { name: anthropic/claude-sonnet-4.5, alias: sonnet }\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - { name: anthropic/claude-sonnet-4.5, alias: pnsonnet }\ngeneral:\n  cost_tracking: true\nprofiles:\n  dev:\n    agents:\n      or:\n        model: sonnet\n      other:\n        model: pnsonnet\n";

/// An old (pre-cutover) conversation file: the `pipeline` field, `profile`
/// null. Every other field is required by `Conversation`.
const OLD_CONVO_JSON: &str = r#"{
  "id": "00000000-0000-0000-0000-000000000000",
  "name": "old conversation",
  "created_at": "2026-01-01T00:00:00Z",
  "updated_at": "2026-01-01T00:00:00Z",
  "messages": [],
  "provider_name": "openrouter",
  "model": "anthropic/claude-3.7-sonnet",
  "metadata": {},
  "pipeline": "dev-team",
  "profile": null
}"#;

// ── item 16: boot auto-migration (B2) ───────────────────────────────────────

/// §11.3 item 16: a legacy global in a tempdir config dir auto-migrates at
/// boot step 0: the pre-0.24 backup is created with the true legacy bytes,
/// the `web-token` file moves into `general.web.token` (and is renamed
/// `web-token.bak`), and `Config::load` then succeeds where it reported
/// `LoadError::Legacy` before.
#[test]
fn boot_auto_migrates_legacy_global() {
    let home = TempHome::new();
    let original = home.write_global(LEGACY_GLOBAL);
    home.write_web_token("sekret-token\n");

    // Boot precondition: the legacy file is `Legacy`, not `Invalid`.
    let pre = Config::load(home.cwd(), RepoMode::Include, &ctx()).expect_err("legacy file");
    assert!(
        matches!(pre, LoadError::Legacy(p) if p == home.global_path()),
        "a legacy global is LoadError::Legacy before boot step 0"
    );

    // Boot step 0 (no pin): `auto_migrate(config_dir, Some(cwd))`.
    let outcome =
        auto_migrate(&home.config_dir(), Some(home.cwd())).expect("boot auto-migration succeeds");
    let migrated = outcome.global.expect("the global file was migrated");
    assert_eq!(migrated.path, home.global_path());
    let pre_path = home.global_path().with_file_name("config.yaml.pre-0.24");
    assert!(
        matches!(migrated.original, Original::Created(ref p) if p == &pre_path),
        "the first migration writes config.yaml.pre-0.24"
    );
    assert_eq!(
        std::fs::read(&pre_path).expect("backup exists"),
        original,
        "the backup holds the true legacy bytes"
    );

    // The rewritten file is the 0.24 layout and carries the token.
    let text = std::fs::read_to_string(home.global_path()).expect("migrated file");
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("migrated file is YAML");
    assert!(!is_legacy(&doc), "the migrated file is no longer legacy");
    assert_eq!(
        doc["general"]["web"]["token"].as_str(),
        Some("sekret-token"),
        "the web-token file moved into general.web.token (trimmed)"
    );

    // The token file is renamed, not copied and not deleted.
    assert!(
        !home.config_dir().join("web-token").exists(),
        "web-token is gone"
    );
    assert_eq!(
        std::fs::read_to_string(home.config_dir().join("web-token.bak"))
            .expect("web-token.bak exists"),
        "sekret-token\n",
        "web-token.bak keeps the original bytes"
    );

    // Boot continues: `Config::load` now succeeds.
    let cfg = Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("boots after migration");
    assert_eq!(cfg.general.web.token.as_deref(), Some("sekret-token"));
}

/// §11.3 item 16: stderr gets the `ℹ Migrated …` line and the notes —
/// exactly `AutoMigrateOutcome::notices()`, verbatim, in order.
#[test]
fn boot_migration_notice_lines_match_stderr_contract() {
    let home = TempHome::new();
    home.write_global(LEGACY_GLOBAL);
    home.write_web_token("sekret-token");

    let outcome =
        auto_migrate(&home.config_dir(), Some(home.cwd())).expect("boot auto-migration succeeds");
    let global = home.global_path().display().to_string();
    let config_dir = home.config_dir().display().to_string();
    assert_eq!(
        outcome.notices(),
        vec![
            format!(
                "ℹ Migrated {global} to the 0.24 layout. The previous file is kept as {global}.pre-0.24 — restore it to go back to PeakBot 0.23."
            ),
            format!(
                "  • web-token file moved into general.web.token; renamed {config_dir}/web-token → web-token.bak."
            ),
        ],
        "the stderr lines are the notice line, then one `•` line per note"
    );
}

/// ASSUMED API (T9a) — compile-time contract: `SessionDeps.boot_notices`
/// exists with the planned type. This fails to compile until T9a lands;
/// the runtime take-once behaviour needs a real `SessionDeps` (gap).
#[allow(dead_code)]
fn boot_notices_take(deps: &peakbot::session::SessionDeps) -> Vec<String> {
    std::mem::take(
        &mut *deps
            .boot_notices
            .lock()
            .expect("boot_notices is not poisoned"),
    )
}

/// §11.3 item 16: the first session shows the boot notices as system
/// messages, a second session does not. The lines the sessions receive are
/// exactly the boot notices (asserted here against the existing seam);
/// they live in `SessionDeps.boot_notices` and are taken once
/// (`boot_notices_take`, ASSUMED API).
#[test]
fn boot_notice_lines_reach_first_session_only() {
    let home = TempHome::new();
    home.write_global(LEGACY_GLOBAL);
    home.write_web_token("sekret-token");

    let outcome =
        auto_migrate(&home.config_dir(), Some(home.cwd())).expect("boot auto-migration succeeds");
    let lines = outcome.notices();
    assert_eq!(lines.len(), 2, "one notice line + one web-token note");
    assert!(
        lines[0].starts_with("ℹ Migrated "),
        "first line is the migration notice"
    );
    assert!(lines[1].starts_with("  • "), "notes are bullet lines");
    // First session: `mem::take` of `boot_notices` returns all lines;
    // second session: the taken vec is empty. Pinned by `boot_notices_take`.
}

/// §11.3 item 16: a legacy repo file in the boot cwd is migrated in the same
/// boot, and its (shadowing) profile leads the catalog — the zero-turn
/// default there.
#[test]
fn boot_migrates_legacy_repo_file_in_boot_cwd() {
    let home = TempHome::new();
    home.write_global(GLOBAL_024);
    let original_repo = home.write_repo(LEGACY_REPO);

    let outcome =
        auto_migrate(&home.config_dir(), Some(home.cwd())).expect("boot auto-migration succeeds");
    assert!(outcome.global.is_none(), "the 0.24 global is untouched");
    let migrated = outcome.repo.expect("the legacy repo file was migrated");
    assert_eq!(migrated.path, home.repo_path());
    let pre = home.repo_path().with_file_name("config.yaml.pre-0.24");
    assert!(
        matches!(migrated.original, Original::Created(ref p) if p == &pre),
        "the repo backup is written"
    );
    assert_eq!(
        std::fs::read(&pre).expect("repo backup exists"),
        original_repo,
        "the repo backup holds the true legacy bytes"
    );
    let doc: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(home.repo_path()).expect("repo file"))
            .expect("migrated repo file is YAML");
    assert!(
        !is_legacy(&doc),
        "the migrated repo file is no longer legacy"
    );

    // The migrated repo file loads, and its profile is the default here.
    let cfg = Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("boots after migration");
    let names: Vec<&str> = cfg.catalog.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        ["dev", "research"],
        "the repo profile shadows the global one and leads"
    );
    let bound = cfg
        .catalog
        .start_bound(None)
        .expect("zero turns: the default bound");
    assert_eq!(
        bound.profile.name, "dev",
        "the repo profile is the default there"
    );
    assert!(
        matches!(
            bound.profile.origin,
            peakbot::config::resolve::Origin::Repo { .. }
        ),
        "the bound profile comes from the repo file"
    );
}

/// §11.3 item 16: under a `--profile` pin the boot step 0 is
/// `auto_migrate(config_dir, None)` — the legacy repo file is untouched, the
/// global still migrates, and the repo file never loads.
#[test]
fn boot_pin_skips_repo_migration() {
    let home = TempHome::new();
    home.write_global(LEGACY_GLOBAL);
    home.write_repo(LEGACY_REPO);
    let repo_bytes = std::fs::read(home.repo_path()).expect("legacy repo file");

    // Pinned boot: `repo_cwd` is None (T9a: `pin.is_none().then_some(cwd)`).
    let outcome = auto_migrate(&home.config_dir(), None).expect("pinned boot migrates the global");
    assert!(
        outcome.global.is_some(),
        "the legacy global still migrates under a pin"
    );
    assert!(outcome.repo.is_none(), "no repo migration under a pin");
    assert_eq!(
        std::fs::read(home.repo_path()).expect("repo file still there"),
        repo_bytes,
        "the legacy repo file is untouched under a pin"
    );
    assert!(
        !home
            .repo_path()
            .with_file_name("config.yaml.pre-0.24")
            .exists(),
        "no repo backup under a pin"
    );

    // Pin semantics: the repo file is not even read.
    let cfg = Config::load(home.cwd(), RepoMode::Ignore, &ctx()).expect("pinned load");
    assert!(
        cfg.catalog
            .iter()
            .all(|p| matches!(p.origin, peakbot::config::resolve::Origin::Global)),
        "no repo profile survives RepoMode::Ignore"
    );
}

/// §11.3 item 16: a failing migration → the `could not migrate …` error (the
/// text boot prints before `PeakBot cannot start with this file. …` and exit
/// 1), the file unchanged, no backup.
#[test]
fn boot_failed_migration_keeps_file_and_reports_could_not_migrate() {
    let home = TempHome::new();
    let original = home.write_global(LEGACY_GLOBAL_BAD);

    let err = auto_migrate(&home.config_dir(), Some(home.cwd()))
        .expect_err("the legacy global fails to migrate");
    assert!(
        matches!(err.file, ConfigFileKind::Global),
        "the failing file is the global"
    );
    assert_eq!(err.path, home.global_path());
    let msg = err.to_string();
    assert!(
        msg.starts_with(&format!(
            "could not migrate {} to the 0.24 layout: ",
            home.global_path().display()
        )),
        "the error names the file: {msg}"
    );
    assert!(
        msg.contains("pipelines[0] has no `name`"),
        "the reason: {msg}"
    );

    assert_eq!(
        std::fs::read(home.global_path()).expect("file still there"),
        original,
        "a failed migration leaves the file unchanged"
    );
    assert!(
        !home
            .global_path()
            .with_file_name("config.yaml.pre-0.24")
            .exists(),
        "a failed migration creates no backup"
    );
}

/// §11.3 item 16 / §4.3 PIN: `--profile` with an unknown profile is a hard
/// error that lists the global profiles in declaration order (repo profiles
/// are not listed — a pin names only global profiles).
#[test]
fn pin_unknown_profile_message() {
    let global = Path::new("/global/config.yaml");
    let cfg = Config::parse(
        Source {
            path: global,
            text: GLOBAL_024,
        },
        None,
        &ctx(),
    )
    .expect("0.24 global parses");

    let msg = cfg
        .catalog
        .start_bound(Some("nope"))
        .expect_err("unknown pin is an error");
    assert_eq!(
        msg,
        "--profile 'nope': unknown profile. Configured profiles: dev, research"
    );

    // A pin that names a repo-origin profile is unknown too.
    let cfg = Config::parse(
        Source {
            path: global,
            text: GLOBAL_024,
        },
        Some(Source {
            path: Path::new("/repo/.peakbot/config.yaml"),
            text: "profiles:\n  only-here:\n    agents:\n      main:\n        model: sonnet\n",
        }),
        &ctx(),
    )
    .expect("global + repo parses");
    let msg = cfg
        .catalog
        .start_bound(Some("only-here"))
        .expect_err("a repo profile is unknown under a pin");
    assert_eq!(
        msg,
        "--profile 'only-here': unknown profile. Configured profiles: dev, research"
    );

    // A valid pin binds the pinned profile's first agent; no pin binds the
    // first profile's first agent.
    assert_eq!(
        cfg.catalog
            .start_bound(Some("research"))
            .expect("valid pin")
            .profile
            .name,
        "research"
    );
    assert_eq!(
        cfg.catalog.start_bound(None).expect("no pin").profile.name,
        "only-here",
        "no pin: the first profile (the repo one) is the default"
    );
}

// ── item 17: reload repo migration (B7, the migration step) ─────────────────

/// §11.3 item 17: `/cd` into a dir with a legacy repo file migrates it
/// (`auto_migrate_repo` — repo file only, never the global); the notices are
/// the system messages, and with zero turns the repo profile is the default
/// there.
#[test]
fn cd_migrates_legacy_repo_file() {
    let home = TempHome::new();
    home.write_global(GLOBAL_024);
    let original_repo = home.write_repo(LEGACY_REPO);

    let migrated = auto_migrate_repo(&home.config_dir(), home.cwd())
        .expect("the repo file migrates")
        .expect("a legacy repo file is rewritten");
    assert_eq!(migrated.path, home.repo_path());
    let pre = home.repo_path().with_file_name("config.yaml.pre-0.24");
    assert!(
        matches!(migrated.original, Original::Created(ref p) if p == &pre),
        "the repo backup is written"
    );
    assert_eq!(
        std::fs::read(&pre).expect("repo backup exists"),
        original_repo,
        "the repo backup holds the true legacy bytes"
    );

    // The notices appear as system messages on the /cd.
    let lines = migrated.notices();
    assert!(
        lines[0].starts_with("ℹ Migrated "),
        "the migration notice: {:?}",
        lines
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("  • ") && l.contains("profile 'dev'")),
        "the copy-note for the repo profile: {lines:?}"
    );

    // The global file is never touched.
    assert_eq!(
        std::fs::read_to_string(home.global_path()).expect("global file"),
        GLOBAL_024,
        "repo migration never touches the global"
    );

    // Zero turns: the migrated repo profile is the default there.
    let cfg =
        Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("loads after /cd migration");
    let bound = cfg.catalog.start_bound(None).expect("zero turns");
    assert_eq!(bound.profile.name, "dev");
    assert!(
        matches!(
            bound.profile.origin,
            peakbot::config::resolve::Origin::Repo { .. }
        ),
        "the default there is the repo profile"
    );
}

/// §11.3 item 17: `/cd` into a dir whose legacy repo file fails to migrate →
/// the `could not migrate …` error for the repo file; the file is unchanged
/// (boot's `⚠ … ignoring this repo's profiles.` copy is the B7 gap).
#[test]
fn cd_failed_repo_migration_reports_could_not_migrate() {
    let home = TempHome::new();
    home.write_global(GLOBAL_024);
    let original_repo = home.write_repo(LEGACY_REPO_BAD);

    let err = auto_migrate_repo(&home.config_dir(), home.cwd())
        .expect_err("the legacy repo file fails to migrate");
    assert!(
        matches!(err.file, ConfigFileKind::Repo),
        "the failing file is the repo file"
    );
    assert_eq!(err.path, home.repo_path());
    let msg = err.to_string();
    assert!(
        msg.starts_with(&format!(
            "could not migrate {} to the 0.24 layout: ",
            home.repo_path().display()
        )),
        "the error names the repo file: {msg}"
    );
    assert!(
        msg.contains("pipelines[0] has no `name`"),
        "the reason: {msg}"
    );

    assert_eq!(
        std::fs::read(home.repo_path()).expect("file still there"),
        original_repo,
        "a failed repo migration leaves the file unchanged"
    );
}

// ── item 10: web token chain ────────────────────────────────────────────────

/// §11.3 item 10 / T9b: the token chain is CLI > env (`$PEAKBOT_WEB_TOKEN`)
/// > `general.web.token`. ASSUMED API: `peakbot::config::resolve_token`.
#[test]
fn token_chain_cli_env_config() {
    use peakbot::config::resolve_token;

    assert_eq!(
        resolve_token(Some("cli"), Some("env"), Some("config")),
        Some("cli".to_string()),
        "the CLI token wins"
    );
    assert_eq!(
        resolve_token(None, Some("env"), Some("config")),
        Some("env".to_string()),
        "the env token beats the config token"
    );
    assert_eq!(
        resolve_token(None, None, Some("config")),
        Some("config".to_string()),
        "the config token is the fallback"
    );
    assert_eq!(
        resolve_token(None, None, None),
        None,
        "no token anywhere: None"
    );
}

/// §11.3 item 10: `service install --token` on a legacy file auto-migrates
/// FIRST (`.pre-0.24` present with the true legacy bytes) and only then sets
/// the token — the token write never lands in the backup. T9b sequence,
/// exercised through the existing seams.
#[test]
fn service_install_token_migrates_legacy_file_first() {
    let home = TempHome::new();
    let original = home.write_global(LEGACY_GLOBAL);

    // Step 1 (T9b): `auto_migrate(config_dir, None)`.
    let outcome = auto_migrate(&home.config_dir(), None).expect("install: migrate first");
    assert!(outcome.global.is_some(), "the legacy file migrated");
    let pre = home.global_path().with_file_name("config.yaml.pre-0.24");
    assert_eq!(
        std::fs::read(&pre).expect("the .pre-0.24 backup exists"),
        original,
        "the backup holds the true legacy bytes"
    );

    // Step 2 (T9b): set the token, validate, save.
    let text = std::fs::read_to_string(home.global_path()).expect("migrated file");
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("migrated file is YAML");
    doc["general"]["web"]["token"] = serde_yaml::Value::String("install-token".into());
    let new_text = serde_yaml::to_string(&doc).expect("YAML");
    let global = home.global_path();
    Config::parse(
        Source {
            path: &global,
            text: &new_text,
        },
        None,
        &ctx(),
    )
    .expect("the token-set file must validate");
    save_config_at(&home.config_dir(), &new_text).expect("save");

    let cfg = Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("boots after install");
    assert_eq!(cfg.general.web.token.as_deref(), Some("install-token"));
}

// ── item 14: cost tracking ──────────────────────────────────────────────────

/// §11.3 item 14: cost tracking is on for an OpenRouter model and off for
/// any other provider — even with `general.cost_tracking: true`.
/// ASSUMED API: `peakbot::config::cost_tracking_enabled` (T7: "cost tracking
/// = general.cost_tracking && OpenRouter").
#[test]
fn cost_tracking_only_openrouter() {
    let global = Path::new("/global/config.yaml");
    let cfg = Config::parse(
        Source {
            path: global,
            text: COST_YAML,
        },
        None,
        &ctx(),
    )
    .expect("cost config parses");
    assert!(
        cfg.general.cost_tracking,
        "general.cost_tracking: true survives the parse"
    );

    let dev = cfg.catalog.profile("dev").expect("the dev profile");
    let openrouter_model = &dev.agents[0].main.model;
    let other_model = &dev.agents[1].main.model;
    assert_eq!(
        openrouter_model.provider_kind,
        peakbot::config::ProviderType::OpenRouter
    );

    use peakbot::config::cost_tracking_enabled;
    assert!(
        cost_tracking_enabled(cfg.general.cost_tracking, openrouter_model),
        "cost tracking is on for an OpenRouter model"
    );
    assert!(
        !cost_tracking_enabled(cfg.general.cost_tracking, other_model),
        "cost tracking is off for any other provider even with general.cost_tracking: true"
    );
}

// ── item 3: conversation persistence (load part) ────────────────────────────

/// §11.3 item 3: an old conversation JSON with `"pipeline": "dev-team"` +
/// `"profile": null` loads as agent `dev-team` (ASSUMED API: T9a's
/// `Conversation.agent` field with serde alias `"pipeline"`); a new save
/// writes `"agent"`, never `"pipeline"`.
#[test]
fn old_convo_pipeline_field_loads_as_agent() {
    let convo: Conversation = serde_json::from_str(OLD_CONVO_JSON)
        .expect("an old conversation file with `pipeline` loads");
    assert_eq!(
        convo.agent,
        Some("dev-team".to_string()),
        "the `pipeline` field loads as `agent`"
    );
    assert_eq!(convo.profile, None, "`profile: null` loads as None");

    // A new save writes `agent` — the `pipeline` key is gone.
    let value = serde_json::to_value(&convo).expect("serialize");
    assert_eq!(value.get("agent"), Some(&serde_json::json!("dev-team")));
    assert!(
        value.get("pipeline").is_none(),
        "a new save writes `agent`, never `pipeline`"
    );

    // A new-layout file loads the same.
    let new_layout = OLD_CONVO_JSON.replace(r#""pipeline": "dev-team""#, r#""agent": "dev-team""#);
    let convo: Conversation =
        serde_json::from_str(&new_layout).expect("a new-layout conversation file loads");
    assert_eq!(convo.agent, Some("dev-team".to_string()));
}
