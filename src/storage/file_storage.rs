//! File-based storage for conversation persistence.
//!
//! Uses atomic writes (temp file + rename) for crash safety.

use super::ConversationStorage;
use crate::conversation::{Conversation, ConversationSummary};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// File-based storage backend for conversations.
///
/// Uses atomic writes (write to temp file, then rename) for crash safety.
pub struct FileStorage {
    storage_dir: PathBuf,
}

impl FileStorage {
    /// Create a new FileStorage with the given storage directory.
    ///
    /// Creates the directory if it doesn't exist.
    pub fn new(storage_dir: PathBuf) -> Result<Self> {
        if !storage_dir.exists() {
            fs::create_dir_all(&storage_dir)
                .context("Failed to create conversation storage directory")?;
        }

        // Clean up any temp files from interrupted writes
        let _ = Self::cleanup_temp_files(&storage_dir);

        Ok(Self { storage_dir })
    }

    /// Get the storage directory path
    pub fn storage_dir(&self) -> &Path {
        &self.storage_dir
    }

    /// Clean up temporary files from interrupted writes
    fn cleanup_temp_files(storage_dir: &Path) -> Result<()> {
        if let Ok(entries) = fs::read_dir(storage_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = path.file_name()
                    && name.to_string_lossy().starts_with(".tmp")
                {
                    tracing::debug!("Cleaning up temp file: {}", path.display());
                    let _ = fs::remove_file(path);
                }
            }
        }
        Ok(())
    }

    /// Get the file path for a conversation
    fn conversation_path(&self, id: Uuid) -> PathBuf {
        self.storage_dir.join(format!("{}.json", id))
    }

    /// Load a conversation from a specific file path
    fn load_from_path(&self, path: &Path) -> Result<Conversation> {
        let content = fs::read_to_string(path).context("Failed to read conversation file")?;

        let conv: Conversation =
            serde_json::from_str(&content).context("Failed to parse conversation JSON")?;

        Ok(conv)
    }
}

impl ConversationStorage for FileStorage {
    fn save(&self, conversation: &Conversation) -> Result<()> {
        let temp_path = self.storage_dir.join(".tmp.json");
        let final_path = self.conversation_path(conversation.id);

        let json = serde_json::to_string_pretty(conversation)?;

        // Write to temp file first
        fs::write(&temp_path, json)?;

        // Atomic rename
        fs::rename(&temp_path, &final_path)?;

        tracing::debug!(
            "Saved conversation {} to {}",
            conversation.id,
            final_path.display()
        );

        Ok(())
    }

    fn load(&self, id: Uuid) -> Result<Conversation> {
        let path = self.conversation_path(id);

        if !path.exists() {
            anyhow::bail!("Conversation not found: {}", id);
        }

        let content = fs::read_to_string(&path).context("Failed to read conversation file")?;

        let conv: Conversation =
            serde_json::from_str(&content).context("Failed to parse conversation JSON")?;

        Ok(conv)
    }

    fn list(&self) -> Result<Vec<ConversationSummary>> {
        let mut summaries = Vec::new();

        if !self.storage_dir.exists() {
            return Ok(summaries);
        }

        let entries =
            fs::read_dir(&self.storage_dir).context("Failed to read storage directory")?;

        for entry in entries.flatten() {
            let path = entry.path();

            // Skip temp files and non-json files
            if let Some(name) = path.file_name() {
                let name_str = name.to_string_lossy();
                if name_str.starts_with('.') || !name_str.ends_with(".json") {
                    continue;
                }
            } else {
                continue;
            }

            // Try to load and summarize
            match self.load_from_path(&path) {
                Ok(conv) => summaries.push(ConversationSummary::from(&conv)),
                Err(e) => {
                    // Skip corrupted files - log but don't fail
                    tracing::warn!(
                        "Skipping corrupted conversation file {}: {}",
                        path.display(),
                        e
                    );
                }
            }
        }

        // Sort by updated_at descending (most recent first)
        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        Ok(summaries)
    }

    fn delete(&self, id: Uuid) -> Result<()> {
        let path = self.conversation_path(id);

        if !path.exists() {
            anyhow::bail!("Conversation not found: {}", id);
        }

        fs::remove_file(&path)?;

        tracing::debug!("Deleted conversation {}", id);

        Ok(())
    }

    fn exists(&self, id: Uuid) -> bool {
        self.conversation_path(id).exists()
    }
}
