//! Storage abstraction for integration testing
//!
//! Re-exports storage types from the main peakbot crate for use in tests.

mod in_memory;

// Re-export InMemoryStorage for tests
#[allow(unused_imports)]
pub use in_memory::InMemoryStorage;
