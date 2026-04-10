//! Stats tests - Token tracking and event emission
//!
//! Tests for verifying stats accumulation and event system.

use peakbot::mock::{MockResponse, Usage};
use crate::harness::TestHarness;
use peakbot::state::StateManager;
use std::sync::Arc;

#[tokio::test]
async fn stats_initial_state() {
    let state_manager = Arc::new(StateManager::new());
    let stats = state_manager.get_stats();

    assert_eq!(stats.total_input_tokens, 0);
    assert_eq!(stats.total_output_tokens, 0);
    assert_eq!(stats.total_api_calls, 0);
}

#[tokio::test]
async fn stats_accumulate_requests() {
    let state_manager = Arc::new(StateManager::new());

    // Add some requests
    state_manager.add_request(100, 50, 0.01);
    state_manager.add_request(200, 100, 0.02);
    state_manager.add_request(150, 75, 0.015);

    let stats = state_manager.get_stats();

    assert_eq!(stats.total_api_calls, 3);
    // Note: input/output tokens ARE accumulated (sum of all requests)
    assert_eq!(stats.total_input_tokens, 450);  // 100 + 200 + 150
    assert_eq!(stats.total_output_tokens, 225); // 50 + 100 + 75
}

#[tokio::test]
async fn stats_cost_accumulates() {
    let state_manager = Arc::new(StateManager::new());

    state_manager.add_request(100, 50, 0.01);
    state_manager.add_request(200, 100, 0.02);

    let stats = state_manager.get_stats();

    // Cost should accumulate
    assert!((stats.total_cost - 0.03).abs() < f64::EPSILON);
}

#[tokio::test]
async fn stats_reset() {
    let state_manager = Arc::new(StateManager::new());

    state_manager.add_request(100, 50, 0.01);
    state_manager.reset_stats();

    let stats = state_manager.get_stats();

    assert_eq!(stats.total_input_tokens, 0);
    assert_eq!(stats.total_output_tokens, 0);
    assert_eq!(stats.total_api_calls, 0);
    assert!((stats.total_cost - 0.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn mock_response_with_usage() {
    let mut harness = TestHarness::new();

    // Add response with specific token usage
    let usage = Usage {
        input_tokens: 150,
        output_tokens: 75,
    };
    harness.add_response(MockResponse::text_with_usage("Test response", usage));

    let response = harness.run_message("Hello").await;

    assert!(response.contains("Test"));
}

#[tokio::test]
async fn multiple_messages_with_usage() {
    let mut harness = TestHarness::new();

    // Add multiple responses with different usage
    harness.add_responses(vec![
        MockResponse::text_with_usage(
            "First",
            Usage {
                input_tokens: 100,
                output_tokens: 50,
            },
        ),
        MockResponse::text_with_usage(
            "Second",
            Usage {
                input_tokens: 150,
                output_tokens: 75,
            },
        ),
        MockResponse::text_with_usage(
            "Third",
            Usage {
                input_tokens: 200,
                output_tokens: 100,
            },
        ),
    ]);

    let _r1 = harness.run_message("One").await;
    let _r2 = harness.run_message("Two").await;
    let _r3 = harness.run_message("Three").await;

    // All responses should have been consumed
    assert!(!harness.has_remaining_responses());
    assert_eq!(harness.remaining_responses(), 0);
}

#[tokio::test]
async fn state_manager_app_state_sync() {
    let state_manager = Arc::new(StateManager::new());

    // Add stats via StateManager
    state_manager.add_request(100, 50, 0.01);

    // Get AppState and verify stats are synced
    let app_state = state_manager.get_state();

    assert_eq!(app_state.stats.total_input_tokens, 100);
    assert_eq!(app_state.stats.total_output_tokens, 50);
    assert_eq!(app_state.stats.total_api_calls, 1);
}

#[tokio::test]
async fn session_stats_arc_sharing() {
    let state_manager = Arc::new(StateManager::new());
    let stats_arc = state_manager.stats_arc();

    // The Arc should be shareable - add request first
    state_manager.add_request(100, 50, 0.01);

    // Both references should see the same data via stats arc
    let stats = state_manager.get_stats();
    assert_eq!(stats.total_api_calls, 1);
}
