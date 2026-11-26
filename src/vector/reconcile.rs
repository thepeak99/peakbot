//! Reconciliation slice for repo-anchored vector stores.
//!
//! Detects changes between reconcile runs (pending marker, missing local
//! state, HEAD change, DB stamp change) and replays the manifest: validating
//! per-file hashes, embedding new/changed files, and pruning deleted ones.
//!
//! The DB file is never deleted or rebuilt. A corrupt replacement (bad redb
//! magic) or a manifest-less replacement is refused with a stale/corrupt error
//! while the pending marker stays set, so unrelated repo files can never be
//! indexed by a "repair". State lives in a per-DB `<db>.peakbot` directory and
//! is only a cache: losing it forces a hash-validating replay, not a rebuild.
//! A local (outside-git) store never reconciles and keeps no state.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::vector::{StoreAnchor, VectorMetadata, VectorStoreError};

use super::{check_anchor_metadata, load_metadata, save_metadata};

/// Reconcile state: written atomically after each successful reconcile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReconcileState {
    pub last_head: String,
    pub db_size: u64,
    pub db_mtime: u128,
    pub last_reconciled: u128,
}

/// Reconcile error variants.
#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("git operation failed: {0}")]
    Git(String),
    #[error(transparent)]
    VectorStore(#[from] Box<VectorStoreError>),
    #[error("reconcile interrupted: {0}")]
    Interrupted(String),
    /// The DB file on disk is not a readable vector store. It is deliberately
    /// left untouched (never deleted or rebuilt over) so an operator can
    /// inspect it.
    #[error("vector DB at {path} is corrupt or unreadable: {detail}")]
    Corrupt { path: String, detail: String },
    /// The DB file exists but carries no manifest. Rebuilding it from the
    /// repository would silently index files the user never opted into, so this
    /// is an explicit error instead.
    #[error(
        "vector DB at {path} has no v2 manifest (stale or replaced); \
         refusing to rebuild it from the repository"
    )]
    NoManifest { path: String },
    /// The git worktree is mid-merge/rebase and is not a stable commit; defer
    /// rather than embed conflict markers.
    #[error("deferring reconcile: {0}")]
    Deferred(String),
    /// The manifest names a path that is not dereferenceable under the store's
    /// anchor (repo-relative for a repo store, absolute for a local store).
    /// Nothing outside the local repo may be read, pruned, or embedded.
    #[error("invalid vector metadata: {0}")]
    InvalidManifest(String),
    /// A repo-anchored DB must live inside the git repo it serves; without one
    /// there is no local anchor to resolve recorded roots against.
    #[error("vector DB at {path} is not inside a git repository")]
    NotInRepo { path: String },
    /// A persisted metadata schema newer than this build: never treated as
    /// legacy or migrated.
    #[error(transparent)]
    Newer(Box<VectorStoreError>),
}

/// redb's file magic (`redb` + 5 sentinel bytes). Checking it ourselves lets a
/// garbage replacement be refused *without* ever handing the file to ruvector,
/// whose `Database::create` can initialize or repair a file in place.
const REDB_MAGIC: &[u8; 9] = b"redb\x1A\x0A\xA9\x0D\x0A";

/// On-disk health of a vector DB file, decided from the header alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DbFileHealth {
    /// No file at the path.
    Missing,
    /// A plausible redb file; safe to hand to ruvector.
    Valid,
    /// Garbage, empty, or unreadable. Never opened, never deleted.
    Corrupt,
}

/// Classify the DB file by reading its redb magic. A missing file is not
/// corrupt; an *empty* file is (no redb file is legitimately zero bytes, so an
/// empty inode is evidence of a bad replacement, not a fresh store).
pub(crate) fn db_file_health(path: &Path) -> DbFileHealth {
    use std::io::Read;

    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return DbFileHealth::Missing,
        Err(_) => return DbFileHealth::Corrupt,
    };
    let mut magic = [0u8; REDB_MAGIC.len()];
    match file.read_exact(&mut magic) {
        Ok(()) if &magic == REDB_MAGIC => DbFileHealth::Valid,
        _ => DbFileHealth::Corrupt,
    }
}

fn corrupt_error(path: &Path, detail: &str) -> ReconcileError {
    ReconcileError::Corrupt {
        path: path.display().to_string(),
        detail: detail.to_string(),
    }
}

/// On-disk classification of the file at a DB path.
///
/// Kept separate from `db_file_health` because "carries a redb header" is not
/// enough to decide how to treat a file: a magic-valid file with no v2
/// record may be a *true empty* store, a legacy v1 store, or a foreign/damaged
/// file that ruvector's open would silently "repair" in place. The probe below
/// decides on a **copy**, so the real file is never mutated just by looking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DbKind {
    /// No file at the path.
    Missing,
    /// A readable v2 store (it carries the `peakbot.vector` record).
    Managed,
    /// A readable store with no v2 record and no rows: adoptable as fresh.
    Fresh,
    /// A readable store with rows but no v2 record: legacy v1, read-only
    /// compatibility (never migrated, never gained whole-root coverage).
    Legacy,
    /// The record's schema is newer than this build: an explicit
    /// incompatibility, never legacy.
    Newer { found: u32 },
    /// A magic-valid file that cannot be opened/read, or whose v2 record is
    /// corrupt. Its bytes must never be handed to ruvector.
    Damaged,
}

/// Path → (size, mtime) → classification. Probing copies the file, so caching
/// by stamp keeps that off the hot path while still re-probing a replacement.
struct Classified {
    size: u64,
    mtime: u128,
    kind: DbKind,
}

static CLASSIFICATIONS: LazyLock<Mutex<HashMap<PathBuf, Classified>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Classify the DB file at `path` without opening (or mutating) it in place.
pub(crate) async fn classify_db(path: &Path) -> DbKind {
    let Some((size, mtime)) = file_stat(path) else {
        forget_classification(path);
        return DbKind::Missing;
    };
    {
        let cache = CLASSIFICATIONS
            .lock()
            .expect("classification map lock poisoned");
        if let Some(known) = cache.get(path)
            && known.size == size
            && known.mtime == mtime
        {
            return known.kind;
        }
    }

    let kind = match db_file_health(path) {
        DbFileHealth::Missing => DbKind::Missing,
        DbFileHealth::Corrupt => DbKind::Damaged,
        DbFileHealth::Valid => {
            let owned = path.to_path_buf();
            // The probe copies + opens the file (sync disk IO + redb).
            tokio::task::spawn_blocking(move || probe_db(&owned))
                .await
                .expect("vector db probe task panicked")
        }
    };
    remember_classification(path, size, mtime, kind);
    kind
}

/// Re-record the classification for a file this process just wrote, so the next
/// freshness check does not copy the DB to re-probe it.
fn refresh_classification(path: &Path, kind: DbKind) {
    if let Some((size, mtime)) = file_stat(path) {
        remember_classification(path, size, mtime, kind);
    }
}

fn remember_classification(path: &Path, size: u64, mtime: u128, kind: DbKind) {
    CLASSIFICATIONS
        .lock()
        .expect("classification map lock poisoned")
        .insert(path.to_path_buf(), Classified { size, mtime, kind });
}

fn forget_classification(path: &Path) {
    CLASSIFICATIONS
        .lock()
        .expect("classification map lock poisoned")
        .remove(path);
}

/// Probe a **copy** of the file: open it, read the v2 record, and count rows.
/// The real file is never opened here, so a damaged/foreign file cannot be
/// repaired or initialized as a side effect of classification.
///
/// The copy normally sits next to the DB (same filesystem, no large staging in
/// `/tmp`); a read-only checkout still gets probed via the system temp dir.
fn probe_db(path: &Path) -> DbKind {
    let sibling = probe_path(path);
    if fs::copy(path, &sibling).is_ok() {
        let kind = probe_copy(&sibling);
        let _ = fs::remove_file(&sibling);
        return kind;
    }
    let _ = fs::remove_file(&sibling);

    let fallback = std::env::temp_dir().join(probe_file_name(path));
    if fs::copy(path, &fallback).is_err() {
        return DbKind::Damaged;
    }
    let kind = probe_copy(&fallback);
    let _ = fs::remove_file(&fallback);
    kind
}

/// Classify an already-copied file.
fn probe_copy(path: &Path) -> DbKind {
    match open_existing_db(path) {
        Ok(db) => match load_metadata(&db) {
            Ok(Some(_)) => DbKind::Managed,
            Ok(None) => match db.is_empty() {
                Ok(true) => DbKind::Fresh,
                Ok(false) => DbKind::Legacy,
                Err(_) => DbKind::Damaged,
            },
            Err(VectorStoreError::MetadataNewer { found, .. }) => DbKind::Newer { found },
            Err(_) => DbKind::Damaged,
        },
        Err(_) => DbKind::Damaged,
    }
}

/// Capacity hint matching the stores we probe (a no-op for the default flat
/// index, kept in sync with `vector::db_options`).
const PROBE_HNSW_MAX_ELEMENTS: usize = 100_000;

/// Open an existing vector DB read-write. Dimensions are irrelevant on reopen
/// (ruvector reuses the stored config); callers only pass a health-checked copy.
fn open_existing_db(path: &Path) -> Result<ruvector_core::vector_db::VectorDB, String> {
    use ruvector_core::types::{DbOptions, DistanceMetric, HnswConfig};
    let options = DbOptions {
        dimensions: 1,
        distance_metric: DistanceMetric::Cosine,
        storage_path: path.display().to_string(),
        hnsw_config: Some(HnswConfig {
            max_elements: PROBE_HNSW_MAX_ELEMENTS,
            ..HnswConfig::default()
        }),
        ..Default::default()
    };
    ruvector_core::vector_db::VectorDB::new(options).map_err(|e| e.to_string())
}

/// A collision-free sibling path for the probe copy (same filesystem as the
/// DB, so no cross-device copy).
fn probe_path(path: &Path) -> PathBuf {
    path.with_file_name(probe_file_name(path))
}

/// A collision-free probe filename: pid + nanos + sequence, `.tmp` (never a
/// supported document extension).
fn probe_file_name(path: &Path) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "vectors.db".to_string());
    format!(".{name}.probe-{}-{nanos}-{seq}.tmp", std::process::id())
}

/// Per-DB state directory path: `<db_path>.peakbot`.
pub(crate) fn state_dir_for(db_path: &Path) -> PathBuf {
    db_path.with_extension(format!(
        "{}.peakbot",
        db_path
            .extension()
            .map_or("db".to_string(), |e| e.to_string_lossy().into_owned())
    ))
}

fn state_file_for(db_path: &Path) -> PathBuf {
    state_dir_for(db_path).join("state.json")
}

fn pending_file_for(db_path: &Path) -> PathBuf {
    state_dir_for(db_path).join("pending")
}

pub(crate) fn now_secs() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u128)
        .unwrap_or(0)
}

pub(crate) fn file_stat(path: &Path) -> Option<(u64, u128)> {
    path.metadata().ok().map(|m| {
        let size = m.len();
        // Nanosecond precision: two replacements inside the same second (a
        // `git checkout` right after an index) must still be detected.
        let mtime = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        (size, mtime)
    })
}

/// Read the reconcile state for a DB. Returns `None` if no state exists.
pub fn read_state(db_path: &Path) -> Result<Option<ReconcileState>, io::Error> {
    let state_file = state_file_for(db_path);
    if !state_file.exists() {
        return Ok(None);
    }
    let json = fs::read_to_string(&state_file)?;
    let state: ReconcileState = serde_json::from_str(&json)?;
    Ok(Some(state))
}

/// Write the reconcile state atomically (write to temp, rename).
pub fn write_state(db_path: &Path, state: &ReconcileState) -> Result<(), io::Error> {
    let dir = state_dir_for(db_path);
    fs::create_dir_all(&dir)?;
    let state_file = state_file_for(db_path);
    let json = serde_json::to_string_pretty(state)?;
    let tmp = dir.join("state.json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, &state_file)?;
    Ok(())
}

/// Touch (or clear) the pending marker to signal an in-progress reconcile.
pub fn set_pending(db_path: &Path, pending: bool) -> io::Result<()> {
    let pending_file = pending_file_for(db_path);
    if pending {
        fs::create_dir_all(state_dir_for(db_path))?;
        fs::write(&pending_file, "pending")?;
    } else if pending_file.exists() {
        fs::remove_file(&pending_file)?;
    }
    Ok(())
}

pub fn is_pending(db_path: &Path) -> bool {
    pending_file_for(db_path).exists()
}

/// Get the current git HEAD for a directory.
pub fn git_head(dir: &Path) -> Result<String, ReconcileError> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .map_err(|e| ReconcileError::Git(format!("git rev-parse failed: {e}")))?;
    if !output.status.success() {
        return Err(ReconcileError::Git(
            "git rev-parse returned non-zero exit code".to_string(),
        ));
    }
    Ok(String::from_utf8(output.stdout)
        .unwrap_or_default()
        .trim()
        .to_string())
}

/// Check if a git merge is in progress (MERGE_HEAD exists) or a rebase/apply
/// session is active. This alone is not enough: a rebase conflict moves HEAD,
/// leaves unmerged index stages, and creates `rebase-merge/` but **no**
/// `MERGE_HEAD`. Use [`git_worktree_unstable`] for the authoritative check.
pub fn git_merge_in_progress(dir: &Path) -> bool {
    let git_dir = dir.join(".git");
    git_dir.join("MERGE_HEAD").exists()
        || git_dir.join("rebase-merge").exists()
        || git_dir.join("rebase-apply").exists()
}

/// True when the index has unmerged (conflict) entries. Uses real
/// `git ls-files -u` output; an empty output means a clean index. A non-git
/// directory (or a git failure) is treated as "no conflicts" so non-repo
/// compatibility stores keep working.
pub fn git_unmerged_entries(dir: &Path) -> bool {
    Command::new("git")
        .args(["ls-files", "-u"])
        .current_dir(dir)
        .output()
        .map(|out| out.status.success() && out.stdout.iter().any(|b| !b.is_ascii_whitespace()))
        .unwrap_or(false)
}

/// True when the worktree is not a stable commit: unmerged index entries (any
/// merge/rebase/cherry-pick conflict) or a merge/rebase in progress. Reconciling
/// against it would embed conflict markers, so callers must defer.
pub fn git_worktree_unstable(dir: &Path) -> bool {
    git_unmerged_entries(dir) || git_merge_in_progress(dir)
}

/// Check if the DB file was replaced on disk (different size or mtime).
pub fn db_stamp_changed(db_path: &Path, state: &ReconcileState) -> bool {
    match file_stat(db_path) {
        Some((size, mtime)) => size != state.db_size || mtime != state.db_mtime,
        None => true, // DB file vanished
    }
}

/// List files under a root for indexing, respecting the recursive flag.
pub fn list_files(root: &Path, recursive: bool) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !recursive {
        if root.is_dir() {
            for entry in fs::read_dir(root)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() {
                    files.push(path);
                }
            }
        } else if root.is_file() {
            files.push(root.to_path_buf());
        }
    } else {
        walk_dir(root, &mut files)?;
    }
    files.sort();
    Ok(files)
}

fn walk_dir(path: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            walk_dir(&entry.path(), files)?;
        }
    } else if path.is_file() {
        files.push(path.to_path_buf());
    }
    Ok(())
}

/// Discover files to index under `root`, honouring `.gitignore`.
///
/// One helper for both `doc_index` collection and reconcile. The `git` command
/// is always rooted at the locally derived repo `anchor` — never at a path the
/// manifest or DB supplied — so the manifest can only ever select files
/// inside this clone. When git cannot answer (non-repo, no binary) the plain
/// walk is used, which keeps local-mode behaviour unchanged.
///
/// `<db>.peakbot` (local state/temp files) is always excluded: `state.json` is
/// a supported extension and would otherwise re-embed itself forever.
pub(crate) fn discover_files(
    anchor: &Path,
    root: &Path,
    recursive: bool,
    db_path: &Path,
) -> io::Result<Vec<PathBuf>> {
    let state_dir = state_dir_for(db_path);
    let mut files = if root.is_file() {
        vec![root.to_path_buf()]
    } else {
        match git_listed_files(anchor, root) {
            Some(listed) => listed
                .into_iter()
                .filter(|file| file.is_file())
                .filter(|file| {
                    // A non-recursive root covers only its direct children.
                    recursive
                        || file
                            .strip_prefix(root)
                            .map(|rel| rel.components().count() == 1)
                            .unwrap_or(false)
                })
                .collect(),
            None => list_files(root, recursive)?,
        }
    };
    files.retain(|file| !file.starts_with(&state_dir));
    files.sort();
    files.dedup();
    Ok(files)
}

/// `git ls-files --cached --others --exclude-standard` limited to `root`'s
/// subtree, resolved against `anchor`. `None` when git cannot answer.
fn git_listed_files(anchor: &Path, root: &Path) -> Option<Vec<PathBuf>> {
    let canonical_root = root.canonicalize().ok()?;
    let rel = canonical_root.strip_prefix(anchor).ok()?;
    let pathspec = if rel.as_os_str().is_empty() {
        ".".to_string()
    } else {
        rel.to_str()?.to_string()
    };
    let output = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
        ])
        .arg(pathspec)
        .current_dir(anchor)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut files = Vec::new();
    for entry in output.stdout.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        // Git prints repo-relative paths with `/` separators.
        let rel = std::str::from_utf8(entry).ok()?;
        files.push(anchor.join(rel));
    }
    Some(files)
}

/// A file to (re)index during reconcile, carrying the metadata of the root that
/// discovered it so new files under a metadata-bearing root inherit that root's
/// author/book/year — exactly replaying the recorded multi-root manifest.
struct Discovered {
    path: PathBuf,
    metadata: HashMap<String, serde_json::Value>,
}

/// The reconcile method: called by ensure_fresh when changes are detected.
///
/// Replays the manifest from the DB, discovering new files under previously
/// indexed roots, embedding only new/changed files, and pruning deleted ones.
///
/// It never deletes or rebuilds a DB file: a corrupt file or a replacement
/// without a v2 manifest is a hard error that leaves the file for inspection
/// and the pending marker set.
pub async fn reconcile(store: &super::VectorStore, db_path: &Path) -> Result<(), ReconcileError> {
    reconcile_impl(store, db_path, false).await
}

/// Reconcile a DB that must be reopened and must carry a v2 manifest.
/// Triggered by a DB stamp change (the on-disk inode was replaced) or by a lost
/// local state directory. A valid replacement with a manifest is replayed with
/// hash reuse; a corrupt file or one with no v2 manifest is refused rather than
/// rebuilt from the repository.
pub async fn reconcile_replaced(
    store: &super::VectorStore,
    db_path: &Path,
) -> Result<(), ReconcileError> {
    reconcile_impl(store, db_path, true).await
}

async fn reconcile_impl(
    store: &super::VectorStore,
    db_path: &Path,
    require_manifest: bool,
) -> Result<(), ReconcileError> {
    // Mark as in-progress first: every failure below must leave the repair flag
    // set, even when no marker existed before this run.
    set_pending(db_path, true).map_err(|e| ReconcileError::Interrupted(e.to_string()))?;

    // Classify the file from the outside before anything opens it in place.
    let kind = classify_db(db_path).await;
    match kind {
        DbKind::Missing => {
            // Nothing on disk to reconcile. A pending marker from an interrupted
            // run is stale; clear it. Never conjure a DB from a read path.
            set_pending(db_path, false).map_err(|e| ReconcileError::Interrupted(e.to_string()))?;
            return Ok(());
        }
        DbKind::Damaged => {
            return Err(corrupt_error(
                db_path,
                "file is not a readable vector store (bad magic, unreadable rows, or corrupt v2 metadata)",
            ));
        }
        DbKind::Newer { found } => {
            return Err(ReconcileError::Newer(Box::new(
                VectorStoreError::MetadataNewer {
                    found,
                    supported: super::VECTOR_SCHEMA,
                },
            )));
        }
        DbKind::Fresh | DbKind::Legacy => {
            if require_manifest {
                // A valid but manifest-less replacement: rebuilding it from the
                // repository would index files the user never opted into.
                return Err(ReconcileError::NoManifest {
                    path: db_path.display().to_string(),
                });
            }
            // A fresh or legacy (v1) store has nothing to reconcile and must
            // never gain whole-root coverage.
            set_pending(db_path, false).map_err(|e| ReconcileError::Interrupted(e.to_string()))?;
            return Ok(());
        }
        DbKind::Managed => {}
    }

    // Use the caller's exact resolved path: recomputing it from the spec would
    // ignore a per-session cwd and could open/lock a different DB file.
    let resolved = super::DbPath(db_path.to_path_buf());
    let anchor = StoreAnchor::derive(db_path);
    let anchored = match &anchor {
        StoreAnchor::Repo(root) => root.clone(),
        StoreAnchor::Local => {
            return Err(ReconcileError::NotInRepo {
                path: db_path.display().to_string(),
            });
        }
    };

    // Open the DB. Opening can still fail for a truncated file whose header
    // survived; that is corruption, not an invitation to rebuild.
    let db = match store.db(&resolved).await {
        Ok(db) => db,
        Err(e) => {
            return Err(corrupt_error(db_path, &format!("failed to open: {e}")));
        }
    };

    // Load the v2 metadata to get the manifest. A corrupt record must never
    // fall through to discovery/pruning (which would mix or delete rows).
    let meta = match load_metadata(&db).map_err(|e| ReconcileError::VectorStore(Box::new(e)))? {
        Some(m) => m,
        None => {
            if require_manifest {
                return Err(ReconcileError::NoManifest {
                    path: db_path.display().to_string(),
                });
            }
            set_pending(db_path, false).map_err(|e| ReconcileError::Interrupted(e.to_string()))?;
            return Ok(());
        }
    };

    // Never reconcile under a different anchor: a relocated store is readable,
    // but replaying its relative IDs would mix contracts.
    check_anchor_metadata(db_path, &meta, &anchor)
        .map_err(|e| ReconcileError::VectorStore(Box::new(e)))?;

    // A half-merged worktree is not a stable commit. Defer before any file is
    // read or embedded (pending or head-change paths can both land here).
    if git_worktree_unstable(&anchored) {
        return Err(ReconcileError::Deferred(
            "git index has unmerged entries (merge or rebase in progress)".to_string(),
        ));
    }

    // Validate identity before touching rows: a fingerprint mismatch must never
    // embed, prune, or rewrite the manifest of a store belonging to a different
    // model/space.
    store
        .check_fingerprint(&resolved, &meta.fingerprint)
        .map_err(|e| ReconcileError::VectorStore(Box::new(e)))?;

    // Walk all roots and collect the set of currently existing files.
    let expected_sources = discover_sources(&meta, db_path, &anchored)?;

    // Embed new/changed files, replaying each root's metadata.
    for (source, discovered) in &expected_sources {
        let metadata = &discovered.metadata;
        let prior_hash = store
            .stored_hash(&resolved, source)
            .await
            .map_err(|e| ReconcileError::VectorStore(Box::new(e)))?;
        let text = match super::parse::extract_text(&discovered.path) {
            Ok(t) => t,
            Err(_) => continue, // Unsupported or read error — skip
        };
        let clean_md = super::sanitize_metadata(metadata);
        let hash = super::content_hash(&text, &clean_md);
        if Some(hash.as_str()) != prior_hash.as_deref() {
            store
                .index_file_inner(&resolved, &discovered.path, metadata)
                .await
                .map_err(|e| ReconcileError::VectorStore(Box::new(e)))?;
        }
    }

    // Prune deleted files (in manifest but not on disk).
    let new_source_set: std::collections::HashSet<String> =
        expected_sources.keys().cloned().collect();
    for source in &meta.manifest.sources {
        if !new_source_set.contains(source) {
            store
                .delete_chunks_from(&resolved, source, 0)
                .await
                .map_err(|e| ReconcileError::VectorStore(Box::new(e)))?;
        }
    }

    // Update the manifest with the new source set only after all embeddings and
    // deletes succeeded — a partial reconcile must leave the old manifest and
    // pending marker intact.
    let mut updated = meta.clone();
    updated.manifest.sources = expected_sources.keys().cloned().collect();
    updated.manifest.sources.sort();
    save_metadata(&db, &updated).map_err(|e| ReconcileError::VectorStore(Box::new(e)))?;

    // Clear pending marker and write state after complete success.
    set_pending(db_path, false).map_err(|e| ReconcileError::Interrupted(e.to_string()))?;
    record_state(db_path)?;

    Ok(())
}

/// Discover every source file that should be indexed for this manifest.
///
/// When roots are recorded (the `doc_index` path) each root is resolved against
/// the locally derived repo anchor and discovered per its recursive flag
/// (`.gitignore` respected); each file inherits that root's metadata. Any
/// recorded source not covered by a root (a file indexed directly via
/// `VectorStore::index_file`) is replayed individually if it still exists — an
/// explicit source, never its siblings. With no roots at all, only the recorded
/// sources are replayed, again as individual files: a bare `index_file` must
/// never grow into parent-directory coverage.
fn discover_sources(
    meta: &VectorMetadata,
    db_path: &Path,
    anchor: &Path,
) -> Result<HashMap<String, Discovered>, ReconcileError> {
    let mut expected_sources: HashMap<String, Discovered> = HashMap::new();

    for root in &meta.manifest.roots {
        // Resolve + validate *before* any filesystem/git access: a manifest can
        // never steer a read outside this clone.
        let root_path = resolve_manifest_path(anchor, &root.root)?;
        let Ok(canonical) = root_path.canonicalize() else {
            continue; // Root is gone (or unreadable) — nothing to discover.
        };
        if !canonical.starts_with(anchor) {
            return Err(ReconcileError::InvalidManifest(format!(
                "root {:?} resolves outside the repository",
                root.root
            )));
        }
        let files = discover_files(anchor, &canonical, root.recursive, db_path)
            .map_err(|e| ReconcileError::Interrupted(e.to_string()))?;
        let metadata: HashMap<String, serde_json::Value> =
            root.metadata.clone().into_iter().collect();
        for file in files {
            if let Ok(rel) = super::relative_source(&file, anchor)
                && super::is_supported(&file)
            {
                expected_sources.entry(rel).or_insert_with(|| Discovered {
                    path: file,
                    metadata: metadata.clone(),
                });
            }
        }
    }

    // Explicit standalone sources: files recorded by a direct `index_file`
    // (which never records a root) still belong to the store. Replay exactly
    // those files when they still exist; never walk their parents.
    let empty = HashMap::new();
    let state_dir = state_dir_for(db_path);
    for source in &meta.manifest.sources {
        if expected_sources.contains_key(source) {
            continue;
        }
        let Ok(file) = resolve_manifest_path(anchor, source) else {
            continue;
        };
        let Ok(canonical) = file.canonicalize() else {
            continue;
        };
        if !canonical.starts_with(anchor) {
            return Err(ReconcileError::InvalidManifest(format!(
                "source {source:?} resolves outside the repository"
            )));
        }
        if canonical.starts_with(&state_dir) {
            continue; // Never index our own local state/temp files.
        }
        if canonical.is_file() && super::is_supported(&canonical) {
            expected_sources.insert(
                source.clone(),
                Discovered {
                    path: canonical,
                    metadata: empty.clone(),
                },
            );
        }
    }

    Ok(expected_sources)
}

/// Resolve a manifest path against the local anchor, mapping a structural
/// validation failure to the reconcile error users see.
fn resolve_manifest_path(anchor: &Path, rel: &str) -> Result<PathBuf, ReconcileError> {
    super::resolve_repo_relative(anchor, rel).map_err(ReconcileError::InvalidManifest)
}

/// Record the reconcile state (HEAD + local DB stamp) after a completed run.
pub(crate) fn record_state(db_path: &Path) -> Result<(), ReconcileError> {
    let head = repo_root(db_path)
        .ok()
        .and_then(|anchor| git_head(&anchor).ok())
        .unwrap_or_default();
    let (db_size, db_mtime) = file_stat(db_path).unwrap_or((0, 0));
    // We just wrote this file and know what it is; cache it so the next
    // freshness check does not copy the DB just to re-classify it.
    refresh_classification(db_path, DbKind::Managed);
    let state = ReconcileState {
        last_head: head,
        db_size,
        db_mtime,
        last_reconciled: now_secs(),
    };
    write_state(db_path, &state).map_err(|e| ReconcileError::Interrupted(e.to_string()))
}

/// `ensure_fresh`: detect changes and reconcile if needed.
///
/// Only repo-anchored stores reconcile. Triggers: pending marker (interrupted
/// reconcile), missing local state (state is a cache, not the source of truth),
/// HEAD change (or a HEAD that cannot be resolved, so freshness is unprovable),
/// or DB stamp change. On every trigger the manifest is replayed and per-file
/// hashes are validated. A local (outside-git) store performs none of this: no
/// git, no state, no pending.
///
/// There is deliberately **no filesystem watcher**: an uncommitted edit to an
/// indexed file is picked up when a trigger fires (a merge marker, a HEAD move,
/// a replaced DB, or lost state), not continuously.
///
/// A file that is not a readable store is never opened in place; see
/// [`classify_db`].
pub async fn ensure_fresh(
    store: &super::VectorStore,
    db_path: &Path,
) -> Result<(), ReconcileError> {
    let anchor = StoreAnchor::derive(db_path);
    if !anchor.is_repo() {
        return Ok(());
    }

    // Use the caller's exact resolved path (see reconcile_impl).
    let resolved = super::DbPath(db_path.to_path_buf());

    // Classify from the outside first. A magic-valid but damaged/foreign file is
    // never handed to ruvector (`create` can repair files in place), and a
    // manifest-less replacement is not mistaken for a fresh store.
    match classify_db(db_path).await {
        DbKind::Missing => {
            if is_pending(db_path) {
                set_pending(db_path, false)
                    .map_err(|e| ReconcileError::Interrupted(e.to_string()))?;
            }
            return Ok(());
        }
        DbKind::Damaged => {
            set_pending(db_path, true).map_err(|e| ReconcileError::Interrupted(e.to_string()))?;
            return Err(corrupt_error(
                db_path,
                "file is not a readable vector store (bad magic, unreadable rows, or corrupt v2 metadata)",
            ));
        }
        DbKind::Newer { found } => {
            return Err(ReconcileError::Newer(Box::new(
                VectorStoreError::MetadataNewer {
                    found,
                    supported: super::VECTOR_SCHEMA,
                },
            )));
        }
        DbKind::Fresh => {
            // A valid but empty store with no v2 record. If local state exists,
            // this path *was* a managed store and the file has been replaced:
            // refuse rather than rebuild (and never open it).
            if read_state(db_path)
                .map_err(|e| ReconcileError::Interrupted(e.to_string()))?
                .is_some()
            {
                return Err(ReconcileError::NoManifest {
                    path: db_path.display().to_string(),
                });
            }
            if is_pending(db_path) {
                set_pending(db_path, false)
                    .map_err(|e| ReconcileError::Interrupted(e.to_string()))?;
            }
            return Ok(());
        }
        DbKind::Legacy => {
            // A populated v1 store: read-only compatibility, never migrated and
            // never given whole-root coverage.
            if is_pending(db_path) {
                set_pending(db_path, false)
                    .map_err(|e| ReconcileError::Interrupted(e.to_string()))?;
            }
            return Ok(());
        }
        DbKind::Managed => {}
    }

    // Check for interrupted reconcile. Evict any pooled handle first: a
    // reconcile must always operate on the on-disk inode, never a stale
    // connection from before an interruption/replacement.
    if is_pending(db_path) {
        super::close_db_handle(&resolved);
        return reconcile(store, db_path).await;
    }

    // Load the DB to check metadata. Never create one from a read path.
    let db = match store.db_if_exists(&resolved).await {
        Ok(Some(db)) => db,
        Ok(None) => return Ok(()),
        Err(e) => {
            set_pending(db_path, true).map_err(|e| ReconcileError::Interrupted(e.to_string()))?;
            return Err(corrupt_error(db_path, &format!("failed to open: {e}")));
        }
    };

    // A corrupt v2 record must fail here, never fall through to discovery
    // (which would prune or mix sources). Mark for repair.
    let meta = match load_metadata(&db) {
        Ok(Some(meta)) => meta,
        Ok(None) => return Ok(()), // replaced between classify and open
        Err(e) => {
            set_pending(db_path, true).map_err(|e| ReconcileError::Interrupted(e.to_string()))?;
            return Err(ReconcileError::VectorStore(Box::new(e)));
        }
    };

    // A store relocated across the repo boundary is readable, but replaying its
    // manifest here would mix IDs. Leave search/read alone; writes refuse.
    if check_anchor_metadata(db_path, &meta, &anchor).is_err() {
        return Ok(());
    }

    // An unstable worktree (merge/rebase/conflict) must never be reconciled:
    // defer before any file is read or embedded.
    let anchored = repo_root(db_path)?;
    if git_worktree_unstable(&anchored) {
        return Err(ReconcileError::Deferred(
            "git index has unmerged entries (merge or rebase in progress)".to_string(),
        ));
    }

    let state = read_state(db_path).map_err(|e| ReconcileError::Interrupted(e.to_string()))?;
    let Some(state) = state else {
        // The local state directory is gone (fresh clone, cleaned cache). That
        // is not corruption: evict the pooled handle so reconcile sees the
        // on-disk inode, then replay the manifest and validate every hash.
        //
        // This is a reopen, not a validation-only pass: if the on-disk file was
        // also replaced by a manifest-less DB, that is reported as stale rather
        // than silently accepted or rebuilt.
        drop(db);
        super::close_db_handle(&resolved);
        return reconcile_replaced(store, db_path).await;
    };

    // Check HEAD change. As above, reconcile on a freshly opened on-disk
    // handle; never write through the connection used for the metadata read.
    // An unresolvable HEAD (no commit yet, broken `.git`) means the stamped
    // HEAD cannot prove freshness, so validate rather than trust it.
    let head_changed = match git_head(&anchored) {
        Ok(head) => head != state.last_head,
        Err(_) => true,
    };
    if head_changed {
        drop(db);
        super::close_db_handle(&resolved);
        return reconcile(store, db_path).await;
    }

    // Check DB stamp change. If the file was replaced on disk, evict the cached
    // handle first so reconcile reads/writes the on-disk inode, never the stale
    // one held by a previous process or checkout.
    //
    // ruvector pools its redb `Database` per path keyed by a `Weak`, so we must
    // drop our live `Arc` (and the registry's) before the reopen or the pool
    // hands back the stale connection.
    if db_stamp_changed(db_path, &state) {
        drop(db);
        super::close_db_handle(&resolved);
        return reconcile_replaced(store, db_path).await;
    }

    Ok(())
}

/// Derive the repo root that this DB belongs to. A repo store is always
/// anchored to the repo containing the DB file; local stores never reconcile,
/// so this is only reached for repo anchors.
fn repo_root(db_path: &Path) -> Result<PathBuf, ReconcileError> {
    super::anchored_root_of(db_path).ok_or_else(|| ReconcileError::NotInRepo {
        path: db_path.display().to_string(),
    })
}
