//! P2 — `build_system_prompt` persona replacement tests (plan §A-Q7, amended
//! by multi-pipeline amendment 1).
//!
//! The `head: Option<&PromptHead>` parameter carries the resolved static head
//! — a pipeline's `orchestrator.persona` when set, else the global `persona:`.
//! These tests cover the `PromptHead::Persona` variant; the locked semantics:
//!
//! - `Some(p)` ⇒ prompt begins with `p` and **does not** contain the built-in
//!   "CODE CRUSADER" content
//! - `None` / whitespace-only ⇒ prompt contains the built-in crusader text
//! - Orchestrator mode (`subagents_active = true`) ⇒ a **configured** persona
//!   leads the prompt (multi-pipeline amendment 1: a pipeline's
//!   `orchestrator.persona` replaces the global one; omitted, the global
//!   persona applies unchanged), while an absent one yields **no** persona at
//!   all — the built-in crusader is never the orchestrator's default
//! - The byte boundary between persona and `# Working Principles` is
//!   identical for both branches (a single `'\n'` join)
//!
//! The `PromptHead::Full` variant (config `system_prompt:`) replaces the
//! persona AND the core tool guidance — `SYSTEM_PROMPT_CORE` is mostly
//! tool-specific coaching (bash section, "NEVER Use Bash/sed/awk", head/tail
//! rules) that lies to a model whose deployment disables those tools. The
//! locked semantics:
//!
//! - `Some(Full(s))` non-blank ⇒ prompt starts with `s.trim()` + `'\n'`,
//!   NO core, NO built-in persona — in agentless AND orchestrator mode
//! - Everything after the head (memory, skills, env block, agents.md,
//!   `# Orchestrator Instructions`) is identical in all cases
//! - Blank `Full` is absent: built-in persona (agentless) + core
//!
//! `resolve_prompt_head` precedence is pinned here too (config `Full` wins
//! outright; else the pipeline persona; else the config head).
//!
//! The dedicated sub-agent-preamble test (plan §A-Q7 row 3 — persona MUST
//! NOT leak into a role preamble) lives next to the helper itself in
//! `src/pipeline/delegate_tool.rs::tests`; it is a GREEN guard today.

use peakbot::skills::SkillRegistry;
use peakbot::{PromptHead, build_system_prompt, resolve_prompt_head};

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
        /* head */ Some(&PromptHead::Persona(custom.to_string())),
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
        Some(&PromptHead::Persona("   \n  \t  \n".to_string())),
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
fn orchestrator_prompt_carries_configured_persona() {
    // Multi-pipeline amendment 1: an orchestrator's persona — a pipeline's own
    // `orchestrator.persona`, or the global `persona:` when the pipeline omits
    // one — REPLACES the global persona and reaches the orchestrator recipe.
    // Only the built-in crusader stays agentless-only.
    let skills = SkillRegistry::new();
    let custom = "SENTINEL-CUSTOM-PERSONA";
    let prompt = build_system_prompt(
        &skills,
        None,
        &cwd(),
        false,
        /* subagents_active */ true,
        /* orchestrator_prompt */ None,
        Some(&PromptHead::Persona(custom.to_string())),
    );
    assert!(
        prompt.contains(custom),
        "orchestrator prompt must carry the configured persona"
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
fn orchestrator_prompt_without_persona_carries_none_at_all() {
    // The other half of amendment 1: nothing configured (no pipeline persona,
    // no global persona) ⇒ the orchestrator leads with the core guidance. The
    // crusader is NOT an orchestrator fallback.
    let skills = SkillRegistry::new();
    let prompt = build_system_prompt(
        &skills,
        None,
        &cwd(),
        false,
        /* subagents_active */ true,
        None,
        /* persona */ None,
    );
    assert!(
        !prompt.contains("CODE CRUSADER"),
        "orchestrator prompt with no configured persona must carry no persona"
    );
    assert!(
        prompt.starts_with("# Working Principles"),
        "with no persona the orchestrator prompt must open on the core guidance; got: {:?}",
        &prompt[..prompt.len().min(80)]
    );
}

#[test]
fn orchestrator_prompt_with_addendum_keeps_orchestrator_prompt_and_persona() {
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
        Some(&PromptHead::Persona(custom.to_string())),
    );
    assert!(
        prompt.contains(extra),
        "orchestrator prompt addendum must still appear"
    );
    assert!(
        prompt.contains(custom),
        "the configured persona and the addendum are independent — both appear"
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
    let head = Some(PromptHead::Persona(custom.to_string()));
    let configured = build_system_prompt(&skills, None, &cwd(), false, false, None, head.as_ref());
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

// ---------------------------------------------------------------------------
// `PromptHead::Full` (config `system_prompt:`) — replaces the persona AND
// the core tool guidance, and nothing beyond the head.
//
// Distinctive strings asserted on (taken verbatim from the fixture files):
//   src/system_prompt_core.txt:24  `### NEVER Use Bash/sed/awk For:`
//   src/system_prompt_core.txt:49  `# Bash Tool Usage`
//   src/system_prompt_core.txt:50  `- Do NOT use `| head` or `| tail` in commands`
//   src/system_prompt_core.txt:1   `# Working Principles`
//   src/system_prompt_persona.txt:6 `CODE CRUSADER`
//   src/system_prompt_memory.txt   `# Memory.md` / `memory.md in the root of the project`
//   src/lib.rs env_block           `# Environment Information`
//   src/lib.rs agents_md_section   `# Agents.md Content`
//   src/lib.rs orchestrator block  `# Orchestrator Instructions`
// ---------------------------------------------------------------------------

#[test]
fn full_head_replaces_persona_and_core() {
    // `Full` owns the entire static head: the built-in persona AND
    // `SYSTEM_PROMPT_CORE` are both suppressed. A deployment that disables
    // bash/think/todo must not ship coaching for tools the model cannot call.
    // The padded input also pins the `trim()` — the prompt must open on the
    // trimmed text, not the raw payload.
    let skills = SkillRegistry::new();
    let full = "RESEARCH ONLY — read files and cite sources, never edit.";
    let cwd = tempfile::tempdir().expect("isolated cwd");
    let prompt = build_system_prompt(
        &skills,
        None,
        cwd.path(),
        /* memory_enabled */ false,
        /* subagents_active */ false,
        /* orchestrator_prompt */ None,
        /* head */ Some(&PromptHead::Full(format!("  \n{full}\n  "))),
    );
    assert!(
        prompt.starts_with(full),
        "agentless prompt must START with the trimmed full head; got: {:?}",
        &prompt[..prompt.len().min(80)]
    );
    assert!(
        !prompt.contains("### NEVER Use Bash/sed/awk For:"),
        "a Full head must suppress the core bash coaching (system_prompt_core.txt:24)"
    );
    assert!(
        !prompt.contains("# Bash Tool Usage"),
        "a Full head must suppress the core `# Bash Tool Usage` section (system_prompt_core.txt:49)"
    );
    assert!(
        !prompt.contains("- Do NOT use `| head` or `| tail` in commands"),
        "a Full head must suppress the core head/tail coaching line (system_prompt_core.txt:50)"
    );
    assert!(
        !prompt.contains("CODE CRUSADER"),
        "a Full head must suppress the built-in crusader persona (system_prompt_persona.txt:6)"
    );
}

#[test]
fn full_head_keeps_every_dynamic_section() {
    // The override replaces the HEAD, not the whole prompt: the memory
    // section, the skills section, the env block and the agents.md content
    // all still follow a `Full` head.
    let cwd = tempfile::tempdir().expect("isolated cwd");
    std::fs::write(
        cwd.path().join("agents.md"),
        "SENTINEL-AGENTS-MD: repo rules live here.\n",
    )
    .expect("agents.md written");

    // Non-empty registry via the public loader: one valid skill folder.
    let skills_root = tempfile::tempdir().expect("skills root");
    let skill_dir = skills_root.path().join("sentinel-test-skill");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: sentinel-test-skill\ndescription: Sentinel skill for prompt tests.\n---\nBody.\n",
    )
    .expect("SKILL.md written");
    let mut skills = SkillRegistry::new();
    let mut warnings = Vec::new();
    skills.load_from_directory(skills_root.path(), &mut warnings);
    assert_eq!(skills.len(), 1, "fixture skill must load");

    let full = "RESEARCH ONLY — read files and cite sources, never edit.";
    let prompt = build_system_prompt(
        &skills,
        None,
        cwd.path(),
        /* memory_enabled */ true,
        /* subagents_active */ false,
        /* orchestrator_prompt */ None,
        /* head */ Some(&PromptHead::Full(full.to_string())),
    );
    assert!(
        prompt.starts_with(full),
        "the full head must still lead the prompt; got: {:?}",
        &prompt[..prompt.len().min(80)]
    );
    assert!(
        prompt.contains("# Memory.md"),
        "the memory section must follow the Full head (memory_enabled=true)"
    );
    assert!(
        prompt.contains("memory.md in the root of the project"),
        "the memory section body must follow the Full head"
    );
    assert!(
        prompt.contains("# Skills"),
        "the skills section must follow the Full head"
    );
    assert!(
        prompt.contains("sentinel-test-skill"),
        "the registered skill must be listed in the skills section"
    );
    assert!(
        prompt.contains("# Environment Information"),
        "the env block must follow the Full head"
    );
    assert!(
        prompt.contains("# Agents.md Content"),
        "the agents.md section must follow the Full head"
    );
    assert!(
        prompt.contains("SENTINEL-AGENTS-MD"),
        "the agents.md content must reach the prompt"
    );
}

#[test]
fn full_head_in_orchestrator_mode_keeps_orchestrator_instructions() {
    // The override is UNCONDITIONAL: in orchestrator mode the `# Orchestrator
    // Instructions` block still follows the Full head, and the crusader and
    // the core are still absent — the same head semantics in both recipes.
    let skills = SkillRegistry::new();
    let full = "RESEARCH ONLY — read files and cite sources, never edit.";
    let extra = "SENTINEL-ORCH-EXTRA: delegate to the team.";
    let cwd = tempfile::tempdir().expect("isolated cwd");
    let prompt = build_system_prompt(
        &skills,
        None,
        cwd.path(),
        /* memory_enabled */ false,
        /* subagents_active */ true,
        /* orchestrator_prompt */ Some(extra),
        /* head */ Some(&PromptHead::Full(full.to_string())),
    );
    assert!(
        prompt.starts_with(full),
        "orchestrator prompt must START with the full head; got: {:?}",
        &prompt[..prompt.len().min(80)]
    );
    assert!(
        prompt.contains("# Orchestrator Instructions"),
        "the orchestrator instructions block must follow the Full head"
    );
    assert!(
        prompt.contains(extra),
        "the orchestrator prompt text must appear after the Full head"
    );
    assert!(
        !prompt.contains("CODE CRUSADER"),
        "the built-in crusader must stay absent in orchestrator mode"
    );
    assert!(
        !prompt.contains("### NEVER Use Bash/sed/awk For:"),
        "the core bash coaching must stay absent in orchestrator mode"
    );
    assert!(
        !prompt.contains("# Bash Tool Usage"),
        "the core `# Bash Tool Usage` section must stay absent in orchestrator mode"
    );
}

#[test]
fn persona_head_still_keeps_core() {
    // Contrast pin: `Persona` replaces ONLY the persona — the core tool
    // guidance still follows. This is what stops anyone "simplifying"
    // Persona and Full into one arm. (The existing join test also fails if
    // the core disappears here, via its `expect("core marker must be
    // present")` precondition — this names the invariant directly.)
    let skills = SkillRegistry::new();
    let custom = "SENTINEL-CUSTOM-PERSONA";
    let cwd = tempfile::tempdir().expect("isolated cwd");
    let prompt = build_system_prompt(
        &skills,
        None,
        cwd.path(),
        /* memory_enabled */ false,
        /* subagents_active */ false,
        /* orchestrator_prompt */ None,
        /* head */ Some(&PromptHead::Persona(custom.to_string())),
    );
    assert!(
        prompt.contains(custom),
        "agentless prompt must carry the configured persona"
    );
    assert!(
        prompt.contains("# Working Principles"),
        "the Persona head must keep the core guidance (system_prompt_core.txt:1)"
    );
    assert!(
        prompt.contains("### NEVER Use Bash/sed/awk For:"),
        "a deep core line must survive the Persona head (system_prompt_core.txt:24)"
    );
    assert!(
        !prompt.contains("CODE CRUSADER"),
        "the built-in crusader must be replaced by the configured persona"
    );
}

#[test]
fn resolve_prompt_head_precedence() {
    // Table over the documented precedence:
    //   1. a config `Full` outranks a pipeline persona (deployment ceiling)
    //   2. else the pipeline's `orchestrator.persona` wins (amendment 1)
    //   3. else the config's own head, or `None` for the built-in
    let full = PromptHead::Full("FULL-HEAD".to_string());
    let cfg_persona = PromptHead::Persona("CFG-PERSONA".to_string());

    assert_eq!(
        resolve_prompt_head(Some(&full), Some("PIPE-PERSONA")),
        Some(PromptHead::Full("FULL-HEAD".to_string())),
        "config Full + pipeline persona ⇒ Full (config wins)"
    );
    assert_eq!(
        resolve_prompt_head(None, Some("PIPE-PERSONA")),
        Some(PromptHead::Persona("PIPE-PERSONA".to_string())),
        "config None + pipeline persona ⇒ Persona(pipeline)"
    );
    assert_eq!(
        resolve_prompt_head(Some(&cfg_persona), Some("PIPE-PERSONA")),
        Some(PromptHead::Persona("PIPE-PERSONA".to_string())),
        "config Persona + pipeline persona ⇒ Persona(pipeline)"
    );
    assert_eq!(
        resolve_prompt_head(Some(&cfg_persona), None),
        Some(PromptHead::Persona("CFG-PERSONA".to_string())),
        "config Persona + no pipeline ⇒ Persona(config)"
    );
    assert_eq!(resolve_prompt_head(None, None), None, "neither ⇒ None");
}

#[test]
fn blank_full_head_falls_back_to_built_in() {
    // Mirrors the whitespace-persona discipline: a whitespace-only `Full`
    // is ABSENT, not an empty head — the built-in persona (agentless) and
    // the core apply, and the payload itself never reaches the prompt.
    let skills = SkillRegistry::new();
    let cwd = tempfile::tempdir().expect("isolated cwd");
    let prompt = build_system_prompt(
        &skills,
        None,
        cwd.path(),
        /* memory_enabled */ false,
        /* subagents_active */ false,
        /* orchestrator_prompt */ None,
        /* head */ Some(&PromptHead::Full("   \n\t ".to_string())),
    );
    assert!(
        prompt.contains("CODE CRUSADER"),
        "a whitespace-only Full head must fall back to the built-in crusader (agentless)"
    );
    assert!(
        prompt.contains("# Working Principles"),
        "a whitespace-only Full head must fall back to the core guidance"
    );
    assert!(
        !prompt.contains("   \n\t "),
        "the whitespace payload itself must not appear in the prompt"
    );
}
