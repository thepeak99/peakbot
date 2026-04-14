//! Test scenarios for integration testing
//!
//! Each scenario file tests a specific domain through the full agent loop.
//! Unit tests for individual components live in their respective source files.

mod compaction_tests;
mod context_tests;
mod e2e_tests;
mod event_tests;
mod message_roundtrip;
mod persistence_tests;
mod stats_tests;
mod stop_tests;
