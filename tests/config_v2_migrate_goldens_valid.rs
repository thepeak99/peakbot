//! Guard for the A6 golden fixtures — does NOT depend on `migrate`, so it is
//! green today. It proves each golden is itself a valid 0.24 config (so a red
//! in `config_v2_migrate` can only mean "migrate is missing/wrong", never "the
//! golden is broken") and that each `.in.yaml` is rejected as legacy (L1) by
//! the existing `Config::parse`.

use std::path::Path;

use peakbot::config::resolve::{Config, ParseCtx, Source};

const GLOBAL_PATH: &str = "/home/u/.config/peakbot/config.yaml";
const L1_MSG: &str =
    "config.yaml uses the pre-0.24 layout — run 'peakbot config migrate' (writes config.yaml.bak)";

const FIXTURES: [&str; 14] = [
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
];

fn read(stem: &str, ext: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/migrate")
        .join(format!("{stem}.{ext}"));
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
            format!("{errs:?}").contains(L1_MSG),
            "{stem}.in.yaml must fail with L1; got {errs:?}"
        );
    }
}

#[test]
fn every_golden_has_a_notes_file() {
    for stem in FIXTURES {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/migrate")
            .join(format!("{stem}.notes.txt"));
        assert!(path.is_file(), "missing {}", path.display());
    }
}
