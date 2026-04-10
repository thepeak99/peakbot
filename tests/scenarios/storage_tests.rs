//! Storage persistence tests
//!
//! Tests for verifying conversation persistence with InMemoryStorage.

use crate::storage::{ConversationStorage, InMemoryStorage};
use peakbot::Conversation;
use uuid::Uuid;

#[tokio::test]
async fn storage_save_and_load() {
    let storage = InMemoryStorage::new();

    // Create a conversation
    let mut conv = Conversation::new("Test".to_string(), "mock-model".to_string());
    conv.add_user_message("Hello".to_string());
    conv.add_assistant_message("Hi there!".to_string());

    // Save it
    storage.save(&conv).unwrap();

    // Load it back
    let loaded = storage.load(conv.id).unwrap();

    assert_eq!(loaded.name, "Test");
    assert_eq!(loaded.messages.len(), 2);
}

#[tokio::test]
async fn storage_list_conversations() {
    let storage = InMemoryStorage::new();

    // Create multiple conversations
    let conv1 = Conversation::new("First".to_string(), "model".to_string());
    let conv2 = Conversation::new("Second".to_string(), "model".to_string());
    let conv3 = Conversation::new("Third".to_string(), "model".to_string());

    storage.save(&conv1).unwrap();
    storage.save(&conv2).unwrap();
    storage.save(&conv3).unwrap();

    // List all
    let list = storage.list().unwrap();

    assert_eq!(list.len(), 3);
}

#[tokio::test]
async fn storage_delete_conversation() {
    let storage = InMemoryStorage::new();

    let conv = Conversation::new("ToDelete".to_string(), "model".to_string());
    storage.save(&conv).unwrap();

    assert!(storage.exists(conv.id));

    storage.delete(conv.id).unwrap();

    assert!(!storage.exists(conv.id));
}

#[tokio::test]
async fn storage_not_found_error() {
    let storage = InMemoryStorage::new();

    let result = storage.load(Uuid::new_v4());

    assert!(result.is_err());
}

#[tokio::test]
async fn storage_conversation_message_preservation() {
    let storage = InMemoryStorage::new();

    let mut conv = Conversation::new("Preserve".to_string(), "model".to_string());
    conv.add_user_message("Message 1".to_string());
    conv.add_assistant_message("Response 1".to_string());
    conv.add_user_message("Message 2".to_string());
    conv.add_assistant_message("Response 2".to_string());

    storage.save(&conv).unwrap();

    let loaded = storage.load(conv.id).unwrap();

    // Verify all messages are preserved
    assert_eq!(loaded.messages.len(), 4);
}
