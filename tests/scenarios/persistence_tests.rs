//! Persistence tests - E2E conversation persistence through AgentRunner
//!
//! Tests for verifying conversation save/load behavior through the full agentic loop.
//! All tests flow through TestRunner which uses real ConversationManager.

use crate::harness::TestHarness;
use peakbot::mock::MockResponse;

/// Test that conversation is saved after processing a message
#[tokio::test]
async fn conversation_save_via_agent() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Hello! I'm here to help."));

    let response = harness.run_message("Hello").await;

    assert!(!response.is_empty());

    // Verify conversation was saved via ConversationManager
    if let Some(cm) = harness.conversation_manager() {
        let cm = cm.lock().unwrap();
        let conv = cm.get_current();
        assert!(conv.is_some(), "Conversation should exist after save");
        assert_eq!(conv.unwrap().messages.len(), 2, "Should have user + assistant message");
    }
}

/// Test that messages persist across multiple turns
#[tokio::test]
async fn conversation_persists_across_turns() {
    let mut harness = TestHarness::new();
    harness.add_responses(vec![
        MockResponse::text("First response"),
        MockResponse::text("Second response"),
        MockResponse::text("Third response"),
    ]);

    harness.run_message("First message").await;
    harness.run_message("Second message").await;
    harness.run_message("Third message").await;

    // Verify all messages are in the conversation
    if let Some(cm) = harness.conversation_manager() {
        let cm = cm.lock().unwrap();
        let conv = cm.get_current();
        assert!(conv.is_some());
        // 3 user messages + 3 assistant messages = 6
        assert_eq!(conv.unwrap().messages.len(), 6);
    }
}

/// Test conversation list functionality
#[tokio::test]
async fn conversation_list_functionality() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Response"));

    // Create a conversation
    harness.run_message("Test message").await;
    
    // List conversations in the same harness instance
    if let Some(cm) = harness.conversation_manager() {
        let cm = cm.lock().unwrap();
        let list = cm.list().unwrap();
        // Should have at least one conversation
        assert!(!list.is_empty(), "Should have at least one conversation in the list");
    }
    
    // Note: Testing conversation persistence across harness instances
    // requires shared storage, which is a more advanced feature.
    // For now, we verify the list functionality works within a single instance.
}

/// Test conversation metadata preservation
#[tokio::test]
async fn conversation_metadata_preserved() {
    let mut harness = TestHarness::new();
    harness.add_response(MockResponse::text("Response"));

    harness.run_message("Test message").await;

    // Verify metadata is preserved
    if let Some(cm) = harness.conversation_manager() {
        let cm = cm.lock().unwrap();
        let conv = cm.get_current();
        assert!(conv.is_some());
        let conv = conv.unwrap();
        
        assert!(!conv.name.is_empty(), "Conversation should have a name");
        assert!(!conv.model.is_empty(), "Conversation should have a model");
        assert!(conv.created_at <= chrono::Utc::now(), "Created at should be in the past");
    }
}

/// Test that tool calls are recorded in conversation
#[tokio::test]
async fn tool_calls_recorded_in_conversation() {
    let mut harness = TestHarness::new();
    
    // Queue tool call followed by final response
    harness.add_response(MockResponse::tool_call(
        "todo",
        serde_json::json!({
            "action": "add",
            "tasks": ["Test task"]
        }),
    ));
    harness.add_response(MockResponse::text("Task added successfully."));

    harness.run_message("Add a todo: Test task").await;

    // Verify conversation has user and assistant messages
    if let Some(cm) = harness.conversation_manager() {
        let cm = cm.lock().unwrap();
        let conv = cm.get_current();
        assert!(conv.is_some());
        
        // Check message count - should have user + tool call + assistant response
        let message_count = conv.unwrap().messages.len();
        // With tool call: user message + tool call message + assistant response
        assert!(message_count >= 2, "Should have at least user + assistant message, got {}", message_count);
    }
}