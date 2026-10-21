//! Captured from the pre-0.24 config code; deleted at the config cutover.
//!
//! Golden plain-text snapshots of what the OLD (current, pre-0.24) code
//! produces for the §11.2 fixtures (a) and (b): for every (old profile) ×
//! (old pipeline) pair the old boot path accepts, this records the main
//! agent's full system prompt, the sorted built-in tool names, the filtered
//! MCP server names, and the subagent roster with preambles. The new
//! (0.24) code is checked against these in B3.
//!
//! Run with `UPDATE_SNAPSHOTS=1` to (re)write the files; without it the
//! test asserts the committed snapshots match.
//!
//! Determinism: fixed cwd (a non-existent dir → no `agents.md` section),
//! the live clock and binary path are normalised to fixed values, a fixed
//! bash shell, an empty skill registry, and no background processes.
//! The vector store gate mirrors boot (enabled flag; `VectorStore::open`
//! is lazy and writes nothing). MCP servers are NOT started — only their
//! filtered names are recorded.

use std::path::{Path, PathBuf};

const SNAPSHOT_DIR: &str = "tests/fixtures/migrate/expected-behaviour";
const FIXTURES: &[(&str, &str)] = &[
    (
        "a_pipeline_team",
        "tests/fixtures/migrate/a_pipeline_team.in.yaml",
    ),
    (
        "b_maintainer_shape",
        "tests/fixtures/migrate/b_maintainer_shape.in.yaml",
    ),
];
/// Non-existent dir: `agents_md_section` degrades to empty, so the prompt is
/// independent of any real repo's `agents.md`.
const FIXED_CWD: &str = "/opt/peakbot-snapshot-cwd";
const FIXED_SHELL_PATH: &str = "/bin/bash";
const FIXED_TIME: &str = "2000-01-01 00:00:00 UTC";
const FIXED_BIN_PATH: &str = "/usr/local/bin/peakbot";

/// Restores `XDG_CONFIG_HOME` and the process cwd on drop — `Config::load`
/// reads both (master config path and per-repo `.peakbot/config.yaml`).
struct EnvGuard {
    xdg: Option<String>,
    cwd: PathBuf,
}

impl EnvGuard {
    fn isolate(xdg: &Path) -> Self {
        let cwd = std::env::current_dir().expect("current dir");
        // An empty dir: no `.peakbot/config.yaml` can leak into the merge.
        let sandbox = tempfile::tempdir().expect("sandbox dir");
        std::env::set_current_dir(sandbox.path()).expect("chdir to sandbox");
        let guard = EnvGuard {
            xdg: std::env::var("XDG_CONFIG_HOME").ok(),
            cwd,
        };
        unsafe { std::env::set_var("XDG_CONFIG_HOME", xdg) };
        std::mem::forget(sandbox);
        guard
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.xdg {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
        let _ = std::env::set_current_dir(&self.cwd);
    }
}

fn update_snapshots() -> bool {
    std::env::var("UPDATE_SNAPSHOTS").is_ok_and(|v| v == "1")
}

/// `env_block` stamps a live clock and the test binary's exe path; fold both
/// to fixed values so the snapshots are byte-stable across runs and hosts.
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

fn first_diff_line(a: &str, b: &str) -> usize {
    a.lines()
        .zip(b.lines())
        .position(|(x, y)| x != y)
        .map_or_else(|| a.lines().count().max(b.lines().count()), |i| i + 1)
}

/// The built-in tool names the old `add_builtin_tools` registers for the main
/// agent under a fixed linux/bash boot, then the profile's `tools:` filter —
/// mirrored from `src/providers/mod.rs::add_builtin_tools` (which is private
/// and rig-generic). `view_image` is Anthropic+vision-only: absent for the
/// openai/openrouter fixtures.
fn builtin_tool_names(
    config: &peakbot::Config,
    searxng_on: bool,
    vector_on: bool,
    pipeline_active: bool,
) -> Vec<String> {
    let mut names: Vec<&str> = vec![
        "file_create",
        "file_str_replace",
        "file_insert",
        "file_read",
        "pdf_read",
        "list_directory",
        "fetch_url",
        "fetch_page",
        "todo",
        "think",
        // Fixed bash shell: bg tool + shell tool.
        "bash_bg",
        "bash",
    ];
    if searxng_on {
        names.push("web_search");
    }
    if vector_on {
        names.push("doc_index");
        names.push("doc_search");
    }
    if pipeline_active {
        names.push("delegate");
    }
    let mut out: Vec<String> = names
        .into_iter()
        .filter(|n| config.tools.allows(n))
        .map(str::to_string)
        .collect();
    out.sort();
    out
}

/// MCP server names that survive the profile's `mcp_servers:` gate — the same
/// predicate `mcp_tools_for_profile` applies, without starting any server.
fn mcp_server_names(config: &peakbot::Config) -> Vec<String> {
    config
        .mcp_servers
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|s| s.name.clone())
        .filter(|name| {
            config
                .active_mcp_server_filter()
                .is_none_or(|f| f.allows(name))
        })
        .collect()
}

/// Every skill name any role's `skills:` filter references. `PipelineSet::build`
/// treats `known_skills = None` as the *empty* universe (its doc's "skip" is
/// misleading), which rejects a role's `skills.only` names as typos. Feeding
/// the config's own referenced names makes the check pass deterministically;
/// it does not affect the preambles, which render from the (empty) skill
/// registry, not from this list.
fn referenced_skill_names(config: &peakbot::Config) -> Vec<String> {
    let mut names: Vec<String> = config
        .pipelines
        .iter()
        .flat_map(|p| p.agents.values())
        .flat_map(|a| a.skills.only.iter().chain(a.skills.disabled.iter()))
        .cloned()
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Render one (profile, pipeline) pair exactly as the old boot path builds it
/// (`create_session` in `src/session.rs`): the shared prompt recipe, the tool
/// registration gates, and the delegate roster.
#[allow(clippy::too_many_arguments)] // mirrors build_system_prompt's call surface
fn render_pair(
    name: &str,
    config: &peakbot::Config,
    set: &peakbot::PipelineSet,
    profile: Option<&str>,
    pipeline: Option<&str>,
    skills: &peakbot::SkillRegistry,
    shell: &peakbot::ShellKind,
    registry_default_alias: &str,
) -> String {
    let active = pipeline.and_then(|p| set.get(p));

    let head = peakbot::resolve_prompt_head(
        config.prompt_head().as_ref(),
        active.and_then(|p| p.orchestrator_persona.as_deref()),
    );
    let prompt = peakbot::build_system_prompt(
        skills,
        Some(shell),
        Path::new(FIXED_CWD),
        config.memory.enabled,
        config.agents_md,
        active.is_some(),
        active.and_then(|p| p.orchestrator_prompt.as_deref()),
        head.as_ref(),
    );

    // Boot-level gates (SessionDeps): the searxng/vector handles are built
    // once from the boot config, independent of the profile.
    let searxng_on = config.searxng.as_ref().is_some_and(|s| s.enabled);
    let vector_on = config.vector_db.as_ref().is_some_and(|v| v.enabled);

    let mut out = String::new();
    out.push_str(&format!(
        "# fixture: {name}\n# profile: {}\n# pipeline: {}\n",
        profile.unwrap_or("none"),
        pipeline.unwrap_or("none"),
    ));
    out.push_str("# determinism: fixed cwd (absent → no agents.md), normalized time/binary path,\n# fixed bash shell, empty skill registry, no background processes; MCP servers not started\n");
    out.push_str(&format!("# boot model alias: {registry_default_alias}\n"));
    if let Some(p) = active {
        out.push_str(&format!(
            "# orchestrator model alias: {}\n",
            p.orchestrator.alias
        ));
    }
    out.push('\n');

    out.push_str("=== MAIN AGENT SYSTEM PROMPT ===\n");
    out.push_str(&normalize(&prompt));
    out.push('\n');

    out.push_str("=== BUILT-IN TOOLS (sorted) ===\n");
    for name in builtin_tool_names(config, searxng_on, vector_on, active.is_some()) {
        out.push_str(&name);
        out.push('\n');
    }
    out.push('\n');

    out.push_str("=== MCP SERVERS (filtered names) ===\n");
    let mcp = mcp_server_names(config);
    if mcp.is_empty() {
        out.push_str("(none)\n");
    } else {
        for name in &mcp {
            out.push_str(name);
            out.push('\n');
        }
    }
    out.push('\n');

    if let Some(p) = active {
        out.push_str("=== SUBAGENT ROSTER ===\n");
        let mut roles = p.registry.role_model_aliases();
        roles.sort();
        for (role, alias) in &roles {
            out.push_str(&format!("{role} -> {alias}\n"));
        }
        // The registry stores the role prompt/skills/agents_md verbatim from
        // the config def, so the preamble is rendered from the config.
        let def = config
            .pipelines
            .iter()
            .find(|d| d.name.trim() == p.name)
            .expect("pipeline def in config");
        for (role, _) in &roles {
            let agent = def.agents.get(role).expect("agent def in config");
            let preamble = peakbot::pipeline::build_sub_agent_preamble(
                &agent.prompt,
                Some(shell),
                Path::new(FIXED_CWD),
                skills,
                &agent.skills,
                &[],
                agent.agents_md && config.agents_md,
            );
            out.push_str(&format!("\n--- preamble: {role} ---\n"));
            out.push_str(&normalize(&preamble));
        }
    }

    out
}

fn capture_fixture(name: &str, yaml_path: &str, update: bool) -> Vec<String> {
    // Resolve the snapshot dir BEFORE the cwd changes below: the guard chdirs
    // to an empty dir, so a relative path would land in the tempdir.
    let snapshot_dir = std::env::current_dir()
        .expect("current dir")
        .join(SNAPSHOT_DIR);
    // Stage the fixture as the master config under a temp XDG_CONFIG_HOME.
    let xdg = tempfile::tempdir().expect("xdg tempdir");
    let cfg_dir = xdg.path().join("peakbot");
    std::fs::create_dir_all(&cfg_dir).expect("config dir");
    std::fs::copy(yaml_path, cfg_dir.join("config.yaml")).expect("stage fixture");

    let _guard = EnvGuard::isolate(xdg.path());

    let base = peakbot::Config::load(None)
        .expect("base config loads")
        .config;
    let profiles: Vec<Option<String>> = std::iter::once(None)
        .chain(base.profile_names().into_iter().map(Some))
        .collect();

    let skills = peakbot::SkillRegistry::new();
    let shell = peakbot::ShellKind::Bash {
        path: FIXED_SHELL_PATH.to_string(),
    };

    let mut files = Vec::new();
    for profile in profiles {
        let config = peakbot::Config::load(profile.as_deref())
            .expect("profile loads")
            .config;
        let registry = config.build_model_registry().expect("registry builds");
        // The role skill-typo check runs against this list; feed the config's
        // own referenced skill names so every pair builds deterministically.
        let known_skills = referenced_skill_names(&config);
        let set = peakbot::PipelineSet::build(&config, &registry, Some(&known_skills))
            .expect("pipelines build");

        let pipelines: Vec<Option<String>> = std::iter::once(None)
            .chain(set.iter().map(|p| Some(p.name.clone())))
            .collect();

        for pipeline in pipelines {
            let text = render_pair(
                name,
                &config,
                &set,
                profile.as_deref(),
                pipeline.as_deref(),
                &skills,
                &shell,
                registry.default_alias(),
            );
            let fname = format!(
                "{name}__{}__{}.txt",
                profile.as_deref().unwrap_or("none"),
                pipeline.as_deref().unwrap_or("none")
            );
            let path = snapshot_dir.join(&fname);
            if update {
                std::fs::create_dir_all(&snapshot_dir).expect("snapshot dir");
                std::fs::write(&path, &text).expect("write snapshot");
                eprintln!("wrote {}", path.display());
            } else {
                let existing = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    panic!(
                        "missing snapshot {} ({e}); regenerate with UPDATE_SNAPSHOTS=1",
                        path.display()
                    )
                });
                if existing != text {
                    panic!(
                        "snapshot mismatch for {} (first diff at line {}); \
                         regenerate with UPDATE_SNAPSHOTS=1 and review the diff",
                        path.display(),
                        first_diff_line(&existing, &text)
                    );
                }
            }
            files.push(fname);
        }
    }
    files
}

#[test]
fn old_behaviour_snapshots_match() {
    let update = update_snapshots();
    let mut expected = Vec::new();
    for (name, path) in FIXTURES {
        expected.extend(capture_fixture(name, path, update));
    }
    if update {
        return;
    }
    // No stale snapshots: every committed file must belong to a live pair.
    for entry in std::fs::read_dir(SNAPSHOT_DIR).expect("snapshot dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|ext| ext == "txt") {
            let fname = path
                .file_name()
                .expect("file name")
                .to_string_lossy()
                .to_string();
            assert!(
                expected.iter().any(|f| f == &fname),
                "stale snapshot: {fname}"
            );
        }
    }
}
