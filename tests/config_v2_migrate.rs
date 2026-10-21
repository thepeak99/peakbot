//! RED golden tests for tasks A6/A6′: `peakbot::config::migrate` —
//! `is_legacy` + the pure `migrate` function.
//!
//! Written against the interface the spec locks in §7.2–7.3:
//!
//! ```rust
//! // peakbot::config::migrate
//! pub struct Migration { pub config: serde_yaml::Value, pub notes: Vec<String> }
//! pub enum MigrateError {
//!     RootNotAMapping,
//!     PipelinesNotAList,
//!     PipelineWithoutName(usize),
//!     NonStringName(String),         // A6′
//!     LegacyProviderType,            // A6′
//!     LegacyProviderModel(String),   // A6′
//! }
//! pub fn is_legacy(doc: &serde_yaml::Value) -> bool;
//! pub fn migrate(old: &serde_yaml::Value) -> Result<Migration, MigrateError>; // no IO
//! ```
//!
//! The A6′ parts (`unique` name helper with the `(name collision).` note
//! texts, `NonStringName`, the provider-default rows, the blank-`system_prompt`
//! notes) are what the m/n/o/p fixtures, the rewritten `h_name_collisions`
//! golden and the variant-matching error tests below pin.
//!
//! The goldens in `tests/fixtures/migrate/` ARE the behaviour spec
//! (§7.3 key mapping, §7.4 worked example, §11.2). For every fixture they
//! pin:
//!
//! 1. `migrate(in).config == out`  (compared as `serde_yaml::Value`);
//! 2. `migrate(in).notes == notes` (one note per line in `<stem>.notes.txt`);
//! 3. `Config::parse(out)` is Ok (empty `known_skills`);
//! 4. determinism: running `migrate` twice yields identical config + notes;
//! 5. `is_legacy(in) == true` and `is_legacy(out) == false`.
//!
//! Decisions the goldens make where the spec leaves room (the dev follows
//! the fixture files):
//!
//! - **Note order** is not fixed by the spec; the `.notes.txt` files pin a
//!   deterministic order: global notes first (`tools` delegate, `searxng`
//!   disabled, `vector_db` disabled, `conversation` dropped keys, legacy
//!   `pipeline:` block), then agent persona notes in `default`-agent order,
//!   then per-profile notes in profile order (rename, pipelines-filter
//!   removal, ui.tabs removal).
//! - The `web-token` note from §7.4 is added by the IO wrapper (§7.5 step
//!   3), not by the pure `migrate` — it is therefore NOT in any
//!   `.notes.txt` golden (the `config_v2_auto_migrate` tests pin it on the
//!   outcome instead).
//! - Note wording the spec does not fix verbatim is defined by the
//!   fixtures: (g) disabled search/vector_db, (h) the two name-collision
//!   renames, (i) the disabled legacy `pipeline:` block.
//! - The behaviour-preserving rule (§7.4): an old subagent without a model
//!   gets an EXPLICIT `model: <default_model>` iff `default_model` differs
//!   from its agent's model (fixtures b and j).

use std::path::Path;

use peakbot::config::migrate::{MigrateError, is_legacy, migrate};
use peakbot::config::resolve::{Config, ParseCtx, Source};

const GLOBAL_PATH: &str = "/home/u/.config/peakbot/config.yaml";

/// §7.2: output top-level key order, and the `general` inner order.
const TOP_ORDER: [&str; 4] = ["providers", "mcps", "profiles", "general"];
const GENERAL_ORDER: [&str; 10] = [
    "web",
    "search",
    "context",
    "conversation",
    "agent_max_turns",
    "cost_tracking",
    "retry",
    "timeouts",
    "http",
    "uploads",
];

/// The 18 golden fixtures: (a)–(k), (l) `general` blocks, and the A6′ set
/// (m) provider defaults, (n) no provider, (o) max_image_base64_bytes drop,
/// (p) blank system_prompts. (i) covers two files.
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

fn fixture_path(stem: &str, ext: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/migrate")
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

fn parse_global(yaml: &str) -> Result<Config, peakbot::config::errors::ConfigErrors> {
    Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text: yaml,
        },
        None,
        &parse_ctx(),
    )
}

fn has_key(v: &serde_yaml::Value, key: &str) -> bool {
    v.as_mapping().is_some_and(|m| m.contains_key(key))
}

/// Mapping keys in document (insertion) order.
fn mapping_keys(v: &serde_yaml::Value) -> Vec<String> {
    v.as_mapping()
        .expect("expected a mapping")
        .keys()
        .map(|k| k.as_str().expect("string key").to_string())
        .collect()
}

/// The five golden assertions for one fixture stem.
fn run_golden(stem: &str) {
    let input = load_fixture(stem, "in");
    let expected_out = load_fixture(stem, "out");
    let expected_notes = load_notes(stem);

    let first = migrate(&input).unwrap_or_else(|e| panic!("fixture {stem}: migrate failed: {e}"));
    let second =
        migrate(&input).unwrap_or_else(|e| panic!("fixture {stem}: migrate (2nd run) failed: {e}"));

    assert_eq!(
        first.config, expected_out,
        "fixture {stem}: migrated config != golden .out.yaml"
    );
    assert_eq!(
        first.notes, expected_notes,
        "fixture {stem}: notes != golden .notes.txt"
    );
    assert!(
        is_legacy(&input),
        "fixture {stem}: the input must be detected as legacy"
    );
    assert!(
        !is_legacy(&first.config),
        "fixture {stem}: the migrated output must not be detected as legacy"
    );
    assert_eq!(
        second.config, first.config,
        "fixture {stem}: migrate is not deterministic (config)"
    );
    assert_eq!(
        second.notes, first.notes,
        "fixture {stem}: migrate is not deterministic (notes)"
    );

    let out_text = serde_yaml::to_string(&first.config).expect("serialize migrated config");
    parse_global(&out_text)
        .unwrap_or_else(|errs| panic!("fixture {stem}: migrated config does not parse: {errs:?}"));
}

#[test]
fn golden_a_pipeline_team() {
    run_golden("a_pipeline_team");
}

#[test]
fn golden_b_maintainer_shape() {
    run_golden("b_maintainer_shape");
}

#[test]
fn golden_c_legacy_provider() {
    run_golden("c_legacy_provider");
}

#[test]
fn golden_d_system_prompt() {
    run_golden("d_system_prompt");
}

#[test]
fn golden_e_profile_overlays() {
    run_golden("e_profile_overlays");
}

#[test]
fn golden_f_tools_delegate() {
    run_golden("f_tools_delegate");
}

#[test]
fn golden_g_disabled_search_vector() {
    run_golden("g_disabled_search_vector");
}

#[test]
fn golden_h_name_collisions() {
    run_golden("h_name_collisions");
}

#[test]
fn golden_i_pipeline_enabled() {
    run_golden("i_pipeline_enabled");
}

#[test]
fn golden_i_pipeline_disabled() {
    run_golden("i_pipeline_disabled");
}

#[test]
fn golden_j_subagent_model_fallback() {
    run_golden("j_subagent_model_fallback");
}

#[test]
fn golden_k1_profile_mcp_agents_md() {
    run_golden("k1_profile_mcp_agents_md");
}

#[test]
fn golden_k2_setup_api_review_team() {
    run_golden("k2_setup_api_review_team");
}

#[test]
fn golden_l_general_blocks() {
    run_golden("l_general_blocks");
}

#[test]
fn golden_m_provider_default_model() {
    run_golden("m_provider_default_model");
}

#[test]
fn golden_n_no_provider() {
    run_golden("n_no_provider");
}

#[test]
fn golden_o_max_image_base64() {
    run_golden("o_max_image_base64");
}

#[test]
fn golden_p_blank_system_prompt() {
    run_golden("p_blank_system_prompt");
}

// ===========================================================================
// Output shape: key order and "only keys present in the input are emitted".
//
// NB: `serde_yaml::Mapping` equality is ORDER-INSENSITIVE, so the golden
// `==` above cannot catch a mis-ordered output. These tests inspect the
// order of what `migrate` actually emits. Order is semantic here: the first
// profile / first agent is the default (§1.1).
// ===========================================================================

fn migrated(stem: &str) -> serde_yaml::Value {
    migrate(&load_fixture(stem, "in"))
        .unwrap_or_else(|e| panic!("fixture {stem}: migrate failed: {e}"))
        .config
}

/// §7.2: top-level keys are emitted in the order providers, mcps, profiles,
/// general — restricted to the keys that exist.
#[test]
fn output_top_level_key_order_is_providers_mcps_profiles_general() {
    for stem in FIXTURES {
        let out = migrated(stem);
        let expected: Vec<String> = TOP_ORDER
            .iter()
            .filter(|k| has_key(&out, k))
            .map(|k| (*k).to_string())
            .collect();
        assert_eq!(
            mapping_keys(&out),
            expected,
            "fixture {stem}: top-level key order must follow providers, mcps, profiles, general"
        );
    }
}

/// §7.2: inside `general` the key order is
/// web, search, context, conversation, agent_max_turns, cost_tracking,
/// retry, timeouts, http, uploads — restricted to present keys.
#[test]
fn general_inner_key_order_follows_spec() {
    let mut saw_general = false;
    for stem in FIXTURES {
        let out = migrated(stem);
        let Some(general) = out.get("general") else {
            continue;
        };
        saw_general = true;
        let expected: Vec<String> = GENERAL_ORDER
            .iter()
            .filter(|k| has_key(general, k))
            .map(|k| (*k).to_string())
            .collect();
        assert_eq!(
            mapping_keys(general),
            expected,
            "fixture {stem}: general inner key order must follow the §7.2 order"
        );
    }
    assert!(saw_general, "at least one fixture must exercise `general`");
}

/// Profile order (`default` first, then old profiles sorted by name), agent
/// order (`main` first, then pipelines in declaration order) and subagent
/// order (declaration order) of the emitted output equal the goldens'.
#[test]
fn profile_agent_and_subagent_order_matches_golden() {
    for stem in FIXTURES {
        let got = migrated(stem);
        let want = load_fixture(stem, "out");
        let got_profiles = &got["profiles"];
        let want_profiles = &want["profiles"];
        assert_eq!(
            mapping_keys(got_profiles),
            mapping_keys(want_profiles),
            "fixture {stem}: profile order"
        );
        for pname in mapping_keys(want_profiles) {
            let (g_agents, w_agents) = (
                &got_profiles[pname.as_str()]["agents"],
                &want_profiles[pname.as_str()]["agents"],
            );
            assert_eq!(
                mapping_keys(g_agents),
                mapping_keys(w_agents),
                "fixture {stem}: agent order in profile {pname}"
            );
            for aname in mapping_keys(w_agents) {
                let (g_sub, w_sub) = (
                    &g_agents[aname.as_str()]["subagents"],
                    &w_agents[aname.as_str()]["subagents"],
                );
                if w_sub.is_null() {
                    assert!(
                        g_sub.is_null(),
                        "fixture {stem}: {pname}/{aname} must not gain subagents"
                    );
                    continue;
                }
                assert_eq!(
                    mapping_keys(g_sub),
                    mapping_keys(w_sub),
                    "fixture {stem}: subagent order in {pname}/{aname}"
                );
            }
        }
    }
}

/// §7.2: "Only keys present in the old file are emitted (no default spam)."
#[test]
fn only_keys_present_in_input_are_emitted() {
    // No mcp_servers and no general-bound keys → exactly providers + profiles.
    for stem in [
        "c_legacy_provider",
        "g_disabled_search_vector",
        "j_subagent_model_fallback",
        "k2_setup_api_review_team",
        "m_provider_default_model",
        "n_no_provider",
        "o_max_image_base64",
        "p_blank_system_prompt",
    ] {
        assert_eq!(
            mapping_keys(&migrated(stem)),
            vec!["providers".to_string(), "profiles".to_string()],
            "fixture {stem}: must emit exactly providers + profiles"
        );
    }
    // Fixture (b) has providers, mcp_servers, profiles and general sources.
    let out = migrated("b_maintainer_shape");
    assert_eq!(
        mapping_keys(&out),
        vec![
            "providers".to_string(),
            "mcps".to_string(),
            "profiles".to_string(),
            "general".to_string(),
        ],
        "fixture b: must emit all four blocks"
    );
    // general carries ONLY keys with an old source (searxng, agent_max_turns).
    assert_eq!(
        mapping_keys(&out["general"]),
        vec!["search".to_string(), "agent_max_turns".to_string()],
        "fixture b: general must not gain default-valued keys"
    );
}

// ===========================================================================
// MigrateError cases (§7.3). The enum is locked: the tests match the EXACT
// variant, not just "some error".
// ===========================================================================

/// §7.3: the root is not a mapping → `MigrateError::RootNotAMapping`.
#[test]
fn migrate_rejects_root_that_is_not_a_mapping() {
    let string_root: serde_yaml::Value = serde_yaml::from_str("just a string").unwrap();
    assert!(
        matches!(migrate(&string_root), Err(MigrateError::RootNotAMapping)),
        "a string root must be MigrateError::RootNotAMapping"
    );
    let list_root: serde_yaml::Value = serde_yaml::from_str("- a\n- b\n").unwrap();
    assert!(
        matches!(migrate(&list_root), Err(MigrateError::RootNotAMapping)),
        "a list root must be MigrateError::RootNotAMapping"
    );
}

/// §7.3: `pipelines` is not a list → `MigrateError::PipelinesNotAList`.
#[test]
fn migrate_rejects_pipelines_that_is_not_a_list() {
    let doc: serde_yaml::Value = serde_yaml::from_str(
        "providers:\n  - name: openai\n    type: openai\n    api_key: k\n    models:\n      - name: m\n        alias: a\npipelines:\n  name: not-a-list\n",
    )
    .unwrap();
    assert!(
        matches!(migrate(&doc), Err(MigrateError::PipelinesNotAList)),
        "a non-list pipelines key must be MigrateError::PipelinesNotAList"
    );
}

/// A6′ §7.3: a non-string profile key is `NonStringName("profiles.42")` —
/// never silently dropped.
#[test]
fn migrate_rejects_non_string_profile_key() {
    let doc: serde_yaml::Value =
        serde_yaml::from_str("profiles:\n  42:\n    tools:\n      only: [think]\n").unwrap();
    assert!(
        matches!(migrate(&doc), Err(MigrateError::NonStringName(p)) if p == "profiles.42"),
        "a numeric profile key must be NonStringName(\"profiles.42\")"
    );
}

/// A6′ §7.3: a non-string `pipelines[i].name` is `NonStringName`.
#[test]
fn migrate_rejects_non_string_pipeline_name() {
    let doc: serde_yaml::Value = serde_yaml::from_str(
        "providers:\n  - name: openai\n    type: openai\n    api_key: k\n    models:\n      - name: m\n        alias: a\npipelines:\n  - name: dev\n    orchestrator:\n      prompt: p\n  - name: 42\n",
    )
    .unwrap();
    assert!(
        matches!(migrate(&doc), Err(MigrateError::NonStringName(p)) if p == "pipelines[1].name"),
        "a numeric pipeline name must be NonStringName(\"pipelines[1].name\")"
    );
}

/// A6′ §7.3: a non-string subagent key is `NonStringName`.
#[test]
fn migrate_rejects_non_string_subagent_key() {
    let doc: serde_yaml::Value = serde_yaml::from_str(
        "providers:\n  - name: openai\n    type: openai\n    api_key: k\n    models:\n      - name: m\n        alias: a\npipelines:\n  - name: dev\n    agents:\n      true:\n        prompt: p\n",
    )
    .unwrap();
    assert!(
        matches!(
            migrate(&doc),
            Err(MigrateError::NonStringName(p)) if p == "pipelines[0].agents.true"
        ),
        "a boolean subagent key must be NonStringName(\"pipelines[0].agents.true\")"
    );
}

/// A6′ §7.3: a legacy `provider:` without `type` → `LegacyProviderType`.
#[test]
fn migrate_rejects_legacy_provider_without_type() {
    let doc: serde_yaml::Value =
        serde_yaml::from_str("provider:\n  config:\n    model: some-model\n").unwrap();
    assert!(
        matches!(migrate(&doc), Err(MigrateError::LegacyProviderType)),
        "a legacy provider without a type must be LegacyProviderType"
    );
}

/// A6′ §7.3: an absent `provider.config.model` for any type that had no 0.23
/// serde default → `LegacyProviderModel(<type>)` (0.23 refused it too).
#[test]
fn migrate_rejects_legacy_provider_model_for_types_without_a_default() {
    for kind in ["ollama", "anthropic", "llama.cpp", "openai"] {
        let doc: serde_yaml::Value = serde_yaml::from_str(&format!(
            "provider:\n  type: {kind}\n  config:\n    api_key: k\n"
        ))
        .unwrap();
        assert!(
            matches!(migrate(&doc), Err(MigrateError::LegacyProviderModel(t)) if t == kind),
            "type {kind} has no 0.23 model default; must be LegacyProviderModel({kind})"
        );
    }
}

/// A6′ §7.3: `openrouter` is the one type WITH a 0.23 default — an absent
/// model there migrates (golden m pins the output and the note), it is not
/// an error.
#[test]
fn migrate_allows_legacy_openrouter_provider_without_model() {
    let doc: serde_yaml::Value =
        serde_yaml::from_str("provider:\n  type: openrouter\n  config:\n    api_key: k\n").unwrap();
    assert!(
        matches!(migrate(&doc), Ok(_)),
        "openrouter had a 0.23 default model; an absent model must migrate, not error"
    );
}
