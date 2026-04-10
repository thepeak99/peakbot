//! Storage trait for conversation persistence
//!
//! This trait enables different storage backends (in-memory for testing,
//! file-based for production) while keeping the ConversationManager generic.

use anyhow::Result;
use peakbot::{Conversation, ConversationSummary};
use uuid::Uuid;

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
