//! File-based storage for conversation persistence.
//!
//! Uses atomic writes (temp file + rename) for crash safety.

use super::ConversationStorage;
use crate::conversation::{Conversation, ConversationSummary};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use uuid::Uuid;

/// Filename of the derived summary index (a cache — the per-conversation
/// JSON files remain authoritative). Lives inside `storage_dir`, so it is
/// skipped by `list`'s scan (it is not a `<uuid>.json`).
const INDEX_FILE: &str = "index.json";

/// File-based storage backend for conversations.
///
/// Uses atomic writes (write to temp file, then rename) for crash safety.
///
/// `list()` is served from a derived `index.json` cache of summaries so a
/// large history (hundreds of MB across thousands of files) lists in one
/// small read instead of deserializing every conversation. The index is a
/// cache, never the source of truth: a missing, corrupt, or stale index is
/// rebuilt from a full scan. `index_lock` serialises index read-modify-write
/// because a single `FileStorage` is shared via `Arc` across web sessions.
pub struct FileStorage {
    storage_dir: PathBuf,
    index_lock: Mutex<()>,
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

        Ok(Self {
            storage_dir,
            index_lock: Mutex::new(()),
        })
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

    /// Path of the derived summary index.
    fn index_path(&self) -> PathBuf {
        self.storage_dir.join(INDEX_FILE)
    }

    /// Count the `<uuid>.json` conversation files on disk (cheap `readdir`,
    /// no parsing). Used as a staleness signal against the index size.
    fn count_conversation_files(&self) -> usize {
        let Ok(entries) = fs::read_dir(&self.storage_dir) else {
            return 0;
        };
        entries
            .flatten()
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                !name.starts_with('.') && name.ends_with(".json") && name != INDEX_FILE
            })
            .count()
    }

    /// Read the summary index from disk. Returns `None` if it is absent or
    /// unreadable — the caller then rebuilds from a full scan.
    fn read_index(&self) -> Option<HashMap<Uuid, ConversationSummary>> {
        let content = fs::read_to_string(self.index_path()).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Atomically write the summary index (temp file + rename), mirroring the
    /// crash-safe write used for conversations.
    fn write_index(&self, index: &HashMap<Uuid, ConversationSummary>) -> Result<()> {
        let temp_path = self.storage_dir.join(".index.tmp.json");
        let json = serde_json::to_string(index)?;
        fs::write(&temp_path, json)?;
        fs::rename(&temp_path, self.index_path())?;
        Ok(())
    }

    /// Rebuild the index by fully scanning every conversation file. This is
    /// the slow path (the pre-index behaviour) — it runs only when the index
    /// is missing, corrupt, or stale, then persists a fresh index so the next
    /// `list` is fast again.
    fn rebuild_index(&self) -> Result<HashMap<Uuid, ConversationSummary>> {
        let mut index = HashMap::new();

        let entries =
            fs::read_dir(&self.storage_dir).context("Failed to read storage directory")?;

        for entry in entries.flatten() {
            let path = entry.path();

            if let Some(name) = path.file_name() {
                let name_str = name.to_string_lossy();
                if name_str.starts_with('.')
                    || !name_str.ends_with(".json")
                    || name_str == INDEX_FILE
                {
                    continue;
                }
            } else {
                continue;
            }

            match self.load_from_path(&path) {
                Ok(conv) => {
                    index.insert(conv.id, ConversationSummary::from(&conv));
                }
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

        // Best-effort persist; a write failure just means the next list
        // rebuilds again (correct, only slow).
        if let Err(e) = self.write_index(&index) {
            tracing::warn!("Failed to write conversation index: {}", e);
        }

        Ok(index)
    }

    /// Upsert a single summary into the index and persist it. Falls back to a
    /// full rebuild if the index is missing or corrupt.
    fn index_upsert(&self, conversation: &Conversation) -> Result<()> {
        let _guard = self.index_lock.lock().unwrap();
        let mut index = match self.read_index() {
            Some(index) => index,
            None => return self.rebuild_index().map(|_| ()),
        };
        index.insert(conversation.id, ConversationSummary::from(conversation));
        self.write_index(&index)
    }

    /// Remove a single summary from the index and persist it. Falls back to a
    /// full rebuild if the index is missing or corrupt.
    fn index_remove(&self, id: Uuid) -> Result<()> {
        let _guard = self.index_lock.lock().unwrap();
        let mut index = match self.read_index() {
            Some(index) => index,
            None => return self.rebuild_index().map(|_| ()),
        };
        index.remove(&id);
        self.write_index(&index)
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

        // Keep the derived index in sync (best-effort: a failure just means
        // the next `list` detects the drift and rebuilds).
        if let Err(e) = self.index_upsert(conversation) {
            tracing::warn!("Failed to update conversation index on save: {}", e);
        }

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
        if !self.storage_dir.exists() {
            return Ok(Vec::new());
        }

        // Serve from the index; rebuild only when it is missing, corrupt, or
        // its entry count no longer matches the files on disk (files copied
        // in/out, or a save/delete that failed to update the index). The
        // count check is a cheap `readdir`, not a parse.
        let _guard = self.index_lock.lock().unwrap();
        let index = match self.read_index() {
            Some(index) if index.len() == self.count_conversation_files() => index,
            _ => self.rebuild_index()?,
        };

        let mut summaries: Vec<ConversationSummary> = index.into_values().collect();

        // Sort by updated_at descending (most recent first)
        summaries.sort_by_key(|b| std::cmp::Reverse(b.updated_at));

        Ok(summaries)
    }

    fn delete(&self, id: Uuid) -> Result<()> {
        let path = self.conversation_path(id);

        if !path.exists() {
            anyhow::bail!("Conversation not found: {}", id);
        }

        fs::remove_file(&path)?;

        tracing::debug!("Deleted conversation {}", id);

        // Keep the derived index in sync (best-effort — see `save`).
        if let Err(e) = self.index_remove(id) {
            tracing::warn!("Failed to update conversation index on delete: {}", e);
        }

        Ok(())
    }

    fn exists(&self, id: Uuid) -> bool {
        self.conversation_path(id).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn conv(name: &str) -> Conversation {
        Conversation::new(name.to_string(), "test".to_string(), "model".to_string())
    }

    #[test]
    fn list_reflects_saves_and_deletes_via_index() {
        let dir = TempDir::new().unwrap();
        let store = FileStorage::new(dir.path().to_path_buf()).unwrap();

        let a = conv("a");
        let b = conv("b");
        store.save(&a).unwrap();
        store.save(&b).unwrap();

        let ids: Vec<Uuid> = store.list().unwrap().iter().map(|s| s.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a.id) && ids.contains(&b.id));
        assert!(store.index_path().exists(), "index file should be written");

        store.delete(a.id).unwrap();
        let ids: Vec<Uuid> = store.list().unwrap().iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![b.id]);
    }

    #[test]
    fn list_rebuilds_when_index_missing() {
        let dir = TempDir::new().unwrap();
        let store = FileStorage::new(dir.path().to_path_buf()).unwrap();

        let c = conv("c");
        store.save(&c).unwrap();

        // Simulate a fresh upgrade / lost index: delete it, list must rebuild.
        fs::remove_file(store.index_path()).unwrap();
        let summaries = store.list().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, c.id);
        assert!(store.index_path().exists(), "list should rewrite the index");
    }

    #[test]
    fn list_rebuilds_when_index_stale_from_external_file() {
        let dir = TempDir::new().unwrap();
        let store = FileStorage::new(dir.path().to_path_buf()).unwrap();

        let a = conv("a");
        store.save(&a).unwrap();

        // A conversation file dropped in out-of-band (e.g. copied from a
        // backup) — the count no longer matches the index, forcing a rebuild.
        let b = conv("b");
        let json = serde_json::to_string_pretty(&b).unwrap();
        fs::write(dir.path().join(format!("{}.json", b.id)), json).unwrap();

        let ids: Vec<Uuid> = store.list().unwrap().iter().map(|s| s.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a.id) && ids.contains(&b.id));
    }

    #[test]
    fn list_recovers_from_corrupt_index() {
        let dir = TempDir::new().unwrap();
        let store = FileStorage::new(dir.path().to_path_buf()).unwrap();

        let c = conv("c");
        store.save(&c).unwrap();
        fs::write(store.index_path(), "{ not valid json").unwrap();

        let summaries = store.list().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, c.id);
    }
}
