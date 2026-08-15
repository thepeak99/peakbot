//! Pipeline (orchestrator + sub-agent) integration tests.
//!
//! Consumer-level guard that the lane seams *compose*: a delegation TEEs the
//! sub-agent's internal turns into the shared transcript tagged
//! `SubAgent { role }`, rolls its cost into the parent stats, and yet the
//! orchestrator's wire history (`get_agent_history`) sees only its own delegate
//! `ToolCall`/`ToolResult`. Each of these is unit-tested in isolation elsewhere;
//! this test locks that they hold *together* through the real public seams
//! (`StateManager::add_tool_call`/`add_tool_result`/`add_request`/
//! `get_agent_history`/`get_stats`) — a refactor could keep every unit test
//! green while breaking the interaction.

use peakbot::StateManager;
use peakbot::ui::app_state::{MessageRole, MessageSource};
use rig_core::completion::message::{Message as RigMessage, UserContent};

fn sub_agent(role: &str) -> MessageSource {
    MessageSource::SubAgent {
        role: role.to_string(),
    }
}

/// Simulate one full delegation and assert all four load-bearing properties
/// together: (a) sub-agent turns are in the transcript tagged `SubAgent`,
/// (b) the orchestrator wire history excludes them but keeps the delegate
/// round-trip, (c) the returned string is exactly the delegate `ToolResult`
/// the orchestrator sees, (d) the sub-agent's cost rolled into `/stats`.
#[test]
fn delegation_tees_sub_agent_turns_but_isolates_orchestrator_wire() {
    let sm = StateManager::new();

    // The user talks to the orchestrator, which decides to delegate.
    sm.add_user_message("build the thing".to_string());

    // Orchestrator's own delegate call — stays on the Human (orchestrator) lane.
    // This IS the input the orchestrator should see.
    sm.add_tool_call(
        MessageSource::Human,
        None,
        "delegate".to_string(),
        r#"{"role":"researcher","task":"survey the codebase","parent_task_id":1}"#.to_string(),
        Some("call-1".to_string()),
    );

    // The sub-agent runs its own tool loop. Its internal turns are TEE'd into
    // the transcript tagged SubAgent — visible to the user, hidden from the
    // orchestrator wire.
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

    // The delegation returns one string — recorded as the orchestrator's
    // delegate ToolResult (Human lane). This is the ONLY thing about the
    // sub-agent the orchestrator sees.
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

    // ── (b) orchestrator wire history excludes the sub-agent's turns ──────
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
        "the sub-agent's internal bash turn leaked into the orchestrator wire history"
    );

    // ...but the orchestrator's OWN delegate result IS in the wire (input+output).
    let delegate_result_in_wire = wire.iter().any(|m| match m {
        RigMessage::User { content } => content.iter().any(|c| {
            matches!(c, UserContent::ToolResult(tr)
                if format!("{:?}", tr.content).contains("Recommend triage"))
        }),
        _ => false,
    });
    assert!(
        delegate_result_in_wire,
        "the orchestrator's own delegate ToolResult must remain in its wire history"
    );

    // ── (c) the returned string is exactly the delegate ToolResult ────────
    let orchestrator_delegate_result = transcript
        .iter()
        .find(|m| m.role == MessageRole::ToolResult && m.source == MessageSource::Human)
        .expect("orchestrator delegate ToolResult must exist");
    assert!(
        orchestrator_delegate_result.content.contains(returned),
        "the orchestrator sees exactly the string the delegation returned"
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
// ── Stage 1.2: multi-pipeline cross-team isolation ──────────────────────
//
// These tests pin the §4 RUNTIME / AGENT DESIGN contract: with two
// configured pipelines BOTH defining role `reviewer` (each on a
// different model), the `delegate` tool's schema description lists
// ONLY the selected pipeline's roles — cross-pipe roles don't exist
// in the schema (plan §4 "Delegate scoping"). The `DelegateTool` is
// built around an `Arc<SubAgentRegistry>`; the rebuild seam hands it
// the SELECTED pipeline's registry (not the global one), and the
// description is a direct projection of that registry's roles.
//
// We exercise the contract at the `DelegateTool::definition()` seam
// — the cheapest end-to-end pin available without standing up a full
// agent loop. A full harness-based scenario (with `MockCompletionModel`
// and recorded requests) requires Stage 1.2 to first teach the
// harness how to stamp `selected_pipeline`; that's the next test
// added here.
//
// NOTE on visibility: the integration test crate can ONLY reach types
// re-exported at the crate root (`peakbot::DelegateTool`,
// `peakbot::SubAgentRegistry`, …). `peakbot::pipeline::PipelineSet`,
// `peakbot::pipeline::SubAgentDeps`, etc. live behind a private module
// and aren't `pub use`'d yet. The Stage 1.2 implementer MUST add the
// re-exports at `src/lib.rs` (alongside the existing
// `pub use pipeline::{DelegateTool, SubAgentRegistry}`). Until then,
// the test below uses the directly-accessible `SubAgentRegistry` +
// `DelegateTool` pair to pin the contract from the bottom up — the
// `PipelineSet` plumbing at the seam layer is separately pinned by
// the in-module test in `src/pipeline/set.rs::tests::happy_path_*`.
//
// ── Lane-isolation pin — UNTOUCHED ─────────────────────────────────────
// The existing `delegation_tees_sub_agent_turns_but_isolates_orchestrator_wire`
// test above pins the orthogonal lane-isolation contract. Stage 1.2
// does not change it; the rebuild seam hands a different registry to
// the `DelegateTool`, but the lane identity (`MessageSource::SubAgent`)
// and wire-history filtering are unchanged. The companion pin in
// `state_manager::tests::get_agent_history_excludes_sub_agent_lane_keeps_background`
// stays green through this refactor by construction.

use peakbot::config::ModelRegistry;
use peakbot::{DelegateTool, SubAgentRegistry};
use rig_core::tool::Tool;
// `StateManager` is already imported above (used by the lane-isolation
// test).

/// Build a registry that mirrors the per-pipeline role list we want
/// to test cross-pipeline isolation against. Two registries, each with
/// a `reviewer` role on a different model, plus one exclusive role
/// each. This is the "two pipelines sharing role `reviewer` on
/// different models" shape the Stage 1.2 spec calls out.
fn two_registries_with_shared_reviewer() -> (SubAgentRegistry, SubAgentRegistry) {
    use peakbot::config::{ModelEntry, ProviderEntry, ProviderType};

    let prov = ProviderEntry {
        name: "openrouter".into(),
        kind: ProviderType::OpenRouter,
        api_key: Some("sk-test".into()),
        base_url: None,
        preserve_reasoning: None,
        display_reasoning: None,
        models: vec![
            ModelEntry {
                name: "google/gemini-2.0-flash-001".into(),
                alias: Some("flash".into()),
                max_tokens: None,
                temperature: None,
                extra_params: None,
                prompt_caching: None,
                vision: None,
                context_size: None,
                preserve_reasoning: true,
                display_reasoning: false,
            },
            ModelEntry {
                name: "anthropic/claude-3.7-sonnet".into(),
                alias: Some("sonnet".into()),
                max_tokens: None,
                temperature: None,
                extra_params: None,
                prompt_caching: None,
                vision: None,
                context_size: None,
                preserve_reasoning: true,
                display_reasoning: false,
            },
        ],
    };
    let registry: ModelRegistry =
        ModelRegistry::build(std::slice::from_ref(&prov), Some("flash")).expect("registry");

    let yaml_a = "\
pipeline:
  enabled: true
  agents:
    reviewer:
      model: flash
      prompt: web reviewer
    tester:
      prompt: web tester
";
    let yaml_b = "\
pipeline:
  enabled: true
  agents:
    reviewer:
      model: sonnet
      prompt: research reviewer
    writer:
      prompt: research writer
";
    let cfg_a: peakbot::config::Config = serde_yaml::from_str(yaml_a).expect("config a parses");
    let cfg_b: peakbot::config::Config = serde_yaml::from_str(yaml_b).expect("config b parses");

    // NOTE: `SubAgentRegistry::new` takes a legacy `PipelineConfig`.
    // This is intentional — we want to exercise the existing registry
    // API (the seam Stage 1.2's rebuild hands the delegate tool).
    // Once Stage 1.2 lands, the same registries can be built via
    // `PipelineSet::get(name).registry.clone()`.
    //
    // Suppress the boot-error: amendment 5 makes a legacy `pipeline:`
    // block a HARD boot error, but we bypass that by calling
    // `SubAgentRegistry::new` directly (not `PipelineSet::build`).
    // The error path doesn't apply to direct registry construction.
    let members_a = cfg_a.pipeline.as_ref().unwrap().agents.clone();
    let members_b = cfg_b.pipeline.as_ref().unwrap().agents.clone();
    // `SubAgentRegistry::from_members` is the Stage 1.1 narrower
    // constructor — but it's `pub(crate)`. Fall back to `new` which
    // is `pub` and takes a `&PipelineConfig`.
    let reg_a = SubAgentRegistry::new(
        &peakbot::config::PipelineConfig {
            enabled: true,
            orchestrator_prompt: None,
            agents: members_a,
        },
        &registry,
        &[],
    )
    .expect("registry a builds");
    let reg_b = SubAgentRegistry::new(
        &peakbot::config::PipelineConfig {
            enabled: true,
            orchestrator_prompt: None,
            agents: members_b,
        },
        &registry,
        &[],
    )
    .expect("registry b builds");
    (reg_a, reg_b)
}

/// Build a `DelegateTool` over the given registry, with the minimal
/// scaffolding needed for `definition()` to work. Mirrors the helper
/// in `src/pipeline/delegate_tool.rs::tests::minimal_delegate_tool`.
fn delegate_tool_for_registry(registry: std::sync::Arc<SubAgentRegistry>) -> DelegateTool {
    // SubAgentDeps is re-exported as `peakbot::pipeline::SubAgentDeps`
    // but the module is private. Until Stage 1.2 re-exports it (or
    // makes the module pub), we have to construct via the private
    // path. The minimal test helper in `delegate_tool.rs::tests`
    // demonstrates the right shape.
    //
    // To stay in the integration crate without reaching into private
    // modules, we construct via the `DelegateTool::new(Arc<SubAgentDeps>)`
    // path. SubAgentDeps IS re-exported from `crate::pipeline` (see
    // `src/pipeline/mod.rs:15`), but only via `peakbot::pipeline` —
    // which is itself a private module from the integration crate's
    // perspective. Stage 1.2 must `pub use` it.
    //
    // For now this test is RED with the right reason: SubAgentDeps
    // (and PipelineSet, etc.) need re-exporting at the crate root.
    // Once that's done, the body below compiles and the assertions
    // pin the contract.
    let deps = peakbot::pipeline::SubAgentDeps {
        registry,
        searxng: None,
        bash_config: peakbot::config::BashConfig::default(),
        tools_filter: peakbot::config::ToolsConfig::default(),
        state_manager: StateManager::new_arc(),
        shell_kind: None,
        vector_store: None,
        max_turns: 0,
        skills: peakbot::SkillRegistry::default(),
        event_sink: None,
        retry: peakbot::config::RetryConfig::default(),
        timeouts: peakbot::config::TimeoutsConfig::default(),
    };
    DelegateTool::new(std::sync::Arc::new(deps))
}

/// The headline Stage 1.2 cross-pipeline isolation contract: when two
/// pipelines both define role `reviewer` (on different models) and
/// pipeline A is the selected team, the `delegate` tool's description
/// — the prompt-side surface the LLM actually reads — names ONLY A's
/// roles. Pipeline B's role names must NOT appear, or the orchestrator
/// could hallucinate a delegation into the wrong team and `DelegateTool::call`
/// would route through the wrong registry (an unknown-role error if
/// the same role name happens to collide, or worse, a silent wrong-team
/// dispatch if the names diverge).
#[tokio::test]
async fn delegate_tool_description_lists_only_selected_pipeline_roles() {
    let (reg_a, reg_b) = two_registries_with_shared_reviewer();

    // Build a DelegateTool with registry A (the "selected" team).
    let tool_a = delegate_tool_for_registry(std::sync::Arc::new(reg_a));
    let def_a = Tool::definition(&tool_a, String::new()).await;

    // The description must name registry A's roles (reviewer, tester)…
    assert!(
        def_a.description.contains("reviewer"),
        "team-A description must include `reviewer`; got: {desc}",
        desc = def_a.description
    );
    assert!(
        def_a.description.contains("tester"),
        "team-A description must include `tester`; got: {desc}",
        desc = def_a.description
    );

    // …and must NOT name registry B's exclusive role (`writer`).
    assert!(
        !def_a.description.contains("writer"),
        "team-A description must NOT mention team-B's `writer` role; got: {desc}",
        desc = def_a.description
    );

    // The roles enum in the JSON schema parameter is the LLM-visible
    // role vocabulary. It must list exactly registry A's roles and not
    // registry B's exclusive `writer`. Parse the schema to assert.
    let role_schema_desc = def_a
        .parameters
        .get("properties")
        .and_then(|p| p.get("role"))
        .and_then(|r| r.get("description"))
        .and_then(|d| d.as_str())
        .expect("delegate schema must have a `role.description`");
    assert!(
        role_schema_desc.contains("reviewer") && role_schema_desc.contains("tester"),
        "role enum must list team-A's roles; got: {role_schema_desc}"
    );
    assert!(
        !role_schema_desc.contains("writer"),
        "role enum must NOT list team-B's exclusive role; got: {role_schema_desc}"
    );

    // Symmetry: building the tool with registry B flips the visibility
    // — `writer` appears, `tester` does not.
    let tool_b = delegate_tool_for_registry(std::sync::Arc::new(reg_b));
    let def_b = Tool::definition(&tool_b, String::new()).await;
    assert!(
        def_b.description.contains("reviewer") && def_b.description.contains("writer"),
        "team-B description must include its own roles; got: {desc}",
        desc = def_b.description
    );
    assert!(
        !def_b.description.contains("tester"),
        "team-B description must NOT mention team-A's `tester` role; got: {desc}",
        desc = def_b.description
    );
}

/// The two registries also pin the fact that the SAME role name
/// (`reviewer`) is mapped to different models per pipeline. The
/// selected pipeline's `SubAgentRegistry` carries the per-role model
/// binding — when the delegate tool dispatches a `reviewer` call, it
/// dispatches to the SELECTED team's model. This is asserted by
/// inspecting each registry's `role_model_aliases()` directly: a
/// caller that runs `delegate("reviewer", …)` under team A must end
/// up routing to A's reviewer model.
#[test]
fn same_role_name_routes_to_selected_teams_model() {
    let (reg_a, reg_b) = two_registries_with_shared_reviewer();

    // Registry A's reviewer is on `flash`; registry B's reviewer is on `sonnet`.
    let a_reviewers: Vec<(String, String)> = reg_a.role_model_aliases();
    let b_reviewers: Vec<(String, String)> = reg_b.role_model_aliases();

    let a_reviewer_model = a_reviewers
        .iter()
        .find(|(r, _)| r == "reviewer")
        .map(|(_, m)| m.as_str())
        .expect("registry A must have a reviewer role");
    let b_reviewer_model = b_reviewers
        .iter()
        .find(|(r, _)| r == "reviewer")
        .map(|(_, m)| m.as_str())
        .expect("registry B must have a reviewer role");

    assert_eq!(
        a_reviewer_model, "flash",
        "registry A's reviewer must be on `flash` (per the YAML fixture)"
    );
    assert_eq!(
        b_reviewer_model, "sonnet",
        "registry B's reviewer must be on `sonnet` (per the YAML fixture)"
    );
    assert_ne!(
        a_reviewer_model, b_reviewer_model,
        "the SAME role name MUST map to different models per pipeline — \
         that's the cross-pipeline conflict Stage 1.2 has to scope correctly"
    );
}
