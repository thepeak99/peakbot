//! A failing tool must not abort the turn — for the orchestrator OR a sub-agent.
//!
//! Both lanes run the same rig agentic loop, so this pins the shared contract:
//! a tool that returns `Err` comes back to the model as a tool result it can
//! read, and the loop continues. Without it, `PromptError::ToolError` would
//! escape and (on the sub-agent lane) end the delegation — see
//! `src/pipeline/handoff.rs`'s `ToolError` arm.

use crate::harness::TestHarness;
use peakbot::mock::MockResponse;
use rig_core::completion::message::{Message, ToolResultContent, UserContent};

/// `file_read` on a missing path returns `Err(FileReadError::Validation(..))`.
/// The model must see that text as a tool result and get another turn.
#[tokio::test]
async fn tool_error_is_returned_to_the_model_and_the_turn_continues() {
    let mut harness = TestHarness::new();

    harness.add_response(MockResponse::tool_call(
        "file_read",
        serde_json::json!({ "path": "/definitely/not/here.rs" }),
    ));
    harness.add_response(MockResponse::text(
        "That file is missing — I'll list the dir.",
    ));

    let response = harness
        .run_message("Read /definitely/not/here.rs please.")
        .await;

    assert_eq!(
        response, "That file is missing — I'll list the dir.",
        "a tool error must not abort the turn; the model should see it and self-correct"
    );
    assert_eq!(
        harness.request_count(),
        2,
        "expected 2 wire calls (failing tool + follow-up), got {}",
        harness.request_count()
    );

    // The second request carries the failure as a ToolResult — that's the
    // "the sub-agent just gets an error and continues" contract.
    let recorded = harness.get_recorded_requests();
    let carried_error = recorded[1].chat_history.iter().any(|msg| match msg {
        Message::User { content } => content.iter().any(|c| match c {
            UserContent::ToolResult(tr) => tr.content.iter().any(|part| match part {
                ToolResultContent::Text(t) => t.text.contains("does not exist"),
                ToolResultContent::Image(_) => false,
            }),
            _ => false,
        }),
        _ => false,
    });
    assert!(
        carried_error,
        "the tool's error text must reach the model as a ToolResult. \
         Second request chat_history: {:#?}",
        recorded[1].chat_history
    );
}
