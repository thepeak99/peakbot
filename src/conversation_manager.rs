//! Conversation Manager - handles CRUD operations for persisted conversations.

use crate::conversation::{Conversation, ConversationSummary, Message};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Configuration for conversation persistence
#[derive(Debug, Clone)]
pub struct ConversationManagerConfig {
    /// Enable auto-save (default: true)
    pub auto_save: bool,
    /// Storage directory for conversations
    pub storage_dir: PathBuf,
    /// Maximum number of conversations to keep (0 = unlimited)
    pub max_conversations: usize,
    /// Auto-load last conversation on startup
    pub auto_resume: bool,
}

impl Default for ConversationManagerConfig {
    fn default() -> Self {
        Self {
            auto_save: true,
            storage_dir: get_default_storage_dir(),
            max_conversations: 50,
            auto_resume: true,
        }
    }
}

/// Get the default storage directory for conversations
fn get_default_storage_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("peakbot")
        .join("conversations")
}

/// Manager for conversation persistence
pub struct ConversationManager {
    config: ConversationManagerConfig,
    current_conversation: Option<Conversation>,
}

impl ConversationManager {
    /// Create a new ConversationManager
    pub fn new(config: ConversationManagerConfig) -> Result<Self> {
        // Create storage directory if it doesn't exist
        if !config.storage_dir.exists() {
            fs::create_dir_all(&config.storage_dir)
                .context("Failed to create conversation storage directory")?;
        }

        // Clean up any temp files from interrupted writes
        let _ = Self::cleanup_temp_files(&config.storage_dir);

        Ok(Self {
            config,
            current_conversation: None,
        })
    }

    /// Clean up temporary files from interrupted writes
    fn cleanup_temp_files(storage_dir: &Path) -> Result<()> {
        if let Ok(entries) = fs::read_dir(storage_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = path.file_name() {
                    if name.to_string_lossy().starts_with(".tmp") {
                        tracing::debug!("Cleaning up temp file: {}", path.display());
                        let _ = fs::remove_file(path);
                    }
                }
            }
        }
        Ok(())
    }

    /// Create a new conversation
    pub fn create_new(&mut self, name: String, model: String) -> Result<&Conversation> {
        // Enforce max conversations limit BEFORE adding the new one
        if self.config.max_conversations > 0 {
            self.enforce_max_conversations()?;
        }

        let conv = Conversation::new(name, model);

        self.current_conversation = Some(conv);

        // Don't auto-save here - only save when there are actual messages
        // This prevents saving empty conversations

        Ok(self.current_conversation.as_ref().unwrap())
    }

    /// Get the current conversation
    pub fn get_current(&self) -> Option<&Conversation> {
        self.current_conversation.as_ref()
    }

    /// Get mutable reference to current conversation
    pub fn get_current_mut(&mut self) -> Option<&mut Conversation> {
        self.current_conversation.as_mut()
    }

    /// Save the current conversation (atomic write)
    pub fn save(&self) -> Result<()> {
        if let Some(ref conv) = self.current_conversation {
            self.save_conversation(conv)?;
        }
        Ok(())
    }

    /// Save a specific conversation to disk
    fn save_conversation(&self, conversation: &Conversation) -> Result<()> {
        let temp_path = self.config.storage_dir.join(".tmp.json");
        let final_path = self
            .config
            .storage_dir
            .join(format!("{}.json", conversation.id));

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

    /// Load a conversation by ID
    pub fn load(&self, id: Uuid) -> Result<Conversation> {
        let path = self.config.storage_dir.join(format!("{}.json", id));

        if !path.exists() {
            anyhow::bail!("Conversation not found: {}", id);
        }

        let content = fs::read_to_string(&path).context("Failed to read conversation file")?;

        let conv: Conversation =
            serde_json::from_str(&content).context("Failed to parse conversation JSON")?;

        Ok(conv)
    }

    /// Get the latest (most recently updated) conversation
    pub fn get_latest(&self) -> Result<Option<Conversation>> {
        let summaries = self.list()?;

        if summaries.is_empty() {
            return Ok(None);
        }

        // Find the most recently updated
        let latest = summaries.iter().max_by_key(|s| s.updated_at).unwrap();

        Ok(Some(self.load(latest.id)?))
    }

    /// List all conversations (returns summaries)
    pub fn list(&self) -> Result<Vec<ConversationSummary>> {
        let mut summaries = Vec::new();

        if !self.config.storage_dir.exists() {
            return Ok(summaries);
        }

        let entries =
            fs::read_dir(&self.config.storage_dir).context("Failed to read storage directory")?;

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

    /// Load a conversation from a specific file path
    fn load_from_path(&self, path: &Path) -> Result<Conversation> {
        let content = fs::read_to_string(path).context("Failed to read conversation file")?;

        let conv: Conversation =
            serde_json::from_str(&content).context("Failed to parse conversation JSON")?;

        Ok(conv)
    }

    /// Delete a conversation by ID
    pub fn delete(&self, id: Uuid) -> Result<()> {
        let path = self.config.storage_dir.join(format!("{}.json", id));

        if !path.exists() {
            anyhow::bail!("Conversation not found: {}", id);
        }

        fs::remove_file(&path)?;

        tracing::debug!("Deleted conversation {}", id);

        Ok(())
    }

    /// Add a user message to the current conversation
    pub fn add_user_message(&mut self, content: String) -> Result<()> {
        if let Some(ref mut conv) = self.current_conversation {
            conv.add_user_message(content);

            if self.config.auto_save {
                self.save()?;
            }
        }
        Ok(())
    }

    /// Add an assistant message to the current conversation
    pub fn add_assistant_message(&mut self, content: String) -> Result<()> {
        if let Some(ref mut conv) = self.current_conversation {
            conv.add_assistant_message(content);

            if self.config.auto_save {
                self.save()?;
            }
        }
        Ok(())
    }

    /// Add a tool result to the current conversation
    pub fn add_tool_result(&mut self, tool_name: String, result: String) -> Result<()> {
        if let Some(ref mut conv) = self.current_conversation {
            conv.add_tool_result(tool_name, result);

            if self.config.auto_save {
                self.save()?;
            }
        }
        Ok(())
    }

    /// Update token statistics for the current conversation
    pub fn update_tokens(&mut self, tokens: u64, cost: f64) -> Result<()> {
        if let Some(ref mut conv) = self.current_conversation {
            conv.update_tokens(tokens, cost);

            if self.config.auto_save {
                self.save()?;
            }
        }
        Ok(())
    }

    /// Rename the current conversation
    pub fn rename(&mut self, name: String) -> Result<()> {
        if let Some(ref mut conv) = self.current_conversation {
            conv.rename(name);

            if self.config.auto_save {
                self.save()?;
            }
        }
        Ok(())
    }

    /// Load a conversation and set it as current
    pub fn load_and_set_current(&mut self, id: Uuid) -> Result<()> {
        let conv = self.load(id)?;
        self.current_conversation = Some(conv);
        Ok(())
    }

    /// Export conversation to JSON string
    pub fn export_json(&self, conversation: &Conversation) -> Result<String> {
        Ok(serde_json::to_string_pretty(conversation)?)
    }

    /// Export conversation to Markdown string
    pub fn export_markdown(&self, conversation: &Conversation) -> Result<String> {
        let mut md = format!("# {}\n\n", conversation.name);
        md.push_str(&format!("**Created**: {}\n", conversation.created_at));
        md.push_str(&format!("**Model**: {}\n", conversation.model));
        md.push_str(&format!(
            "**Messages**: {}\n\n",
            conversation.metadata.message_count
        ));

        if conversation.metadata.total_tokens > 0 {
            md.push_str(&format!("**Total Tokens**: {}\n", conversation.metadata.total_tokens));
        }
        if conversation.metadata.total_cost > 0.0 {
            md.push_str(&format!("**Total Cost**: ${:.4}\n", conversation.metadata.total_cost));
        }

        md.push_str("\n---\n\n");

        for msg in &conversation.messages {
            match msg {
                Message::User { content, timestamp } => {
                    md.push_str(&format!(
                        "## User ({})\n\n{}\n\n",
                        timestamp.format("%Y-%m-%d %H:%M"),
                        content
                    ));
                }
                Message::Assistant { content, timestamp } => {
                    md.push_str(&format!(
                        "## Assistant ({})\n\n{}\n\n",
                        timestamp.format("%Y-%m-%d %H:%M"),
                        content
                    ));
                }
                Message::ToolResult {
                    tool_name,
                    result,
                    timestamp,
                } => {
                    md.push_str(&format!(
                        "### Tool: {} ({})\n\n```\n{}\n```\n\n",
                        tool_name,
                        timestamp.format("%Y-%m-%d %H:%M"),
                        result
                    ));
                }
            }
        }

        Ok(md)
    }

    /// Enforce maximum conversations limit
    fn enforce_max_conversations(&self) -> Result<()> {
        if self.config.max_conversations == 0 {
            return Ok(());
        }

        let mut conversations = self.list()?;

        // Delete oldest if we're at or over the limit (before adding new one)
        // This ensures we end up with at most max_conversations after adding the new one
        if conversations.len() >= self.config.max_conversations {
            // Sort by updated_at descending (newest first)
            conversations.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

            // Delete oldest - we want to keep at most max-1 old ones so that after adding
            // the new one, we have at most max. Since sorted newest-first, delete from
            // index (max_conversations - 1) onwards to make room for one more.
            let delete_from = self.config.max_conversations.saturating_sub(1);
            for conv in conversations.iter().skip(delete_from) {
                tracing::debug!("Deleting old conversation {} to enforce limit", conv.id);
                let _ = self.delete(conv.id);
            }
        }

        Ok(())
    }

    /// Get the storage directory path
    pub fn storage_dir(&self) -> &Path {
        &self.config.storage_dir
    }

    /// Check if auto-save is enabled
    pub fn auto_save_enabled(&self) -> bool {
        self.config.auto_save
    }

    /// Check if there's a current conversation
    pub fn has_current(&self) -> bool {
        self.current_conversation.is_some()
    }

    /// Clear the current conversation (for /new command)
    pub fn clear_current(&mut self) {
        self.current_conversation = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn temp_manager() -> (ConversationManager, tempfile::TempDir) {
        let temp_dir = tempdir().unwrap();
        let config = ConversationManagerConfig {
            auto_save: true,
            storage_dir: temp_dir.path().to_path_buf(),
            max_conversations: 5,
            auto_resume: true,
        };
        let manager = ConversationManager::new(config).unwrap();
        (manager, temp_dir)
    }

    #[test]
    fn test_create_new_conversation() {
        let (mut manager, _temp) = temp_manager();

        let conv = manager
            .create_new("Test".to_string(), "claude-3".to_string())
            .unwrap();

        assert_eq!(conv.name, "Test");
        assert_eq!(conv.model, "claude-3");
        assert!(manager.has_current());
    }

    #[test]
    fn test_save_and_load() {
        let (mut manager, _temp) = temp_manager();

        manager
            .create_new("Test".to_string(), "claude-3".to_string())
            .unwrap();
        manager.add_user_message("Hello".to_string()).unwrap();

        let id = manager.get_current().unwrap().id;

        // Load by ID
        let loaded = manager.load(id).unwrap();

        assert_eq!(loaded.name, "Test");
        assert_eq!(loaded.messages.len(), 1);
    }

    #[test]
    fn test_list_conversations() {
        let (mut manager, _temp) = temp_manager();

        manager
            .create_new("Conv1".to_string(), "claude-3".to_string())
            .unwrap();
        manager.save().unwrap();
        manager
            .create_new("Conv2".to_string(), "claude-3".to_string())
            .unwrap();
        manager.save().unwrap();

        let list = manager.list().unwrap();

        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_delete_conversation() {
        let (mut manager, _temp) = temp_manager();

        let conv = manager
            .create_new("Test".to_string(), "claude-3".to_string())
            .unwrap();
        let id = conv.id;

        // Need to save the conversation first (since create_new no longer auto-saves)
        manager.save().unwrap();

        manager.delete(id).unwrap();

        let result = manager.load(id);
        assert!(result.is_err());
    }

    #[test]
    fn test_export_markdown() {
        let (mut manager, _temp) = temp_manager();

        manager
            .create_new("Test".to_string(), "claude-3".to_string())
            .unwrap();
        manager.add_user_message("Hello".to_string()).unwrap();
        manager
            .add_assistant_message("Hi there!".to_string())
            .unwrap();

        let conv = manager.get_current().unwrap();
        let md = manager.export_markdown(conv).unwrap();

        assert!(md.contains("# Test"));
        assert!(md.contains("Hello"));
        assert!(md.contains("Hi there!"));
    }

    #[test]
    fn test_max_conversations_limit() {
        let temp_dir = tempdir().unwrap();
        let config = ConversationManagerConfig {
            auto_save: true,
            storage_dir: temp_dir.path().to_path_buf(),
            max_conversations: 2,
            auto_resume: true,
        };
        let mut manager = ConversationManager::new(config).unwrap();

        manager
            .create_new("Conv1".to_string(), "claude-3".to_string())
            .unwrap();
        manager
            .create_new("Conv2".to_string(), "claude-3".to_string())
            .unwrap();
        manager
            .create_new("Conv3".to_string(), "claude-3".to_string())
            .unwrap();

        let list = manager.list().unwrap();

        // Should have max 2 conversations
        assert!(list.len() <= 2);
    }
}
