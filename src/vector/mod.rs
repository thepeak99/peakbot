//! Vector store: a thin, `Send + Sync` wrapper around ruvector's `VectorDB`
//! plus the embeddings client, shared by the `doc_index` / `doc_search` tools.
//!
//! Open redb handles live in the process-wide `DB_HANDLES` registry, keyed
//! by resolved `DbPath`: every `VectorStore` (profile, session, reload
//! rebuild) pointing at the same resolved path shares one handle, because
//! two handles over one redb file would diverge. A `SessionRelative` spec
//! resolves against the session cwd at tool-call time; a `Fixed` spec is
//! fully resolved at parse time. Each DB is created on first write (first
//! `index_file` that inserts chunks). Reads (`search`) and the re-index
//! skip-check are pure no-ops when no DB exists.
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
use std::sync::{Arc, LazyLock, Mutex};

use ruvector_core::types::{DbOptions, DistanceMetric, HnswConfig, SearchQuery, VectorEntry};
use ruvector_core::vector_db::VectorDB;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::OnceCell;

use crate::config::resolve::{DbPathSpec, VectorSpec};

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

/// An absolute, lexically-normalized path to a vector DB file.
/// The ONLY constructor is `VectorStore::db_path_for`, so a relative path
/// can never reach ruvector (where it would silently bind to the process
/// boot cwd — the bug this ticket fixes).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct DbPath(PathBuf);

impl DbPath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn display(&self) -> std::path::Display<'_> {
        self.0.display()
    }
}

/// Shared vector store. Cheap to clone (everything is `Arc`-backed). The redb
/// file is created lazily on the first write — see the module-level
/// "Lazy materialization" note.
#[derive(Clone)]
pub struct VectorStore {
    inner: Arc<StoreInner>,
}

/// A lazily-opened redb handle: the per-path `OnceCell` collapses
/// concurrent first-opens of the same path into one `Arc<VectorDB>`.
type DbHandle = Arc<OnceCell<Arc<VectorDB>>>;

/// Every open redb handle in the process, keyed by resolved path. The
/// `Mutex` guards only the map lookup and is **never** held across an await;
/// the per-path `OnceCell` collapses concurrent first-opens of the *same*
/// path into one handle. Handles are never closed (process lifetime).
static DB_HANDLES: LazyLock<Mutex<HashMap<DbPath, DbHandle>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct StoreInner {
    /// The resolved spec. `db_path_for` turns `spec.db_path` into the
    /// concrete `DbPath` for a session cwd.
    spec: VectorSpec,
    embeddings: EmbeddingsClient,
}

impl VectorStore {
    /// Build the store. Touches no disk: only constructs the embeddings
    /// client. The redb file is created on the first write (see
    /// [`VectorStore::db`]), with the handle shared process-wide via
    /// `DB_HANDLES`.
    pub fn open(spec: &VectorSpec) -> Result<Self, VectorStoreError> {
        let embeddings = EmbeddingsClient::new(&spec.embeddings);
        Ok(Self {
            inner: Arc::new(StoreInner {
                spec: spec.clone(),
                embeddings,
            }),
        })
    }

    /// Test accessor over the process-wide `DB_HANDLES` registry: the
    /// handle for this store's spec resolved against `session_cwd`,
    /// materializing the redb file on first access (the same lazy-open rule
    /// as the production write path). `pub` + hidden so integration tests
    /// can prove handle sharing across rebuilt stores.
    #[doc(hidden)]
    pub async fn handle_for_test(
        &self,
        session_cwd: &Path,
    ) -> Result<Arc<VectorDB>, VectorStoreError> {
        self.db(&self.db_path_for(session_cwd)).await
    }

    /// The DB handle for `db_path`, creating the redb file (and parent dir) on
    /// first call. This is the **only** path that materializes a store — call
    /// it solely when about to write.
    ///
    /// ⚠ On reopen of an existing path, ruvector rebuilds the index from disk
    /// using the STORED dimensions/metric — the `DbOptions` here only take
    /// effect when creating a fresh DB. A model whose output dimension differs
    /// from an existing DB surfaces as a clear error on insert, never as silent
    /// corruption.
    async fn db(&self, db_path: &DbPath) -> Result<Arc<VectorDB>, VectorStoreError> {
        // Take the cell out of the registry and drop the guard *before*
        // awaiting: a std `MutexGuard` must never span an await point.
        // Opening then happens under the per-path `OnceCell`, so the map
        // stays unlocked for other paths while one path is being opened.
        let cell = {
            let mut dbs = DB_HANDLES.lock().expect("vector db map lock poisoned");
            Arc::clone(dbs.entry(db_path.clone()).or_default())
        };

        let db = cell
            .get_or_try_init(|| async {
                let path = db_path.clone();
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

    /// The DB handle iff the file already exists on disk. Reads go through
    /// here so they never create anything: a directory that was never indexed
    /// has no DB, and asking about it must not conjure one. Async because
    /// opening is blocking I/O, same as [`VectorStore::db`].
    async fn db_if_exists(
        &self,
        db_path: &DbPath,
    ) -> Result<Option<Arc<VectorDB>>, VectorStoreError> {
        if !db_path.as_path().exists() {
            return Ok(None);
        }
        self.db(db_path).await.map(Some)
    }

    /// Resolve the spec's `db_path` for a session cwd. `Fixed` was fully
    /// resolved at parse time — it ignores the cwd, even an empty one.
    /// `SessionRelative` keeps today's per-cwd rule. Pure except for one
    /// `canonicalize` on the (existing) base dir. Call once per tool call.
    pub fn db_path_for(&self, session_cwd: &Path) -> DbPath {
        match &self.inner.spec.db_path {
            DbPathSpec::Fixed(p) => DbPath(normalize_lexical(p)),
            DbPathSpec::SessionRelative(p) => {
                let base = if session_cwd.as_os_str().is_empty() {
                    std::env::current_dir().unwrap_or_else(|_| session_cwd.to_path_buf())
                } else {
                    session_cwd.to_path_buf()
                };
                let base = base.canonicalize().unwrap_or(base);
                DbPath(normalize_lexical(&base.join(p)))
            }
        }
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
        db_path: &DbPath,
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
        let prior_hash = self.stored_hash(db_path, &source).await?;
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
                self.delete_chunks_from(db_path, &source, 0).await?;
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
        let db = self.db(db_path).await?;
        tokio::task::spawn_blocking(move || db.insert_batch(&entries))
            .await
            .expect("vector insert task panicked")?;

        // If the file shrank, chunks 0..n were overwritten in place but any
        // higher-indexed rows from the previous version are now orphans. Reap
        // them. (No-op for a brand-new file, and cheap when nothing shrank.)
        if is_update {
            self.delete_chunks_from(db_path, &source, n).await?;
        }

        Ok(if is_update {
            IndexOutcome::Updated(n)
        } else {
            IndexOutcome::Indexed(n)
        })
    }

    /// Embed `query` and return the top `k` most similar chunks. If nothing has
    /// been indexed at `db_path` there is no DB file — return no hits without
    /// touching the network or disk.
    pub async fn search(
        &self,
        db_path: &DbPath,
        query: &str,
        k: usize,
    ) -> Result<Vec<Hit>, VectorStoreError> {
        // Bail before embedding: no DB file means no hits, and asking must not
        // cost a network round-trip (nor create the file).
        let Some(db) = self.db_if_exists(db_path).await? else {
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
    /// case where no DB exists at `db_path` yet, which must not create it.
    async fn stored_hash(
        &self,
        db_path: &DbPath,
        source: &str,
    ) -> Result<Option<String>, VectorStoreError> {
        let Some(db) = self.db_if_exists(db_path).await? else {
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
    /// no-op when there is no DB at `db_path` (nothing to delete).
    async fn delete_chunks_from(
        &self,
        db_path: &DbPath,
        source: &str,
        start: usize,
    ) -> Result<(), VectorStoreError> {
        let Some(db) = self.db_if_exists(db_path).await? else {
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
/// Lexically normalize a path: drop `.` components, pop on `..` (never past
/// root for absolute paths). No filesystem access — the DB file may not exist.
pub(crate) fn normalize_lexical(p: &Path) -> PathBuf {
    let mut components = p.components().peekable();
    let mut result = PathBuf::new();

    // Preserve absolute prefix
    if let Some(first) = components.peek() {
        use std::path::Component;
        if let Component::RootDir = first {
            result.push("/");
            components.next();
        }
    }

    for component in components {
        if component.as_os_str() == "." {
            continue;
        }
        if component.as_os_str() == ".." {
            // Never pop past the root for absolute paths
            if result.components().count() > 1 || !p.is_absolute() {
                result.pop();
            }
        } else {
            result.push(component);
        }
    }

    result
}

/// Capacity hint for the HNSW index: a no-op for 2.4.0's default FlatGraph
/// index, kept bounded for older stored configs.
const HNSW_MAX_ELEMENTS: usize = 100_000;

/// The `DbOptions` every store is created with: cosine over the embedding
/// dimension, HNSW capacity capped at [`HNSW_MAX_ELEMENTS`].
fn db_options(path: &Path, dimensions: usize) -> DbOptions {
    DbOptions {
        dimensions,
        distance_metric: DistanceMetric::Cosine,
        storage_path: path.display().to_string(),
        hnsw_config: Some(HnswConfig {
            max_elements: HNSW_MAX_ELEMENTS,
            ..HnswConfig::default()
        }),
        ..Default::default()
    }
}

/// Create (or reopen) the redb-backed DB at `db_path`, creating the parent
/// directory if missing. This is the single point that touches disk, invoked
/// lazily from [`VectorStore::db`] on the first write. Errors carry the path
/// so a failure names *which* DB broke, now that there can be several.
fn create_db(db_path: &DbPath, dimensions: usize) -> Result<Arc<VectorDB>, VectorStoreError> {
    let path = db_path.as_path();
    // `DbPath` is absolute by construction, so the parent is always a real
    // directory to create — no empty-parent case to guard.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| VectorStoreError::Io {
            path: parent.display().to_string(),
            source,
        })?;
    }

    let opts = db_options(path, dimensions);
    let db = VectorDB::new(opts).map_err(|source| VectorStoreError::Open {
        path: path.display().to_string(),
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

/// Test helper: a `VectorSpec` from a `VectorDbConfig` literal with no
/// profile origin — absolute → `Fixed`, relative → `SessionRelative`.
#[cfg(test)]
pub(crate) fn test_spec(config: &crate::config::VectorDbConfig) -> VectorSpec {
    let db_path = if Path::new(&config.db_path).is_absolute() {
        DbPathSpec::Fixed(PathBuf::from(config.db_path.as_str()))
    } else {
        DbPathSpec::SessionRelative(PathBuf::from(config.db_path.as_str()))
    };
    VectorSpec {
        db_path,
        embeddings: config.embeddings.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EmbeddingsConfig, VectorDbConfig};

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

    // The 10M HNSW default over-reserves ~35 GiB in hnsw_rs 0.3.4 — cap it.
    #[test]
    fn db_options_caps_hnsw_max_elements() {
        let opts = db_options(Path::new("x/vectors.db"), 1024);
        assert_eq!(
            opts.hnsw_config.expect("hnsw index").max_elements,
            HNSW_MAX_ELEMENTS
        );
        assert_eq!(opts.dimensions, 1024);
        assert_eq!(opts.distance_metric, DistanceMetric::Cosine);
        assert_eq!(opts.storage_path, "x/vectors.db");
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
            db_path: dir.path().join("v.db").display().to_string(),
            embeddings: EmbeddingsConfig {
                base_url: "http://unused.invalid".into(),
                api_key: None,
                model: "test".into(),
                dimensions: 3,
            },
        };
        let store = VectorStore::open(&test_spec(&config)).unwrap();
        // Config path is absolute, so the session cwd is irrelevant here.
        let db_path = store.db_path_for(Path::new(""));
        // Materialize the DB so we can seed it directly.
        let db = store.db(&db_path).await.unwrap();

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
        store.delete_chunks_from(&db_path, source, 1).await.unwrap();

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

    /// 4-dimension config for the characterization tests below (the 3-d
    /// `test_config` can't express the 45° vector `cosine_score_is_distance`
    /// needs).
    fn test_config_4d(db_path: impl Into<String>) -> VectorDbConfig {
        VectorDbConfig {
            db_path: db_path.into(),
            embeddings: EmbeddingsConfig {
                base_url: "http://unused.invalid".into(),
                api_key: None,
                model: "test".into(),
                dimensions: 4,
            },
        }
    }

    /// Raw-vector search seam: mirrors `VectorStore::search` minus the
    /// embeddings call, so no network endpoint is needed.
    fn search_all(
        db: &VectorDB,
        query: &[f32],
        k: usize,
    ) -> Vec<ruvector_core::types::SearchResult> {
        db.search(SearchQuery {
            vector: query.to_vec(),
            k,
            filter: None,
            ef_search: None,
        })
        .unwrap()
    }

    /// Sibling of `delete_chunks_from_removes_trailing_orphans`: after the
    /// orphan reaping, a search with an orphan's exact vector must not surface
    /// the deleted ids (the orphan test only checked `get`).
    #[tokio::test]
    async fn reindex_orphans_absent_from_search() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config_4d(dir.path().join("v.db").display().to_string());
        let store = VectorStore::open(&test_spec(&config)).unwrap();
        // Config path is absolute, so the session cwd is irrelevant here.
        let db_path = store.db_path_for(Path::new(""));
        let db = store.db(&db_path).await.unwrap();

        let source = "shrinking.txt";
        let entries = [
            VectorEntry {
                id: Some(chunk_id(source, 0)),
                vector: vec![1.0, 0.0, 0.0, 0.0],
                metadata: None,
            },
            VectorEntry {
                id: Some(chunk_id(source, 1)),
                vector: vec![0.0, 1.0, 0.0, 0.0],
                metadata: None,
            },
            VectorEntry {
                id: Some(chunk_id(source, 2)),
                vector: vec![0.0, 0.0, 1.0, 0.0],
                metadata: None,
            },
        ];
        db.insert_batch(&entries).unwrap();

        // File shrank to 1 chunk → chunks 1 and 2 are reaped.
        store.delete_chunks_from(&db_path, source, 1).await.unwrap();

        // Searching with an orphan's exact vector must not return it.
        let hits = search_all(&db, &[0.0, 1.0, 0.0, 0.0], 4);
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert!(
            !ids.contains(&chunk_id(source, 1).as_str()),
            "orphan chunk 1 must not surface in search: {ids:?}"
        );
        assert!(
            !ids.contains(&chunk_id(source, 2).as_str()),
            "orphan chunk 2 must not surface in search: {ids:?}"
        );

        // The surviving chunk is still found for its own vector.
        let hits = search_all(&db, &[1.0, 0.0, 0.0, 0.0], 4);
        assert_eq!(
            hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
            [chunk_id(source, 0).as_str()]
        );
    }

    /// Dropping every handle and reopening the same path must yield the same
    /// data: `get` returns the stored vector and search finds the rows.
    #[tokio::test]
    async fn reopen_preserves_data() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config_4d(dir.path().join("v.db").display().to_string());

        {
            let store = VectorStore::open(&test_spec(&config)).unwrap();
            let db_path = store.db_path_for(Path::new(""));
            let db = store.db(&db_path).await.unwrap();
            db.insert_batch(&[
                VectorEntry {
                    id: Some(chunk_id("reopen.txt", 0)),
                    vector: vec![1.0, 0.0, 0.0, 0.0],
                    metadata: None,
                },
                VectorEntry {
                    id: Some(chunk_id("reopen.txt", 1)),
                    vector: vec![0.0, 1.0, 0.0, 0.0],
                    metadata: None,
                },
            ])
            .unwrap();
            // `db` and `store` drop here; the registry keeps the handle, but
            // the assertions below only rely on the on-disk data.
        }

        // Fresh store over the same path must see the same data.
        let store = VectorStore::open(&test_spec(&config)).unwrap();
        let db_path = store.db_path_for(Path::new(""));
        let db = store.db(&db_path).await.unwrap();

        let entry = db
            .get(&chunk_id("reopen.txt", 0))
            .unwrap()
            .expect("chunk 0 must survive the reopen");
        assert_eq!(entry.vector, vec![1.0, 0.0, 0.0, 0.0]);

        let hits = search_all(&db, &[1.0, 0.0, 0.0, 0.0], 2);
        assert_eq!(hits.len(), 2, "both chunks must be searchable after reopen");
        assert_eq!(hits[0].id, chunk_id("reopen.txt", 0));
    }

    /// Deleting a row takes it out of `get` (None), makes a second delete
    /// report `false`, and removes it from search results.
    #[tokio::test]
    async fn delete_then_get_none_and_absent_from_search() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config_4d(dir.path().join("v.db").display().to_string());
        let store = VectorStore::open(&test_spec(&config)).unwrap();
        let db_path = store.db_path_for(Path::new(""));
        let db = store.db(&db_path).await.unwrap();

        let id = chunk_id("deleted.txt", 0);
        db.insert_batch(&[VectorEntry {
            id: Some(id.clone()),
            vector: vec![1.0, 0.0, 0.0, 0.0],
            metadata: None,
        }])
        .unwrap();
        assert!(
            db.get(&id).unwrap().is_some(),
            "row must exist before delete"
        );

        assert!(
            db.delete(&id).unwrap(),
            "first delete must report the row existed"
        );
        assert!(
            db.get(&id).unwrap().is_none(),
            "row must be gone after delete"
        );
        assert!(
            !db.delete(&id).unwrap(),
            "deleting an absent row must report false"
        );

        let hits = search_all(&db, &[1.0, 0.0, 0.0, 0.0], 4);
        assert!(
            hits.iter().all(|h| h.id != id),
            "deleted row must not surface in search: {hits:?}"
        );
    }

    /// Re-inserting a deleted id with a different vector replaces the row:
    /// `get` returns the new vector and a search for it ranks the id first.
    #[tokio::test]
    async fn reinsert_deleted_id_returns_new_vector() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config_4d(dir.path().join("v.db").display().to_string());
        let store = VectorStore::open(&test_spec(&config)).unwrap();
        let db_path = store.db_path_for(Path::new(""));
        let db = store.db(&db_path).await.unwrap();

        let id = chunk_id("reinsert.txt", 0);
        let old = vec![1.0, 0.0, 0.0, 0.0];
        let new = vec![0.0, 1.0, 0.0, 0.0];
        db.insert_batch(&[VectorEntry {
            id: Some(id.clone()),
            vector: old.clone(),
            metadata: None,
        }])
        .unwrap();
        assert!(
            db.delete(&id).unwrap(),
            "delete of the existing row must succeed"
        );
        db.insert_batch(&[VectorEntry {
            id: Some(id.clone()),
            vector: new.clone(),
            metadata: None,
        }])
        .unwrap();

        let entry = db.get(&id).unwrap().expect("re-inserted row must exist");
        assert_eq!(
            entry.vector, new,
            "get must return the new vector, not the old one"
        );

        let hits = search_all(&db, &new, 4);
        assert_eq!(
            hits[0].id, id,
            "search for the new vector must rank the id first"
        );
    }

    /// With Cosine, `SearchResult::score` is the cosine *distance*
    /// (1 − similarity): identical vectors score ≈ 0, orthogonal ≈ 1, and
    /// results come back sorted ascending. `k` covers all entries so the
    /// approximate index returns the exact ranking.
    #[tokio::test]
    async fn cosine_score_is_distance() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config_4d(dir.path().join("v.db").display().to_string());
        let store = VectorStore::open(&test_spec(&config)).unwrap();
        let db_path = store.db_path_for(Path::new(""));
        let db = store.db(&db_path).await.unwrap();

        // a = query; b orthogonal to a; c at 45° to a.
        let a = vec![1.0, 0.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0, 0.0];
        let c = vec![1.0, 1.0, 0.0, 0.0];
        db.insert_batch(&[
            VectorEntry {
                id: Some("score-a".into()),
                vector: a.clone(),
                metadata: None,
            },
            VectorEntry {
                id: Some("score-b".into()),
                vector: b.clone(),
                metadata: None,
            },
            VectorEntry {
                id: Some("score-c".into()),
                vector: c.clone(),
                metadata: None,
            },
        ])
        .unwrap();

        let hits = search_all(&db, &a, 3);
        assert_eq!(hits.len(), 3, "k covers all entries, so all must come back");
        assert!(
            hits.windows(2).all(|w| w[0].score <= w[1].score),
            "results must be sorted by ascending score: {hits:?}"
        );

        assert_eq!(hits[0].id, "score-a", "identical vector must rank first");
        assert!(
            hits[0].score.abs() < 1e-4,
            "identical vector scores ≈ 0 distance, got {}",
            hits[0].score
        );
        assert_eq!(hits[1].id, "score-c", "45° vector must rank second");
        let expected_c = 1.0 - std::f32::consts::FRAC_1_SQRT_2;
        assert!(
            (hits[1].score - expected_c).abs() < 1e-3,
            "45° vector scores ≈ 1 − 1/√2, got {}",
            hits[1].score
        );
        assert_eq!(hits[2].id, "score-b", "orthogonal vector must rank last");
        assert!(
            (hits[2].score - 1.0).abs() < 1e-3,
            "orthogonal vector scores ≈ 1, got {}",
            hits[2].score
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
            db_path: db_path.display().to_string(),
            embeddings: EmbeddingsConfig {
                base_url: "http://unused.invalid".into(),
                api_key: None,
                model: "test".into(),
                dimensions: 3,
            },
        };

        let store = VectorStore::open(&test_spec(&config)).unwrap();
        let resolved = store.db_path_for(Path::new(""));
        assert!(!db_path.exists(), "open() must not create the DB file");

        // A read (search) before anything is indexed is a pure no-op: empty
        // results, no network embed call, no DB file.
        let hits = store.search(&resolved, "anything", 3).await.unwrap();
        assert!(hits.is_empty(), "search on an empty store returns no hits");
        assert!(
            !db_path.exists(),
            "search before any index must not create the DB"
        );

        // The first write materializes the store.
        let _db = store.db(&resolved).await.unwrap();
        assert!(db_path.exists(), "first write must create the DB file");
    }

    fn test_config(db_path: impl Into<String>) -> VectorDbConfig {
        VectorDbConfig {
            db_path: db_path.into(),
            embeddings: EmbeddingsConfig {
                base_url: "http://unused.invalid".into(),
                api_key: None,
                model: "test".into(),
                dimensions: 3,
            },
        }
    }

    #[test]
    fn db_path_resolves_relative_against_session_cwd() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let store = VectorStore::open(&test_spec(&test_config("./.peakbot/vectors.db"))).unwrap();
        let first_base = first.path().canonicalize().unwrap();
        let second_base = second.path().canonicalize().unwrap();

        let first_path = store.db_path_for(first.path());
        let second_path = store.db_path_for(second.path());

        assert_eq!(first_path.as_path(), first_base.join(".peakbot/vectors.db"));
        assert_eq!(
            second_path.as_path(),
            second_base.join(".peakbot/vectors.db")
        );
        assert_ne!(first_path, second_path);
        assert!(!first_path.as_path().exists());
        assert!(!second_path.as_path().exists());
    }

    #[test]
    fn absolute_config_db_path_ignores_session_cwd() {
        let db = tempfile::tempdir().unwrap().path().join("vectors.db");
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let store = VectorStore::open(&test_spec(&test_config(db.display().to_string()))).unwrap();

        assert_eq!(
            store.db_path_for(first.path()),
            store.db_path_for(second.path())
        );
        assert_eq!(store.db_path_for(first.path()).as_path(), db);
    }

    #[test]
    fn normalize_lexical_removes_current_directory_components() {
        assert_eq!(normalize_lexical(Path::new("./a/b")), PathBuf::from("a/b"));
    }

    #[test]
    fn normalize_lexical_resolves_parent_components() {
        assert_eq!(normalize_lexical(Path::new("a/../b")), PathBuf::from("b"));
    }

    #[test]
    fn normalize_lexical_preserves_absolute_paths() {
        assert_eq!(normalize_lexical(Path::new("/a/../b")), PathBuf::from("/b"));
    }

    #[tokio::test]
    async fn first_write_lands_under_session_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let store = VectorStore::open(&test_spec(&test_config("./.peakbot/vectors.db"))).unwrap();
        let path = store.db_path_for(dir.path());
        let _db = store.db(&path).await.unwrap();

        assert!(path.as_path().is_absolute());
        assert!(
            path.as_path()
                .starts_with(dir.path().canonicalize().unwrap())
        );
        assert!(dir.path().join(".peakbot/vectors.db").exists());
    }

    #[tokio::test]
    async fn distinct_session_cwds_get_independent_dbs() {
        use ruvector_core::types::VectorEntry;
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let store = VectorStore::open(&test_spec(&test_config("./.peakbot/vectors.db"))).unwrap();
        let path_a = store.db_path_for(a.path());
        let path_b = store.db_path_for(b.path());
        let id = "synthetic-id";
        store
            .db(&path_a)
            .await
            .unwrap()
            .insert_batch(&[VectorEntry {
                id: Some(id.into()),
                vector: vec![1.0, 0.0, 0.0],
                metadata: None,
            }])
            .unwrap();

        assert!(store.db(&path_b).await.unwrap().get(id).unwrap().is_none());
        assert!(path_a.as_path().exists());
        assert!(path_b.as_path().exists());
    }

    #[tokio::test]
    async fn same_session_cwd_shares_one_handle() {
        let dir = tempfile::tempdir().unwrap();
        let store = VectorStore::open(&test_spec(&test_config("./.peakbot/vectors.db"))).unwrap();
        let path = store.db_path_for(dir.path());
        let first = store.db(&path).await.unwrap();
        let second = store.db(&path).await.unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn search_opens_an_existing_db_without_creating_one() {
        let indexed = tempfile::tempdir().unwrap();
        let unindexed = tempfile::tempdir().unwrap();
        let config = test_config("./.peakbot/vectors.db");
        let store = VectorStore::open(&test_spec(&config)).unwrap();
        let indexed_path = store.db_path_for(indexed.path());
        store.db(&indexed_path).await.unwrap();

        let fresh = VectorStore::open(&test_spec(&config)).unwrap();
        assert!(
            fresh
                .db_if_exists(&fresh.db_path_for(indexed.path()))
                .await
                .unwrap()
                .is_some()
        );
        let unindexed_path = fresh.db_path_for(unindexed.path());
        assert!(fresh.db_if_exists(&unindexed_path).await.unwrap().is_none());
        assert!(!unindexed_path.as_path().exists());
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

// ── T2: process-wide vector handle registry (RED — written before the code) ──
//
// Spec §6.4 / plan M1-T2: the per-store `StoreInner.dbs` map moves to a
// process-wide `static DB_HANDLES:
// LazyLock<Mutex<HashMap<DbPath, Arc<OnceCell<Arc<VectorDB>>>>>>`,
// `VectorStore::open` takes a `&VectorSpec`, and `db_path_for` honours
// `DbPathSpec::Fixed` (ignore cwd) vs `SessionRelative` (today's per-cwd
// rule).
//
// API the tests assume (exact signatures):
//
// ```rust
// use std::path::{Path, PathBuf};
// use std::sync::Arc;
// use ruvector_core::vector_db::VectorDB;
//
// impl VectorStore {
//     /// T2: takes a resolved spec instead of the legacy `VectorDbConfig`.
//     pub fn open(spec: &crate::config::resolve::VectorSpec)
//         -> Result<Self, VectorStoreError>;
//
//     /// Test accessor over the process-wide `DB_HANDLES` registry: the
//     /// handle for this store's spec resolved against `session_cwd`,
//     /// materializing the redb file on first access (same lazy-open rule as
//     /// the production write path). `#[cfg(test)]` / `pub(crate)`.
//     pub(crate) async fn handle_for_test(
//         &self,
//         session_cwd: &Path,
//     ) -> Result<Arc<VectorDB>, VectorStoreError>;
// }
//
// `VectorSpec` / `DbPathSpec` already exist in `crate::config::resolve`
// (Phase A); `VectorSpec { db_path, embeddings }` and `DbPathSpec::Fixed /
// SessionRelative(PathBuf)` have `pub` fields.
//
// No network: the embeddings client is built with a dummy URL and `embed` is
// never called; a real redb file is only ever created under a `tempdir`.
#[cfg(test)]
mod t2_tests {
    use super::*;
    use crate::config::EmbeddingsConfig;
    use crate::config::resolve::{DbPathSpec, VectorSpec};

    /// A `VectorSpec` with a dummy embeddings endpoint (never called) and a
    /// `Fixed` DB path. Dimensions 3 keep the synthetic DBs tiny.
    fn fixed_spec(path: impl Into<PathBuf>) -> VectorSpec {
        VectorSpec {
            db_path: DbPathSpec::Fixed(path.into()),
            embeddings: EmbeddingsConfig {
                base_url: "http://unused.invalid/v1".into(),
                api_key: None,
                model: "test".into(),
                dimensions: 3,
            },
        }
    }

    fn session_relative_spec(path: impl Into<PathBuf>) -> VectorSpec {
        VectorSpec {
            db_path: DbPathSpec::SessionRelative(path.into()),
            embeddings: EmbeddingsConfig {
                base_url: "http://unused.invalid/v1".into(),
                api_key: None,
                model: "test".into(),
                dimensions: 3,
            },
        }
    }

    /// Spec §11.3 item 8: two `VectorStore`s (e.g. two profiles, two
    /// sessions, a reload rebuild) pointing at the same resolved `Fixed`
    /// path must share ONE process-wide handle — `Arc::ptr_eq` through the
    /// `handle_for_test` accessor. Two handles over one redb file would
    /// diverge, so this is load-bearing, not a style preference.
    #[tokio::test]
    async fn two_stores_same_fixed_path_share_handle() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("shared.db");

        let a = VectorStore::open(&fixed_spec(db.clone())).unwrap();
        let b = VectorStore::open(&fixed_spec(db.clone())).unwrap();
        let cwd = dir.path();

        let ha = a.handle_for_test(cwd).await.unwrap();
        let hb = b.handle_for_test(cwd).await.unwrap();
        assert!(
            Arc::ptr_eq(&ha, &hb),
            "two stores over the same Fixed path must share one process-wide handle"
        );

        // The registry is keyed by resolved path, not by store identity: a
        // different path must yield a DIFFERENT handle (guards against a
        // degenerate registry that hands out one handle for everything).
        let other = dir.path().join("other.db");
        let c = VectorStore::open(&fixed_spec(other.clone())).unwrap();
        let hc = c.handle_for_test(cwd).await.unwrap();
        assert!(
            !Arc::ptr_eq(&ha, &hc),
            "a different resolved path must get its own handle"
        );
        assert!(
            other.exists(),
            "materializing the other path creates its own file"
        );
    }

    /// A `Fixed` path is fully resolved at parse time: `db_path_for` must
    /// return the exact path for ANY session cwd (including the empty one),
    /// and both stores resolve it to the same process-wide handle.
    #[tokio::test]
    async fn fixed_path_ignores_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("fixed.db");
        let cwd_a = tempfile::tempdir().unwrap();
        let cwd_b = tempfile::tempdir().unwrap();

        let store = VectorStore::open(&fixed_spec(db.clone())).unwrap();
        assert_eq!(
            store.db_path_for(cwd_a.path()).as_path(),
            db.as_path(),
            "Fixed must resolve to exactly the parsed path, cwd-independent"
        );
        assert_eq!(
            store.db_path_for(cwd_b.path()),
            store.db_path_for(cwd_a.path()),
            "Fixed must ignore the session cwd"
        );
        assert_eq!(
            store.db_path_for(Path::new("")).as_path(),
            db.as_path(),
            "an empty session cwd must not fall back to the process cwd"
        );
        // Resolution must not touch the disk: only the handle materializes.
        assert!(!db.exists(), "db_path_for must not create the DB file");

        // Same Fixed path opened from two different cwds → one handle.
        let s_a = VectorStore::open(&fixed_spec(db.clone())).unwrap();
        let s_b = VectorStore::open(&fixed_spec(db.clone())).unwrap();
        let ha = s_a.handle_for_test(cwd_a.path()).await.unwrap();
        let hb = s_b.handle_for_test(cwd_b.path()).await.unwrap();
        assert!(
            Arc::ptr_eq(&ha, &hb),
            "stores at the same Fixed path must share one handle across cwds"
        );
        assert!(
            db.exists(),
            "the handle materializes the DB at the fixed path"
        );
    }

    /// A `SessionRelative` path (a relative `db_path` in a GLOBAL profile)
    /// keeps today's behaviour: it resolves against the session cwd, so two
    /// cwds get two distinct paths — and two distinct process-wide handles.
    #[tokio::test]
    async fn session_relative_resolves_per_cwd() {
        let cwd_a = tempfile::tempdir().unwrap();
        let cwd_b = tempfile::tempdir().unwrap();
        let rel = PathBuf::from("./.peakbot/vectors.db");

        let store = VectorStore::open(&session_relative_spec(rel.clone())).unwrap();
        let pa = store.db_path_for(cwd_a.path());
        let pb = store.db_path_for(cwd_b.path());

        assert_eq!(
            pa.as_path(),
            cwd_a
                .path()
                .canonicalize()
                .unwrap()
                .join(".peakbot/vectors.db")
                .as_path(),
            "SessionRelative must resolve against session cwd A"
        );
        assert_eq!(
            pb.as_path(),
            cwd_b
                .path()
                .canonicalize()
                .unwrap()
                .join(".peakbot/vectors.db")
                .as_path(),
            "SessionRelative must resolve against session cwd B"
        );
        assert_ne!(pa, pb, "different cwds must give different resolved paths");
        assert!(!pa.as_path().exists(), "resolution must not create the DB");
        assert!(!pb.as_path().exists(), "resolution must not create the DB");

        // The registry is keyed by the RESOLVED path: per-cwd paths must get
        // per-cwd handles, and one store must hand out both consistently.
        let ha = store.handle_for_test(cwd_a.path()).await.unwrap();
        let hb = store.handle_for_test(cwd_b.path()).await.unwrap();
        assert!(
            !Arc::ptr_eq(&ha, &hb),
            "SessionRelative cwds resolve to different paths → different handles"
        );
        assert!(
            pa.as_path().exists(),
            "handle A materializes the DB under cwd A"
        );
        assert!(
            pb.as_path().exists(),
            "handle B materializes the DB under cwd B"
        );
        assert!(
            Arc::ptr_eq(&ha, &store.handle_for_test(cwd_a.path()).await.unwrap()),
            "the same resolved path must keep returning the same handle"
        );
    }
}
