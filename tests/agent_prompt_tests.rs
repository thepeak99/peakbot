//! T4 — RED tests for `build_agent_prompt`, the 0.24 system-prompt builder
//! (spec §6.1 prompt recipe; plan M1/T4).
//!
//! **Status: RED — fails to compile on the missing
//! `peakbot::build_agent_prompt` (the ONLY expected compile error).**
//! Everything else targets already-landed code: T1 `Config::parse`
//! (`peakbot::config::resolve`), `SkillRegistry`, `ShellKind`, and the A1
//! snapshots under `tests/fixtures/migrate/expected-behaviour/` (captured
//! from the OLD code by `tests/old_behaviour_snapshots.rs`).
//!
//! Assumed API (plan M1/T4; the old `build_system_prompt` stays until T8a):
//! ```ignore
//! // lib.rs, beside `build_system_prompt`
//! pub fn build_agent_prompt(
//!     agent: &peakbot::config::resolve::ResolvedAgent,
//!     profile: &peakbot::config::resolve::ResolvedProfile,
//!     skills: &SkillRegistry,
//!     shell: Option<ShellKind>,
//!     cwd: &std::path::Path,
//! ) -> String;
//! ```
//!
//! The recipe (spec §6.1, output order):
//! 1. static head: `Head::BuiltIn` → `src/system_prompt_persona.txt` — now ALSO
//!    for agents with subagents (the orchestrator special case is gone);
//!    `Head::Persona(p)` → trimmed `p`; `Head::Whole(p)` → trimmed `p`;
//! 2. core tool guidance + shell section — except `Head::Whole`;
//! 3. `memory.md` section — iff `profile.memory.enabled`;
//! 4. skills section — filtered by `agent.main.skills` (new: today the main
//!    agent sees ALL skills);
//! 5. env block (cwd / time / version / os / binary / shell);
//! 6. `agents.md` section — iff `agent.main.agents_md`;
//! 7. `\n# Instructions\n\n{prompt}\n` — iff `agent.prompt` is set, WITH or
//!    without subagents (old heading: `# Orchestrator Instructions`).
//!
//! Byte boundaries are today's: a persona is trimmed and followed by `\n`,
//! then the core starts; the built-in persona file's own trailing `\n` is the
//! boundary in the `BuiltIn` case.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use peakbot::config::resolve::{Config, Head, ParseCtx, ResolvedAgent, ResolvedProfile, Source};
use peakbot::skills::SkillRegistry;
use peakbot::tools::env::{McpSource, ToolEnv, main_agent_tool_names, sub_agent_tool_names};
use peakbot::{ShellKind, build_agent_prompt};

const GLOBAL_PATH: &str = "/home/u/.config/peakbot/config.yaml";

/// Same determinism constants as `tests/old_behaviour_snapshots.rs`, so the
/// prompts built here line up with the committed A1 snapshots byte-for-byte.
const FIXED_CWD: &str = "/opt/peakbot-snapshot-cwd"; // non-existent → no agents.md
const FIXED_SHELL_PATH: &str = "/bin/bash";
const FIXED_TIME: &str = "2000-01-01 00:00:00 UTC";
const FIXED_BIN_PATH: &str = "/usr/local/bin/peakbot";

/// The built-in persona, byte-for-byte (its trailing `\n` is the boundary
/// before the core guidance).
const BUILTIN_PERSONA: &str = include_str!("../src/system_prompt_persona.txt");
/// The first line of `src/system_prompt_core.txt` — the core's identity.
const CORE_HEAD: &str = "# Working Principles";

const SNAPSHOT_DIR: &str = "tests/fixtures/migrate/expected-behaviour";
const A1_FIXTURES: &[&str] = &["a_pipeline_team", "b_maintainer_shape"];

const PROVIDERS: &str = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\n";

fn parse_global(yaml: &str) -> Config {
    Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text: yaml,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .unwrap_or_else(|e| panic!("config must parse: {e:?}"))
}

/// One profile `p` (the providers const above) with the given indented body.
fn cfg_with(profile_body: &str) -> Config {
    parse_global(&format!("{PROVIDERS}profiles:\n  p:\n{profile_body}"))
}

/// The fixed bash shell every prompt in this file is built with.
fn fixed_shell() -> Option<ShellKind> {
    Some(ShellKind::Bash {
        path: FIXED_SHELL_PATH.to_string(),
    })
}

/// Build the prompt for `(profile, agent)` of a parsed config — the T4 call.
fn build_prompt(
    cfg: &Config,
    profile_name: &str,
    agent_name: &str,
    skills: &SkillRegistry,
    cwd: &Path,
) -> String {
    let profile: &ResolvedProfile = cfg
        .catalog
        .profile(profile_name)
        .unwrap_or_else(|| panic!("profile '{profile_name}' in catalog"));
    let agent: &ResolvedAgent = profile
        .agents
        .iter()
        .find(|a| a.main.name == agent_name)
        .unwrap_or_else(|| panic!("agent '{agent_name}' in profile '{profile_name}'"));
    build_agent_prompt(agent, profile, skills, fixed_shell(), cwd)
}

/// Profile `p`, agent `agent`, empty skill registry, the fixed absent cwd.
fn prompt_for(cfg: &Config, agent: &str) -> String {
    build_prompt(cfg, "p", agent, &SkillRegistry::new(), Path::new(FIXED_CWD))
}

/// `env_block` stamps a live clock and the test binary's exe path; fold both
/// to fixed values (the same normalisation `old_behaviour_snapshots.rs` uses).
fn normalize(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if line.starts_with("- **Current Time**: ") {
            out.push_str(&format!("- **Current Time**: {FIXED_TIME}"));
        } else if line.starts_with("- **PeakBot Binary Path**: ") {
            out.push_str(&format!("- **PeakBot Binary Path**: {FIXED_BIN_PATH}"));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// A registry holding exactly the named skills, discovered the way boot
/// discovers them (a directory of `SKILL.md` folders).
fn skill_registry(names: &[&str]) -> (tempfile::TempDir, SkillRegistry) {
    let tmp = tempfile::tempdir().expect("skills tempdir");
    for name in names {
        let dir = tmp.path().join(name);
        std::fs::create_dir_all(&dir).expect("skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Skill {name}\n---\n# Body\n"),
        )
        .expect("SKILL.md");
    }
    let mut registry = SkillRegistry::new();
    let mut warnings = Vec::new();
    registry.load_from_directory(tmp.path(), &mut warnings);
    assert!(
        warnings.is_empty(),
        "test skills must parse cleanly: {warnings:?}"
    );
    (tmp, registry)
}

// ---------- §6.1 items 1–2: the three head kinds ----------

#[test]
fn system_prompt_head_builtin() {
    // Single agent: the built-in persona leads, the core follows — today's
    // agentless recipe, unchanged.
    let cfg = cfg_with("    agents:\n      main:\n        model: a\n");
    let prompt = prompt_for(&cfg, "main");
    assert!(
        prompt.starts_with(BUILTIN_PERSONA),
        "the built-in persona file leads the prompt"
    );
    assert!(
        prompt.contains(CORE_HEAD),
        "the core tool guidance follows the persona"
    );

    // The headline change (spec §6.1 change 1): an agent WITH subagents and no
    // persona gets the built-in persona too — the orchestrator special case
    // is gone (today `build_system_prompt` skips the persona there).
    let cfg = cfg_with(
        "    agents:\n      main:\n        model: a\n        subagents:\n          s:\n            prompt: do it\n",
    );
    let prompt = prompt_for(&cfg, "main");
    assert!(
        prompt.starts_with(BUILTIN_PERSONA),
        "a subagent-bearing BuiltIn agent also leads with the built-in persona"
    );
    assert!(
        prompt.contains(CORE_HEAD),
        "…and the core guidance still follows"
    );
}

#[test]
fn system_prompt_head_persona() {
    let cfg = cfg_with(
        "    agents:\n      main:\n        model: a\n        persona: \"  You are the lead.  \"\n",
    );
    let prompt = prompt_for(&cfg, "main");
    assert!(
        prompt.starts_with("You are the lead.\n# Working Principles"),
        "trimmed persona + `\\n`, then the core starts — today's byte boundary"
    );
}

#[test]
fn system_prompt_head_whole() {
    // `core_guidance: false` (A1): the persona is the WHOLE static head.
    let cfg = cfg_with(
        "    agents:\n      main:\n        model: a\n        core_guidance: false\n        persona: You are the whole head.\n",
    );
    let prompt = prompt_for(&cfg, "main");
    assert!(
        prompt.starts_with("You are the whole head.\n"),
        "the trimmed persona is the whole head, followed by `\\n`"
    );
    assert!(
        !prompt.contains(CORE_HEAD),
        "core tool guidance is off — the persona replaces it (shell section included)"
    );
    assert!(
        prompt.contains("# Memory.md"),
        "the memory section is not part of the head and stays on"
    );
}

// ---------- §6.1 item 8: the `# Instructions` heading ----------

#[test]
fn instructions_heading_single_and_team() {
    // Single agent (no subagents): `prompt` renders `# Instructions` too
    // (spec §6.1 change 2 — today the heading is orchestrator-only).
    let cfg = cfg_with(
        "    agents:\n      solo:\n        model: a\n        prompt: Just do the thing.\n",
    );
    let prompt = prompt_for(&cfg, "solo");
    assert!(
        prompt.contains("\n# Instructions\n\nJust do the thing.\n"),
        "single agent: `\\n# Instructions\\n\\n{{prompt}}\\n`"
    );
    assert!(
        !prompt.contains("# Orchestrator Instructions"),
        "the old heading is gone"
    );
    assert!(
        prompt.rfind("# Instructions").unwrap()
            > prompt.rfind("# Environment Information").unwrap(),
        "the instructions section is the last piece of the prompt"
    );

    // Team agent: the SAME heading, not `# Orchestrator Instructions`.
    let cfg = cfg_with(
        "    agents:\n      team:\n        model: a\n        prompt: Lead the team.\n        subagents:\n          s:\n            prompt: do it\n",
    );
    let prompt = prompt_for(&cfg, "team");
    assert!(
        prompt.contains("\n# Instructions\n\nLead the team.\n"),
        "team agent: same `# Instructions` heading"
    );
    assert!(
        !prompt.contains("# Orchestrator Instructions"),
        "the old heading is gone"
    );

    // No `prompt`: no heading at all.
    let cfg = cfg_with("    agents:\n      main:\n        model: a\n");
    assert!(
        !prompt_for(&cfg, "main").contains("# Instructions"),
        "absent prompt: no `# Instructions` section"
    );
}

// ---------- §6.1 item 3: the memory toggle ----------

#[test]
fn memory_toggle() {
    // Default: on (memory.enabled defaults to true).
    let cfg = cfg_with("    agents:\n      main:\n        model: a\n");
    assert!(
        prompt_for(&cfg, "main").contains("# Memory.md"),
        "the memory.md section is on by default"
    );

    // `profile.memory.enabled: false` → the section disappears.
    let cfg =
        cfg_with("    memory:\n      enabled: false\n    agents:\n      main:\n        model: a\n");
    assert!(
        !prompt_for(&cfg, "main").contains("# Memory.md"),
        "memory.enabled: false drops the section"
    );
}

// ---------- §6.1 item 7: the agents.md toggle ----------

#[test]
fn agents_md_toggle() {
    // The old `agents_md_section` discovers the file case-insensitively —
    // the new builder must find it the same way.
    for fname in ["agents.md", "AGENTS.md", "Agents.Md"] {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(fname), "Be kind to the codebase.\n")
            .expect("write agents.md");

        let on = cfg_with("    agents:\n      main:\n        model: a\n        agents_md: true\n");
        let prompt_on = build_prompt(&on, "p", "main", &SkillRegistry::new(), dir.path());
        assert!(
            prompt_on.contains("# Agents.md Content"),
            "{fname}: agents_md: true injects the section"
        );
        assert!(
            prompt_on.contains("Be kind to the codebase."),
            "{fname}: the file content is inlined"
        );

        let off =
            cfg_with("    agents:\n      main:\n        model: a\n        agents_md: false\n");
        let prompt_off = build_prompt(&off, "p", "main", &SkillRegistry::new(), dir.path());
        assert!(
            !prompt_off.contains("Agents.md Content"),
            "{fname}: agents_md: false omits the section (no profile ceiling — the member's flag is final)"
        );

        // The main agent's default is ON (unlike subagents, whose default is off).
        let default_on = cfg_with("    agents:\n      main:\n        model: a\n");
        let prompt_def = build_prompt(&default_on, "p", "main", &SkillRegistry::new(), dir.path());
        assert!(
            prompt_def.contains("# Agents.md Content"),
            "{fname}: the main agent's agents_md default is on"
        );
    }
}

// ---------- §6.1 item 4: main-agent skill filtering (NEW) ----------

#[test]
fn main_agent_skills_filtered() {
    let (tmp, skills) = skill_registry(&["alpha", "beta", "gamma"]);
    let cwd = Path::new(FIXED_CWD);

    // Default filter (enabled, empty lists): every skill — today's main-agent
    // view, preserved.
    let cfg = cfg_with("    agents:\n      main:\n        model: a\n");
    let prompt = build_prompt(&cfg, "p", "main", &skills, cwd);
    for name in ["alpha", "beta", "gamma"] {
        assert!(
            prompt.contains(&format!("- {name}: ")),
            "default filter lists {name}"
        );
    }
    // Recipe order: the skills section sits between memory and the env block.
    let skills_at = prompt.find("# Skills").expect("skills section");
    let env_at = prompt.find("# Environment Information").expect("env block");
    assert!(
        skills_at < env_at,
        "the skills section precedes the env block"
    );

    // Allowlist: only `beta` reaches the main agent.
    let cfg = cfg_with(
        "    agents:\n      main:\n        model: a\n        skills:\n          only: [beta]\n",
    );
    let prompt = build_prompt(&cfg, "p", "main", &skills, cwd);
    assert!(
        prompt.contains("- beta: "),
        "the allowlisted skill is listed"
    );
    assert!(
        !prompt.contains("- alpha: "),
        "a non-allowlisted skill is filtered out"
    );
    assert!(
        !prompt.contains("- gamma: "),
        "a non-allowlisted skill is filtered out"
    );

    // Blocklist: `alpha` is removed, the rest stay.
    let cfg = cfg_with(
        "    agents:\n      main:\n        model: a\n        skills:\n          disabled: [alpha]\n",
    );
    let prompt = build_prompt(&cfg, "p", "main", &skills, cwd);
    assert!(
        !prompt.contains("- alpha: "),
        "the blocked skill is filtered out"
    );
    assert!(prompt.contains("- beta: "), "unblocked skills stay");
    assert!(prompt.contains("- gamma: "), "unblocked skills stay");

    // Master switch off: no skills section at all (not even the header).
    let cfg = cfg_with(
        "    agents:\n      main:\n        model: a\n        skills:\n          enabled: false\n",
    );
    let prompt = build_prompt(&cfg, "p", "main", &skills, cwd);
    assert!(
        !prompt.contains("# Skills"),
        "enabled: false means no skills section"
    );
    let _ = tmp; // keep the skill dirs alive until here
}

// ---------- A1 equivalence: the migrated fixtures vs the OLD snapshots ----------

/// One committed A1 snapshot: `<fixture>__<old profile|none>__<old pipeline|none>.txt`.
struct A1Pair {
    file: String,
    fixture: String,
    old_profile: String,
    old_pipeline: String,
    snapshot: String,
}

fn a1_pairs() -> Vec<A1Pair> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT_DIR);
    let mut pairs = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("A1 snapshot dir") {
        let path = entry.expect("snapshot entry").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".txt") else {
            continue;
        };
        let (fixture, rest) = stem.split_once("__").expect("fixture prefix");
        let (profile, pipeline) = rest.split_once("__").expect("profile__pipeline");
        pairs.push(A1Pair {
            file: name.to_string(),
            fixture: fixture.to_string(),
            old_profile: profile.to_string(),
            old_pipeline: pipeline.to_string(),
            snapshot: std::fs::read_to_string(&path).expect("read snapshot"),
        });
    }
    pairs.sort_by(|a, b| a.file.cmp(&b.file)); // deterministic order
    pairs
}

/// The main-agent prompt in a snapshot: the lines between the two markers,
/// minus the blank separator line `render_pair` appends after the prompt.
fn prompt_part(snapshot: &str) -> String {
    let lines: Vec<&str> = snapshot.lines().collect();
    let start = lines
        .iter()
        .position(|l| *l == "=== MAIN AGENT SYSTEM PROMPT ===")
        .expect("prompt marker");
    let end = lines
        .iter()
        .position(|l| *l == "=== BUILT-IN TOOLS (sorted) ===")
        .expect("tools marker");
    let mut body = lines[start + 1..end].to_vec();
    assert_eq!(
        body.last(),
        Some(&""),
        "a blank line separates the prompt from the tool list"
    );
    body.pop();
    body.join("\n") + "\n"
}

/// The migrated golden for one fixture, through THE boundary (`Config::parse`).
/// `known_skills` stays empty: fixture (a)'s `skills.only: [web_search,
/// github]` then only produces F6 warnings, which never touch the prompt.
fn parse_fixture_out(fixture: &str) -> Config {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/migrate")
        .join(format!("{fixture}.out.yaml"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    Config::parse(
        Source {
            path: &path,
            text: &text,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .unwrap_or_else(|e| panic!("migrated {fixture} must parse: {e:?}"))
}

/// The §11.2 behaviour-equivalence check, prompt half: build the REAL prompt
/// for every migrated `(profile, agent)` pair and compare it to the OLD
/// snapshot. Allowed differences are ONLY the two documented §6.1 changes —
/// each is applied to the expected text AND asserted explicitly, never
/// silently ignored.
#[test]
fn a1_prompt_equivalence() {
    let pairs = a1_pairs();
    assert_eq!(
        pairs.len(),
        6,
        "the six committed A1 snapshots (a: none/research/software-team; b: none/dev-team + research/none)"
    );
    let mut fixtures: Vec<&str> = pairs.iter().map(|p| p.fixture.as_str()).collect();
    fixtures.sort();
    fixtures.dedup();
    assert_eq!(
        fixtures, A1_FIXTURES,
        "only fixtures (a) and (b) carry A1 snapshots"
    );

    let mut configs: HashMap<String, Config> = HashMap::new();

    for pair in &pairs {
        let cfg = configs
            .entry(pair.fixture.clone())
            .or_insert_with(|| parse_fixture_out(&pair.fixture));

        // The migration's mapping: the old base config → the profile's FIRST
        // agent; each old pipeline → a same-named agent; old profile overlays
        // → same-named profiles (spec §7.2 / Q2).
        let profile_name = if pair.old_profile == "none" {
            "default"
        } else {
            pair.old_profile.as_str()
        };
        let profile: &ResolvedProfile = cfg
            .catalog
            .profile(profile_name)
            .unwrap_or_else(|| panic!("{}: profile '{profile_name}'", pair.file));
        let agent: &ResolvedAgent = if pair.old_pipeline == "none" {
            &profile.agents[0]
        } else {
            profile
                .agents
                .iter()
                .find(|a| a.main.name == pair.old_pipeline)
                .unwrap_or_else(|| panic!("{}: agent '{}'", pair.file, pair.old_pipeline))
        };

        let expected_old = prompt_part(&pair.snapshot);

        // Determinism: the same fixed inputs as the old capture — absent cwd
        // (no agents.md), fixed bash shell, empty skill registry; the live
        // clock and binary path are normalised exactly like
        // old_behaviour_snapshots.rs does.
        let skills = SkillRegistry::new();
        let actual = normalize(&build_agent_prompt(
            agent,
            profile,
            &skills,
            fixed_shell(),
            Path::new(FIXED_CWD),
        ));

        // Delta 1 (spec §6.1 change 1): an agent with subagents and NO
        // configured persona gets the built-in persona. The old builder
        // skipped it there — so the old snapshot must lack it, and the new
        // prompt must lead with it.
        let persona_delta = matches!(agent.head, Head::BuiltIn) && !agent.subagents.is_empty();
        let mut expected = String::new();
        if persona_delta {
            assert!(
                !expected_old.starts_with(BUILTIN_PERSONA),
                "{}: the old snapshot must lack the built-in persona for this delta to be real",
                pair.file
            );
            expected.push_str(BUILTIN_PERSONA);
        }
        expected.push_str(&expected_old);

        // Delta 2 (spec §6.1 change 2): `# Orchestrator Instructions` becomes
        // `# Instructions` (rendered for single agents too).
        let heading_delta = expected.contains("# Orchestrator Instructions");
        if heading_delta {
            expected = expected.replace("# Orchestrator Instructions", "# Instructions");
        }

        assert_eq!(
            actual, expected,
            "{}: the new prompt must equal the A1 snapshot modulo the two documented deltas",
            pair.file
        );

        if persona_delta {
            assert!(
                actual.starts_with(BUILTIN_PERSONA),
                "{}: the subagent-bearing BuiltIn agent now leads with the built-in persona",
                pair.file
            );
        }
        if heading_delta {
            assert!(
                actual.contains("# Instructions"),
                "{}: the new heading",
                pair.file
            );
            assert!(
                !actual.contains("# Orchestrator Instructions"),
                "{}: the old heading is gone",
                pair.file
            );
        }
        if !persona_delta && !heading_delta {
            // No documented delta applies: byte-identical to the old code.
            assert_eq!(
                actual, expected_old,
                "{}: an unchanged pair must be byte-identical to the old snapshot",
                pair.file
            );
        }
    }
}

// ---------- A1 equivalence: the tool-list half ----------
//
// The snapshots also captured the OLD main agent's `BUILT-IN TOOLS (sorted)`
// list. The 0.24 build registers the same built-ins (plus `delegate` and the
// profile's MCP tools); subagents get the SAME list minus `delegate` — the
// only allowed diff of the cutover (subagents now get the MCP tools too).

/// The snapshot's `BUILT-IN TOOLS (sorted)` section, as a sorted list.
fn tools_part(snapshot: &str) -> Vec<String> {
    let lines: Vec<&str> = snapshot.lines().collect();
    let start = lines
        .iter()
        .position(|l| *l == "=== BUILT-IN TOOLS (sorted) ===")
        .expect("tools marker");
    let end = lines
        .iter()
        .position(|l| *l == "=== MCP SERVERS (filtered names) ===")
        .expect("mcp marker");
    lines[start + 1..end]
        .iter()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// The `ToolEnv` for `profile` with ONE fake MCP tool per server the
/// profile's `mcps` filter allows — standing in for the live handles the
/// old capture could not list (the snapshot recorded server NAMES only).
fn env_with_fake_mcp(cfg: &Config, profile: &peakbot::config::resolve::ResolvedProfile) -> ToolEnv {
    let mut env = ToolEnv::for_profile(cfg, profile, &Arc::new(Vec::new()));
    let groups: Vec<(String, Vec<String>)> = cfg
        .mcps
        .iter()
        .filter(|server| profile.mcps.allows(&server.name))
        .map(|server| {
            (
                server.name.clone(),
                vec![format!("{}_fake_tool", server.name)],
            )
        })
        .collect();
    let fake_names: Vec<String> = groups
        .iter()
        .flat_map(|(_, tools)| tools.iter().cloned())
        .collect();
    env.mcp = McpSource::for_test(&groups, profile.mcps.clone());
    assert!(
        env.mcp.tool_names() == fake_names,
        "the fake MCP tools project through the profile filter"
    );
    env
}

/// The §11.2 behaviour-equivalence check, tool-list half: the new main
/// agent's built-ins equal the OLD snapshot's list exactly; the subagents'
/// list is the main list minus `delegate` (and carries the same MCP tools —
/// the only allowed diff of the cutover).
#[test]
fn a1_tool_list_equivalence() {
    let pairs = a1_pairs();
    assert_eq!(pairs.len(), 6, "the six committed A1 snapshots");
    let mut configs: HashMap<String, Config> = HashMap::new();

    for pair in &pairs {
        let cfg = configs
            .entry(pair.fixture.clone())
            .or_insert_with(|| parse_fixture_out(&pair.fixture));

        let profile_name = if pair.old_profile == "none" {
            "default"
        } else {
            pair.old_profile.as_str()
        };
        let profile: &ResolvedProfile = cfg
            .catalog
            .profile(profile_name)
            .unwrap_or_else(|| panic!("{}: profile '{profile_name}'", pair.file));
        let agent: &ResolvedAgent = if pair.old_pipeline == "none" {
            &profile.agents[0]
        } else {
            profile
                .agents
                .iter()
                .find(|a| a.main.name == pair.old_pipeline)
                .unwrap_or_else(|| panic!("{}: agent '{}'", pair.file, pair.old_pipeline))
        };

        let expected_old = tools_part(&pair.snapshot);
        let env = env_with_fake_mcp(cfg, profile);
        let main = main_agent_tool_names(&env, agent);
        let fake_mcp: Vec<String> = env.mcp.tool_names();

        // Strip the (new) MCP tools and sort: the built-ins must be exactly
        // the old snapshot's list — including `delegate` iff the old pair
        // had a pipeline (subagents).
        let builtins: Vec<String> = main
            .iter()
            .filter(|name| !fake_mcp.contains(name))
            .cloned()
            .collect();
        let mut builtins_sorted = builtins.clone();
        builtins_sorted.sort();
        assert_eq!(
            builtins_sorted, expected_old,
            "{}: the main agent's built-ins must equal the old snapshot's list",
            pair.file
        );

        // The allowed diff: every subagent gets the SAME built-ins and the
        // SAME MCP tools — never `delegate` (no nesting).
        for sub in &agent.subagents {
            let sub_names = sub_agent_tool_names(&env, sub);
            let expected_sub: Vec<String> = main
                .iter()
                .filter(|name| *name != "delegate")
                .cloned()
                .collect();
            let mut sub_sorted = sub_names.clone();
            sub_sorted.sort();
            let mut expected_sub_sorted = expected_sub;
            expected_sub_sorted.sort();
            assert_eq!(
                sub_sorted, expected_sub_sorted,
                "{}: subagent '{}' must get the main tool list minus delegate (MCP tools included)",
                pair.file, sub.member.name
            );
        }
    }
}
