//! 0.24 port of the old P2 `build_system_prompt` persona tests onto
//! `build_agent_prompt` (spec §6.1).
//!
//! The old `PromptHead` / `resolve_prompt_head` seam is gone: the head is
//! resolved at PARSE time into `agent.head` (`Head::BuiltIn` / `Persona` /
//! `Whole`), so these tests drive the real builder through `Config::parse`
//! and pin the same locked semantics:
//!
//! - `Head::Persona` ⇒ prompt begins with the trimmed persona and **does
//!   not** contain the built-in "CODE CRUSADER" content; the core follows
//! - `Head::BuiltIn` ⇒ the built-in crusader text leads (for single agents
//!   AND subagent-bearing agents — the orchestrator special case is gone)
//! - `Head::Whole` (`core_guidance: false`) ⇒ the persona is the whole
//!   static head: no core, no shell section, no built-in persona; every
//!   dynamic section (memory, skills, env, agents.md, `# Instructions`)
//!   still follows
//! - The byte boundary between persona and `# Working Principles` is a
//!   single `'\n'` join in both the `BuiltIn` and `Persona` branches
//!
//! The old `resolve_prompt_head` precedence table is deleted with the
//! function: in 0.24 there is exactly one head per agent, decided by the
//! parse (see `tests/persona_round_trip_tests.rs` for the head resolution
//! and its blank-persona errors).

use std::path::Path;

use peakbot::config::resolve::{Config, Head, ParseCtx, ResolvedAgent, ResolvedProfile, Source};
use peakbot::skills::SkillRegistry;
use peakbot::{ShellKind, build_agent_prompt};

const GLOBAL_PATH: &str = "/home/u/.config/peakbot/config.yaml";
/// Non-existent cwd: no agents.md, deterministic env block.
const FIXED_CWD: &str = "/opt/peakbot-prompt-cwd";

const BUILTIN_PERSONA: &str = include_str!("../src/system_prompt_persona.txt");
/// The first line of `src/system_prompt_core.txt` — the core's identity.
const CORE_HEAD: &str = "# Working Principles";

const PROVIDERS: &str = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n";

/// Profile `p` (the providers const above) with the given indented body.
fn cfg_with(profile_body: &str) -> Config {
    Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text: &format!("{PROVIDERS}profiles:\n  p:\n{profile_body}"),
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .unwrap_or_else(|e| panic!("config must parse: {e:?}"))
}

fn prompt_for<'a>(
    cfg: &'a Config,
    agent: &str,
    shell: Option<ShellKind>,
    cwd: &Path,
) -> (String, &'a ResolvedAgent, &'a ResolvedProfile) {
    let profile: &ResolvedProfile = cfg
        .catalog
        .profile("p")
        .unwrap_or_else(|| panic!("profile 'p' in catalog"));
    let agent: &ResolvedAgent = profile
        .agents
        .iter()
        .find(|a| a.main.name == agent)
        .unwrap_or_else(|| panic!("agent '{agent}' in profile 'p'"));
    (
        build_agent_prompt(agent, profile, &SkillRegistry::new(), shell, cwd),
        agent,
        profile,
    )
}

fn bash() -> Option<ShellKind> {
    Some(ShellKind::Bash {
        path: "/bin/sh".to_string(),
    })
}

#[test]
fn persona_head_replaces_crusader_and_keeps_core() {
    // `Head::Persona` replaces ONLY the persona — the core tool guidance
    // still follows (the old `agentless_prompt_with_some_persona_*` +
    // `persona_head_still_keeps_core`).
    let cfg = cfg_with(
        "    agents:\n      main:\n        model: a\n        persona: \"  SENTINEL-CUSTOM-PERSONA — neutral, no character.  \"\n",
    );
    let (prompt, agent, _) = prompt_for(&cfg, "main", bash(), Path::new(FIXED_CWD));
    assert!(
        matches!(agent.head, Head::Persona(_)),
        "fixture sanity: the agent carries a Persona head"
    );
    assert!(
        prompt.contains("SENTINEL-CUSTOM-PERSONA — neutral, no character."),
        "the prompt must carry the configured (trimmed) persona"
    );
    assert!(
        !prompt.contains("CODE CRUSADER"),
        "the prompt must NOT carry the built-in crusader persona when a custom one is set"
    );
    assert!(
        prompt.contains(CORE_HEAD),
        "the Persona head must keep the core tool guidance"
    );
    assert!(
        prompt.contains("# Shell Tool Usage"),
        "the Persona head must keep the rendered shell section (a shell is present)"
    );
}

#[test]
fn builtin_head_keeps_crusader_and_core() {
    // No persona configured ⇒ the built-in crusader leads, the core follows.
    let cfg = cfg_with("    agents:\n      main:\n        model: a\n");
    let (prompt, agent, _) = prompt_for(&cfg, "main", None, Path::new(FIXED_CWD));
    assert!(
        matches!(agent.head, Head::BuiltIn),
        "fixture sanity: absent persona resolves to BuiltIn"
    );
    assert!(
        prompt.starts_with(BUILTIN_PERSONA),
        "the prompt with no configured persona must keep the built-in crusader"
    );
    assert!(
        prompt.contains(CORE_HEAD),
        "the core follows the built-in persona"
    );
}

#[test]
fn subagent_bearing_agent_with_persona_carries_the_persona() {
    // The old "orchestrator carries the configured persona": an agent WITH
    // subagents and a Persona head leads with that persona — the built-in
    // crusader is never mixed in, and the core still follows.
    let cfg = cfg_with(
        "    agents:\n      team:\n        model: a\n        persona: SENTINEL-CUSTOM-PERSONA\n        subagents:\n          s:\n            prompt: do it\n",
    );
    let (prompt, agent, _) = prompt_for(&cfg, "team", None, Path::new(FIXED_CWD));
    assert!(
        !agent.subagents.is_empty(),
        "fixture sanity: team has a subagent"
    );
    assert!(
        prompt.contains("SENTINEL-CUSTOM-PERSONA"),
        "the subagent-bearing agent must carry the configured persona"
    );
    assert!(
        !prompt.contains("CODE CRUSADER"),
        "the configured persona replaces the built-in crusader"
    );
    assert!(
        prompt.contains(CORE_HEAD),
        "the core tool guidance still follows"
    );
}

#[test]
fn subagent_bearing_agent_without_persona_gets_the_built_in() {
    // The documented 0.24 delta (spec §6.1 change 1): the old orchestrator
    // got NO persona at all — in 0.24 a subagent-bearing `Head::BuiltIn`
    // agent leads with the built-in persona, same as a single agent.
    let cfg = cfg_with(
        "    agents:\n      team:\n        model: a\n        subagents:\n          s:\n            prompt: do it\n",
    );
    let (prompt, agent, _) = prompt_for(&cfg, "team", None, Path::new(FIXED_CWD));
    assert!(
        !agent.subagents.is_empty() && matches!(agent.head, Head::BuiltIn),
        "fixture sanity: a subagent-bearing BuiltIn agent"
    );
    assert!(
        prompt.starts_with(BUILTIN_PERSONA),
        "the built-in persona now leads a subagent-bearing agent (the orchestrator special case is gone)"
    );
    assert!(prompt.contains(CORE_HEAD), "…and the core follows");
}

#[test]
fn persona_and_instructions_are_independent() {
    // The old "orchestrator addendum keeps persona + addendum": the
    // `# Instructions` section (`agent.prompt`) and the head are
    // independent — both appear, the instructions last.
    let cfg = cfg_with(
        "    agents:\n      team:\n        model: a\n        persona: SENTINEL-CUSTOM-PERSONA\n        prompt: SENTINEL-ORCH-EXTRA\n        subagents:\n          s:\n            prompt: do it\n",
    );
    let (prompt, _, _) = prompt_for(&cfg, "team", None, Path::new(FIXED_CWD));
    assert!(
        prompt.contains("SENTINEL-CUSTOM-PERSONA"),
        "the configured persona appears"
    );
    assert!(
        prompt.contains("\n# Instructions\n\nSENTINEL-ORCH-EXTRA\n"),
        "the instructions appear under the 0.24 heading"
    );
    assert!(
        prompt.rfind("# Instructions").unwrap()
            > prompt.rfind("# Environment Information").unwrap(),
        "the instructions section is the last piece of the prompt"
    );
}

#[test]
fn persona_to_core_join_is_a_single_newline_for_both_branches() {
    // The lone `push('\n')` is not decoration: a configured persona is
    // byte-compatible with the built-in join — exactly one `'\n'` before
    // `# Working Principles` in both branches.
    let configured = cfg_with(
        "    agents:\n      main:\n        model: a\n        persona: MY CUSTOM PERSONA\n",
    );
    let builtin = cfg_with("    agents:\n      main:\n        model: a\n");
    let (prompt_c, _, _) = prompt_for(&configured, "main", None, Path::new(FIXED_CWD));
    let (prompt_b, _, _) = prompt_for(&builtin, "main", None, Path::new(FIXED_CWD));

    let core_idx_c = prompt_c
        .find(CORE_HEAD)
        .expect("core marker must be present");
    let core_idx_b = prompt_b
        .find(CORE_HEAD)
        .expect("core marker must be present");

    assert_eq!(
        prompt_c.as_bytes()[core_idx_c - 1],
        b'\n',
        "configured persona must end in a single \\n before the core"
    );
    assert_eq!(
        prompt_b.as_bytes()[core_idx_b - 1],
        b'\n',
        "built-in persona must end in a single \\n before the core"
    );
    assert!(
        !prompt_c.contains("MY CUSTOM PERSONA\n\n# Working Principles"),
        "the join is \\n, not \\n\\n"
    );
}

#[test]
fn whole_head_replaces_persona_and_core() {
    // `Head::Whole` (`core_guidance: false`) owns the entire static head:
    // the built-in persona AND the core tool guidance (including the shell
    // section) are all suppressed. The padded input pins the `trim()` — the
    // prompt opens on the trimmed text.
    let cfg = cfg_with(
        "    agents:\n      main:\n        model: a\n        core_guidance: false\n        persona: \"  \\nRESEARCH ONLY — read files and cite sources, never edit.\\n  \"\n",
    );
    let (prompt, agent, _) = prompt_for(&cfg, "main", bash(), Path::new(FIXED_CWD));
    assert!(
        matches!(agent.head, Head::Whole(_)),
        "fixture sanity: core_guidance: false resolves to Whole"
    );
    assert!(
        prompt.starts_with("RESEARCH ONLY — read files and cite sources, never edit."),
        "the prompt must START with the trimmed whole head; got: {:?}",
        &prompt[..prompt.len().min(80)]
    );
    assert!(
        !prompt.contains(CORE_HEAD),
        "a Whole head must suppress the core guidance"
    );
    assert!(
        !prompt.contains("# Shell Tool Usage"),
        "a Whole head must suppress the shell section even with a shell present"
    );
    assert!(
        !prompt.contains("CODE CRUSADER"),
        "a Whole head must suppress the built-in crusader persona"
    );
}

#[test]
fn whole_head_keeps_every_dynamic_section() {
    // The override replaces the HEAD, not the whole prompt: the memory
    // section, the skills section, the env block and the agents.md content
    // all still follow a `Whole` head.
    let cwd = tempfile::tempdir().expect("isolated cwd");
    std::fs::write(
        cwd.path().join("agents.md"),
        "SENTINEL-AGENTS-MD: repo rules live here.\n",
    )
    .expect("agents.md written");

    // One valid skill folder, discovered the way boot discovers skills.
    let skill_dir = cwd.path().join("sentinel-test-skill");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: sentinel-test-skill\ndescription: Sentinel skill for prompt tests.\n---\nBody.\n",
    )
    .expect("SKILL.md written");
    let mut skills = SkillRegistry::new();
    let mut warnings = Vec::new();
    skills.load_from_directory(cwd.path(), &mut warnings);
    assert_eq!(skills.len(), 1, "fixture skill must load");

    let cfg = cfg_with(
        "    agents:\n      main:\n        model: a\n        core_guidance: false\n        persona: RESEARCH ONLY — read files and cite sources, never edit.\n",
    );
    let profile = cfg.catalog.profile("p").expect("profile p");
    let agent = &profile.agents[0];
    let prompt = build_agent_prompt(agent, profile, &skills, None, cwd.path());
    assert!(
        prompt.starts_with("RESEARCH ONLY — read files and cite sources, never edit."),
        "the whole head must still lead the prompt; got: {:?}",
        &prompt[..prompt.len().min(80)]
    );
    assert!(
        prompt.contains("# Memory.md"),
        "the memory section must follow the Whole head (memory on by default)"
    );
    assert!(
        prompt.contains("# Skills"),
        "the skills section must follow the Whole head"
    );
    assert!(
        prompt.contains("sentinel-test-skill"),
        "the registered skill must be listed in the skills section"
    );
    assert!(
        prompt.contains("# Environment Information"),
        "the env block must follow the Whole head"
    );
    assert!(
        prompt.contains("# Agents.md Content"),
        "the agents.md section must follow the Whole head (main agent default on)"
    );
    assert!(
        prompt.contains("SENTINEL-AGENTS-MD"),
        "the agents.md content must reach the prompt"
    );
}

#[test]
fn whole_head_in_subagent_agent_keeps_instructions() {
    // The override is UNCONDITIONAL: for a subagent-bearing agent the
    // `# Instructions` block still follows the Whole head, and the crusader
    // and the core are still absent — the same head semantics everywhere.
    let cfg = cfg_with(
        "    agents:\n      team:\n        model: a\n        core_guidance: false\n        persona: RESEARCH ONLY — read files and cite sources, never edit.\n        prompt: \"SENTINEL-ORCH-EXTRA: delegate to the team.\"\n        subagents:\n          s:\n            prompt: do it\n",
    );
    let (prompt, agent, _) = prompt_for(&cfg, "team", bash(), Path::new(FIXED_CWD));
    assert!(
        !agent.subagents.is_empty() && matches!(agent.head, Head::Whole(_)),
        "fixture sanity: a subagent-bearing Whole agent"
    );
    assert!(
        prompt.starts_with("RESEARCH ONLY — read files and cite sources, never edit."),
        "the prompt must START with the whole head; got: {:?}",
        &prompt[..prompt.len().min(80)]
    );
    assert!(
        prompt.contains("\n# Instructions\n\nSENTINEL-ORCH-EXTRA: delegate to the team.\n"),
        "the instructions block must follow the Whole head"
    );
    assert!(
        !prompt.contains("CODE CRUSADER"),
        "the built-in crusader must stay absent"
    );
    assert!(
        !prompt.contains(CORE_HEAD),
        "the core guidance must stay absent (Whole head)"
    );
    assert!(
        !prompt.contains("# Shell Tool Usage"),
        "the shell section must stay absent (Whole head)"
    );
}
