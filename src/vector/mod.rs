//! Vector store: a thin, `Send + Sync` wrapper around ruvector's `VectorDB`
//! plus the embeddings client, shared by the `doc_index` / `doc_search` tools.
//!
//! One store is opened at startup and injected into both tools (mirroring
//! `SearchTool::new(config)`). ruvector's `VectorDB` is itself `Send + Sync`
//! and redb is single-writer, so a single shared handle behind `Arc` is the
//! correct shape — two independently-opened handles on the same path would
//! race.
//!
//! ## Lazy materialization
//! Opening the store builds only the embeddings client — it touches no disk.
//! The redb file at `db_path` is created on the **first write** (first
//! `index_file` that actually inserts chunks). Reads (`search`) and the
//! re-index skip-check before that point are pure no-ops: config-enabled is
//! not the same as on-disk. All DB access routes through one lazily-initialised
//! cell so the "nothing on disk until first index" invariant holds everywhere.
//!
//! ## Idempotent re-index
//! Each chunk's id is a stable hash of `(source_path, chunk_index)`, so
//! re-indexing an unchanged file overwrites the same ids (no duplicates). We
//! also store the file's content `sha256` in metadata, letting `doc_index`
//! skip files whose hash is unchanged — making "point it at the folder again"
//! safe and fast. When a file shrinks to fewer chunks, the now-orphaned
//! trailing rows are reaped (`delete_chunks_from`) so a shrunken or emptied
//! file leaves nothing stale behind.

mod chunk;
mod embeddings;
mod parse;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ruvector_core::types::{DbOptions, DistanceMetric, SearchQuery, VectorEntry};
use ruvector_core::vector_db::VectorDB;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::OnceCell;

use crate::config::VectorDbConfig;

pub use embeddings::{EmbeddingsClient, EmbeddingsError};
pub use parse::{ParseError, extension_of, is_supported};

/// Default number of results returned by `doc_search` when `k` is unspecified.
pub const DEFAULT_K: usize = 5;

#[derive(Debug, Error)]
pub enum VectorStoreError {
    #[error("failed to open vector DB at {path}: {source}")]
    Open {
        path: String,
        #[source]
        source: ruvector_core::error::RuvectorError,
    },
    #[error("vector DB operation failed: {0}")]
    Db(#[from] ruvector_core::error::RuvectorError),
    #[error(transparent)]
    Embeddings(#[from] EmbeddingsError),
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error("failed to read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Reserved metadata keys owned by the store. User-supplied metadata that
/// collides with any of these is dropped at the boundary so it can never
/// shadow the fields search relies on.
const RESERVED_METADATA_KEYS: [&str; 4] = ["source", "chunk_index", "text", "sha256"];

/// One indexed chunk hit returned from a search.
pub struct Hit {
    pub source: String,
    pub chunk_index: usize,
    pub text: String,
    pub score: f32,
    /// User-supplied metadata stored at index time (author, book, year, …).
    /// Reserved keys are already stripped out — this is only the caller's data.
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Per-file outcome counters for an index run.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct IndexReport {
    /// Files newly indexed (not previously in the store).
    pub indexed: usize,
    /// Files skipped because their content hash was unchanged.
    pub skipped: usize,
    /// Files re-indexed because their content changed.
    pub updated: usize,
    /// Files skipped because their extension is unsupported.
    pub unsupported: usize,
    /// Total chunks written across all indexed/updated files.
    pub chunks: usize,
}

/// Shared vector store. Cheap to clone (everything is `Arc`-backed). The redb
/// file is created lazily on the first write — see the module-level
/// "Lazy materialization" note.
#[derive(Clone)]
pub struct VectorStore {
    inner: Arc<StoreInner>,
}

struct StoreInner {
    /// The redb-backed DB handle, created on first write. `None` until then.
    db: OnceCell<Arc<VectorDB>>,
    /// Where the DB will be created. Held so materialization is config-free.
    db_path: String,
    embeddings: EmbeddingsClient,
}

impl VectorStore {
    /// Build the store. Touches no disk: only constructs the embeddings client
    /// and records where the DB *will* live. The redb file is created on the
    /// first write (see [`VectorStore::db`]).
    pub fn open(config: &VectorDbConfig) -> Result<Self, VectorStoreError> {
        let embeddings = EmbeddingsClient::new(&config.embeddings);
        Ok(Self {
            inner: Arc::new(StoreInner {
                db: OnceCell::new(),
                db_path: config.db_path.clone(),
                embeddings,
            }),
        })
    }

    /// The DB handle, creating the redb file (and parent dir) on first call.
    /// This is the **only** path that materializes the store — call it solely
    /// when about to write.
    ///
    /// ⚠ On reopen of an existing path, ruvector rebuilds the index from disk
    /// using the STORED dimensions/metric — the `DbOptions` here only take
    /// effect when creating a fresh DB. A model whose output dimension differs
    /// from an existing DB surfaces as a clear error on insert, never as silent
    /// corruption.
    async fn db(&self) -> Result<Arc<VectorDB>, VectorStoreError> {
        let db = self
            .inner
            .db
            .get_or_try_init(|| async {
                let path = self.inner.db_path.clone();
                let dimensions = self.inner.embeddings.dimensions();
                // ruvector's create is sync + does disk IO — keep it off the
                // async runtime.
                tokio::task::spawn_blocking(move || create_db(&path, dimensions))
                    .await
                    .expect("vector db create task panicked")
            })
            .await?;
        Ok(db.clone())
    }

    /// The DB handle iff it has already been materialized. Reads use this so
    /// they never create the file: an un-indexed store is empty by definition.
    fn db_if_materialized(&self) -> Option<Arc<VectorDB>> {
        self.inner.db.get().cloned()
    }

    /// Index a single file: parse → chunk → embed → upsert. Returns the number
    /// of chunks written, or `None` if the file was skipped because its content
    /// hash is unchanged since the last index. Unsupported extensions return a
    /// `ParseError::Unsupported` for the caller to count and continue.
    ///
    /// `doc_index` aggregates these outcomes across a directory walk into an
    /// [`IndexReport`].
    pub async fn index_file(
        &self,
        path: &Path,
        user_metadata: &HashMap<String, serde_json::Value>,
    ) -> Result<IndexOutcome, VectorStoreError> {
        if !parse::is_supported(path) {
            return Err(VectorStoreError::Parse(ParseError::Unsupported(
                parse::extension_of(path),
            )));
        }

        let source = path.display().to_string();
        let text = parse::extract_text(path)?;

        // Strip reserved keys once, here at the boundary; the interior trusts
        // that `clean_metadata` holds only caller-owned fields.
        let clean_metadata = sanitize_metadata(user_metadata);

        // The hash covers content AND metadata, so re-indexing the same text
        // with a new author/book/year is an update, not a silent skip.
        let content_hash = content_hash(&text, &clean_metadata);

        // Skip if this exact content is already indexed (id of chunk 0 carries
        // the file's content hash in metadata). One lookup decides both
        // skip-vs-reindex and whether this is an update.
        let prior_hash = self.stored_hash(&source)?;
        if prior_hash.as_deref() == Some(content_hash.as_str()) {
            return Ok(IndexOutcome::Skipped);
        }
        let is_update = prior_hash.is_some();

        let chunks = chunk::split(&text);
        if chunks.is_empty() {
            // Empty/whitespace-only file: nothing to index. If this file had
            // chunks before, reap them all so the now-empty file leaves nothing
            // behind in the store.
            if is_update {
                self.delete_chunks_from(&source, 0).await?;
                return Ok(IndexOutcome::Updated(0));
            }
            return Ok(IndexOutcome::Indexed(0));
        }

        let vectors = self.inner.embeddings.embed(&chunks).await?;
        let mut entries = Vec::with_capacity(chunks.len());
        for (i, (text, vector)) in chunks.into_iter().zip(vectors).enumerate() {
            // Start from the caller's metadata, then layer the reserved system
            // fields on top — reserved keys always win (they were already
            // stripped from `clean_metadata`, so this can't conflict).
            let mut metadata = clean_metadata.clone();
            metadata.insert("source".into(), serde_json::json!(source));
            metadata.insert("chunk_index".into(), serde_json::json!(i));
            metadata.insert("text".into(), serde_json::json!(text));
            metadata.insert("sha256".into(), serde_json::json!(content_hash));
            entries.push(VectorEntry {
                id: Some(chunk_id(&source, i)),
                vector,
                metadata: Some(metadata),
            });
        }

        let n = entries.len();
        // The first write materializes the store. ruvector's VectorDB is sync;
        // run the insert off the async runtime so the agent loop isn't starved.
        let db = self.db().await?;
        tokio::task::spawn_blocking(move || db.insert_batch(&entries))
            .await
            .expect("vector insert task panicked")?;

        // If the file shrank, chunks 0..n were overwritten in place but any
        // higher-indexed rows from the previous version are now orphans. Reap
        // them. (No-op for a brand-new file, and cheap when nothing shrank.)
        if is_update {
            self.delete_chunks_from(&source, n).await?;
        }

        Ok(if is_update {
            IndexOutcome::Updated(n)
        } else {
            IndexOutcome::Indexed(n)
        })
    }

    /// Embed `query` and return the top `k` most similar chunks. If nothing has
    /// been indexed yet the store isn't materialized — return no hits without
    /// touching the network or disk.
    pub async fn search(&self, query: &str, k: usize) -> Result<Vec<Hit>, VectorStoreError> {
        let Some(db) = self.db_if_materialized() else {
            return Ok(Vec::new());
        };

        let vector = {
            let mut v = self.inner.embeddings.embed(&[query.to_string()]).await?;
            v.pop().unwrap_or_default()
        };

        let results = tokio::task::spawn_blocking(move || {
            db.search(SearchQuery {
                vector,
                k,
                filter: None,
                ef_search: None,
            })
        })
        .await
        .expect("vector search task panicked")?;

        let hits = results
            .into_iter()
            .map(|r| {
                let md = r.metadata.unwrap_or_default();
                Hit {
                    source: string_field(&md, "source"),
                    chunk_index: md.get("chunk_index").and_then(|v| v.as_u64()).unwrap_or(0)
                        as usize,
                    text: string_field(&md, "text"),
                    score: r.score,
                    metadata: strip_reserved(md),
                }
            })
            .collect();
        Ok(hits)
    }

    /// The content hash stored for a file's chunk 0, if the file was indexed
    /// before. Returns `None` if the file is not in the store — including the
    /// case where nothing has been indexed yet (store not materialized), which
    /// must not create the DB file.
    fn stored_hash(&self, source: &str) -> Result<Option<String>, VectorStoreError> {
        let Some(db) = self.db_if_materialized() else {
            return Ok(None);
        };
        let id = chunk_id(source, 0);
        match db.get(&id)? {
            Some(entry) => Ok(entry
                .metadata
                .as_ref()
                .map(|md| string_field(md, "sha256"))
                .filter(|s| !s.is_empty())),
            None => Ok(None),
        }
    }

    /// Delete chunk rows for `source` from index `start` upward, stopping at the
    /// first absent id. Chunks are always written as a contiguous `0..count`
    /// run, so a miss means we've passed the end. Used after a re-index to
    /// reap orphans left when a file shrinks to fewer chunks (or to zero). A
    /// no-op when the store isn't materialized (nothing to delete).
    async fn delete_chunks_from(&self, source: &str, start: usize) -> Result<(), VectorStoreError> {
        let Some(db) = self.db_if_materialized() else {
            return Ok(());
        };
        let source = source.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), VectorStoreError> {
            let mut i = start;
            // `delete` returns false when the id was absent — our stop signal.
            while db.delete(&chunk_id(&source, i))? {
                i += 1;
            }
            Ok(())
        })
        .await
        .expect("vector delete task panicked")
    }
}

/// Create (or reopen) the redb-backed DB at `db_path`, creating the parent
/// directory if missing. This is the single point that touches disk, invoked
/// lazily from [`VectorStore::db`] on the first write.
fn create_db(db_path: &str, dimensions: usize) -> Result<Arc<VectorDB>, VectorStoreError> {
    let path = PathBuf::from(db_path);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|source| VectorStoreError::Io {
            path: parent.display().to_string(),
            source,
        })?;
    }

    let opts = DbOptions {
        dimensions,
        distance_metric: DistanceMetric::Cosine,
        storage_path: db_path.to_string(),
        ..Default::default()
    };
    let db = VectorDB::new(opts).map_err(|source| VectorStoreError::Open {
        path: db_path.to_string(),
        source,
    })?;
    Ok(Arc::new(db))
}

/// Stable, deterministic id for a chunk: `sha256(source + "\0" + index)`.
/// Re-indexing the same file overwrites the same ids (idempotent).
fn chunk_id(source: &str, index: usize) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    hasher.update([0u8]);
    hasher.update(index.to_le_bytes());
    format!("{:x}", hasher.finalize())
}

/// Hash that covers the file text and the (already-sanitized) user metadata.
/// Folding metadata in means changing only an author/book/year — same text —
/// still re-indexes instead of being skipped as unchanged. Keys are sorted so
/// the hash is independent of `HashMap` iteration order.
fn content_hash(text: &str, metadata: &HashMap<String, serde_json::Value>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let mut keys: Vec<&String> = metadata.keys().collect();
    keys.sort();
    for k in keys {
        hasher.update([0u8]);
        hasher.update(k.as_bytes());
        hasher.update([0u8]);
        hasher.update(metadata[k].to_string().as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Drop reserved keys from caller-supplied metadata so it can never shadow the
/// system fields. Applied once at the index boundary.
fn sanitize_metadata(
    md: &HashMap<String, serde_json::Value>,
) -> HashMap<String, serde_json::Value> {
    md.iter()
        .filter(|(k, _)| !RESERVED_METADATA_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Remove the system-owned reserved keys, leaving only the caller's metadata.
/// Used when building a `Hit` so callers see only what they stored.
fn strip_reserved(
    mut md: HashMap<String, serde_json::Value>,
) -> HashMap<String, serde_json::Value> {
    for key in RESERVED_METADATA_KEYS {
        md.remove(key);
    }
    md
}

fn string_field(md: &HashMap<String, serde_json::Value>, key: &str) -> String {
    md.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Result of indexing one file.
#[derive(Debug, PartialEq, Eq)]
pub enum IndexOutcome {
    /// File newly indexed, with the number of chunks written.
    Indexed(usize),
    /// File changed and re-indexed, with the number of chunks written.
    Updated(usize),
    /// File unchanged since last index — nothing written.
    Skipped,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EmbeddingsConfig;

    #[test]
    fn chunk_id_is_stable_and_index_sensitive() {
        assert_eq!(chunk_id("a.txt", 0), chunk_id("a.txt", 0));
        assert_ne!(chunk_id("a.txt", 0), chunk_id("a.txt", 1));
        assert_ne!(chunk_id("a.txt", 0), chunk_id("b.txt", 0));
    }

    #[test]
    fn content_hash_changes_with_text_and_metadata() {
        let empty = HashMap::new();
        assert_ne!(content_hash("hello", &empty), content_hash("world", &empty));
        assert_eq!(content_hash("hello", &empty), content_hash("hello", &empty));

        // Same text, different metadata → different hash (so a metadata-only
        // change still re-indexes instead of being skipped).
        let mut md = HashMap::new();
        md.insert("author".to_string(), serde_json::json!("Tolkien"));
        assert_ne!(content_hash("hello", &empty), content_hash("hello", &md));
    }

    #[test]
    fn content_hash_is_key_order_independent() {
        let mut a = HashMap::new();
        a.insert("author".to_string(), serde_json::json!("A"));
        a.insert("year".to_string(), serde_json::json!("1999"));
        let mut b = HashMap::new();
        b.insert("year".to_string(), serde_json::json!("1999"));
        b.insert("author".to_string(), serde_json::json!("A"));
        assert_eq!(content_hash("t", &a), content_hash("t", &b));
    }

    #[test]
    fn sanitize_metadata_drops_reserved_keys() {
        let mut md = HashMap::new();
        md.insert("author".to_string(), serde_json::json!("A"));
        md.insert("source".to_string(), serde_json::json!("hacker.txt"));
        md.insert("text".to_string(), serde_json::json!("evil"));
        let clean = sanitize_metadata(&md);
        assert_eq!(clean.len(), 1);
        assert!(clean.contains_key("author"));
        assert!(!clean.contains_key("source"));
        assert!(!clean.contains_key("text"));
    }

    #[test]
    fn strip_reserved_leaves_only_user_metadata() {
        let mut md = HashMap::new();
        md.insert("source".to_string(), serde_json::json!("a.txt"));
        md.insert("chunk_index".to_string(), serde_json::json!(0));
        md.insert("text".to_string(), serde_json::json!("body"));
        md.insert("sha256".to_string(), serde_json::json!("deadbeef"));
        md.insert("book".to_string(), serde_json::json!("LOTR"));
        let user = strip_reserved(md);
        assert_eq!(user.len(), 1);
        assert_eq!(user.get("book"), Some(&serde_json::json!("LOTR")));
    }

    /// Re-indexing a file that shrank to fewer chunks must delete the orphaned
    /// trailing chunks, not leave them behind to surface in search. Exercises
    /// the deletion primitive directly with synthetic rows so no embeddings
    /// endpoint is needed.
    #[tokio::test]
    async fn delete_chunks_from_removes_trailing_orphans() {
        use ruvector_core::types::VectorEntry;

        let dir = tempfile::tempdir().unwrap();
        let config = VectorDbConfig {
            enabled: true,
            db_path: dir.path().join("v.db").display().to_string(),
            embeddings: EmbeddingsConfig {
                base_url: "http://unused.invalid".into(),
                api_key: None,
                model: "test".into(),
                dimensions: 3,
            },
        };
        let store = VectorStore::open(&config).unwrap();
        // Materialize the DB so we can seed it directly.
        let db = store.db().await.unwrap();

        // Seed a 3-chunk file: chunks 0, 1, 2.
        let source = "shrinking.txt";
        let entries: Vec<VectorEntry> = (0..3)
            .map(|i| VectorEntry {
                id: Some(chunk_id(source, i)),
                vector: vec![i as f32, 0.0, 0.0],
                metadata: None,
            })
            .collect();
        db.insert_batch(&entries).unwrap();

        // File shrank to 1 chunk → delete everything from index 1 onward.
        store.delete_chunks_from(source, 1).await.unwrap();

        assert!(
            db.get(&chunk_id(source, 0)).unwrap().is_some(),
            "chunk 0 must survive"
        );
        assert!(
            db.get(&chunk_id(source, 1)).unwrap().is_none(),
            "orphan chunk 1 must be deleted"
        );
        assert!(
            db.get(&chunk_id(source, 2)).unwrap().is_none(),
            "orphan chunk 2 must be deleted"
        );
    }

    /// The store must not touch disk until the first *write*. Opening it,
    /// and even searching before anything is indexed, must leave the path
    /// untouched; only the first index materializes the DB file. Exercises the
    /// invariant directly via the getters so no embeddings endpoint is needed.
    #[tokio::test]
    async fn open_does_not_create_db_until_first_write() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("sub").join("vectors.db");
        let config = VectorDbConfig {
            enabled: true,
            db_path: db_path.display().to_string(),
            embeddings: EmbeddingsConfig {
                base_url: "http://unused.invalid".into(),
                api_key: None,
                model: "test".into(),
                dimensions: 3,
            },
        };

        let store = VectorStore::open(&config).unwrap();
        assert!(!db_path.exists(), "open() must not create the DB file");

        // A read (search) before anything is indexed is a pure no-op: empty
        // results, no network embed call, no DB file.
        let hits = store.search("anything", 3).await.unwrap();
        assert!(hits.is_empty(), "search on an empty store returns no hits");
        assert!(
            !db_path.exists(),
            "search before any index must not create the DB"
        );

        // The first write materializes the store.
        let _db = store.db().await.unwrap();
        assert!(db_path.exists(), "first write must create the DB file");
    }

    #[test]
    fn string_field_handles_missing_and_wrong_type() {
        let mut md: HashMap<String, serde_json::Value> = HashMap::new();
        md.insert("source".into(), serde_json::json!("x.txt"));
        md.insert("chunk_index".into(), serde_json::json!(3));
        assert_eq!(string_field(&md, "source"), "x.txt");
        assert_eq!(string_field(&md, "missing"), "");
        // Non-string value → empty, not a panic.
        assert_eq!(string_field(&md, "chunk_index"), "");
    }
}
