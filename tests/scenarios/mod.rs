//! Test scenarios for integration testing
//!
//! Each scenario file tests a specific domain through the full agent loop.
//! Unit tests for individual components live in their respective source files.

mod bash_tests;
mod bg_tests;
mod chat_render_tests;
mod compaction_tests;
mod context_tests;
mod e2e_tests;
mod event_tests;
mod message_roundtrip;
mod no_set_current_dir;
mod persistence_tests;
mod pipeline_tests;
mod powershell_tests;
mod queued_input_tests;
mod stats_tests;
mod stop_tests;
mod tool_error_recovery;
mod unknown_tool_call_recovery;
