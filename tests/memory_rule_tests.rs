//! Memory rule (derived rule; Local-edition gate).
//!
//! The locked rule: **the memory.md prompt section (and the compaction
//! memory flush, which shares the same gate) is present iff
//! `memory.enabled ∧ file_create is registered`** in the full edition, and
//! false in the full edition when a `tools:` filter disables `file_create`.
//! In the Local edition `file_create` IS registered (confined to the
//! conversation folder) but the memory section is always absent: the
//! memory-driving predicate (`tools::env::file_create_registered`) is
//! host-only on purpose.
//!
//! **Seam:** `build_agent_prompt` decides the memory section through the
//! derived predicate `memory_applies` (`src/lib.rs`), which reads
//! `tools::env::file_create_registered`; the compaction flush call site uses
//! the same predicate.
//!
//! **Oracle:** `main_agent_tool_names(env, agent)` (pure, public) is the
//! single source of truth for "which built-ins a build registers" — the test
//! asserts the oracle row too, so the prompt can never drift from the actual
//! registration.
//!
//! Runs in BOTH editions (`cargo test --locked … --test memory_rule_tests`):
//! the host rows exercise the filter axis; the Local row pins "never
//! present, even with `memory.enabled: true`".

use std::path::Path;
use std::sync::Arc;

use peakbot::config::resolve::{Config, ParseCtx, Source};
use peakbot::skills::SkillRegistry;
use peakbot::tools::env::{ToolEnv, main_agent_tool_names};
use peakbot::{ShellKind, build_agent_prompt};

const GLOBAL_PATH: &str = "/home/u/.config/peakbot/config.yaml";
/// Non-existent cwd → no `agents.md` section; the prompt stays deterministic
/// for the sections under test.
const CWD: &str = "/opt/peakbot-memory-rule-cwd";

/// A distinctive line of `src/system_prompt_memory.txt` (the whole section is
/// `include_str!`-ed verbatim, so presence of the line ⇔ presence of the
/// section).
const MEMORY_MARKER: &str = "# Memory.md";

/// One provider + one profile `dev`. `memory:` enabled per row; `tools:`
/// filter per row (absent = allow all).
fn global_yaml(memory_enabled: bool, disable_file_create: bool) -> String {
    let tools = if disable_file_create {
        // `file_create` is a known tool name in both editions (the Local
        // edition registers it too), so the filter parses everywhere.
        "    tools:\n      disabled: [file_create]\n"
    } else {
        ""
    };
    format!(
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\nprofiles:\n  dev:\n    memory:\n      enabled: {memory_enabled}\n{tools}    agents:\n      main:\n        model: a\n"
    )
}

fn parse_global(yaml: &str) -> Config {
    Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text: yaml,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .expect("fixture global must parse")
}

/// One table row: (memory.enabled, tools filter disables file_create).
/// The expected presence is the spec rule itself:
/// `memory.enabled ∧ file_create registered` — computed per edition.
struct Row {
    memory_enabled: bool,
    disable_file_create: bool,
}

impl Row {
    /// `file_create` is registered in BOTH editions iff the `tools:` filter
    /// allows it (the Local edition registers it as a confined file tool).
    fn file_create_registered(&self) -> bool {
        !self.disable_file_create
    }

    /// The memory.md prompt section: present iff `memory.enabled ∧
    /// file_create registered` in the host edition; NEVER present in the
    /// Local edition (the memory rule is host-only on purpose, even though
    /// `file_create` is registered there).
    fn expect_present(&self) -> bool {
        cfg!(feature = "host") && self.memory_enabled && self.file_create_registered()
    }
}

fn rows() -> Vec<Row> {
    // Full 2x2 table in both editions. In Local, the (memory enabled,
    // file_create registered) row pins "registered but memory stays off".
    let mut out = Vec::new();
    for memory_enabled in [true, false] {
        for disable_file_create in [false, true] {
            out.push(Row {
                memory_enabled,
                disable_file_create,
            });
        }
    }
    out
}

#[test]
fn memory_section_present_iff_memory_enabled_and_file_create_registered() {
    for row in rows() {
        let cfg = parse_global(&global_yaml(row.memory_enabled, row.disable_file_create));
        let profile = cfg
            .catalog
            .profile("dev")
            .expect("profile dev must resolve")
            .clone();
        let agent = &profile.agents[0];
        let skills = SkillRegistry::new();

        // Oracle sanity: the edition's registration matches the row's setup.
        let env = ToolEnv::for_profile(&cfg, &profile, &Arc::new(Vec::new()));
        let registered = main_agent_tool_names(&env, agent);
        let file_create_registered = registered.iter().any(|n| n == "file_create");
        assert_eq!(
            file_create_registered,
            row.file_create_registered(),
            "oracle: file_create registered = {file_create_registered}, expected {} \
             (row: memory_enabled={}, disable_file_create={}); registered = {registered:?}",
            row.file_create_registered(),
            row.memory_enabled,
            row.disable_file_create
        );

        // The rule under test: section present iff enabled ∧ registered.
        let prompt =
            build_agent_prompt(agent, &profile, &skills, None::<ShellKind>, Path::new(CWD));
        let present = prompt.contains(MEMORY_MARKER);
        assert_eq!(
            present,
            row.expect_present(),
            "memory section present = {present}, expected {} (row: memory_enabled={}, \
             file_create registered = {file_create_registered}) — the rule is \
             `memory.enabled ∧ file_create registered`",
            row.expect_present(),
            row.memory_enabled
        );
    }
}
