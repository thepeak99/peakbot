//! Storage abstraction for integration testing
//!
//! Re-exports storage types from the main peakbot crate for use in tests.

mod in_memory;

// Re-export InMemoryStorage for tests
pub use in_memory::InMemoryStorage;

// Re-export ConversationStorage trait from peakbot
pub use peakbot::ConversationStorage;
