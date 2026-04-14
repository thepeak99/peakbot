//! Conversation Manager - handles CRUD operations for persisted conversations.

use crate::conversation::{Conversation, ConversationSummary, Message};
use crate::storage::ConversationStorage;
use anyhow::Result;
use uuid::Uuid;

/// Configuration for conversation persistence (storage-agnostic)
#[derive(Debug, Clone)]
pub struct ConversationManagerConfig {
    /// Enable auto-save (default: true)
    pub auto_save: bool,
    /// Maximum number of conversations to keep (0 = unlimited)
    pub max_conversations: usize,
}

impl Default for ConversationManagerConfig {
    fn default() -> Self {
        Self {
            auto_save: true,
            max_conversations: 50,
        }
    }
}

/// Manager for conversation persistence (generic over storage backend)
pub struct ConversationManager<S: ConversationStorage> {
    config: ConversationManagerConfig,
    current_conversation: Option<Conversation>,
    storage: S,
}

impl<S: ConversationStorage> ConversationManager<S> {
    /// Create a new ConversationManager with the given storage backend
    pub fn new(storage: S, config: ConversationManagerConfig) -> Result<Self> {
        Ok(Self {
            config,
            current_conversation: None,
            storage,
        })
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

    /// Save the current conversation using the storage backend
    pub fn save(&self) -> Result<()> {
        if let Some(ref conv) = self.current_conversation {
            self.storage.save(conv)?;
        }
        Ok(())
    }

    /// Load a conversation by ID using the storage backend
    pub fn load(&self, id: Uuid) -> Result<Conversation> {
        self.storage.load(id)
    }

    /// Get the latest (most recently updated) conversation
    pub fn get_latest(&self) -> Result<Option<Conversation>> {
        let summaries = self.list()?;

        if summaries.is_empty() {
            return Ok(None);
        }

        // Find the most recently updated
        let latest = summaries.iter().max_by_key(|s| s.updated_at).unwrap();

        Ok(Some(self.storage.load(latest.id)?))
    }

    /// List all conversations (returns summaries)
    pub fn list(&self) -> Result<Vec<ConversationSummary>> {
        self.storage.list()
    }

    /// Delete a conversation by ID using the storage backend
    pub fn delete(&self, id: Uuid) -> Result<()> {
        self.storage.delete(id)
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

    /// Add a tool call to the current conversation (without auto-save)
    pub fn add_tool_call(
        &mut self,
        tool_name: String,
        arguments: String,
        call_id: Option<String>,
    ) -> Result<()> {
        if let Some(ref mut conv) = self.current_conversation {
            conv.add_tool_call(tool_name, arguments, call_id);
            // Auto-save is handled by the caller (ConversationHandler) to avoid nested locks
        }
        Ok(())
    }

    /// Add a tool result to the current conversation (without auto-save)
    pub fn add_tool_result(
        &mut self,
        tool_name: String,
        arguments: String,
        result: String,
        call_id: Option<String>,
    ) -> Result<()> {
        if let Some(ref mut conv) = self.current_conversation {
            conv.add_tool_result(tool_name, arguments, result, call_id);
        }
        Ok(())
    }

    /// Rename the current conversation (without auto-save)
    pub fn rename(&mut self, name: String) -> Result<()> {
        if let Some(ref mut conv) = self.current_conversation {
            conv.rename(name);
            // Auto-save is handled by the caller (ConversationHandler) to avoid nested locks
        }
        Ok(())
    }

    /// Load a conversation and set it as current
    pub fn load_and_set_current(&mut self, id: Uuid) -> Result<()> {
        let conv = self.storage.load(id)?;
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
                Message::ToolCall {
                    tool_name,
                    arguments,
                    timestamp,
                    ..
                } => {
                    md.push_str(&format!(
                        "### Tool Call: {} ({})\n\n```json\n{}\n```\n\n",
                        tool_name,
                        timestamp.format("%Y-%m-%d %H:%M"),
                        arguments
                    ));
                }
                Message::ToolResult {
                    tool_name,
                    arguments,
                    result,
                    timestamp,
                    ..
                } => {
                    md.push_str(&format!(
                        "### Tool Result: {} ({})\n\n**Arguments:**\n```json\n{}\n```\n\n**Output:**\n```\n{}\n```\n\n",
                        tool_name,
                        timestamp.format("%Y-%m-%d %H:%M"),
                        arguments,
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
                let _ = self.storage.delete(conv.id);
            }
        }

        Ok(())
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
