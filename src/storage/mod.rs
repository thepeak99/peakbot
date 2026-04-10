//! Storage trait for conversation persistence.
//!
//! This trait enables different storage backends (in-memory for testing,
//! file-based for production) while keeping the ConversationManager generic.

mod file_storage;

pub use file_storage::FileStorage;
pub use crate::conversation::{Conversation, ConversationSummary};
pub use anyhow::Result;
pub use uuid::Uuid;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Trait for conversation storage backends.
///
/// Implement this trait to add new storage backends (e.g., database, S3).
pub trait ConversationStorage: Send + Sync {
    /// Save a conversation
    fn save(&self, conversation: &Conversation) -> Result<()>;

    /// Load a conversation by ID
    fn load(&self, id: Uuid) -> Result<Conversation>;

    /// List all conversation summaries
    fn list(&self) -> Result<Vec<ConversationSummary>>;

    /// Delete a conversation by ID
    fn delete(&self, id: Uuid) -> Result<()>;

    /// Check if a conversation exists
    fn exists(&self, id: Uuid) -> bool;
}

/// In-memory storage for testing purposes.
///
/// This storage keeps conversations in memory and is useful for tests
/// where file system access is not desired.
pub struct InMemoryStorage {
    conversations: Arc<RwLock<HashMap<Uuid, Conversation>>>,
}

impl InMemoryStorage {
    /// Create a new in-memory storage
    pub fn new() -> Self {
        Self {
            conversations: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl ConversationStorage for InMemoryStorage {
    fn save(&self, conversation: &Conversation) -> Result<()> {
        let mut map = self.conversations.write().unwrap();
        map.insert(conversation.id, conversation.clone());
        Ok(())
    }

    fn load(&self, id: Uuid) -> Result<Conversation> {
        let map = self.conversations.read().unwrap();
        map.get(&id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Conversation not found: {}", id))
    }

    fn list(&self) -> Result<Vec<ConversationSummary>> {
        let map = self.conversations.read().unwrap();
        Ok(map.values().map(|c| ConversationSummary::from(c)).collect())
    }

    fn delete(&self, id: Uuid) -> Result<()> {
        let mut map = self.conversations.write().unwrap();
        map.remove(&id);
        Ok(())
    }

    fn exists(&self, id: Uuid) -> bool {
        let map = self.conversations.read().unwrap();
        map.contains_key(&id)
    }
}
