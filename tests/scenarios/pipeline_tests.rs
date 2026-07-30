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
        "delegate".to_string(),
        r#"{"role":"researcher","task":"survey the codebase","parent_task_id":1}"#.to_string(),
        Some("call-1".to_string()),
    );

    // The sub-agent runs its own tool loop. Its internal turns are TEE'd into
    // the transcript tagged SubAgent — visible to the user, hidden from the
    // orchestrator wire.
    sm.add_tool_call(
        sub_agent("researcher"),
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
