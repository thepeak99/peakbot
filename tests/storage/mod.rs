//! Storage abstraction for integration testing

mod in_memory;
mod storage_trait;

pub use in_memory::InMemoryStorage;
pub use storage_trait::ConversationStorage;
