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
pub mod merge_driver;
mod parse;
mod reconcile;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use ruvector_core::types::{DbOptions, DistanceMetric, HnswConfig, SearchQuery, VectorEntry};
use ruvector_core::vector_db::VectorDB;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::OnceCell;

use crate::config::resolve::{DbPathSpec, VectorSpec};
use crate::utils::normalize_lexical;

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
    #[error("reconciliation failed: {0}")]
    Reconcile(#[from] reconcile::ReconcileError),
    #[error("failed to read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "source {path} is outside the repository root that owns this vector DB — index files inside the repo, or keep the DB outside the repo to use absolute local IDs"
    )]
    OutsideRoot { path: String },
    #[error("vector DB {path} fingerprint mismatch: stored {stored}, requested {requested}")]
    FingerprintMismatch {
        path: String,
        stored: String,
        requested: String,
    },
    #[error("invalid vector DB metadata: {0}")]
    InvalidMetadata(String),
    #[error(
        "vector DB metadata schema {found} is newer than this build supports (max {supported}); upgrade PeakBot or use a compatible store"
    )]
    MetadataNewer { found: u32, supported: u32 },
    #[error(
        "vector DB {path} anchor mismatch: {detail}; refusing to write or reconcile so source IDs cannot mix"
    )]
    AnchorMismatch { path: String, detail: String },
    #[error(
        "vector DB {path} predates the current v2 format (no `peakbot.vector` metadata); run a migration before indexing — search remains available"
    )]
    MigrationRequired { path: String },
}

/// Reserved metadata keys owned by the store. User-supplied metadata that
/// collides with any of these is dropped at the boundary so it can never
/// shadow the fields search relies on.
const RESERVED_METADATA_KEYS: [&str; 4] = ["source", "chunk_index", "text", "sha256"];

/// ruvector config key holding the universal v2 record. Presence marks a
/// store as managed; absence means a legacy v1 store (read-only compatibility)
/// or an empty store that is adopted on its first write.
const VECTOR_METADATA_KEY: &str = "peakbot.vector";
/// Universal v2 metadata schema version.
const VECTOR_SCHEMA: u32 = 2;
/// Distance metric name recorded in the fingerprint (matches `DbOptions`).
const VECTOR_METRIC: &str = "cosine";

/// Which anchoring contract a store's source IDs and roots follow. A store
/// inside a git repo uses repo-relative IDs (clone-portable); a store outside
/// any repo uses absolute local IDs. Persisted so a relocated store is still
/// structurally readable, while writes can refuse a boundary crossing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum AnchorKind {
    Repo,
    Local,
}

/// The anchor derived from the DB's *current* location. This is the only
/// source of truth for resolving recorded paths; metadata never persists an
/// absolute repo path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StoreAnchor {
    Repo(PathBuf),
    Local,
}

impl StoreAnchor {
    pub(crate) fn derive(db_path: &Path) -> Self {
        match anchored_root_of(db_path) {
            Some(root) => Self::Repo(root),
            None => Self::Local,
        }
    }

    fn kind(&self) -> AnchorKind {
        match self {
            Self::Repo(_) => AnchorKind::Repo,
            Self::Local => AnchorKind::Local,
        }
    }

    pub(crate) fn is_repo(&self) -> bool {
        matches!(self, Self::Repo(_))
    }
}

/// The immutable identity of a store: everything whose change would make
/// existing vectors meaningless. Deliberately excludes `base_url`, `api_key`
/// and the anchor — the same model served from a different endpoint, or the
/// same embedding space in a different checkout, is still the same space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EmbeddingFingerprint {
    schema: u32,
    model: String,
    dimensions: usize,
    metric: String,
    chunk_chars: usize,
    chunk_overlap: usize,
}

/// One doc_index root recorded in the manifest. Each call to `doc_index`
/// carries its own recursive flag and metadata; the same root upserts without
/// duplicates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ManifestRoot {
    /// The root as a repo-relative POSIX path (`""` is the repo root itself)
    /// for a repo-anchored store, or an absolute local path for a local store.
    /// Repo roots are never creator-absolute: each clone resolves them against
    /// the root derived from its own DB location, so the store is portable.
    root: String,
    /// Was the root indexed recursively?
    recursive: bool,
    /// The user-supplied metadata for this root (author, book, year, …).
    #[serde(default)]
    metadata: BTreeMap<String, serde_json::Value>,
}

/// The mutable bookkeeping for a store: the collection of doc_index roots and
/// the global sorted source set. `roots` is kept sorted by root path for
/// deterministic serialization.
///
/// Deliberately carries **no** absolute repo path: the repo root is always
/// derived locally from the current DB path (`anchored_root_of`), which is what
/// makes a committed DB usable from a clone at a different absolute path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct VectorManifest {
    /// The collection of doc_index roots recorded for this store, sorted by
    /// root path for deterministic serialization.
    #[serde(default)]
    roots: Vec<ManifestRoot>,
    /// Global sorted list of all indexed source ids (not root-scoped).
    #[serde(default)]
    sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct VectorMetadata {
    schema: u32,
    anchor: AnchorKind,
    fingerprint: EmbeddingFingerprint,
    manifest: VectorManifest,
}

/// How an `index_file` call relates to the v2 contract.
enum DbWrite {
    /// Create the v2 record (fingerprint before the first row).
    Fresh,
    /// An existing managed store: validate + extend the manifest.
    Existing(VectorMetadata),
}

/// Read and structurally validate the v2 record. A missing record is `None`
/// (legacy or empty); a newer schema is a distinct error so callers never
/// mistake it for legacy. Path shape is validated against the **persisted**
/// anchor kind, not the derived location: that keeps a relocated store
/// readable while [`check_anchor_metadata`] guards writes.
fn load_metadata(db: &VectorDB) -> Result<Option<VectorMetadata>, VectorStoreError> {
    let Some(json) = db.load_config_value(VECTOR_METADATA_KEY)? else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(&json)
        .map_err(|e| VectorStoreError::InvalidMetadata(format!("not valid JSON: {e}")))?;
    let schema = value
        .get("schema")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| VectorStoreError::InvalidMetadata("missing integer `schema`".into()))?
        as u32;
    if schema > VECTOR_SCHEMA {
        return Err(VectorStoreError::MetadataNewer {
            found: schema,
            supported: VECTOR_SCHEMA,
        });
    }
    if schema != VECTOR_SCHEMA {
        return Err(VectorStoreError::InvalidMetadata(format!(
            "unsupported schema {schema}"
        )));
    }
    let meta: VectorMetadata = serde_json::from_value(value)
        .map_err(|e| VectorStoreError::InvalidMetadata(e.to_string()))?;
    if meta.fingerprint.schema != VECTOR_SCHEMA {
        return Err(VectorStoreError::InvalidMetadata(format!(
            "fingerprint schema {} does not match schema {VECTOR_SCHEMA}",
            meta.fingerprint.schema
        )));
    }
    // Validate every recorded path on read: the manifest travels through git,
    // so a tampered/hand-edited record must never steer a file or git read
    // outside the local repo (or at an absolute location from another clone).
    validate_metadata_paths(&meta)?;
    Ok(Some(meta))
}

/// Reject a manifest path that is not a plain repo-relative POSIX path.
///
/// `""` means the repo root itself. Anything absolute, Windows-prefixed,
/// containing `.`/`..`, or using backslashes is refused before any disk access.
fn validate_repo_relative(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Ok(());
    }
    if path.contains('\\') {
        return Err(format!("{path:?} is not a POSIX path"));
    }
    if Path::new(path).is_absolute() {
        return Err(format!("{path:?} is absolute"));
    }
    for component in Path::new(path).components() {
        match component {
            std::path::Component::Normal(_) => {}
            std::path::Component::CurDir => {
                return Err(format!("{path:?} contains a '.' component"));
            }
            std::path::Component::ParentDir => {
                return Err(format!("{path:?} contains a '..' component"));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(format!("{path:?} contains a root/prefix component"));
            }
        }
    }
    Ok(())
}

/// Reject an absolute local path that is not already normalized: local source
/// IDs are full paths, so `.`/`..` components would make groups ambiguous.
fn validate_local_absolute(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("is empty".to_string());
    }
    if !Path::new(path).is_absolute() {
        return Err(format!("{path:?} is not absolute"));
    }
    for component in Path::new(path).components() {
        match component {
            std::path::Component::Normal(_)
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {}
            std::path::Component::CurDir => {
                return Err(format!("{path:?} contains a '.' component"));
            }
            std::path::Component::ParentDir => {
                return Err(format!("{path:?} contains a '..' component"));
            }
        }
    }
    Ok(())
}

fn validate_metadata_paths(meta: &VectorMetadata) -> Result<(), VectorStoreError> {
    let invalid = |what: &str, e: String| VectorStoreError::InvalidMetadata(format!("{what}: {e}"));
    let check = |path: &str| match meta.anchor {
        AnchorKind::Repo => validate_repo_relative(path),
        AnchorKind::Local => validate_local_absolute(path),
    };
    for root in &meta.manifest.roots {
        check(&root.root).map_err(|e| invalid("invalid manifest root", e))?;
    }
    for source in &meta.manifest.sources {
        check(source).map_err(|e| invalid("invalid manifest source", e))?;
    }
    Ok(())
}

/// Ensure a store's persisted anchor kind matches the anchor derived from its
/// current location. A mismatch is a relocation across the repo boundary:
/// reads may still use the existing rows, but a write/reconcile must refuse so
/// relative and absolute source IDs never mix.
pub(crate) fn check_anchor_metadata(
    db_path: &Path,
    meta: &VectorMetadata,
    anchor: &StoreAnchor,
) -> Result<(), VectorStoreError> {
    if meta.anchor == anchor.kind() {
        return Ok(());
    }
    let detail = match (meta.anchor, anchor) {
        (AnchorKind::Repo, StoreAnchor::Local) => {
            "metadata records repository-relative source IDs but the DB is outside any git repository"
        }
        (AnchorKind::Local, StoreAnchor::Repo(_)) => {
            "metadata records absolute local source IDs but the DB is inside a git repository"
        }
        // `check_anchor_metadata` only runs when the kinds differ.
        _ => unreachable!("anchor kind compared equal above"),
    };
    Err(VectorStoreError::AnchorMismatch {
        path: db_path.display().to_string(),
        detail: detail.to_string(),
    })
}

/// Resolve a validated repo-relative manifest path against the locally derived
/// repo root. The result is lexically contained; callers canonicalize existing
/// paths before reading so a symlink cannot escape either.
pub(crate) fn resolve_repo_relative(anchor: &Path, rel: &str) -> Result<PathBuf, String> {
    validate_repo_relative(rel)?;
    let joined = if rel.is_empty() {
        anchor.to_path_buf()
    } else {
        anchor.join(rel)
    };
    let normalized = normalize_lexical(&joined);
    if !normalized.starts_with(anchor) {
        return Err(format!("{rel:?} resolves outside the repository root"));
    }
    Ok(normalized)
}

fn save_metadata(db: &VectorDB, meta: &VectorMetadata) -> Result<(), VectorStoreError> {
    let json = serde_json::to_string(meta)
        .map_err(|e| VectorStoreError::InvalidMetadata(e.to_string()))?;
    db.save_config_value(VECTOR_METADATA_KEY, &json)?;
    Ok(())
}

/// Derive the repo root for a DB from its location: find the nearest `.git`
/// ancestor of the DB path (treating the DB path as a file whose parent
/// directory is the starting point). Returns `None` if the DB lives outside any
/// repo. Works even when the DB file itself does not yet exist, because it
/// searches from the nearest existing ancestor of the DB's parent.
///
/// This is the **only** source of the repo root: manifests never persist an
/// absolute path, so each clone resolves its own.
pub(crate) fn anchored_root_of(db_path: &Path) -> Option<PathBuf> {
    // The DB path is a file; start searching from its parent directory.
    let parent = db_path.parent()?;
    // Find the nearest existing ancestor (the parent may not exist yet).
    let mut cur = parent;
    while !cur.exists() {
        cur = cur.parent()?;
    }
    let canonical = std::fs::canonicalize(cur).ok()?;
    // Search from that ancestor up for a `.git` entry.
    let mut search = canonical.as_path();
    loop {
        if search.join(".git").exists() {
            return Some(search.to_path_buf());
        }
        search = search.parent()?;
    }
}

/// Git-aware file discovery for a repo-anchored store: resolve the repo anchor
/// from the **local** DB path and hand it to [`reconcile::discover_files`].
/// Returns `None` when the DB is outside a repo (callers fall back to the plain
/// local walk, which deliberately does not read `.gitignore`).
pub(crate) fn git_discover_files(
    db_path: &DbPath,
    root: &Path,
    recursive: bool,
) -> Option<Vec<PathBuf>> {
    let anchor = anchored_root_of(db_path.as_path())?;
    reconcile::discover_files(&anchor, root, recursive, db_path.as_path()).ok()
}

/// Drop the cached redb handle for `db_path` from the process-wide registry.
/// Used before reopening a DB that was replaced on disk (git checkout/pull or
/// another process writing a new inode), so subsequent reads/writes go through
/// the on-disk file rather than a stale inode. Other live `Arc` clones keep the
/// old handle alive; this only evicts the registry's reference.
pub(crate) fn close_db_handle(db_path: &DbPath) {
    let mut dbs = DB_HANDLES.lock().expect("vector db map lock poisoned");
    dbs.remove(db_path);
}

/// The source id of `path` relative to a known anchored repo root. Errors with
/// `OutsideRoot` when the path lies outside the anchored root.
pub(crate) fn relative_source(
    path: &Path,
    anchored_root: &Path,
) -> Result<String, VectorStoreError> {
    let canonical = std::fs::canonicalize(path).map_err(|source| VectorStoreError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let rel = canonical
        .strip_prefix(anchored_root)
        .map_err(|_| VectorStoreError::OutsideRoot {
            path: path.display().to_string(),
        })?;
    let source = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    Ok(source)
}

/// The source id of `path` for a local (outside-git) store: the canonical
/// absolute path with POSIX separators. Local IDs are not portable by design;
/// they identify rows on this machine only.
pub(crate) fn absolute_source(path: &Path) -> Result<String, VectorStoreError> {
    let canonical = std::fs::canonicalize(path).map_err(|source| VectorStoreError::Io {
        path: path.display().to_string(),
        source,
    })?;
    // Normalize to POSIX separators; the leading root stays a single `/`.
    Ok(canonical.to_string_lossy().replace('\\', "/"))
}

/// Key `path` by the anchor's contract: repo-relative for a repo-anchored
/// store, absolute for a local store.
pub(crate) fn source_id(anchor: &StoreAnchor, path: &Path) -> Result<String, VectorStoreError> {
    match anchor {
        StoreAnchor::Repo(root) => relative_source(path, root),
        StoreAnchor::Local => absolute_source(path),
    }
}

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

/// Universal vector store. Cheap to clone (everything is `Arc`-backed). The redb
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

/// Per-DB async locks serializing reconcile against search/index. A reconcile
/// rewrites rows and swaps the on-disk inode; a concurrent reader/writer must
/// not observe or race that. Keyed by resolved path so independent DBs stay
/// independent. The outer std `Mutex` guards only the map lookup.
static DB_LOCKS: LazyLock<Mutex<HashMap<DbPath, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Return the async lock for `db_path`, creating it on first request.
pub(crate) fn vector_lock(db_path: &DbPath) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = DB_LOCKS.lock().expect("vector lock map poisoned");
    Arc::clone(locks.entry(db_path.clone()).or_default())
}

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

    /// Turn-start reconciliation seam. Resolves a repo-anchored store's
    /// freshness at the start of every turn (before any doc tool call). A no-op
    /// for a local (outside-git) store; for a repo store, detects and replays
    /// the manifest if a pending marker, missing local state, HEAD change, or
    /// DB stamp change is found. The caller passes the resolved `DbPath` for
    /// the session cwd. Errors are logged but never fail the turn — the next
    /// turn will retry.
    pub async fn ensure_fresh_at_turn_start(
        &self,
        db_path: &DbPath,
    ) -> Result<(), reconcile::ReconcileError> {
        reconcile::ensure_fresh(self, db_path.as_path()).await
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

    /// Open the DB handle for `db_path`, creating the redb file (and parent
    /// dir) on first call. This is the **only** path that materializes a store
    /// — call it solely when about to write.
    ///
    /// ⚠ On reopen of an existing path, ruvector rebuilds the index from disk
    /// using the STORED dimensions/metric — the `DbOptions` here only take
    /// effect when creating a fresh DB. A model whose output dimension differs
    /// from an existing DB surfaces as a clear error on insert, never as silent
    /// corruption.
    pub(crate) async fn db(&self, db_path: &DbPath) -> Result<Arc<VectorDB>, VectorStoreError> {
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
    pub(crate) async fn db_if_exists(
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
    ///
    /// Writes are serialized against reconcile/search via the per-path async
    /// lock. A repo-anchored store is brought fresh before the write; a local
    /// store has no git/state work at all.
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

        let lock = vector_lock(db_path);
        let _guard = lock.lock().await;

        let anchor = StoreAnchor::derive(db_path.as_path());
        if anchor.is_repo() {
            // Repo stores must be fresh before any write (pending/HEAD/stamp).
            reconcile::ensure_fresh(self, db_path.as_path())
                .await
                .map_err(VectorStoreError::from)?;
        }

        let outcome = self.index_file_inner(db_path, path, user_metadata).await?;

        // Record state after a successful index so ensure_fresh knows the
        // current HEAD and DB stamp match what is on disk. Local stores keep no
        // reconcile state.
        if anchor.is_repo() {
            self.record_state_after_index(db_path).await?;
        }

        Ok(outcome)
    }

    /// Write the reconcile state (HEAD + DB stamp) after a successful write.
    ///
    /// The state stamp must describe the DB **as it is on disk now**: metadata
    /// written after the state would leave a stale stamp and trigger a needless
    /// full replay on the next search.
    async fn record_state_after_index(&self, db_path: &DbPath) -> Result<(), VectorStoreError> {
        let db = self.db(db_path).await?;
        if load_metadata(&db)?.is_none() {
            return Ok(());
        }
        reconcile::record_state(db_path.as_path()).map_err(VectorStoreError::from)
    }

    /// Internal indexing path called after the freshness check.
    async fn index_file_inner(
        &self,
        db_path: &DbPath,
        path: &Path,
        user_metadata: &HashMap<String, serde_json::Value>,
    ) -> Result<IndexOutcome, VectorStoreError> {
        // Repo stores key chunks by a repo-relative source id; local stores use
        // the absolute path.
        let (write, source) = self.write_plan_for(db_path, path).await?;

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

        // A fresh store records its identity BEFORE the first row, so a
        // partially written DB can never be read under the wrong identity.
        if let DbWrite::Fresh = &write {
            let anchor = StoreAnchor::derive(db_path.as_path());
            let metadata = VectorMetadata {
                schema: VECTOR_SCHEMA,
                anchor: anchor.kind(),
                fingerprint: self.fingerprint(),
                manifest: VectorManifest {
                    roots: Vec::new(),
                    sources: vec![source.clone()],
                },
            };
            save_metadata(&db, &metadata)?;
        }

        let db_for_insert = Arc::clone(&db);
        tokio::task::spawn_blocking(move || db_for_insert.insert_batch(&entries))
            .await
            .expect("vector insert task panicked")?;

        // Keep the manifest's source set complete. `sort` makes the union
        // deterministic regardless of index order or clone.
        if let DbWrite::Existing(metadata) = &write
            && !metadata.manifest.sources.iter().any(|s| s == &source)
        {
            let mut updated = metadata.clone();
            updated.manifest.sources.push(source.clone());
            updated.manifest.sources.sort();
            save_metadata(&db, &updated)?;
        }

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

    /// Record a doc_index root in the manifest, upserting by root ID so the
    /// same root does not duplicate. The caller (DocIndexTool) passes the
    /// actual requested root + recursive flag + metadata; this method is the
    /// bookkeeping seam between the tool layer and the store. A store that has
    /// not been materialized yet is a no-op.
    ///
    /// Runs under the same per-DB async lock as `index_file`/`search`, so a
    /// concurrent reconcile can never overwrite the root list with a stale
    /// snapshot. Reconcile state is refreshed **after** the metadata save, so
    /// its DB stamp describes the file that is actually on disk.
    pub async fn record_root(
        &self,
        db_path: &DbPath,
        root: PathBuf,
        recursive: bool,
        metadata: &HashMap<String, serde_json::Value>,
    ) -> Result<(), VectorStoreError> {
        let lock = vector_lock(db_path);
        let _guard = lock.lock().await;

        let Some(db) = self.db_if_exists(db_path).await? else {
            // Nothing was indexed (e.g. a walk with only unsupported files), so
            // there is no manifest to extend — and asking must not create one.
            return Ok(());
        };
        let Some(mut meta) = load_metadata(&db)? else {
            return Ok(());
        };
        let anchor = StoreAnchor::derive(db_path.as_path());
        check_anchor_metadata(db_path.as_path(), &meta, &anchor)?;
        let root_id = source_id(&anchor, &root)?;
        // Upsert: find existing entry with the same root path, or append.
        let root_entry = ManifestRoot {
            root: root_id,
            recursive,
            metadata: sanitize_metadata(metadata).into_iter().collect(),
        };
        let mut roots = meta.manifest.roots.clone();
        if let Some(existing) = roots.iter_mut().find(|r| r.root == root_entry.root) {
            *existing = root_entry;
        } else {
            roots.push(root_entry);
        }
        // Deterministic order: sorted by root path.
        roots.sort_by(|a, b| a.root.cmp(&b.root));
        meta.manifest.roots = roots;
        save_metadata(&db, &meta)?;
        // Refresh the stamp *after* the metadata save (see doc comment).
        if anchor.is_repo() {
            reconcile::record_state(db_path.as_path()).map_err(VectorStoreError::from)?;
        }
        Ok(())
    }

    /// Decide how this write relates to the v2 contract and return the source
    /// id to key chunks by.
    ///
    /// The on-disk file is classified first (see [`reconcile::classify_db`]) so
    /// a magic-valid but damaged or foreign file is never opened for writing or
    /// mistaken for a fresh store.
    async fn write_plan_for(
        &self,
        db_path: &DbPath,
        path: &Path,
    ) -> Result<(DbWrite, String), VectorStoreError> {
        let anchor = StoreAnchor::derive(db_path.as_path());
        match reconcile::classify_db(db_path.as_path()).await {
            // No file yet, or a valid store that carries no rows and no v2
            // record: adopt the v2 contract on this write.
            reconcile::DbKind::Missing | reconcile::DbKind::Fresh => {
                Ok((DbWrite::Fresh, source_id(&anchor, path)?))
            }
            // A populated v1 DB is read-only compatibility; never silently
            // migrated or extended.
            reconcile::DbKind::Legacy => Err(VectorStoreError::MigrationRequired {
                path: db_path.display().to_string(),
            }),
            reconcile::DbKind::Newer { found } => Err(VectorStoreError::MetadataNewer {
                found,
                supported: VECTOR_SCHEMA,
            }),
            // The file is not a readable store (or not ours). Leave its bytes
            // untouched and fail rather than repair it in place.
            reconcile::DbKind::Damaged => Err(VectorStoreError::InvalidMetadata(format!(
                "vector DB {} is damaged; refusing to write to it",
                db_path.display()
            ))),
            reconcile::DbKind::Managed => {
                let Some(db) = self.db_if_exists(db_path).await? else {
                    // Replaced/deleted between classification and open.
                    return Ok((DbWrite::Fresh, source_id(&anchor, path)?));
                };
                let Some(metadata) = load_metadata(&db)? else {
                    return Ok((DbWrite::Fresh, source_id(&anchor, path)?));
                };
                check_anchor_metadata(db_path.as_path(), &metadata, &anchor)?;
                self.check_fingerprint(db_path, &metadata.fingerprint)?;
                let source = source_id(&anchor, path)?;
                Ok((DbWrite::Existing(metadata), source))
            }
        }
    }

    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint {
            schema: VECTOR_SCHEMA,
            model: self.inner.spec.embeddings.model.clone(),
            dimensions: self.inner.spec.embeddings.dimensions,
            metric: VECTOR_METRIC.to_string(),
            chunk_chars: chunk::CHUNK_SIZE,
            chunk_overlap: chunk::CHUNK_OVERLAP,
        }
    }

    /// Reject a store whose identity differs from this spec. Called before any
    /// write and on every read of a managed store, so a mismatch can never
    /// corrupt rows.
    fn check_fingerprint(
        &self,
        db_path: &DbPath,
        stored: &EmbeddingFingerprint,
    ) -> Result<(), VectorStoreError> {
        let requested = self.fingerprint();
        if &requested != stored {
            return Err(VectorStoreError::FingerprintMismatch {
                path: db_path.display().to_string(),
                stored: format!("{stored:?}"),
                requested: format!("{requested:?}"),
            });
        }
        Ok(())
    }

    /// Embed `query` and return the top `k` most similar chunks. If nothing has
    /// been indexed at `db_path` there is no DB file — return no hits without
    /// touching the network or disk.
    ///
    /// Repo-anchored stores are serialized against reconcile: the per-path async
    /// lock is held from freshness check through the actual search, so a
    /// concurrent reconcile can never race a reader into serving stale/partial
    /// rows. Local stores do no freshness/git work.
    pub async fn search(
        &self,
        db_path: &DbPath,
        query: &str,
        k: usize,
    ) -> Result<Vec<Hit>, VectorStoreError> {
        // Bail before embedding/opening: no DB file means no hits, and asking
        // must not create one.
        if !db_path.as_path().exists() {
            return Ok(Vec::new());
        }

        let lock = vector_lock(db_path);
        let _guard = lock.lock().await;

        let anchor = StoreAnchor::derive(db_path.as_path());
        if anchor.is_repo() {
            // Freshness first: it classifies the on-disk file (and sets the
            // pending repair marker on corruption) *before* any handle is
            // opened, so a raw open error can never bypass the safety contract.
            // Reconcile may also have swapped the on-disk inode.
            reconcile::ensure_fresh(self, db_path.as_path())
                .await
                .map_err(VectorStoreError::from)?;
        }
        self.search_fresh(db_path, query, k).await
    }

    /// The actual read: fingerprint check + query embed + HNSW search. Called
    /// with the per-path lock held.
    async fn search_fresh(
        &self,
        db_path: &DbPath,
        query: &str,
        k: usize,
    ) -> Result<Vec<Hit>, VectorStoreError> {
        let Some(db) = self.db_if_exists(db_path).await? else {
            return Ok(Vec::new());
        };

        // A managed store must match this spec's identity — a mismatch (e.g. a
        // different model) errors without touching rows. A legacy store (no
        // record) is readable, read-only compatibility. Anchor shape is
        // deliberately *not* re-checked here: a relocated store may still serve
        // its existing rows; writes enforce the boundary.
        if let Some(metadata) = load_metadata(&db)? {
            self.check_fingerprint(db_path, &metadata.fingerprint)?;
        }

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
    pub(crate) async fn stored_hash(
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
    pub(crate) async fn delete_chunks_from(
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
pub(crate) fn content_hash(text: &str, metadata: &HashMap<String, serde_json::Value>) -> String {
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
pub(crate) fn sanitize_metadata(
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
        let db = dir.path().join("v.db");

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
