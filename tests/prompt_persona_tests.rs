//! P2 — `build_system_prompt` persona replacement tests (plan §A-Q7).
//!
//! **Status: compile-fail until P2 lands.** This file targets the planned
//! `build_system_prompt(... persona: Option<&str>, ...)` parameter. The
//! current signature has no `persona` argument and always emits
//! `SYSTEM_PROMPT_PERSONA` (the crusader text) when `subagents_active` is
//! false. Once P2 wires the optional parameter through, these tests will
//! compile and assert the locked semantics from §A-Q7:
//!
//! - `Some(p)` ⇒ prompt begins with `p` and **does not** contain the built-in
//!   "CODE CRUSADER" content
//! - `None` / whitespace-only ⇒ prompt contains the built-in crusader text
//! - Orchestrator mode (`subagents_active = true`) ⇒ **neither** persona
//!   appears regardless of the configured value
//! - The byte boundary between persona and `# Working Principles` is
//!   identical for both branches (a single `'\n'` join)
//!
//! The dedicated sub-agent-preamble test (plan §A-Q7 row 3 — persona MUST
//! NOT leak into a role preamble) lives next to the helper itself in
//! `src/pipeline/delegate_tool.rs::tests`; it is a GREEN guard today.
//!
//! Until the impl lands, this file fails to compile and `cargo test` stops
//! at this integration target. That is the RED state.

use peakbot::build_system_prompt;
use peakbot::skills::SkillRegistry;

fn cwd() -> std::path::PathBuf {
    std::env::temp_dir()
}

#[test]
fn agentless_prompt_with_some_persona_replaces_crusader() {
    let skills = SkillRegistry::new();
    let custom = "SENTINEL-CUSTOM-PERSONA — neutral, no character.";
    let prompt = build_system_prompt(
        &skills,
        None,
        &cwd(),
        /* memory_enabled */ false,
        /* subagents_active */ false,
        /* orchestrator_prompt */ None,
        /* persona */ Some(custom),
    );
    assert!(
        prompt.contains(custom),
        "agentless prompt must carry the configured persona"
    );
    assert!(
        !prompt.contains("CODE CRUSADER"),
        "agentless prompt must NOT carry the built-in crusader persona when a custom one is set"
    );
}

#[test]
fn agentless_prompt_with_none_persona_keeps_crusader() {
    // Plan §A-Q7: "None / whitespace-only → built-in persona present." This
    // is the GREEN guard against an over-aggressive impl that defaults to
    // empty-string instead of falling back.
    let skills = SkillRegistry::new();
    let prompt = build_system_prompt(
        &skills,
        None,
        &cwd(),
        false,
        false,
        None,
        /* persona */ None,
    );
    assert!(
        prompt.contains("CODE CRUSADER"),
        "agentless prompt with no configured persona must keep the built-in crusader"
    );
}

#[test]
fn agentless_prompt_with_whitespace_only_persona_keeps_crusader() {
    // §A-Q7: "trim, empty → None" applies at both the accessor AND the
    // prompt builder (mirroring `orchestrator_prompt`'s discipline).
    let skills = SkillRegistry::new();
    let prompt = build_system_prompt(
        &skills,
        None,
        &cwd(),
        false,
        false,
        None,
        Some("   \n  \t  \n"),
    );
    assert!(
        prompt.contains("CODE CRUSADER"),
        "whitespace-only persona must be treated as absent and fall back to the built-in"
    );
    assert!(
        !prompt.contains("   \n  \t  \n"),
        "the whitespace payload itself must not appear in the prompt"
    );
}

#[test]
fn orchestrator_prompt_drops_configured_persona() {
    // §A-Q7: orchestrator mode is unchanged by the persona key. The crusader
    // is dropped (and now also: the configured persona is dropped) — the
    // whole recipe is gated on `!subagents_active`.
    let skills = SkillRegistry::new();
    let custom = "SENTINEL-CUSTOM-PERSONA";
    let prompt = build_system_prompt(
        &skills,
        None,
        &cwd(),
        false,
        /* subagents_active */ true,
        /* orchestrator_prompt */ None,
        Some(custom),
    );
    assert!(
        !prompt.contains(custom),
        "orchestrator prompt must NOT carry the configured persona"
    );
    assert!(
        !prompt.contains("CODE CRUSADER"),
        "orchestrator prompt must NOT carry the built-in crusader"
    );
    assert!(
        prompt.contains("# Working Principles"),
        "orchestrator prompt must still carry the core tool guidance"
    );
}

#[test]
fn orchestrator_prompt_with_addendum_keeps_orchestrator_prompt_and_drops_persona() {
    let skills = SkillRegistry::new();
    let custom = "SENTINEL-CUSTOM-PERSONA";
    let extra = "SENTINEL-ORCH-EXTRA";
    let prompt = build_system_prompt(
        &skills,
        None,
        &cwd(),
        false,
        true,
        Some(extra),
        Some(custom),
    );
    assert!(
        prompt.contains(extra),
        "orchestrator prompt addendum must still appear"
    );
    assert!(
        !prompt.contains(custom),
        "persona must not leak into orchestrator prompt even with addendum"
    );
}

#[test]
fn persona_to_core_join_is_a_single_newline_for_both_branches() {
    // §A-Q7: "system_prompt_persona.txt ends with exactly one \n and
    // system_prompt_core.txt starts with `# Working Principles` (no leading
    // newline). The lone `push('\n')` is not decoration: reproducing that
    // single newline makes a custom persona byte-compatible with the
    // built-in join."
    //
    // We test the simpler invariant: both prompts end their persona with
    // exactly one '\n' immediately before `# Working Principles`. The join
    // is `\n`, not `\n\n`.
    let skills = SkillRegistry::new();
    let custom = "MY CUSTOM PERSONA";
    let configured = build_system_prompt(&skills, None, &cwd(), false, false, None, Some(custom));
    let builtin = build_system_prompt(&skills, None, &cwd(), false, false, None, None);

    let core_idx_configured = configured
        .find("# Working Principles")
        .expect("core marker must be present");
    let core_idx_builtin = builtin
        .find("# Working Principles")
        .expect("core marker must be present");

    assert_eq!(
        configured.as_bytes()[core_idx_configured - 1],
        b'\n',
        "configured persona must end in a single \\n before the core"
    );
    assert_eq!(
        builtin.as_bytes()[core_idx_builtin - 1],
        b'\n',
        "built-in persona must end in a single \\n before the core"
    );
}
