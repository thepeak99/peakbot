//! E2E tests — Full agent loop with only the LLM mocked
//!
//! These tests verify the complete pipeline:
//!   User message → TestRunner → Rig agentic loop → Real tools → Real StateManager
//!
//! Only MockCompletionModel is faked. Everything else is production code.

use crate::harness::TestHarness;
use peakbot::AgentEvent;
use peakbot::mock::{MockResponse, Usage};

/// Helper: minimal valid todo tool_call JSON with `thought` filled in.
fn todo_add_args(tasks: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "thought": "Adding tasks",
        "action": "add",
        "tasks": tasks
    })
}

fn todo_update_args(id: usize, status: &str) -> serde_json::Value {
    serde_json::json!({
        "thought": "Updating task",
        "action": "update",
        "task_id": id,
        "status": status
    })
}

fn todo_remove_args(id: usize) -> serde_json::Value {
    serde_json::json!({
        "thought": "Removing task",
        "action": "remove",
        "task_id": id
    })
}

// ── Todo tool through the agent loop ──────────────────────────────────────

/// The quintessential E2E test: model returns a tool_call → Rig dispatches
/// to real TodoTool → TodoTool writes to StateManager → AppState reflects it.
#[tokio::test]
async fn todo_add_through_agent_loop() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::tool_call(
        "todo",
        todo_add_args(&["Fix the bug"]),
    ));
    harness.add_response(MockResponse::text("Done"));

    harness.run_message("Add a todo: Fix the bug").await;

    // StateManager.todos must reflect the tool execution
    let todos = harness.get_todos();
    assert_eq!(todos.len(), 1, "Should have 1 todo after tool call");
    assert_eq!(todos[0].task, "Fix the bug");

    // AppState must be synced
    let state = harness.get_state();
    assert_eq!(state.todo.items.len(), 1, "AppState should reflect todo");

    // Events must be emitted
    let events = harness.drain_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { tool_name, .. } if tool_name == "todo")),
        "Should emit ToolCall event for todo"
    );
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::ToolResult { tool_name, success, .. } if tool_name == "todo" && *success)),
        "Should emit ToolResult event with success=true"
    );

    // Conversation must be saved
    if let Some(cm) = harness.conversation_manager() {
        let cm = cm.lock().unwrap();
        let conv = cm.get_current();
        assert!(conv.is_some(), "Conversation should exist");
        assert!(
            conv.unwrap().messages.len() >= 2,
            "Should have user + assistant messages"
        );
    }
}

/// Add multiple todos through the agent loop, verify all arrive in StateManager.
#[tokio::test]
async fn todo_add_multiple_through_agent_loop() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::tool_call(
        "todo",
        todo_add_args(&["Task one", "Task two", "Task three"]),
    ));
    harness.add_response(MockResponse::text("Added all three."));

    harness.run_message("Add three tasks").await;

    let todos = harness.get_todos();
    assert_eq!(todos.len(), 3);
    assert_eq!(todos[0].task, "Task one");
    assert_eq!(todos[1].task, "Task two");
    assert_eq!(todos[2].task, "Task three");
}

/// Add then update a todo through two agent turns.
#[tokio::test]
async fn todo_update_status_through_agent_loop() {
    let mut harness = TestHarness::new();

    // Turn 1: add
    harness.add_response(MockResponse::tool_call(
        "todo",
        todo_add_args(&["Write tests"]),
    ));
    harness.add_response(MockResponse::text("Added."));

    harness.run_message("Add todo: Write tests").await;
    assert_eq!(harness.get_todos().len(), 1);

    // Turn 2: update status
    harness.add_response(MockResponse::tool_call(
        "todo",
        todo_update_args(1, "completed"),
    ));
    harness.add_response(MockResponse::text("Marked done."));

    harness.run_message("Mark task 1 completed").await;

    let todos = harness.get_todos();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].status, peakbot::TodoStatus::Completed);
}

/// Add, then remove — full lifecycle through the agent loop.
#[tokio::test]
async fn todo_lifecycle_through_agent_loop() {
    let mut harness = TestHarness::new();

    // Add
    harness.add_response(MockResponse::tool_call(
        "todo",
        todo_add_args(&["Temporary task"]),
    ));
    harness.add_response(MockResponse::text("Added."));
    harness.run_message("Add a task").await;
    assert_eq!(harness.get_todos().len(), 1);

    // Remove
    harness.add_response(MockResponse::tool_call("todo", todo_remove_args(1)));
    harness.add_response(MockResponse::text("Removed."));
    harness.run_message("Remove task 1").await;

    let todos = harness.get_todos();
    assert!(todos.is_empty(), "Todo list should be empty after remove");
}

// ── Stats through the agent loop ──────────────────────────────────────────

/// Stats (tokens, cost, API calls) must flow from SessionHook → StateManager
/// after a real agent call.
#[tokio::test]
async fn stats_update_through_agent_loop() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text_with_usage(
        "Hello!",
        Usage {
            input_tokens: 200,
            output_tokens: 80,
        },
    ));

    harness.run_message("Hi").await;

    let stats = harness.get_stats();
    assert!(
        stats.total_api_calls > 0,
        "API calls should be tracked after agent loop"
    );
    // Token counts are per-request (overwritten by rig), cost accumulates
    assert!(
        stats.total_input_tokens > 0,
        "Input tokens should be nonzero after agent loop"
    );
    assert!(
        stats.total_output_tokens > 0,
        "Output tokens should be nonzero after agent loop"
    );
}

/// Cost must accumulate across multiple agent turns.
#[tokio::test]
async fn cost_accumulates_across_turns() {
    let mut harness = TestHarness::new();

    harness.add_response(MockResponse::text_with_usage(
        "First",
        Usage {
            input_tokens: 100,
            output_tokens: 50,
        },
    ));
    harness.add_response(MockResponse::text_with_usage(
        "Second",
        Usage {
            input_tokens: 150,
            output_tokens: 75,
        },
    ));

    harness.run_message("One").await;
    let cost_after_one = harness.get_stats().total_cost;

    harness.run_message("Two").await;
    let cost_after_two = harness.get_stats().total_cost;

    assert!(
        cost_after_two > cost_after_one,
        "Cost should grow across turns: before={}, after={}",
        cost_after_one,
        cost_after_two,
    );
}

/// Stats must be reflected in AppState (sync from StateManager).
#[tokio::test]
async fn stats_synced_to_app_state() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text_with_usage(
        "Response",
        Usage {
            input_tokens: 100,
            output_tokens: 50,
        },
    ));

    harness.run_message("Test").await;

    let state = harness.get_state();
    assert!(
        state.stats.total_api_calls > 0,
        "AppState.stats should reflect API calls"
    );
    assert!(
        state.stats.total_input_tokens > 0,
        "AppState.stats should reflect input tokens"
    );
}

// ── Combined: todo + stats + events in one flow ───────────────────────────

/// Full pipeline: tool call triggers events, updates state, records cost.
#[tokio::test]
async fn full_pipeline_tool_call_with_stats_and_events() {
    let mut harness = TestHarness::new();

    harness.add_response(MockResponse::tool_call_with_usage(
        "todo",
        todo_add_args(&["E2E task"]),
        Usage {
            input_tokens: 300,
            output_tokens: 40,
        },
    ));
    harness.add_response(MockResponse::text_with_usage(
        "Task added.",
        Usage {
            input_tokens: 350,
            output_tokens: 30,
        },
    ));

    harness.run_message("Add a task").await;

    // 1. State updated
    let todos = harness.get_todos();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].task, "E2E task");

    // 2. Stats accumulated
    let stats = harness.get_stats();
    assert!(stats.total_api_calls > 0);

    // 3. Events in correct order
    let events = harness.drain_events();
    let tool_call_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCall { tool_name, .. } if tool_name == "todo"));
    let tool_result_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolResult { tool_name, .. } if tool_name == "todo"));
    assert!(tool_call_idx.is_some(), "Should have ToolCall event");
    assert!(tool_result_idx.is_some(), "Should have ToolResult event");
    assert!(
        tool_call_idx < tool_result_idx,
        "ToolCall should come before ToolResult"
    );

    // 4. Conversation saved with messages
    if let Some(cm) = harness.conversation_manager() {
        let cm = cm.lock().unwrap();
        let conv = cm.get_current();
        assert!(conv.is_some());
        assert!(conv.unwrap().messages.len() >= 2);
    }
}
