//! RED golden tests for task A6′: `peakbot::config::migrate::migrate_repo`
//! — the pure repo-file mapping (spec §7.6).
//!
//! `migrate_repo` does not exist yet. These tests are written against the
//! interface the spec locks in §7.2:
//!
//! ```rust
//! // peakbot::config::migrate
//! pub struct Migration { pub config: serde_yaml::Value, pub notes: Vec<String> }
//! /// §7.6: a legacy repo file → `{profiles: …}` only, built on the CURRENT
//! /// global file. `global` must already be a valid 0.24 document.
//! pub fn migrate_repo(
//!     old_repo: &serde_yaml::Value,
//!     global: &serde_yaml::Value,
//!     global_path: &std::path::Path,   // used only inside note texts
//! ) -> Result<Migration, MigrateError>;
//! ```
//!
//! Fixtures in `tests/fixtures/migrate/repo/` (§11.2 (r1)–(r8) plus `r6b`,
//! the blank-repo-persona half of (r6)). For every fixture they pin:
//!
//! 1. `migrate_repo(in, global, GLOBAL_PATH).config == out` (as `Value`);
//! 2. `…notes == notes` (one note per line in `<stem>.notes.txt`; the note
//!    order of §7.6 is fixed: A/B in file order · delegate · blank
//!    `system_prompt` · step-7 agent notes · D · E; C alone);
//! 3. determinism: running it twice yields identical config + notes;
//! 4. `is_legacy(in) == true` and `is_legacy(out) == false`;
//! 5. `Config::parse(global, Some(out))` is Ok — except `r8`, whose migrated
//!    file uses an alias only the dropped repo `providers:` declared, so the
//!    parse MUST fail (auto_migrate maps that to `Invalid`).
//!
//! `r3`/`r4`/`r5` pin that `profiles: {}` is a valid repo file (RepoFile has
//! no E2 check): their outs are `profiles: {}` and assertion 5 covers them.

use std::path::Path;

use peakbot::config::migrate::{is_legacy, migrate_repo};
use peakbot::config::resolve::{Config, ParseCtx, Source};

/// Absolute path the notes cite; the fixtures' `.notes.txt` lines are written
/// against it.
const GLOBAL_PATH: &str = "/home/u/.config/peakbot/config.yaml";
const REPO_PATH: &str = "/home/u/repo/.peakbot/config.yaml";

/// §11.2 (r1)–(r8) plus r6b (blank repo persona).
const REPO_FIXTURES: [&str; 9] = ["r1", "r2", "r3", "r4", "r5", "r6", "r6b", "r7", "r8"];

/// The one fixture whose migrated output must NOT parse (repo-only alias).
const REPO_INVALID: &str = "r8";

fn fixture_path(stem: &str, ext: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/migrate/repo")
        .join(format!("{stem}.{ext}"))
}

fn load_fixture(stem: &str, ext: &str) -> serde_yaml::Value {
    let path = fixture_path(stem, &format!("{ext}.yaml"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_yaml::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

/// One note per line; an empty file is zero notes.
fn load_notes(stem: &str) -> Vec<String> {
    let path = fixture_path(stem, "notes.txt");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
        .lines()
        .map(str::to_string)
        .collect()
}

fn parse_ctx() -> ParseCtx<'static> {
    ParseCtx { known_skills: &[] }
}

/// The IO-layer validation of a migrated repo file: global + repo together.
fn parse_repo(
    global_text: &str,
    repo_text: &str,
) -> Result<Config, peakbot::config::errors::ConfigErrors> {
    Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text: global_text,
        },
        Some(Source {
            path: Path::new(REPO_PATH),
            text: repo_text,
        }),
        &parse_ctx(),
    )
}

/// The five golden assertions for one repo fixture stem.
fn run_repo_golden(stem: &str) {
    let input = load_fixture(stem, "in");
    let global = load_fixture(stem, "global");
    let expected_out = load_fixture(stem, "out");
    let expected_notes = load_notes(stem);

    let first = migrate_repo(&input, &global, Path::new(GLOBAL_PATH))
        .unwrap_or_else(|e| panic!("repo {stem}: migrate_repo failed: {e}"));
    let second = migrate_repo(&input, &global, Path::new(GLOBAL_PATH))
        .unwrap_or_else(|e| panic!("repo {stem}: migrate_repo (2nd run) failed: {e}"));

    assert_eq!(
        first.config, expected_out,
        "repo {stem}: migrated repo config != golden .out.yaml"
    );
    assert_eq!(
        first.notes, expected_notes,
        "repo {stem}: notes != golden .notes.txt (order and text are exact)"
    );
    assert!(
        is_legacy(&input),
        "repo {stem}: the input must be detected as legacy"
    );
    assert!(
        !is_legacy(&first.config),
        "repo {stem}: the migrated output must not be detected as legacy"
    );
    assert_eq!(
        second.config, first.config,
        "repo {stem}: migrate_repo is not deterministic (config)"
    );
    assert_eq!(
        second.notes, first.notes,
        "repo {stem}: migrate_repo is not deterministic (notes)"
    );

    // A repo file may only contain `profiles:`, and the single repo profile
    // (if any: step 4 yields `profiles: {}`) shadows the FIRST global profile.
    let keys: Vec<&str> = first
        .config
        .as_mapping()
        .expect("repo output must be a mapping")
        .keys()
        .map(|k| k.as_str().expect("string key"))
        .collect();
    assert_eq!(
        keys,
        vec!["profiles"],
        "repo {stem}: the migrated repo file must contain exactly `profiles:`"
    );
    let first_global_profile = global["profiles"]
        .as_mapping()
        .and_then(|m| m.keys().next())
        .and_then(|k| k.as_str())
        .expect("the global fixture must have a first profile");
    let repo_profiles = &first.config["profiles"];
    assert!(
        repo_profiles
            .as_mapping()
            .is_some_and(|m| m.is_empty() || m.contains_key(first_global_profile)),
        "repo {stem}: the repo profile must be named after the first global profile '{first_global_profile}'"
    );

    let out_text = serde_yaml::to_string(&first.config).expect("serialize migrated repo config");
    let global_text =
        std::fs::read_to_string(fixture_path(stem, "global.yaml")).expect("read global fixture");
    let parse_result = parse_repo(&global_text, &out_text);
    if stem == REPO_INVALID {
        assert!(
            parse_result.is_err(),
            "repo {stem}: the migrated file must FAIL Config::parse (repo-only alias)"
        );
    } else {
        parse_result
            .unwrap_or_else(|errs| panic!("repo {stem}: migrated config does not parse: {errs:?}"));
    }
}

#[test]
fn repo_r1_worked_example() {
    run_repo_golden("r1");
}

#[test]
fn repo_r2_pipelines_replace_extra_agents() {
    run_repo_golden("r2");
}

#[test]
fn repo_r3_machine_wide_only() {
    run_repo_golden("r3");
}

#[test]
fn repo_r4_unknown_key() {
    run_repo_golden("r4");
}

#[test]
fn repo_r5_only_profiles() {
    run_repo_golden("r5");
}

#[test]
fn repo_r6_persona_over_static_prompt() {
    run_repo_golden("r6");
}

#[test]
fn repo_r6b_blank_persona_is_builtin() {
    run_repo_golden("r6b");
}

#[test]
fn repo_r7_first_profile_not_named_default() {
    run_repo_golden("r7");
}

#[test]
fn repo_r8_repo_only_alias_fails_parse() {
    run_repo_golden("r8");
}

// ===========================================================================
// Targeted pins for the §7.6 rules the goldens cover in combination.
// ===========================================================================

fn migrated_repo(stem: &str) -> serde_yaml::Value {
    let input = load_fixture(stem, "in");
    let global = load_fixture(stem, "global");
    migrate_repo(&input, &global, Path::new(GLOBAL_PATH))
        .unwrap_or_else(|e| panic!("repo {stem}: migrate_repo failed: {e}"))
        .config
}

/// §7.6 r1: `bash: {env: E}` REPLACES the profile env wholesale (0.23
/// replaced the whole `bash` block) — the global `GIT_PAGER` is gone.
#[test]
fn repo_bash_env_replaces_wholesale() {
    let env = &migrated_repo("r1")["profiles"]["default"]["env"];
    assert_eq!(
        env.as_mapping().map(|m| m.len()),
        Some(1),
        "the repo bash env replaces the global env wholesale; got {env:?}"
    );
    assert_eq!(
        env["CARGO_TERM_COLOR"],
        serde_yaml::Value::String("never".into())
    );
}

/// §7.6 r2: the fallback model for an old subagent without a `model` is the
/// repo `default_model` (0.23 semantics) — written explicitly because the
/// new fallback (the agent's model) differs.
#[test]
fn repo_subagent_without_model_gets_the_repo_default_model() {
    let worker =
        &migrated_repo("r2")["profiles"]["default"]["agents"]["main-2"]["subagents"]["worker"];
    assert_eq!(
        worker["model"],
        serde_yaml::Value::String("senior".into()),
        "the subagent's old fallback (repo default_model) must be pinned explicitly"
    );
}

/// §7.6 step 4: no APPLIED key → `profiles: {}` and no profile copy, so
/// later global edits keep reaching this repo.
#[test]
fn repo_with_no_applied_keys_yields_empty_profiles() {
    for stem in ["r3", "r4", "r5"] {
        assert_eq!(
            migrated_repo(stem),
            serde_yaml::from_str::<serde_yaml::Value>("profiles: {}\n").unwrap(),
            "repo {stem}: must be exactly `profiles: {{}}`"
        );
    }
}

/// §7.6 step 5: a `null` key counts as absent — no note for it.
#[test]
fn repo_null_key_is_absent_without_a_note() {
    let notes = migrate_repo(
        &load_fixture("r3", "in"),
        &load_fixture("r3", "global"),
        Path::new(GLOBAL_PATH),
    )
    .unwrap()
    .notes;
    assert!(
        !notes.iter().any(|n| n.starts_with("'searxng'")),
        "a null key counts as absent; got a searxng note: {notes:?}"
    );
}
