//! Agent + sub-agent delegation integration tests (0.24 port of the old
//! pipeline tests).
//!
//! Consumer-level guards that the lane seams *compose*: a delegation TEEs the
//! sub-agent's internal turns into the shared transcript tagged
//! `SubAgent { role }`, rolls its cost into the parent stats, and yet the
//! main agent's wire history (`get_agent_history`) sees only its own
//! `delegate` `ToolCall`/`ToolResult`. Each of these is unit-tested in
//! isolation elsewhere; these tests lock that they hold *together* through
//! the real public seams (`StateManager::add_tool_call`/`add_tool_result`/
//! `add_request`/`get_agent_history`/`get_stats` and the `DelegateTool`
//! surface the LLM reads).
//!
//! The 0.23 "two pipelines sharing a role name" scenario is gone with the
//! pipelines: a session runs exactly ONE resolved `(profile, agent)` — the
//! `Bound` — and the `delegate` tool's roster is `bound.agent().subagents`.
//! The cross-team isolation contract survives as: the schema names ONLY the
//! bound agent's subagents, never the siblings' (a different agent in the
//! same profile is as foreign as a different pipeline used to be).

use peakbot::StateManager;
use peakbot::config::resolve::{Config, ParseCtx, Source};
use peakbot::pipeline::{DelegateTool, SubAgentDeps};
use peakbot::skills::SkillRegistry;
use peakbot::tools::env::ToolEnv;
use peakbot::ui::app_state::{MessageRole, MessageSource};
use rig_core::completion::message::{Message as RigMessage, UserContent};
use rig_core::tool::Tool;
use std::path::Path;
use std::sync::Arc;

fn sub_agent(role: &str) -> MessageSource {
    MessageSource::SubAgent {
        role: role.to_string(),
    }
}

/// Simulate one full delegation and assert all four load-bearing properties
/// together: (a) sub-agent turns are in the transcript tagged `SubAgent`,
/// (b) the main agent's wire history excludes them but keeps the delegate
/// round-trip, (c) the returned string is exactly the delegate `ToolResult`
/// the main agent sees, (d) the sub-agent's cost rolled into `/stats`.
#[test]
fn delegation_tees_sub_agent_turns_but_isolates_main_agent_wire() {
    let sm = StateManager::new();

    // The user talks to the main agent, which decides to delegate.
    sm.add_user_message("build the thing".to_string());

    // The main agent's own delegate call — stays on the Human (main agent)
    // lane. This IS the input the main agent should see.
    sm.add_tool_call(
        MessageSource::Human,
        None,
        "delegate".to_string(),
        r#"{"role":"researcher","task":"survey the codebase","parent_task_id":1}"#.to_string(),
        Some("call-1".to_string()),
    );

    // The sub-agent runs its own tool loop. Its internal turns are TEE'd into
    // the transcript tagged SubAgent — visible to the user, hidden from the
    // main agent's wire.
    sm.add_tool_call(
        sub_agent("researcher"),
        None,
        "bash".to_string(),
        r#"{"command":"grep -r TODO"}"#.to_string(),
        Some("sub-call-1".to_string()),
    );
    sm.add_tool_result(
        sub_agent("researcher"),
        "bash".to_string(),
        r#"{"command":"grep -r TODO"}"#.to_string(),
        "found 3 TODOs".to_string(),
        Some("sub-call-1".to_string()),
    );

    // The sub-agent's cost rolls into the parent stats (lane-agnostic).
    sm.add_request(&MessageSource::Human, 100, 50, 0.02);

    // The delegation returns one string — recorded as the main agent's
    // delegate ToolResult (Human lane). This is the ONLY thing about the
    // sub-agent the main agent sees.
    let returned = "Brief: 3 TODOs, all in src/legacy.rs. Recommend triage.";
    sm.add_tool_result(
        MessageSource::Human,
        "delegate".to_string(),
        r#"{"role":"researcher","task":"survey the codebase","parent_task_id":1}"#.to_string(),
        returned.to_string(),
        Some("call-1".to_string()),
    );

    // Trailing assistant so nothing is stripped as a "trailing user" turn.
    sm.add_assistant_message("Here's what I found.".to_string());

    // ── (a) sub-agent turns appear in the transcript tagged SubAgent ──────
    let transcript = sm.get_state().chat.messages;
    let sub_agent_turns = transcript
        .iter()
        .filter(|m| m.source == sub_agent("researcher"))
        .count();
    assert_eq!(
        sub_agent_turns, 2,
        "the sub-agent's ToolCall + ToolResult must be in the transcript, tagged SubAgent"
    );

    // ── (b) main agent wire history excludes the sub-agent's turns ────────
    let wire = sm.get_agent_history();

    let sub_agent_bash_leaked = wire.iter().any(|m| match m {
        RigMessage::User { content } => content.iter().any(|c| {
            matches!(c, UserContent::ToolResult(tr)
                if format!("{:?}", tr.content).contains("found 3 TODOs"))
        }),
        RigMessage::Assistant { content, .. } => format!("{content:?}").contains("sub-call-1"),
        _ => false,
    });
    assert!(
        !sub_agent_bash_leaked,
        "the sub-agent's internal bash turn leaked into the main agent's wire history"
    );

    // ...but the main agent's OWN delegate result IS in the wire (input+output).
    let delegate_result_in_wire = wire.iter().any(|m| match m {
        RigMessage::User { content } => content.iter().any(|c| {
            matches!(c, UserContent::ToolResult(tr)
                if format!("{:?}", tr.content).contains("Recommend triage"))
        }),
        _ => false,
    });
    assert!(
        delegate_result_in_wire,
        "the main agent's own delegate ToolResult must remain in its wire history"
    );

    // ── (c) the returned string is exactly the delegate ToolResult ────────
    let main_delegate_result = transcript
        .iter()
        .find(|m| m.role == MessageRole::ToolResult && m.source == MessageSource::Human)
        .expect("main agent delegate ToolResult must exist");
    assert!(
        main_delegate_result.content.contains(returned),
        "the main agent sees exactly the string the delegation returned"
    );

    // ── (d) the sub-agent's cost rolled into /stats ───────────────────────
    let stats = sm.get_stats();
    assert_eq!(stats.total_input_tokens, 100);
    assert_eq!(stats.total_output_tokens, 50);
    assert!(
        (stats.total_cost - 0.02).abs() < f64::EPSILON,
        "sub-agent cost must roll into /stats, got {}",
        stats.total_cost
    );
}

// ── Delegate scoping: the schema names ONLY the bound agent's subagents ────
//
// The 0.23 "two pipelines sharing role `reviewer`" scenario: a session runs
// ONE `(profile, agent)`, so the isolation axis is now the AGENT. Two agents
// in the same profile both define `reviewer` (on different models); the
// `delegate` tool built for agent A must name ONLY A's subagents — a
// sibling agent's roster is as invisible as a foreign pipeline used to be.

const DELEGATE_GLOBAL: &str = "providers:\n  - name: openrouter\n    type: openrouter\n    api_key: sk-test\n    models:\n      - { name: google/gemini-2.0-flash-001, alias: flash }\n      - { name: anthropic/claude-3.7-sonnet, alias: sonnet }\nprofiles:\n  dev:\n    agents:\n      team:\n        model: flash\n        subagents:\n          reviewer:\n            model: flash\n            prompt: web reviewer\n          tester:\n            prompt: web tester\n      ops:\n        model: sonnet\n        subagents:\n          reviewer:\n            model: sonnet\n            prompt: research reviewer\n          writer:\n            prompt: research writer\n";

fn parse_delegate_global() -> Config {
    Config::parse(
        Source {
            path: Path::new("/global/config.yaml"),
            text: DELEGATE_GLOBAL,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .expect("delegate fixture must parse")
}

/// A `DelegateTool` over the bound `(dev, <agent>)` — the same wiring the
/// provider build uses (`build_bound_agent`).
fn delegate_tool_for_agent(cfg: &Config, agent_name: &str) -> DelegateTool {
    let profile = cfg.catalog.profile("dev").expect("the dev profile");
    let agent_index = profile
        .agents
        .iter()
        .position(|a| a.main.name == agent_name)
        .unwrap_or_else(|| panic!("agent '{agent_name}' in the fixture"));
    let bound = peakbot::config::resolve::Bound {
        profile: Arc::clone(profile),
        agent_index,
    };
    let deps = SubAgentDeps {
        bound,
        orchestrator_model: cfg
            .registry
            .resolve(&profile.agents[agent_index].main.model.alias)
            .cloned()
            .expect("the agent's model is in the registry"),
        tool_env: Arc::new(ToolEnv::for_profile(cfg, profile, &Arc::new(Vec::new()))),
        state_manager: StateManager::new_arc(),
        shell_kind: None,
        skills: SkillRegistry::default(),
        event_sink: None,
        retry: cfg.general.retry.clone(),
    };
    DelegateTool::new(Arc::new(deps))
}

/// The headline scoping contract: with two agents both defining role
/// `reviewer` (on different models), the `delegate` tool's description and
/// role schema — the prompt-side surface the LLM actually reads — name ONLY
/// the bound agent's subagents. A sibling's role names must NOT appear, or
/// the main agent could hallucinate a delegation into a sub-agent that does
/// not exist in its bound.
#[tokio::test]
async fn delegate_tool_description_lists_only_the_bound_agents_subagents() {
    let cfg = parse_delegate_global();

    // The tool built for `team`…
    let def_a = Tool::definition(&delegate_tool_for_agent(&cfg, "team"), String::new()).await;
    assert!(
        def_a.description.contains("reviewer"),
        "team description must include `reviewer`; got: {}",
        def_a.description
    );
    assert!(
        def_a.description.contains("tester"),
        "team description must include `tester`; got: {}",
        def_a.description
    );
    assert!(
        !def_a.description.contains("writer"),
        "team description must NOT mention the sibling agent's `writer`; got: {}",
        def_a.description
    );

    // The roles text in the JSON schema parameter is the LLM-visible role
    // vocabulary. It must list exactly the bound agent's subagents.
    let role_schema_desc = def_a
        .parameters
        .get("properties")
        .and_then(|p| p.get("role"))
        .and_then(|r| r.get("description"))
        .and_then(|d| d.as_str())
        .expect("delegate schema must have a `role.description`");
    assert!(
        role_schema_desc.contains("reviewer") && role_schema_desc.contains("tester"),
        "role text must list team's subagents; got: {role_schema_desc}"
    );
    assert!(
        !role_schema_desc.contains("writer"),
        "role text must NOT list the sibling agent's `writer`; got: {role_schema_desc}"
    );

    // Symmetry: the tool built for `ops` flips the visibility — `writer`
    // appears, `tester` does not.
    let def_b = Tool::definition(&delegate_tool_for_agent(&cfg, "ops"), String::new()).await;
    assert!(
        def_b.description.contains("reviewer") && def_b.description.contains("writer"),
        "ops description must include its own subagents; got: {}",
        def_b.description
    );
    assert!(
        !def_b.description.contains("tester"),
        "ops description must NOT mention team's `tester`; got: {}",
        def_b.description
    );
}

/// The same subagent name (`reviewer`) maps to different models per agent.
/// The bound carries the per-subagent model binding — a `reviewer` call
/// under `team` routes to team's model, under `ops` to ops'.
#[test]
fn same_subagent_name_routes_to_the_bound_agents_model() {
    let cfg = parse_delegate_global();
    let profile = cfg.catalog.profile("dev").expect("the dev profile");
    let team = profile
        .agents
        .iter()
        .find(|a| a.main.name == "team")
        .expect("team agent");
    let ops = profile
        .agents
        .iter()
        .find(|a| a.main.name == "ops")
        .expect("ops agent");

    let team_reviewer = team
        .subagents
        .iter()
        .find(|s| s.member.name == "reviewer")
        .expect("team has a reviewer subagent");
    let ops_reviewer = ops
        .subagents
        .iter()
        .find(|s| s.member.name == "reviewer")
        .expect("ops has a reviewer subagent");

    assert_eq!(
        team_reviewer.member.model.alias, "flash",
        "team's reviewer must be on `flash` (per the fixture)"
    );
    assert_eq!(
        ops_reviewer.member.model.alias, "sonnet",
        "ops' reviewer must be on `sonnet` (per the fixture)"
    );
    assert_ne!(
        team_reviewer.member.model.alias, ops_reviewer.member.model.alias,
        "the SAME subagent name MUST map to different models per agent — \
         that's the cross-agent conflict the bound has to scope correctly"
    );
}

// ── A bound without subagents ⇒ an empty role enum ─────────────────────────
//
// The 0.23 "dangling selection ⇒ no delegate" invariant in its 0.24 form:
// the `delegate` tool exists iff the bound agent has subagents
// (`delegate_enabled`), and a bound whose agent has none exposes an empty
// role enum — no sub-agent is invokable.

/// A global with one subagent-FREE agent.
const NO_SUBAGENTS_GLOBAL: &str = "providers:\n  - name: openrouter\n    type: openrouter\n    api_key: sk-test\n    models:\n      - { name: google/gemini-2.0-flash-001, alias: flash }\nprofiles:\n  dev:\n    agents:\n      solo:\n        model: flash\n";

#[tokio::test]
async fn bound_without_subagents_yields_an_empty_delegate_role_enum() {
    let cfg = Config::parse(
        Source {
            path: Path::new("/global/config.yaml"),
            text: NO_SUBAGENTS_GLOBAL,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .expect("no-subagents fixture must parse");

    // The tool is not registered at all for such a bound (the F3 seam)…
    assert!(
        !peakbot::tools::env::delegate_enabled(&cfg.catalog.profile("dev").unwrap().agents[0]),
        "no subagents ⇒ no delegate tool"
    );

    // …and even built directly, it exposes an empty role enum.
    let profile = cfg.catalog.profile("dev").unwrap();
    let bound = peakbot::config::resolve::Bound {
        profile: Arc::clone(profile),
        agent_index: 0,
    };
    let deps = SubAgentDeps {
        bound,
        orchestrator_model: cfg
            .registry
            .resolve(&profile.agents[0].main.model.alias)
            .cloned()
            .expect("the agent's model is in the registry"),
        tool_env: Arc::new(ToolEnv::for_profile(&cfg, profile, &Arc::new(Vec::new()))),
        state_manager: StateManager::new_arc(),
        shell_kind: None,
        skills: SkillRegistry::default(),
        event_sink: None,
        retry: cfg.general.retry.clone(),
    };
    let def = Tool::definition(&DelegateTool::new(Arc::new(deps)), String::new()).await;
    let role_desc = def
        .parameters
        .get("properties")
        .and_then(|p| p.get("role"))
        .and_then(|r| r.get("description"))
        .and_then(|d| d.as_str())
        .unwrap_or("");
    assert!(
        role_desc.trim_end().ends_with("One of:"),
        "the role list after 'One of:' must be empty; got: {role_desc}"
    );
    assert!(
        def.description.contains("Available roles: \n"),
        "no role names may follow 'Available roles:'; got: {}",
        def.description
    );
}
