//! File-based storage for conversation persistence.
//!
//! Uses atomic writes (temp file + rename) for crash safety.

use super::ConversationStorage;
use crate::conversation::{Conversation, ConversationSummary};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use uuid::Uuid;

/// Filename of the derived summary index (a cache — the per-conversation
/// JSON files remain authoritative). Lives inside `storage_dir`, so it is
/// skipped by `list`'s scan (it is not a `<uuid>.json`).
const INDEX_FILE: &str = "index.json";

/// On-disk shape of `index.json`. Fingerprint and entries live in ONE
/// document so a crash mid-write can never pair a fresh fingerprint with a
/// stale entry map — a failed parse simply folds into a rebuild.
#[derive(Debug, Serialize, Deserialize)]
struct ConversationIndex {
    /// sha256 hex of the sorted conversation-file names. Names only, never
    /// content or mtime: in-band updates arrive through
    /// `index_upsert`/`index_remove` under `index_lock`, and hashing content
    /// would make every save by one process force a full rescan in every
    /// other process sharing the store.
    fingerprint: String,
    entries: HashMap<Uuid, ConversationSummary>,
}

/// File-based storage backend for conversations.
///
/// Uses atomic writes (write to temp file, then rename) for crash safety.
///
/// `list()` is served from a derived `index.json` cache of summaries so a
/// large history (hundreds of MB across thousands of files) lists in one
/// small read instead of deserializing every conversation. The index is a
/// cache, never the source of truth: it records a fingerprint of the
/// conversation-file name set and is rebuilt from a full scan whenever that
/// set changes or the index fails to parse. `index_lock` serialises index
/// read-modify-write because a single `FileStorage` is shared via `Arc`
/// across web sessions.
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

    /// True for the `<uuid>.json` files the index mirrors (skips dotfiles,
    /// temp files, and the index itself).
    fn is_conversation_file(name: &str) -> bool {
        !name.starts_with('.') && name.ends_with(".json") && name != INDEX_FILE
    }

    /// sha256 hex of the sorted conversation-file names (cheap `readdir`, no
    /// parsing). The index is trusted while this matches the fingerprint it
    /// recorded; corrupt or duplicate-id files are part of the set, so the
    /// signal converges where a file COUNT never could.
    fn dir_fingerprint(&self) -> String {
        let mut names: Vec<String> = match fs::read_dir(&self.storage_dir) {
            Ok(entries) => entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| Self::is_conversation_file(n))
                .collect(),
            Err(_) => Vec::new(),
        };
        names.sort();
        let mut h = sha2::Sha256::new();
        for n in &names {
            h.update(n.as_bytes());
            h.update(b"\n");
        }
        format!("{:x}", h.finalize())
    }

    /// Read the summary index from disk. Returns `None` if it is absent or
    /// unreadable — the caller then rebuilds from a full scan.
    ///
    /// This also covers schema changes: a bare pre-fingerprint map or a
    /// `ConversationSummary` field added without `#[serde(default)]` (e.g.
    /// `cwd`) fails to parse here, folds into `None`, and triggers the
    /// rebuild — no separate migration code needed.
    fn read_index(&self) -> Option<ConversationIndex> {
        let content = fs::read_to_string(self.index_path()).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Atomically write the summary index (temp file + rename), mirroring the
    /// crash-safe write used for conversations.
    fn write_index(&self, index: &ConversationIndex) -> Result<()> {
        let temp_path = self.storage_dir.join(".index.tmp.json");
        // Stream: 256 KiB fixed buffer recycles forever; grows in ~256 KiB
        // chunks instead of one ~703 KiB String that exceeds glibc's
        // 32 MiB mmap threshold on every event-driven persist.
        let f = File::create(&temp_path)?;
        let mut writer = BufWriter::with_capacity(256 * 1024, f);
        serde_json::to_writer(&mut writer, index)?;
        // BufWriter::Drop swallows I/O errors — recover explicitly so a truncated
        // write never gets renamed into place.
        let f = writer
            .into_inner()
            .map_err(|e| anyhow::anyhow!("failed to flush index BufWriter: {e}"))?;
        f.sync_all()?;
        drop(f);
        fs::rename(&temp_path, self.index_path())?;
        Ok(())
    }

    /// Rebuild the index by fully scanning every conversation file. This is
    /// the slow path (the pre-index behaviour) — it runs only when the index
    /// is missing, corrupt, or the directory's file set no longer matches
    /// the recorded fingerprint, then persists a fresh index so the next
    /// `list` is fast again.
    fn rebuild_index(&self) -> Result<HashMap<Uuid, ConversationSummary>> {
        let mut entries = HashMap::new();
        let mut names = Vec::new();

        let dir_entries =
            fs::read_dir(&self.storage_dir).context("Failed to read storage directory")?;

        for entry in dir_entries.flatten() {
            let path = entry.path();

            if let Some(name) = path.file_name() {
                let name_str = name.to_string_lossy();
                if !Self::is_conversation_file(&name_str) {
                    continue;
                }
                names.push(name_str.into_owned());
            } else {
                continue;
            }

            match self.load_from_path(&path) {
                Ok(conv) => {
                    entries.insert(conv.id, ConversationSummary::from(&conv));
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

        // The fingerprint describes the set as scanned, including the files
        // that failed to load — otherwise they would re-trigger a rebuild on
        // every single list().
        names.sort();
        let mut h = sha2::Sha256::new();
        for n in &names {
            h.update(n.as_bytes());
            h.update(b"\n");
        }
        let index = ConversationIndex {
            fingerprint: format!("{:x}", h.finalize()),
            entries,
        };

        // Best-effort persist; a write failure just means the next list
        // rebuilds again (correct, only slow).
        if let Err(e) = self.write_index(&index) {
            tracing::warn!("Failed to write conversation index: {}", e);
        }

        Ok(index.entries)
    }

    /// Upsert a single summary into the index and persist it. If the index is
    /// missing, seed a fresh cache from this conversation rather than
    /// re-scanning disk — with an empty fingerprint that matches no real
    /// directory, so the next `list()` still rebuilds and heals the entries
    /// the seed skipped.
    fn index_upsert(&self, conversation: &Conversation) -> Result<()> {
        let _guard = self.index_lock.lock().unwrap();
        let mut index = match self.read_index() {
            Some(index) => index,
            None => {
                // Seed a fresh index from the in-memory conversation instead
                // of falling back to rebuild_index: a rebuild would re-read
                // the just-written (potentially many-MiB) conversation file
                // back from disk via read_to_string, blowing save's peak
                // alloc. The empty fingerprint can never match a real
                // directory hash, so list() detects the missing entries and
                // rebuilds lazily.
                let seeded = ConversationIndex {
                    fingerprint: String::new(),
                    entries: HashMap::from([(
                        conversation.id,
                        ConversationSummary::from(conversation),
                    )]),
                };
                return self.write_index(&seeded);
            }
        };
        index
            .entries
            .insert(conversation.id, ConversationSummary::from(conversation));
        index.fingerprint = self.dir_fingerprint();
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
        index.entries.remove(&id);
        index.fingerprint = self.dir_fingerprint();
        self.write_index(&index)
    }
}

impl ConversationStorage for FileStorage {
    fn save(&self, conversation: &Conversation) -> Result<()> {
        let temp_path = self
            .storage_dir
            .join(format!(".tmp.{}.json", conversation.id));
        let final_path = self.conversation_path(conversation.id);

        // Stream: 256 KiB fixed buffer recycles forever; grows in ~256 KiB
        // chunks instead of one ~64 MiB String that exceeds glibc's 32 MiB
        // mmap threshold and forces a fresh non-main-arena heap per persist.
        let f = File::create(&temp_path)?;
        let mut writer = BufWriter::with_capacity(256 * 1024, f);
        serde_json::to_writer_pretty(&mut writer, conversation)?;
        // BufWriter::Drop swallows I/O errors — recover explicitly so a
        // truncated write never gets renamed into place as a "good" file.
        let f = writer
            .into_inner()
            .map_err(|e| anyhow::anyhow!("failed to flush conversation BufWriter: {e}"))?;
        f.sync_all()?;
        drop(f);

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
        // the directory's file set no longer matches the recorded
        // fingerprint (files copied in/out). Content-only changes never
        // reach this check: they arrive via save/delete, which update the
        // index and fingerprint under the same lock.
        let _guard = self.index_lock.lock().unwrap();
        let fingerprint = self.dir_fingerprint();
        let index = match self.read_index() {
            Some(index) if index.fingerprint == fingerprint => index.entries,
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
        Conversation::new(
            name.to_string(),
            "test".to_string(),
            "model".to_string(),
            String::new(),
        )
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

    // ── recent_dirs: stale (pre-cwd) index forces a rebuild ────────────────
    //
    // `ConversationSummary` gains a REQUIRED `cwd: String` field (no
    // `#[serde(default)]` — deliberate). This test hand-writes an index.json
    // shaped like the CURRENT (pre-feature) on-disk format — entries with no
    // `cwd` key — next to a real saved conversation, then asserts `list()`
    // detects the index no longer parses as `HashMap<Uuid,
    // ConversationSummary>`, rebuilds from the conversation files, and
    // persists a fresh index carrying `cwd`.
    //
    // Until `ConversationSummary::cwd` exists, `summary.cwd` below fails to
    // COMPILE — the expected RED for this test today.
    #[test]
    fn list_rebuilds_when_index_predates_cwd() {
        let dir = TempDir::new().unwrap();
        let store = FileStorage::new(dir.path().to_path_buf()).unwrap();

        let known_cwd = "/pre/cwd/index/rebuild/target";
        let mut c = conv("has-a-cwd");
        c.cwd = known_cwd.to_string();
        store.save(&c).unwrap();

        // Build a v1-shaped index entry: take the real current summary
        // serialization and strip the `cwd` key, mirroring exactly what a
        // pre-cwd on-disk `index.json` looked like.
        let summary = ConversationSummary::from(&c);
        let mut entry = serde_json::to_value(&summary).unwrap();
        entry.as_object_mut().unwrap().remove("cwd");

        let mut legacy_index = serde_json::Map::new();
        legacy_index.insert(c.id.to_string(), entry);
        let legacy_json = serde_json::Value::Object(legacy_index).to_string();
        fs::write(store.index_path(), &legacy_json).unwrap();

        // Sanity: the hand-written file really does lack `cwd` (guards
        // against the fixture silently drifting and this test going green
        // for the wrong reason).
        assert!(
            !legacy_json.contains("\"cwd\""),
            "fixture must be pre-cwd shaped: {legacy_json}"
        );

        let summaries = store.list().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(
            summaries[0].cwd, known_cwd,
            "a stale pre-cwd index must be rejected and rebuilt from the \
             conversation files, restoring cwd"
        );

        // The rebuilt index persisted to disk must now carry the cwd key.
        let on_disk = fs::read_to_string(store.index_path()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&on_disk).unwrap();
        let rewritten_entry = &parsed["entries"][c.id.to_string()];
        assert_eq!(
            rewritten_entry.get("cwd").and_then(|v| v.as_str()),
            Some(known_cwd),
            "rebuilt index.json must persist cwd; got: {on_disk}"
        );
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

    // ── index fingerprint: rebuild only when the file SET changes ─────────

    /// A corrupt conversation file makes the entry count permanently smaller
    /// than the file count. Counting files as the staleness signal therefore
    /// rebuilds on EVERY list(); a fingerprint of the file-name set converges
    /// after one rebuild.
    #[test]
    fn list_trusts_index_despite_unindexable_files() {
        let dir = TempDir::new().unwrap();
        let store = FileStorage::new(dir.path().to_path_buf()).unwrap();

        let good = conv("good");
        store.save(&good).unwrap();

        // Unparseable file dropped in out-of-band: every rebuild skips it,
        // so entries (1) < files (2) — the drift never heals by itself.
        fs::write(dir.path().join("deadbeef-corrupt.json"), "{ not valid").unwrap();

        let summaries = store.list().unwrap();
        assert_eq!(
            summaries.len(),
            1,
            "first list rebuilds once, skipping the corrupt file"
        );

        // Tamper with the cached index WITHOUT touching the dir: drop the
        // good entry. A rebuild would resurrect it; a convergent cache
        // serves what the index says.
        let raw = fs::read_to_string(store.index_path()).unwrap();
        let mut parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        parsed["entries"]
            .as_object_mut()
            .expect("index.json must be a fingerprint envelope")
            .remove(&good.id.to_string());
        fs::write(store.index_path(), parsed.to_string()).unwrap();

        let summaries = store.list().unwrap();
        assert!(
            summaries.is_empty(),
            "unchanged file set must be served from the index, not rebuilt"
        );
    }

    /// The fingerprint tracks the file-name set: out-of-band adds/removes
    /// change it and force exactly one rebuild.
    #[test]
    fn list_rebuilds_only_when_file_set_changes() {
        let dir = TempDir::new().unwrap();
        let store = FileStorage::new(dir.path().to_path_buf()).unwrap();

        let a = conv("a");
        store.save(&a).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);

        let b = conv("b");
        let json = serde_json::to_string_pretty(&b).unwrap();
        fs::write(dir.path().join(format!("{}.json", b.id)), json).unwrap();
        let ids: Vec<Uuid> = store.list().unwrap().iter().map(|s| s.id).collect();
        assert!(ids.contains(&b.id), "added file must appear after rebuild");

        fs::remove_file(dir.path().join(format!("{}.json", b.id))).unwrap();
        let ids: Vec<Uuid> = store.list().unwrap().iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![a.id], "removed file must drop out after rebuild");
    }

    /// When the index is unreadable, `index_upsert` seeds a single-entry
    /// cache from memory (avoiding a re-read of the just-written file). That
    /// seed must NOT be fingerprint-trusted, or the entries it skipped would
    /// stay hidden forever.
    #[test]
    fn seeded_index_from_upsert_is_not_trusted() {
        let dir = TempDir::new().unwrap();
        let store = FileStorage::new(dir.path().to_path_buf()).unwrap();

        let a = conv("a");
        store.save(&a).unwrap();
        fs::write(store.index_path(), "{ not valid json").unwrap();

        let b = conv("b");
        store.save(&b).unwrap();

        let ids: Vec<Uuid> = store.list().unwrap().iter().map(|s| s.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(
            ids.contains(&a.id) && ids.contains(&b.id),
            "a seeded index must heal on the next list, not look fresh"
        );
    }
}
