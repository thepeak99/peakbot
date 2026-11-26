//! Git merge driver for a universal-v2 vector DB.
//!
//! Invoked as `peakbot vectors merge-driver %O %A %B %P`, the exact interface
//! a `.gitattributes` merge driver gets from git. The driver runs before any
//! UI boot: it reads no user config, never touches the keyring, and never
//! embeds or reaches the network. Everything it needs is already stored in the
//! two git temp DBs:
//!
//!  * `%O` (ancestor) — **ignored**; it may be absent or garbage.
//!  * `%A` (ours/current) — read, merged, and replaced atomically.
//!  * `%B` (theirs) — read only.
//!  * `%P` — working-tree path. It is the **anchor source**: the driver derives
//!    repo-vs-local from it (never from the temporary `%A`/`%B` locations) and
//!    uses it to locate `<P>.peakbot/pending`.
//!
//! Merge semantics (see `tests/vector_merge_driver.rs`):
//!
//!  * A source group is keyed by its whole `metadata.source`; overlapping
//!    groups are taken from ours verbatim (vector, text, metadata).
//!  * Theirs-only groups are adopted with their original `VectorEntry` values.
//!  * Manifests union deterministically: roots sorted by path with ours winning
//!    an identical root; the global `sources` list is a sorted deduplicated
//!    union.
//!  * Both inputs must match the anchor derived from `%P`. An unusable `%B`
//!    (v1, foreign fingerprint, wrong anchor, corrupt) keeps ours without
//!    mixing. A corrupt `%A` may fall back to a usable `%B`. With both
//!    unusable, `%A` is left byte-identical.
//!  * Every attempt on a repo-anchored `%P` marks `<P>.peakbot/pending`
//!    best-effort, so the next user turn replays the manifest and repairs
//!    whatever was left behind. A local store keeps no state.
//!
//! The result is written to `%A` through a temp sibling plus an atomic rename:
//! a crash mid-merge can never leave a half-written database in the working
//! tree. We never open `%A` at its own path (only its copy), so no pooled
//! ruvector handle can keep writing to the inode git is about to replace.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ruvector_core::types::{DbOptions, DistanceMetric, HnswConfig, VectorEntry};
use ruvector_core::vector_db::VectorDB;
use thiserror::Error;

use super::reconcile::{DbFileHealth, db_file_health, set_pending};
use super::{
    ManifestRoot, StoreAnchor, VECTOR_SCHEMA, VectorManifest, VectorMetadata,
    check_anchor_metadata, load_metadata, save_metadata,
};

/// Capacity hint matching the stores this driver merges (a no-op for the
/// default flat index, kept in sync with `vector::db_options`).
const HNSW_MAX_ELEMENTS: usize = 100_000;

#[derive(Debug, Error)]
pub enum MergeDriverError {
    /// `%A` does not exist. Git always materializes the current side, so a
    /// missing file is a protocol error, not a mergeable state.
    #[error("vector merge driver: current database %A at {path} does not exist")]
    MissingCurrent { path: String },
    #[error("vector merge driver: failed to {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("vector merge driver: failed to merge {path}: {detail}")]
    Merge { path: String, detail: String },
}

/// Run the merge driver for `[%O, %A, %B, %P]`.
///
/// `Ok(())` means git can take the result (compatible merge, resolved overlap,
/// or a deliberate "keep ours"); `Err` is a real failure worth surfacing to the
/// user. The pending marker is attempted on every path, including failures.
pub async fn run(args: [&str; 4]) -> Result<(), MergeDriverError> {
    let [_original, current, theirs, working] = args;
    let working = Path::new(working);
    // The anchor comes from the working-tree path git passes as `%P`, never
    // from the temporary `%A`/`%B` copies: git may stage them outside the repo,
    // while the working file still belongs to it.
    let anchor = StoreAnchor::derive(working);
    let result = merge(Path::new(current), Path::new(theirs), &anchor);
    // Always ask the next turn to reconcile: a compatible merge can still have
    // changed rows, and an unusable side must be repaired from the worktree.
    // A local (outside-git) store keeps no state, so skip the marker there.
    if anchor.is_repo() {
        mark_pending(working);
    }
    result
}

/// Best-effort `<P>.peakbot/pending`. This is the canonical marker
/// `reconcile::ensure_fresh` reads, so we reuse `set_pending` rather than
/// inventing a second path rule. A failure only warns — git must not fail a
/// merge over a cache marker.
fn mark_pending(working: &Path) {
    if let Err(e) = set_pending(working, true) {
        // The driver runs before tracing is initialized on the CLI path, so a
        // `tracing::warn!` would be invisible; print to stderr directly.
        eprintln!(
            "warning: vector merge driver could not mark {} pending: {e}",
            working.display()
        );
    }
}

fn merge(current: &Path, theirs: &Path, anchor: &StoreAnchor) -> Result<(), MergeDriverError> {
    match fs::metadata(current) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => {
            return Err(MergeDriverError::MissingCurrent {
                path: current.display().to_string(),
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(MergeDriverError::MissingCurrent {
                path: current.display().to_string(),
            });
        }
        Err(source) => {
            return Err(MergeDriverError::Io {
                action: "stat",
                path: current.display().to_string(),
                source,
            });
        }
    }

    // A corrupt %A has no vectors to preserve. Adopting a usable %B wholesale
    // is a safe, well-defined recovery; otherwise leave the bytes for
    // inspection (the pending marker drives repair on the next turn).
    if db_file_health(current) == DbFileHealth::Corrupt {
        if usable_side(theirs, anchor).is_some() {
            return replace_with_file(current, theirs);
        }
        return Ok(());
    }

    // Merge into a private copy of %A. Nothing touches the real file until the
    // result is complete.
    let temp = temp_sibling(current);
    if let Err(source) = fs::copy(current, &temp) {
        return Err(MergeDriverError::Io {
            action: "copy",
            path: current.display().to_string(),
            source,
        });
    }

    let outcome = merge_into_copy(&temp, theirs, anchor);
    match outcome {
        Ok(true) => replace_with_temp(&temp, current),
        Ok(false) => {
            // Deliberate keep-ours: discard the copy, leave %A untouched.
            remove_temp(&temp);
            Ok(())
        }
        Err(e) => {
            remove_temp(&temp);
            Err(e)
        }
    }
}

/// Merge `theirs` into the temp copy of ours. Returns `Ok(true)` when the copy
/// holds a result that should replace `%A`, `Ok(false)` when ours must be kept
/// as-is.
fn merge_into_copy(
    temp: &Path,
    theirs: &Path,
    anchor: &StoreAnchor,
) -> Result<bool, MergeDriverError> {
    let ours_db = match open_db(temp) {
        Ok(db) => db,
        Err(detail) => {
            // The copy carries a plausible header but will not open. Treat ours
            // as unusable for merging; keep the original.
            warn_unusable(temp, &detail);
            return Ok(false);
        }
    };

    let ours_meta = match load_metadata(&ours_db) {
        Ok(Some(meta)) => meta,
        Ok(None) => {
            // A v1/legacy DB has no v2 contract to merge against.
            return Ok(false);
        }
        Err(e) => {
            warn_unusable(temp, &e.to_string());
            return Ok(false);
        }
    };
    if ours_meta.schema != VECTOR_SCHEMA {
        return Ok(false);
    }
    // Both inputs must follow the same anchor contract as the working file.
    // A mismatch means git is merging copies of a store from a different anchor contract;
    // mixing their IDs would corrupt the source groups.
    if check_anchor_metadata(temp, &ours_meta, anchor).is_err() {
        return Ok(false);
    }

    let ours_rows = match collect_rows(&ours_db) {
        Ok(rows) => rows,
        Err(detail) => {
            warn_unusable(temp, &detail);
            return Ok(false);
        }
    };
    // A whole metadata.source group is overlapping if either side records the
    // source — the manifest is authoritative, rows are the safety net.
    let mut ours_sources: BTreeSet<String> = ours_meta.manifest.sources.iter().cloned().collect();
    ours_sources.extend(ours_rows.iter().filter_map(source_of).map(str::to_string));

    let Some((theirs_meta, theirs_rows)) = usable_side(theirs, anchor) else {
        return Ok(false);
    };
    // Only a store in the same embedding space may be mixed in. Schema is
    // part of the fingerprint, but checking it explicitly documents intent.
    if theirs_meta.schema != VECTOR_SCHEMA || theirs_meta.fingerprint != ours_meta.fingerprint {
        return Ok(false);
    }

    // Adopt every row of a theirs-only group, verbatim. A naive id-based merge
    // would leak extra chunks into an overlapping group, so overlap is decided
    // per source, never per row.
    let mut adopted: Vec<VectorEntry> = Vec::new();
    for entry in theirs_rows {
        let Some(source) = source_of(&entry) else {
            // A row without a source id cannot be grouped safely; skip it.
            continue;
        };
        if ours_sources.contains(source) {
            continue;
        }
        let Some(id) = entry.id.as_deref() else {
            continue;
        };
        match ours_db.get(id) {
            Ok(Some(_)) => {}
            Ok(None) => adopted.push(entry),
            Err(detail) => {
                warn_unusable(temp, &detail.to_string());
                return Ok(false);
            }
        }
    }

    if !adopted.is_empty() {
        ours_db
            .insert_batch(&adopted)
            .map_err(|e| MergeDriverError::Merge {
                path: temp.display().to_string(),
                detail: format!("failed to copy theirs-only vectors: {e}"),
            })?;
    }

    let merged = merged_metadata(&ours_meta, &theirs_meta, &adopted);
    save_metadata(&ours_db, &merged).map_err(|e| MergeDriverError::Merge {
        path: temp.display().to_string(),
        detail: format!("failed to write merged manifest: {e}"),
    })?;

    // Drop the pooled handle before the rename: redb commits/fsyncs/unlocks on
    // drop, and a still-open handle on the temp inode would race the replace.
    drop(ours_db);
    Ok(true)
}

/// Union the two manifests. Ours wins an overlapping root; sources are the
/// sorted
/// union of both sides (plus adopted rows, defensively). Both sides' roots are
/// already repo-relative POSIX ids, so a union is meaningful across clones.
fn merged_metadata(
    ours: &VectorMetadata,
    theirs: &VectorMetadata,
    adopted: &[VectorEntry],
) -> VectorMetadata {
    let mut roots: BTreeMap<String, ManifestRoot> = BTreeMap::new();
    for root in &ours.manifest.roots {
        roots.insert(root.root.clone(), root.clone());
    }
    for root in &theirs.manifest.roots {
        // Ours wins an identical root path.
        roots
            .entry(root.root.clone())
            .or_insert_with(|| root.clone());
    }

    let mut sources: BTreeSet<String> = ours.manifest.sources.iter().cloned().collect();
    sources.extend(theirs.manifest.sources.iter().cloned());
    sources.extend(adopted.iter().filter_map(source_of).map(str::to_string));

    VectorMetadata {
        schema: ours.schema,
        anchor: ours.anchor,
        fingerprint: ours.fingerprint.clone(),
        manifest: VectorManifest {
            roots: roots.into_values().collect(),
            sources: sources.into_iter().collect(),
        },
    }
}

/// Read a DB that is plausible enough to merge from. `None` means the file is
/// missing/corrupt, is not a v2 store, or cannot be read — all cases
/// where ours must be kept without mixing.
fn usable_side(path: &Path, anchor: &StoreAnchor) -> Option<(VectorMetadata, Vec<VectorEntry>)> {
    if db_file_health(path) != DbFileHealth::Valid {
        return None;
    }
    let db = match open_db(path) {
        Ok(db) => db,
        Err(detail) => {
            warn_unusable(path, &detail);
            return None;
        }
    };
    match load_metadata(&db) {
        Ok(Some(meta)) => {
            if check_anchor_metadata(path, &meta, anchor).is_err() {
                warn_unusable(path, "metadata anchor does not match the working tree");
                return None;
            }
            match collect_rows(&db) {
                Ok(rows) => Some((meta, rows)),
                Err(detail) => {
                    warn_unusable(path, &detail);
                    None
                }
            }
        }
        Ok(None) => None,
        Err(e) => {
            warn_unusable(path, &e.to_string());
            None
        }
    }
}

/// Open an existing vector DB. Dimensions are irrelevant on reopen (ruvector
/// reuses the stored config); the health probe has already guaranteed the file
/// exists and carries a redb header, so this never initializes a garbage file.
fn open_db(path: &Path) -> Result<VectorDB, String> {
    let options = DbOptions {
        dimensions: 1,
        distance_metric: DistanceMetric::Cosine,
        storage_path: path.display().to_string(),
        hnsw_config: Some(HnswConfig {
            max_elements: HNSW_MAX_ELEMENTS,
            ..HnswConfig::default()
        }),
        ..Default::default()
    };
    VectorDB::new(options).map_err(|e| e.to_string())
}

fn collect_rows(db: &VectorDB) -> Result<Vec<VectorEntry>, String> {
    let keys = db.keys().map_err(|e| e.to_string())?;
    let mut rows = Vec::with_capacity(keys.len());
    for id in keys {
        if let Some(entry) = db.get(&id).map_err(|e| e.to_string())? {
            rows.push(entry);
        }
    }
    Ok(rows)
}

fn source_of(entry: &VectorEntry) -> Option<&str> {
    entry
        .metadata
        .as_ref()?
        .get("source")?
        .as_str()
        .filter(|s| !s.is_empty())
}

/// Warn on stderr. The driver runs before tracing is initialized on the CLI
/// path, so `tracing::warn!` would be invisible here.
fn warn_unusable(path: &Path, detail: &str) {
    eprintln!(
        "warning: vector merge driver: treating {} as unusable and keeping ours: {detail}",
        path.display()
    );
}

/// A collision-free temp sibling of `path` (same directory, so the final
/// rename stays on one filesystem and is atomic).
fn temp_sibling(path: &Path) -> PathBuf {
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
    path.with_file_name(format!(
        ".{name}.merge-{}-{nanos}-{seq}.tmp",
        std::process::id()
    ))
}

fn remove_temp(temp: &Path) {
    match fs::remove_file(temp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!(
            "warning: vector merge driver: could not remove temp file {}: {e}",
            temp.display()
        ),
    }
}

/// Copy a clean result over `%A`, fsyncing the temp first so the rename cannot
/// publish a torn file.
fn replace_with_temp(temp: &Path, current: &Path) -> Result<(), MergeDriverError> {
    if let Ok(file) = fs::File::open(temp) {
        let _ = file.sync_all();
    }
    fs::rename(temp, current).map_err(|source| MergeDriverError::Io {
        action: "replace",
        path: current.display().to_string(),
        source,
    })
}

/// Byte-copy `source` over `current` via a temp sibling. Used only for the
/// corrupt-ours fallback, where we must not hand the corrupt file to ruvector.
fn replace_with_file(current: &Path, source: &Path) -> Result<(), MergeDriverError> {
    let temp = temp_sibling(current);
    if let Err(e) = fs::copy(source, &temp) {
        // The fallback is optional: our own file is unusable anyway, so a copy
        // failure leaves it for the next-turn repair instead of failing git.
        remove_temp(&temp);
        eprintln!(
            "warning: vector merge driver: could not copy usable theirs into {}: {e}",
            current.display()
        );
        return Ok(());
    }
    if let Ok(file) = fs::File::open(&temp) {
        let _ = file.sync_all();
    }
    match fs::rename(&temp, current) {
        Ok(()) => Ok(()),
        Err(e) => {
            remove_temp(&temp);
            eprintln!(
                "warning: vector merge driver: could not replace corrupt {}: {e}",
                current.display()
            );
            Ok(())
        }
    }
}
