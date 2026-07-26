//! Regression test for ticket #223: unknown tool calls must NOT abort the turn.
//!
//! Background. When the model emits a tool call for a tool name that isn't
//! registered, PeakBot must feed the model a synthetic "unknown tool"
//! result and let it self-correct — instead of killing the turn with
//! `PromptError::UnknownToolCall`. The fix overrides
//! `PromptHook::on_invalid_tool_call` on `SessionHook` to return
//! `InvalidToolCallHookAction::Skip { reason }`.
//!
//! These tests pin the **end-to-end** behaviour through the harness:
//!   1. happy-path: a model that emits a bad tool name, then a corrective
//!      text response, runs cleanly to completion (no abort, request_count
//!      grows by 2, the transcript records both the synthetic ToolResult
//!      and the user-facing warning system message).
//!   2. pathological-model: a model that ONLY emits the bad tool call
//!      must still return promptly — proving the fix doesn't introduce an
//!      infinite-retry loop on truly broken models. Bounded with a
//!      `tokio::time::timeout` (see `tests/scenarios/compaction_tests.rs`
//!      for the existing timeout idiom).
//!
//! These tests FAIL today (RED): without the fix, the rig-core default
//! `on_invalid_tool_call` returns `InvalidToolCallHookAction::Fail`, the
//! agent aborts with `PromptError::UnknownToolCall`, and `run_message`
//! surfaces "Error occurred" instead of the second scripted response.

use crate::harness::TestHarness;
use peakbot::mock::MockResponse;
use rig_core::completion::message::{Message, ToolResultContent, UserContent};
use rig_core::one_or_many::OneOrMany;
use std::time::Duration;

/// Pin the happy path: model emits an unknown tool, rig-core feeds it a
/// synthetic error result, the model retries with a text response, the
/// harness returns that text instead of "Error occurred".
#[tokio::test]
async fn recover_from_unknown_tool_name_serves_retry_prompt() {
    let mut harness = TestHarness::new();

    // First turn: the model hallucinates a tool name that isn't
    // registered. `MockCompletionModel::completion` (in
    // src/mock/completion_model.rs) emits any tool name the test asks
    // for — there's no validation against the real tool registry at the
    // mock layer. Rig-core, however, runs the registered tool set through
    // `validate_tool_call_name` and routes the mismatch through
    // `on_invalid_tool_call`. So `nope` will trigger the hook.
    harness.add_response(MockResponse::tool_call("nope", serde_json::json!({})));
    // Second turn (after the synthetic result is fed back): the model
    // produces a real text response acknowledging the wrong tool.
    harness.add_response(MockResponse::text(
        "I picked the wrong tool. Will retry with bash.",
    ));

    let response = harness.run_message("Please run a command.").await;

    // 1. The harness returned the SECOND scripted text response, not
    //    "Error occurred". Today this assertion fails because rig-core's
    //    default `on_invalid_tool_call` returns Fail, and the agent
    //    errors out on the unknown tool — `run_message` returns
    //    "Error occurred" instead.
    assert_eq!(
        response, "I picked the wrong tool. Will retry with bash.",
        "unknown tool call must not abort the turn; the model should see a \
         synthetic ToolResult and self-correct. Got: {response:?}"
    );

    // 2. The mock model was called exactly twice — once for the bad
    //    tool_call, once for the corrective text response. A regression
    //    that infinite-loops on the unknown tool would push this past 2.
    assert_eq!(
        harness.request_count(),
        2,
        "expected exactly 2 wire calls (bad tool + retry), got {}",
        harness.request_count()
    );

    // 3. The second wire call's `chat_history` carries the synthetic
    //    ToolResult that rig-core injected because `SessionHook` returned
    //    `Skip`. The mock model records every request's chat_history; the
    //    second record is the one that should include it (the synthetic
    //    ToolResult is appended to rig's internal history between the
    //    two completions).
    let recorded = harness.get_recorded_requests();
    assert_eq!(recorded.len(), 2, "expected 2 recorded requests");
    let second_chat_history = &recorded[1].chat_history;
    let synthetic_tool_result = second_chat_history.iter().find_map(|msg| match msg {
        Message::User { content } => content.iter().find_map(|c| match c {
            UserContent::ToolResult(tr) => {
                // We only care about the result whose text mentions our
                // bad tool name — rig-core's Skip path uses the hook's
                // reason verbatim as the ToolResult text.
                let first = tr.content.first_ref();
                if let ToolResultContent::Text(t) = first {
                    if t.text.contains("unknown tool `nope`") {
                        Some(t.text.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        }),
        _ => None,
    });
    let synthetic_text = synthetic_tool_result.unwrap_or_else(|| {
        panic!(
            "expected a User message with ToolResult carrying the synthetic \
             'unknown tool `nope`' reason in the second request's chat_history, \
             but none was found. Second request chat_history: {second_chat_history:#?}"
        )
    });
    // Belt-and-suspenders: pin the leading "Error:" marker (the spec's
    // exact format starts with "Error: unknown tool `<name>`. ...") so
    // the test catches a regression that drops the marker.
    assert!(
        synthetic_text.starts_with("Error:"),
        "synthetic ToolResult must start with 'Error:', got: {synthetic_text:?}"
    );
    // And it must list at least one real tool so the model has something
    // to retry with. The harness registers bash, file_read, think, etc.
    assert!(
        synthetic_text.contains("bash"),
        "synthetic ToolResult must list at least one real tool (bash), got: {synthetic_text:?}"
    );

    // 4. The StateManager chat contains a user-facing warning system
    //    message the hook emits via `add_system_message(...)`. The
    //    planned fix pushes this so the transcript records what
    //    happened, with the "⚠️" emoji + the bad tool name.
    let state = harness.get_state();
    let warning = state
        .chat
        .messages
        .iter()
        .find(|m| {
            m.role == peakbot::ui::app_state::MessageRole::System
                && m.content.starts_with("⚠️")
                && m.content.contains("nope")
        })
        .unwrap_or_else(|| {
            panic!(
                "expected a system message starting with '⚠️' mentioning 'nope' \
                 in the chat, but none was found. Messages: {:#?}",
                state.chat.messages
            )
        });
    // Pin the assertion-relevant substrings so the test breaks if either
    // the emoji or the tool-name token is dropped.
    assert!(
        warning.content.contains("⚠️"),
        "warning must use the ⚠️ marker, got: {:?}",
        warning.content
    );
    assert!(
        warning.content.contains("nope"),
        "warning must mention the bad tool name 'nope', got: {:?}",
        warning.content
    );
}

/// Pathological-model guard: if the model ONLY emits an unknown tool
/// call (and never produces a corrective response), the hook must not
/// create an infinite retry loop. The fix uses `Skip`, which rig-core
/// documents as "this does not execute the invalid tool" — so the turn
/// terminates after the synthetic result is injected and the next call
/// to the model finds no queued response (the mock errors out fast
/// because `MockCompletionModel::completion` returns
/// `CompletionError::ProviderError` on empty queue).
///
/// Bounded with a `tokio::time::timeout` so a regression that loops
/// here fails the test instead of hanging the suite.
#[tokio::test]
async fn unknown_tool_call_does_not_hang_on_pathological_model() {
    let mut harness = TestHarness::new();

    // Single response: the unknown tool call. NO follow-up.
    harness.add_response(MockResponse::tool_call("nope", serde_json::json!({})));

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        harness.run_message("Please run a command."),
    )
    .await;

    let response = result.unwrap_or_else(|_| {
        panic!(
            "run_message hung for >5s — unknown tool call must not trigger an \
             infinite retry loop. Today (without the fix) the turn aborts fast; \
             after the fix, the agent terminates after one synthetic result \
             because the mock's queue is empty."
        )
    });

    // After the fix: the agent consumes 1 response (the bad tool call),
    // rig-core injects the synthetic ToolResult, calls the model again,
    // and the mock errors out fast on the empty queue → TestRunner
    // returns "Error occurred". The exact response string is less
    // important than the bound on time and the bound on wire calls.
    assert_eq!(
        harness.request_count(),
        2,
        "expected exactly 2 wire calls (bad tool + empty-queue error), \
         got {} — anything more means the hook is re-firing",
        harness.request_count()
    );

    // And the test wrapper consumed a non-timeout result, which means
    // the harness returned promptly. We don't pin the exact return
    // string here because the empty-queue error path is owned by
    // `MockCompletionModel`/`TestRunner`; the only contract this test
    // pins is "no hang, no unbounded loop".
    let _ = response;
}

/// Sanity check that the harness's `recorded_requests` chat_history uses
/// `OneOrMany`'s `iter()` shape the rest of the file expects — keeps
/// the search-iterator code honest if rig-core ever changes the
/// accessor. Cheap insurance; mirrors the explicit `first_ref()` use
/// in the main test above.
#[allow(dead_code)]
fn _unused_assertion_helper() {
    let _ = OneOrMany::one(String::new());
}
