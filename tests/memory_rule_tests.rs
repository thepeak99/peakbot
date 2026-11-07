//! Memory rule (mobile plan §2.7 derived rule / §4.3 test 3) — RED.
//!
//! The locked rule: **the memory.md prompt section (and the compaction
//! memory flush, which shares the same gate) is present iff
//! `memory.enabled ∧ file_create is registered`** — always false in the
//! Local edition, and false in the full edition when a `tools:` filter
//! disables `file_create`.
//!
//! **RED today by BEHAVIOUR (full edition only):** the current gate is
//! `memory.enabled && cfg!(feature = "host")` (the private `memory_applies`
//! in `src/lib.rs`), which does NOT consult the `tools:` filter. Row
//! (enabled, `file_create` disabled) fails: the prompt carries the memory
//! section although `file_create` is not registered. Every other row is
//! green today and stays a guard.
//!
//! **The seam the builder (L5) must implement:** `build_agent_prompt`
//! decides the memory section by the SAME registration check the tool build
//! uses — `file_create` in `main_agent_tool_names` for the bound agent
//! (i.e. `profile.tools.allows("file_create")` in the full edition, never in
//! Local). The compaction flush call site in `agent_loop` must stay on the
//! same derived rule (same predicate, not a copy of `cfg!(feature = "host")`).
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
        // Host-only row: in Local `file_create` is not a known tool and the
        // parse would (correctly) reject the name.
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
    /// `file_create` is registered in the full edition iff the filter
    /// allows it; in Local it is never registered (not a Local tool name).
    fn file_create_registered(&self) -> bool {
        cfg!(feature = "host") && !self.disable_file_create
    }

    /// The locked rule: section present iff `memory.enabled ∧ file_create
    /// registered`.
    fn expect_present(&self) -> bool {
        self.memory_enabled && self.file_create_registered()
    }
}

fn rows() -> Vec<Row> {
    let mut out = vec![
        Row {
            memory_enabled: true,
            disable_file_create: false,
        },
        Row {
            memory_enabled: false,
            disable_file_create: false,
        },
    ];
    if cfg!(feature = "host") {
        // The RED row: enabled memory + `file_create` filtered out ⇒ no
        // section, even in the full edition.
        out.push(Row {
            memory_enabled: true,
            disable_file_create: true,
        });
    }
    // Local: the first row IS the "enabled memory, no file tools" row —
    // expect_present() is false there, so Local pins "never present, even
    // with memory.enabled: true" on that row. No filter row: `file_create`
    // is not a Local tool name at all (the parse would reject it).
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
        let prompt = build_agent_prompt(agent, &profile, &skills, None::<ShellKind>, Path::new(CWD));
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
