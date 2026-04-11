//! Stats tests - Token tracking through the agent loop
//!
//! Integration tests for verifying stats accumulation via TestHarness.
//! Unit tests for StateManager live in src/state/state_manager.rs.

use crate::harness::TestHarness;
use peakbot::mock::{MockResponse, Usage};

#[tokio::test]
async fn mock_response_with_usage() {
    let mut harness = TestHarness::new();

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
