//! Vector store: a thin, `Send + Sync` wrapper around ruvector's `VectorDB`
//! plus the embeddings client, shared by the `doc_index` / `doc_search` tools.
//!
//! One store is opened at startup and injected into both tools (mirroring
//! `SearchTool::new(config)`). ruvector's `VectorDB` is itself `Send + Sync`
//! and redb is single-writer, so a single shared handle behind `Arc` is the
//! correct shape — two independently-opened handles on the same path would
//! race.
//!
//! ## Idempotent re-index
//! Each chunk's id is a stable hash of `(source_path, chunk_index)`, so
//! re-indexing an unchanged file overwrites the same ids (no duplicates). We
//! also store the file's content `sha256` in metadata, letting `doc_index`
//! skip files whose hash is unchanged — making "point it at the folder again"
//! safe and fast.

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

/// One indexed chunk hit returned from a search.
pub struct Hit {
    pub source: String,
    pub chunk_index: usize,
    pub text: String,
    pub score: f32,
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

/// Shared vector store. Cheap to clone (everything is `Arc`-backed).
#[derive(Clone)]
pub struct VectorStore {
    db: Arc<VectorDB>,
    embeddings: EmbeddingsClient,
}

impl VectorStore {
    /// Open (or create) the store at `config.db_path` and build the embeddings
    /// client. The parent directory is created if missing.
    ///
    /// ⚠ On reopen of an existing path, ruvector rebuilds the index from disk
    /// using the STORED dimensions/metric — the `DbOptions` passed here only
    /// take effect when creating a fresh DB. A model whose output dimension
    /// differs from an existing DB surfaces as a clear error on first insert
    /// (via [`EmbeddingsError::DimMismatch`] / a ruvector dimension error),
    /// never as silent corruption.
    pub fn open(config: &VectorDbConfig) -> Result<Self, VectorStoreError> {
        let path = PathBuf::from(&config.db_path);
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| VectorStoreError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }

        let embeddings = EmbeddingsClient::new(&config.embeddings);
        let opts = DbOptions {
            dimensions: embeddings.dimensions(),
            distance_metric: DistanceMetric::Cosine,
            storage_path: config.db_path.clone(),
            ..Default::default()
        };
        let db = VectorDB::new(opts).map_err(|source| VectorStoreError::Open {
            path: config.db_path.clone(),
            source,
        })?;

        Ok(Self {
            db: Arc::new(db),
            embeddings,
        })
    }

    /// Index a single file: parse → chunk → embed → upsert. Returns the number
    /// of chunks written, or `None` if the file was skipped because its content
    /// hash is unchanged since the last index. Unsupported extensions return a
    /// `ParseError::Unsupported` for the caller to count and continue.
    ///
    /// `doc_index` aggregates these outcomes across a directory walk into an
    /// [`IndexReport`].
    pub async fn index_file(&self, path: &Path) -> Result<IndexOutcome, VectorStoreError> {
        if !parse::is_supported(path) {
            return Err(VectorStoreError::Parse(ParseError::Unsupported(
                parse::extension_of(path),
            )));
        }

        let source = path.display().to_string();
        let text = parse::extract_text(path)?;
        let content_hash = sha256_hex(text.as_bytes());

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
            // Empty/whitespace-only file: nothing to index, but it's not an
            // error. Treat as zero-chunk index.
            return Ok(if is_update {
                IndexOutcome::Updated(0)
            } else {
                IndexOutcome::Indexed(0)
            });
        }

        let vectors = self.embeddings.embed(&chunks).await?;
        let mut entries = Vec::with_capacity(chunks.len());
        for (i, (text, vector)) in chunks.into_iter().zip(vectors).enumerate() {
            let mut metadata: HashMap<String, serde_json::Value> = HashMap::new();
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
        // ruvector's VectorDB is sync; run the insert off the async runtime so
        // the agent loop isn't starved on a large batch.
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || db.insert_batch(&entries))
            .await
            .expect("vector insert task panicked")?;

        Ok(if is_update {
            IndexOutcome::Updated(n)
        } else {
            IndexOutcome::Indexed(n)
        })
    }

    /// Embed `query` and return the top `k` most similar chunks.
    pub async fn search(&self, query: &str, k: usize) -> Result<Vec<Hit>, VectorStoreError> {
        let vector = {
            let mut v = self.embeddings.embed(&[query.to_string()]).await?;
            v.pop().unwrap_or_default()
        };

        let db = self.db.clone();
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
                }
            })
            .collect();
        Ok(hits)
    }

    /// The content hash stored for a file's chunk 0, if the file was indexed
    /// before. Returns `None` if the file is not in the store.
    fn stored_hash(&self, source: &str) -> Result<Option<String>, VectorStoreError> {
        let id = chunk_id(source, 0);
        match self.db.get(&id)? {
            Some(entry) => Ok(entry
                .metadata
                .as_ref()
                .map(|md| string_field(md, "sha256"))
                .filter(|s| !s.is_empty())),
            None => Ok(None),
        }
    }
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

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
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

    #[test]
    fn chunk_id_is_stable_and_index_sensitive() {
        assert_eq!(chunk_id("a.txt", 0), chunk_id("a.txt", 0));
        assert_ne!(chunk_id("a.txt", 0), chunk_id("a.txt", 1));
        assert_ne!(chunk_id("a.txt", 0), chunk_id("b.txt", 0));
    }

    #[test]
    fn sha256_changes_with_content() {
        assert_ne!(sha256_hex(b"hello"), sha256_hex(b"world"));
        assert_eq!(sha256_hex(b"hello"), sha256_hex(b"hello"));
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
