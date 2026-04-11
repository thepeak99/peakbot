//! In-memory storage for integration testing
//!
//! Provides a fast, isolated storage backend for tests.
//! No disk I/O, no cleanup needed.

use anyhow::Result;
use peakbot::ConversationStorage;
use peakbot::{Conversation, ConversationSummary};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// In-memory storage for tests.
///
/// All conversations are held in memory - no persistence.
/// Fast and isolated for testing.
pub struct InMemoryStorage {
    conversations: Arc<Mutex<HashMap<Uuid, Conversation>>>,
}

impl InMemoryStorage {
    /// Create a new empty InMemoryStorage
    pub fn new() -> Self {
        Self {
            conversations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get the number of stored conversations
    pub fn len(&self) -> usize {
        self.conversations.lock().unwrap().len()
    }

    /// Check if storage is empty
    pub fn is_empty(&self) -> bool {
        self.conversations.lock().unwrap().is_empty()
    }

    /// Get all conversation IDs
    pub fn ids(&self) -> Vec<Uuid> {
        self.conversations.lock().unwrap().keys().cloned().collect()
    }
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl ConversationStorage for InMemoryStorage {
    fn save(&self, conversation: &Conversation) -> Result<()> {
        let mut storage = self.conversations.lock().unwrap();
        storage.insert(conversation.id, conversation.clone());
        Ok(())
    }

    fn load(&self, id: Uuid) -> Result<Conversation> {
        let storage = self.conversations.lock().unwrap();
        storage
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Conversation not found: {}", id))
    }

    fn list(&self) -> Result<Vec<ConversationSummary>> {
        let storage = self.conversations.lock().unwrap();
        let mut summaries: Vec<ConversationSummary> =
            storage.values().map(ConversationSummary::from).collect();
        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(summaries)
    }

    fn delete(&self, id: Uuid) -> Result<()> {
        let mut storage = self.conversations.lock().unwrap();
        storage.remove(&id);
        Ok(())
    }

    fn exists(&self, id: Uuid) -> bool {
        self.conversations.lock().unwrap().contains_key(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peakbot::Conversation;

    #[test]
    fn test_save_and_load() {
        let storage = InMemoryStorage::new();

        let conv = Conversation::new("Test".to_string(), "mock-model".to_string());
        storage.save(&conv).unwrap();

        let loaded = storage.load(conv.id).unwrap();
        assert_eq!(loaded.name, "Test");
    }

    #[test]
    fn test_not_found() {
        let storage = InMemoryStorage::new();

        let result = storage.load(Uuid::new_v4());
        assert!(result.is_err());
    }

    #[test]
    fn test_list() {
        let storage = InMemoryStorage::new();

        let conv1 = Conversation::new("First".to_string(), "model".to_string());
        let conv2 = Conversation::new("Second".to_string(), "model".to_string());

        storage.save(&conv1).unwrap();
        storage.save(&conv2).unwrap();

        let list = storage.list().unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_delete() {
        let storage = InMemoryStorage::new();

        let conv = Conversation::new("Test".to_string(), "model".to_string());
        storage.save(&conv).unwrap();

        assert!(storage.exists(conv.id));

        storage.delete(conv.id).unwrap();

        assert!(!storage.exists(conv.id));
    }
}
