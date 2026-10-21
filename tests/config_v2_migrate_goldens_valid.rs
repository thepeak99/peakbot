//! Guard for the A6/A6′ golden fixtures — does NOT depend on `migrate` or
//! `migrate_repo`, so it is green today. It proves each golden is itself a
//! valid 0.24 config (so a red in `config_v2_migrate` /
//! `config_v2_migrate_repo` can only mean "migrate is missing/wrong", never
//! "the golden is broken") and that each `.in.yaml` is rejected as legacy
//! (L1) by the existing `Config::parse`.
//!
//! The repo goldens (`tests/fixtures/migrate/repo/`) are checked the way the
//! IO layer validates them: `Config::parse(global, Some(out))`. All repo
//! outs must parse — except `r8`, whose whole point is that the migrated
//! repo file does NOT validate (an alias only the dropped repo `providers:`
//! declared); the guard pins that it fails.

use std::path::Path;

use peakbot::config::resolve::{Config, ParseCtx, Source};

const GLOBAL_PATH: &str = "/home/u/.config/peakbot/config.yaml";
const REPO_PATH: &str = "/home/u/repo/.peakbot/config.yaml";
// The full L1 text is pinned in `config_v2_parse`; match the stable prefix.
const L1_PREFIX: &str = "config.yaml uses the pre-0.24 layout";

const FIXTURES: [&str; 18] = [
    "a_pipeline_team",
    "b_maintainer_shape",
    "c_legacy_provider",
    "d_system_prompt",
    "e_profile_overlays",
    "f_tools_delegate",
    "g_disabled_search_vector",
    "h_name_collisions",
    "i_pipeline_enabled",
    "i_pipeline_disabled",
    "j_subagent_model_fallback",
    "k1_profile_mcp_agents_md",
    "k2_setup_api_review_team",
    "l_general_blocks",
    "m_provider_default_model",
    "n_no_provider",
    "o_max_image_base64",
    "p_blank_system_prompt",
];

/// Repo goldens §11.2 (r1)–(r8) plus r6b (blank repo persona).
const REPO_FIXTURES: [&str; 9] = ["r1", "r2", "r3", "r4", "r5", "r6", "r6b", "r7", "r8"];

/// The one repo golden whose output must NOT validate (auto_migrate maps it
/// to `Invalid`).
const REPO_INVALID: &str = "r8";

fn path_for(dir: &str, stem: &str, ext: &str) -> std::path::PathBuf {
    let mut path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/migrate");
    if !dir.is_empty() {
        path = path.join(dir);
    }
    path.join(format!("{stem}.{ext}"))
}

fn read(stem: &str, ext: &str) -> String {
    let path = path_for("", stem, ext);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn read_repo(stem: &str, ext: &str) -> String {
    let path = path_for("repo", stem, ext);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn parse(text: &str) -> Result<Config, peakbot::config::errors::ConfigErrors> {
    Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
}

#[test]
fn every_golden_out_parses_as_a_valid_current_config() {
    for stem in FIXTURES {
        let text = read(stem, "out.yaml");
        if let Err(errs) = parse(&text) {
            panic!("golden {stem}.out.yaml does not parse: {errs:?}");
        }
    }
}

#[test]
fn every_golden_in_is_rejected_with_the_legacy_hint() {
    for stem in FIXTURES {
        let text = read(stem, "in.yaml");
        let errs = parse(&text).expect_err("a legacy input must not parse");
        assert!(
            format!("{errs:?}").contains(L1_PREFIX),
            "{stem}.in.yaml must fail with L1; got {errs:?}"
        );
    }
}

#[test]
fn every_repo_golden_out_parses_with_its_global() {
    for stem in REPO_FIXTURES {
        let global = read_repo(stem, "global.yaml");
        let out = read_repo(stem, "out.yaml");
        let result = Config::parse(
            Source {
                path: Path::new(GLOBAL_PATH),
                text: &global,
            },
            Some(Source {
                path: Path::new(REPO_PATH),
                text: &out,
            }),
            &ParseCtx { known_skills: &[] },
        );
        if stem == REPO_INVALID {
            // r8: the migrated repo file must NOT validate (repo-only alias).
            assert!(
                result.is_err(),
                "repo {stem}.out.yaml must fail Config::parse (repo-only alias); it parsed"
            );
        } else {
            if let Err(errs) = result {
                panic!("repo {stem}.out.yaml does not parse with its global: {errs:?}");
            }
        }
    }
}

#[test]
fn every_repo_golden_global_parses_on_its_own() {
    for stem in REPO_FIXTURES {
        let global = read_repo(stem, "global.yaml");
        if let Err(errs) = parse(&global) {
            panic!("repo {stem}.global.yaml does not parse: {errs:?}");
        }
    }
}

#[test]
fn every_golden_has_a_notes_file() {
    for stem in FIXTURES {
        let path = path_for("", stem, "notes.txt");
        assert!(path.is_file(), "missing {}", path.display());
    }
    for stem in REPO_FIXTURES {
        let path = path_for("repo", stem, "notes.txt");
        assert!(path.is_file(), "missing {}", path.display());
    }
}
