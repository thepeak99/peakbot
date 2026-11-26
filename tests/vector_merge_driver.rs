//! PR1 vector DB: merge-driver logic tests (TDD — written before implementation).
//!
//! ## Locked driver contract (from the final PR1 spec)
//!
//! The single binary entry point is:
//!
//! ```text
//! peakbot vectors merge-driver %O %A %B %P
//! ```
//!
//! with standard Git merge-driver placeholder semantics:
//!
//!  * `%O` — ancestor/“base” temp file. **Ignored.** It is allowed to be
//!    absent or garbage: the driver never reads it.
//!  * `%A` — the *current* (ours) temp file. The driver reads it and writes
//!    the merged result back into it.
//!  * `%B` — the *other* (theirs) temp file.
//!  * `%P` — the working-tree pathname of the file being merged. Used to
//!    locate the repair marker at `<P>.peakbot/pending`.
//!
//! ### What the driver may and may not do
//!
//! * Git temp inputs only. It must not read user config, boot the app, touch
//!   the OS keyring, or reach the network / embeddings endpoint.
//! * It must not read the indexed *source* files on disk — source ids/vectors
//!   are copied from the DBs, not re-derived from files.
//! * On every attempt it marks `<P>.peakbot/pending` (so the next user turn
//!   reconciles); if that marker cannot be written it warns but still succeeds.
//!
//! ### Merge semantics
//!
//! * Overlapping source groups (same repo-relative source id in ours and
//!   theirs) — **ours wins**, whole vectors/text/metadata.
//! * Theirs-only sources — adopted with identical vectors/text/metadata.
//! * Ours-only sources — retained.
//! * Multi-root manifests — union deterministically; for the same root path
//!   **ours wins** (recursive flag + metadata). Global `sources` are the sorted
//!   union.
//! * A compatible merge and a resolved overlap both *succeed* (exit 0).
//! * Missing v2 metadata / a v1 `%B` / fingerprint mismatch / a corrupt `%B`:
//!   keep ours without mixing, leave pending, and succeed whenever ours is
//!   usable.
//! * Corrupt `%A` + valid `%B`: may copy theirs as a safe fallback.
//! * Both unusable: leave ours and pending set, without crashing.
//!
//! ## Assumed Rust interface (stated here so the builder can align)
//!
//! ```ignore
//! // src/vector/merge_driver.rs
//! pub async fn run(args: [&str; 4]) -> Result<(), MergeDriverError>;
//! ```
//!
//! `Ok(())` ⇒ exit 0 (success / resolved), `Err(_)` ⇒ a real error (bad
//! arguments, `%A` unreadable). The binary maps `Ok`→0 and `Err`→non-zero and
//! prints the message. We drive this directly (in-process) so the redb handle
//! cache is shared and there is no cross-process lock.
//!
//! Content is inspected through the raw DB handle so reads never trigger
//! `ensure_fresh` — the driver's output is examined exactly as written, not
//! reconciled. The manifest is read through the same reconcile-free handle.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use peakbot::config::EmbeddingsConfig;
use peakbot::config::resolve::{DbPathSpec, VectorSpec};
use peakbot::vector::{DbPath, EmbeddingsClient, Hit, VectorStore};

/// ruvector config key holding the v2 record.
const META_KEY: &str = "peakbot.vector";

// ── Specs ──────────────────────────────────────────────────────────────

fn spec_at(db: PathBuf, url: &str, model: &str) -> VectorSpec {
    VectorSpec {
        db_path: DbPathSpec::Fixed(db),
        embeddings: EmbeddingsConfig {
            base_url: url.to_string(),
            api_key: None,
            model: model.to_string(),
            dimensions: 3,
        },
    }
}

fn mark_repo(dir: &Path) {
    std::fs::create_dir_all(dir.join(".git")).expect("create .git marker");
}

// ── Building input stores (in-process, with a mock embeddings endpoint) ──

/// Open (or create) a v2 store anchored to `repo`, returning it and its
/// resolved DB path. The caller indexes sources through it.
fn open_store(repo: &Path, db: &Path, url: &str) -> (VectorStore, DbPath) {
    mark_repo(repo);
    let store = VectorStore::open(&spec_at(db.to_path_buf(), url, "test-model")).unwrap();
    let dp = store.db_path_for(Path::new(""));
    (store, dp)
}

/// Write `content` to `repo/rel` and index it into a v2 store.
async fn index_one(
    store: &VectorStore,
    dp: &DbPath,
    repo: &Path,
    rel: &str,
    content: &str,
    md: &HashMap<String, serde_json::Value>,
) {
    let p = repo.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&p, content).unwrap();
    store.index_file(dp, &p, md).await.expect("index source");
}

/// Read the v2 manifest from a DB file without triggering reconcile.
async fn manifest(db: &Path, url: &str) -> serde_json::Value {
    let store = VectorStore::open(&spec_at(db.to_path_buf(), url, "test-model")).unwrap();
    let handle = store.handle_for_test(Path::new("")).await.unwrap();
    let raw = handle
        .load_config_value(META_KEY)
        .expect("read config value")
        .expect("v2 metadata present");
    serde_json::from_str(&raw).expect("metadata is valid JSON")
}

/// Search a DB file through its raw handle, bypassing turn-start reconcile:
/// the merge driver's output must be inspected exactly as written.
async fn search_local(db: &Path, url: &str, query: &str) -> Vec<Hit> {
    let spec = spec_at(db.to_path_buf(), url, "test-model");
    let store = VectorStore::open(&spec).unwrap();
    let handle = store.handle_for_test(Path::new("")).await.unwrap();
    let client = EmbeddingsClient::new(&spec.embeddings);
    let vector = client
        .embed(&[query.to_string()])
        .await
        .unwrap()
        .pop()
        .unwrap();
    let results = handle
        .search(ruvector_core::types::SearchQuery {
            vector,
            k: 100,
            filter: None,
            ef_search: None,
        })
        .unwrap();
    results
        .into_iter()
        .map(|r| {
            let md = r.metadata.unwrap_or_default();
            Hit {
                source: md
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                chunk_index: md.get("chunk_index").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                text: md
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                score: r.score,
                metadata: HashMap::new(),
            }
        })
        .collect()
}

fn sources_of(hits: &[Hit]) -> Vec<String> {
    let mut v: Vec<String> = hits.iter().map(|h| h.source.clone()).collect();
    v.sort();
    v.dedup();
    v
}

/// `<P>.peakbot` — the per-DB state directory location.
fn state_dir(p: &Path) -> PathBuf {
    PathBuf::from(format!("{}.peakbot", p.display()))
}

fn pending_marker(p: &Path) -> PathBuf {
    state_dir(p).join("pending")
}

fn assert_pending(p: &Path, context: &str) {
    assert!(
        pending_marker(p).exists(),
        "{context}: the driver must always mark <P>.peakbot/pending (looked at {})",
        pending_marker(p).display()
    );
}

/// Run the driver; `true` iff it reported success (`Ok`, exit 0).
async fn driver_ok(o: &str, a: &str, b: &str, p: &str) -> bool {
    peakbot::vector::merge_driver::run([o, a, b, p])
        .await
        .is_ok()
}

async fn spawn_embeddings(dimensions: usize) -> String {
    let app = Router::new()
        .route("/embeddings", post(embed_handler))
        .with_state(dimensions);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock embeddings");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://{addr}")
}

fn fake_vector(text: &str, dimensions: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; dimensions];
    for (i, b) in text.bytes().enumerate() {
        v[i % dimensions] += f32::from(b);
    }
    if v.iter().all(|x| *x == 0.0) {
        v[0] = 1.0;
    }
    v
}

/// Build a genuine v1 store directly via ruvector: a row with an absolute
/// source path and **no** `peakbot.vector` metadata key. This is what a
/// pre-v2 store looks like, and the merge driver must treat it as unusable.
fn build_v1_store(db: &Path, source: &Path, content: &str) {
    let opts = ruvector_core::types::DbOptions {
        dimensions: 3,
        distance_metric: ruvector_core::types::DistanceMetric::Cosine,
        storage_path: db.display().to_string(),
        hnsw_config: Some(ruvector_core::types::HnswConfig {
            max_elements: 100_000,
            ..ruvector_core::types::HnswConfig::default()
        }),
        ..Default::default()
    };
    let raw = ruvector_core::vector_db::VectorDB::new(opts).expect("create v1 db");
    let mut metadata = HashMap::new();
    metadata.insert(
        "source".to_string(),
        serde_json::json!(source.display().to_string()),
    );
    metadata.insert("chunk_index".to_string(), serde_json::json!(0));
    metadata.insert("text".to_string(), serde_json::json!(content));
    metadata.insert("sha256".to_string(), serde_json::json!("v1-hash"));
    raw.insert_batch(&[ruvector_core::types::VectorEntry {
        id: None,
        vector: fake_vector(content, 3),
        metadata: Some(metadata),
    }])
    .expect("insert v1 row");
}

async fn embed_handler(
    State(dimensions): State<usize>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let inputs = body["input"].as_array().cloned().unwrap_or_default();
    let data: Vec<serde_json::Value> = inputs
        .iter()
        .map(|input| {
            let text = input.as_str().unwrap_or_default();
            serde_json::json!({ "embedding": fake_vector(text, dimensions) })
        })
        .collect();
    Json(serde_json::json!({ "data": data }))
}

// ── Fixture: our DB at `a` (temp %A, fresh inode), theirs at `b`, P = `p` ──
//
// We never open `a` through `VectorStore` before the driver runs: it is a byte
// copy of the built store, so the driver opens a fresh inode with no cached
// handle. `p` is the logical working-tree path used only for the pending
// marker.

struct Fixture {
    repo: tempfile::TempDir,
    a: PathBuf,
    b: PathBuf,
    p: PathBuf,
    o: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let repo = tempfile::tempdir().unwrap();
        mark_repo(repo.path());
        let a = repo.path().join("a.db");
        let b = repo.path().join("b.db");
        let p = repo.path().join("vectors.db");
        let o = repo.path().join("base.db"); // intentionally absent
        Self { repo, a, b, p, o }
    }

    /// Build OUR store with standalone sources, then copy it to `%A`.
    async fn build_ours(&self, url: &str, sources: &[(&str, &str)]) {
        let src = self.repo.path().join("ours-src.db");
        let (store, dp) = open_store(self.repo.path(), &src, url);
        for (rel, content) in sources {
            index_one(&store, &dp, self.repo.path(), rel, content, &HashMap::new()).await;
        }
        drop(store);
        std::fs::copy(&src, &self.a).expect("copy ours to %A");
    }

    /// Build THEIR store with standalone sources.
    async fn build_theirs(&self, url: &str, sources: &[(&str, &str)]) {
        let (store, dp) = open_store(self.repo.path(), &self.b, url);
        for (rel, content) in sources {
            index_one(&store, &dp, self.repo.path(), rel, content, &HashMap::new()).await;
        }
        drop(store);
    }
}

// ══════════════════════════════════════════════════════════════════════════
// Happy path
// ══════════════════════════════════════════════════════════════════════════

/// Compatible merge: no overlapping sources. Ours-only stays, theirs-only is
/// adopted, and the merged result is written back into `%A`. Pending is set.
#[tokio::test]
async fn compatible_merge_keeps_ours_and_adopts_theirs_only() {
    let url = spawn_embeddings(3).await;
    let fx = Fixture::new();
    fx.build_ours(&url, &[("ours.txt", "ours body")]).await;
    fx.build_theirs(&url, &[("theirs.txt", "theirs body")])
        .await;

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(ok, "a compatible merge must report success");

    let hits = search_local(&fx.a, &url, "body").await;
    assert_eq!(
        sources_of(&hits),
        vec!["ours.txt".to_string(), "theirs.txt".to_string()],
        "the merged %A must contain ours-only and theirs-only sources"
    );
    assert_pending(&fx.p, "compatible merge");
}

/// `%O` (the ancestor) is ignored: nonexistent is fine.
#[tokio::test]
async fn missing_original_file_is_ignored() {
    let url = spawn_embeddings(3).await;
    let fx = Fixture::new();
    assert!(!fx.o.exists(), "fixture precondition: %O absent");
    fx.build_ours(&url, &[("ours.txt", "ours body")]).await;
    fx.build_theirs(&url, &[("theirs.txt", "theirs body")])
        .await;

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(ok, "a missing %O must not be an error");
}

/// `%O` may even hold garbage; it is never read.
#[tokio::test]
async fn garbage_original_file_is_ignored() {
    let url = spawn_embeddings(3).await;
    let fx = Fixture::new();
    std::fs::write(&fx.o, b"not a database, ignored").unwrap();
    fx.build_ours(&url, &[("ours.txt", "ours body")]).await;
    fx.build_theirs(&url, &[("theirs.txt", "theirs body")])
        .await;

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(ok, "a garbage %O must be ignored, not read");
    let hits = search_local(&fx.a, &url, "body").await;
    assert_eq!(
        sources_of(&hits),
        vec!["ours.txt".to_string(), "theirs.txt".to_string()]
    );
}

// ══════════════════════════════════════════════════════════════════════════
// Overlap: ours wins, theirs discarded, still success
// ══════════════════════════════════════════════════════════════════════════

/// The same repo-relative source id in both sides: ours' vectors/text win and
/// theirs' version of that source is discarded. Non-overlapping sources merge.
#[tokio::test]
async fn overlapping_source_group_ours_wins_and_still_succeeds() {
    let url = spawn_embeddings(3).await;
    let fx = Fixture::new();
    fx.build_ours(
        &url,
        &[("both.txt", "ours version"), ("solo.txt", "only ours")],
    )
    .await;
    fx.build_theirs(&url, &[("both.txt", "theirs version")])
        .await;

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(ok, "a resolved overlap must report success");

    // Exactly one both.txt, carrying OUR text; theirs must not appear.
    let hits = search_local(&fx.a, &url, "version").await;
    let both: Vec<&Hit> = hits.iter().filter(|h| h.source == "both.txt").collect();
    assert_eq!(both.len(), 1, "exactly one overlapping source survives");
    assert!(
        both[0].text.contains("ours version"),
        "ours wins the overlap, got: {}",
        both[0].text
    );
    assert!(
        hits.iter().all(|h| !h.text.contains("theirs version")),
        "theirs' overlapping vector must be discarded: {:?}",
        hits.iter().map(|h| &h.text).collect::<Vec<_>>()
    );
    assert_pending(&fx.p, "resolved overlap");
}

// ══════════════════════════════════════════════════════════════════════════
// Theirs-only adoption preserves vectors/text/metadata and reads no files
// ══════════════════════════════════════════════════════════════════════════

/// Read every stored row for each source from a DB file via its raw ruvector
/// handle. Used to prove exact row copying: identical `VectorEntry`s (id,
/// vector, metadata) cannot be faked by an equal-text search score, so this
/// directly verifies the "whole metadata.source group" merge contract.
async fn raw_rows_by_source(db: &Path, url: &str) -> HashMap<String, Vec<serde_json::Value>> {
    let store = VectorStore::open(&spec_at(db.to_path_buf(), url, "test-model")).unwrap();
    let handle = store.handle_for_test(Path::new("")).await.unwrap();
    let mut out: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    for id in handle.keys().unwrap() {
        let entry = handle.get(&id).unwrap().unwrap();
        let md = entry.metadata.unwrap_or_default();
        let source = md
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        // Serialize the row to JSON so vectors (f32 slices) and maps compare
        // by value regardless of internal ordering. chunk_index is included in
        // the map, so the row list can be sorted to compare groups.
        out.entry(source).or_default().push(serde_json::json!({
            "id": id,
            "vector": entry.vector,
            "metadata": md,
        }));
    }
    for rows in out.values_mut() {
        rows.sort_by(|a, b| {
            let ia = a["metadata"]["chunk_index"].as_u64().unwrap_or(0);
            let ib = b["metadata"]["chunk_index"].as_u64().unwrap_or(0);
            ia.cmp(&ib)
        });
    }
    out
}

/// A theirs-only source is adopted byte-for-byte: identical text, identical
/// user metadata, and an identical stored vector (proved by comparing the
/// exact-text search score before and after the merge — the driver cannot
/// re-embed, so equal scores mean the vector was copied).
#[tokio::test]
async fn theirs_only_source_is_adopted_with_identical_vector_text_metadata() {
    let url = spawn_embeddings(3).await;
    let fx = Fixture::new();
    fx.build_ours(&url, &[("ours.txt", "ours body")]).await;

    // Theirs-only source with custom metadata.
    let src = fx.repo.path().join("theirs-src.db");
    let (store, dp) = open_store(fx.repo.path(), &src, &url);
    let mut md = HashMap::new();
    md.insert("author".to_string(), serde_json::json!("carol"));
    index_one(
        &store,
        &dp,
        fx.repo.path(),
        "adopted.txt",
        "adopted body",
        &md,
    )
    .await;
    drop(store);
    std::fs::copy(&src, &fx.b).unwrap();

    // Score of the adopted text in the SOURCE db, before merge.
    let before = search_local(&fx.b, &url, "adopted body").await;
    let before_hit = before
        .iter()
        .find(|h| h.source == "adopted.txt")
        .expect("adopted text present in theirs");
    let before_score = before_hit.score;
    let before_md = before_hit.metadata.get("author").cloned();

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(ok, "adoption is a compatible merge");

    let after = search_local(&fx.a, &url, "adopted body").await;
    let after_hit = after
        .iter()
        .find(|h| h.source == "adopted.txt")
        .expect("adopted source present in merged %A");

    assert_eq!(
        after_hit.text, "adopted body",
        "adopted text must be identical"
    );
    assert_eq!(
        after_hit.metadata.get("author").cloned(),
        before_md,
        "adopted user metadata must be identical"
    );
    assert!(
        (after_hit.score - before_score).abs() < 1e-6,
        "adopted vector must be identical (score {} vs {}); the driver cannot \
         re-embed, so equal scores prove a byte-copied vector",
        before_score,
        after_hit.score,
    );

    // Stronger proof: compare the exact stored entries (vector + all metadata)
    // from theirs' DB to ours after the merge. If the driver had dropped or
    // altered the row, either vector bytes or the sha256/metadata would differ.
    let theirs_entries = raw_rows_by_source(&fx.b, &url).await;
    let merged_entries = raw_rows_by_source(&fx.a, &url).await;
    assert_eq!(
        merged_entries.get("adopted.txt"),
        theirs_entries.get("adopted.txt"),
        "the adopted group's vectors and metadata must be byte-identical to theirs"
    );
}

/// Overlapping whole-group semantics with multiple chunks: if ours has fewer
/// chunks than theirs for the same source, the merge must not leak theirs'
/// extra chunk(s) into the group. Ours wins, period.
#[tokio::test]
async fn overlapping_multichunk_group_ours_wins_whole_group() {
    let url = spawn_embeddings(3).await;
    let fx = Fixture::new();

    // Ours: 1500 chars → 2 chunks. Theirs: 2500 chars → 3 chunks. Different
    // content so the chunk vectors are distinct.
    let ours_text: String = "o".repeat(1500);
    let theirs_text: String = "t".repeat(2500);
    let src = fx.repo.path().join("ours-src.db");
    let (store, dp) = open_store(fx.repo.path(), &src, &url);
    std::fs::write(fx.repo.path().join("both.txt"), &ours_text).unwrap();
    store
        .index_file(&dp, &fx.repo.path().join("both.txt"), &HashMap::new())
        .await
        .unwrap();
    drop(store);
    std::fs::copy(&src, &fx.a).unwrap();

    let (store, dp) = open_store(fx.repo.path(), &fx.b, &url);
    std::fs::write(fx.repo.path().join("both.txt"), &theirs_text).unwrap();
    store
        .index_file(&dp, &fx.repo.path().join("both.txt"), &HashMap::new())
        .await
        .unwrap();
    drop(store);

    let ours_entries_before = raw_rows_by_source(&fx.a, &url).await;

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(ok, "resolved overlap must succeed");

    let ours_entries_after = raw_rows_by_source(&fx.a, &url).await;
    assert_eq!(
        ours_entries_after.get("both.txt"),
        Some(&ours_entries_before["both.txt"]),
        "the overlapping group must be byte-identical to ours (2 chunks, no leaked 3rd)"
    );
    assert_eq!(
        ours_entries_after["both.txt"].len(),
        2,
        "ours had 2 chunks; theirs had 3 — exactly 2 must survive"
    );
}

/// The driver must not read the original source files: theirs' source file is
/// deleted before the merge, yet its vector/text are still adopted.
#[tokio::test]
async fn their_source_file_is_never_read_during_merge() {
    let url = spawn_embeddings(3).await;
    let fx = Fixture::new();
    fx.build_ours(&url, &[("ours.txt", "ours body")]).await;
    fx.build_theirs(&url, &[("theirs.txt", "theirs body")])
        .await;

    // Remove the source files from disk — neither may be read.
    std::fs::remove_file(fx.repo.path().join("theirs.txt")).unwrap();
    std::fs::remove_file(fx.repo.path().join("ours.txt")).ok();

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(ok, "merge must not depend on source files existing");

    let hits = search_local(&fx.a, &url, "body").await;
    assert!(
        hits.iter().any(|h| h.text.contains("theirs body")),
        "theirs-only vector must be adopted with no source file on disk"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// Multi-root manifest: deterministic union, ours wins same root
// ══════════════════════════════════════════════════════════════════════════

/// Two DBs of the SAME repo record overlapping roots. The merged manifest has
/// one entry per root, ours' recursive/metadata winning, plus each side's
/// unique root. Roots are deterministically ordered.
#[cfg(feature = "host")]
#[tokio::test]
async fn multi_root_manifest_unions_deterministically_ours_wins_same_root() {
    use peakbot::tools::DocIndexTool;
    use rig_core::tool::Tool;

    let url = spawn_embeddings(3).await;
    let repo = tempfile::tempdir().unwrap();
    mark_repo(repo.path());

    // Both DBs index the same physical files.
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::create_dir_all(repo.path().join("docs")).unwrap();
    std::fs::create_dir_all(repo.path().join("docs-theirs")).unwrap();
    std::fs::write(repo.path().join("src/a.rs"), "fn a() {}").unwrap();
    std::fs::write(repo.path().join("docs/g.md"), "# g").unwrap();
    std::fs::write(repo.path().join("docs-theirs/t.md"), "# t").unwrap();

    // OUR store: src(recursive=true, author=alice) + docs(recursive=true).
    let ours_src = repo.path().join("ours-src.db");
    let (ours, _dp) = open_store(repo.path(), &ours_src, &url);
    let mut alice = HashMap::new();
    alice.insert("author".to_string(), serde_json::json!("alice"));
    for (path, recursive) in [("src", true), ("docs", true)] {
        let tool = DocIndexTool::new(ours.clone()).with_session_cwd(repo.path().to_path_buf());
        let args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(serde_json::json!({
            "path": path, "recursive": recursive, "metadata": alice,
        }))
        .unwrap();
        tool.call(args).await.unwrap();
    }
    drop(ours);

    // THEIR store: src(recursive=false, author=bob) + docs-theirs(recursive=true).
    let (theirs, _dp) = open_store(repo.path(), &repo.path().join("theirs.db"), &url);
    let mut bob = HashMap::new();
    bob.insert("author".to_string(), serde_json::json!("bob"));
    for (path, recursive) in [("src", false), ("docs-theirs", true)] {
        let tool = DocIndexTool::new(theirs.clone()).with_session_cwd(repo.path().to_path_buf());
        let args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(serde_json::json!({
            "path": path, "recursive": recursive, "metadata": bob,
        }))
        .unwrap();
        tool.call(args).await.unwrap();
    }
    drop(theirs);

    let a = repo.path().join("a.db");
    std::fs::copy(&ours_src, &a).unwrap();
    let p = repo.path().join("vectors.db");

    let ok = driver_ok(
        "/nonexistent-base.db",
        a.to_str().unwrap(),
        repo.path().join("theirs.db").to_str().unwrap(),
        p.to_str().unwrap(),
    )
    .await;
    assert!(ok, "compatible multi-root manifest merge succeeds");

    let meta = manifest(&a, &url).await;
    let roots = meta["manifest"]["roots"].as_array().expect("roots array");
    let root_paths: Vec<String> = roots
        .iter()
        .map(|r| r["root"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        root_paths.len(),
        3,
        "one entry per root (src, docs, docs-theirs): {root_paths:?}"
    );

    // Deterministic order (sorted by root path).
    let mut sorted = root_paths.clone();
    sorted.sort();
    assert_eq!(
        root_paths, sorted,
        "roots must be deterministically ordered"
    );

    let find = |needle: &str| {
        roots
            .iter()
            .find(|r| r["root"].as_str().unwrap().ends_with(needle))
            .unwrap_or_else(|| panic!("root {needle} missing: {root_paths:?}"))
    };

    // Ours wins for the `src` root.
    let src = find("src");
    assert_eq!(
        src["recursive"], true,
        "ours wins the recursive flag for src"
    );
    assert_eq!(
        src["metadata"]["author"],
        serde_json::json!("alice"),
        "ours wins the metadata for src"
    );

    // Theirs' unique root is adopted.
    let docs_theirs = find("docs-theirs");
    assert_eq!(
        docs_theirs["metadata"]["author"],
        serde_json::json!("bob"),
        "a theirs-only root is adopted with its own metadata"
    );
}

/// Global `sources` is the sorted union of both sides, without duplicates.
#[tokio::test]
async fn global_sources_are_sorted_union_without_duplicates() {
    let url = spawn_embeddings(3).await;
    let fx = Fixture::new();
    fx.build_ours(&url, &[("alpha.txt", "a"), ("both.txt", "ours")])
        .await;
    fx.build_theirs(&url, &[("both.txt", "theirs"), ("zeta.txt", "z")])
        .await;

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(ok);

    let meta = manifest(&fx.a, &url).await;
    let sources: Vec<String> = meta["manifest"]["sources"]
        .as_array()
        .expect("sources array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        sources,
        vec![
            "alpha.txt".to_string(),
            "both.txt".to_string(),
            "zeta.txt".to_string()
        ],
        "global sources are the deduped, sorted union"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// Unusable theirs: keep ours without mixing, succeed, leave pending
// ══════════════════════════════════════════════════════════════════════════

/// Theirs is a populated v1 store with no v2 record (hand-built via
/// ruvector): not adoptable, so ours is kept whole — no v1 rows leak in.
#[tokio::test]
async fn v1_theirs_is_not_adopted_ours_kept() {
    let url = spawn_embeddings(3).await;
    let fx = Fixture::new();
    fx.build_ours(&url, &[("ours.txt", "ours body")]).await;

    // v1 theirs: absolute source id, no `peakbot.vector` metadata.
    let legacy = fx.repo.path().join("legacy.txt");
    std::fs::write(&legacy, "legacy body").unwrap();
    build_v1_store(&fx.b, &legacy, "legacy body");

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(ok, "keeping ours when theirs is v1 must succeed");

    let hits = search_local(&fx.a, &url, "body").await;
    assert_eq!(
        sources_of(&hits),
        vec!["ours.txt".to_string()],
        "v1 theirs must not be mixed into ours"
    );
    assert_pending(&fx.p, "v1 theirs");
}

/// Theirs carries a different fingerprint (a different model): keep ours.
#[tokio::test]
async fn fingerprint_mismatch_theirs_is_not_adopted_ours_kept() {
    let url = spawn_embeddings(3).await;
    let fx = Fixture::new();
    fx.build_ours(&url, &[("ours.txt", "ours body")]).await;

    // Theirs built under a DIFFERENT model → different fingerprint.
    mark_repo(fx.repo.path());
    let store = VectorStore::open(&spec_at(fx.b.clone(), &url, "other-model")).unwrap();
    let dp = store.db_path_for(Path::new(""));
    index_one(
        &store,
        &dp,
        fx.repo.path(),
        "foreign.txt",
        "foreign body",
        &HashMap::new(),
    )
    .await;
    drop(store);

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(ok, "a fingerprint mismatch keeps ours and still succeeds");

    let hits = search_local(&fx.a, &url, "body").await;
    assert_eq!(
        sources_of(&hits),
        vec!["ours.txt".to_string()],
        "a mismatched-fingerprint theirs must not be mixed in"
    );
    assert_pending(&fx.p, "fingerprint mismatch");
}

/// Theirs is corrupt garbage: keep ours, succeed, leave pending, no mixing.
#[tokio::test]
async fn corrupt_theirs_keeps_ours_without_mixing() {
    let url = spawn_embeddings(3).await;
    let fx = Fixture::new();
    fx.build_ours(&url, &[("ours.txt", "ours body")]).await;
    std::fs::write(&fx.b, b"garbage that is not a vector db").unwrap();

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(ok, "corrupt theirs is resolved by keeping ours");

    let hits = search_local(&fx.a, &url, "body").await;
    assert_eq!(
        sources_of(&hits),
        vec!["ours.txt".to_string()],
        "ours survives intact when theirs is corrupt"
    );
    assert_pending(&fx.p, "corrupt theirs");
}

// ══════════════════════════════════════════════════════════════════════════
// Unusable ours
// ══════════════════════════════════════════════════════════════════════════

/// Our DB file is missing: a real error (nothing usable to fall back to).
#[tokio::test]
async fn missing_ours_is_a_real_error() {
    let url = spawn_embeddings(3).await;
    let fx = Fixture::new();
    // `a` never created.
    fx.build_theirs(&url, &[("theirs.txt", "theirs body")])
        .await;
    assert!(!fx.a.exists());

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(
        !ok,
        "a missing %A must be a real error (no usable ours, no safe fallback)"
    );
}

/// Corrupt ours + valid theirs: a safe fallback may copy theirs into %A.
#[tokio::test]
async fn corrupt_ours_with_valid_theirs_falls_back_to_theirs() {
    let url = spawn_embeddings(3).await;
    let fx = Fixture::new();
    std::fs::write(&fx.a, b"garbage ours").unwrap();
    fx.build_theirs(&url, &[("theirs.txt", "theirs body")])
        .await;

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(ok, "corrupt ours is resolved by adopting valid theirs");

    let hits = search_local(&fx.a, &url, "body").await;
    assert!(
        hits.iter().any(|h| h.text.contains("theirs body")),
        "the safe fallback must materialize theirs' content"
    );
    assert_pending(&fx.p, "corrupt ours fallback");
}

/// Both sides unusable: leave ours in place, keep pending, do not crash.
#[tokio::test]
async fn both_unusable_leaves_ours_and_pending_without_crashing() {
    let _url = spawn_embeddings(3).await;
    let fx = Fixture::new();
    let ours_bytes = b"our garbage bytes".to_vec();
    std::fs::write(&fx.a, &ours_bytes).unwrap();
    std::fs::write(&fx.b, b"their garbage bytes").unwrap();

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(ok, "both-unusable must not crash; it leaves ours");

    assert_eq!(
        std::fs::read(&fx.a).unwrap(),
        ours_bytes,
        "ours must be left untouched when neither side is usable"
    );
    assert_pending(&fx.p, "both unusable");
}

// ══════════════════════════════════════════════════════════════════════════
// Pending marker: always attempted; unwritable path warns but does not fail
// ══════════════════════════════════════════════════════════════════════════

/// When `<P>.peakbot` cannot be created (it is a plain file), the driver warns
/// but still completes the merge successfully.
#[tokio::test]
async fn unwritable_pending_path_warns_but_does_not_fail_merge() {
    let url = spawn_embeddings(3).await;
    let fx = Fixture::new();
    fx.build_ours(&url, &[("ours.txt", "ours body")]).await;
    fx.build_theirs(&url, &[("theirs.txt", "theirs body")])
        .await;

    // Block `<P>.peakbot` from being created as a directory.
    std::fs::write(state_dir(&fx.p), b"i am a file, not a directory").unwrap();

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(
        ok,
        "an unwritable pending path must warn, not fail the merge"
    );

    // The merge output is still complete.
    let hits = search_local(&fx.a, &url, "body").await;
    assert_eq!(
        sources_of(&hits),
        vec!["ours.txt".to_string(), "theirs.txt".to_string()],
        "the merge must complete even when the pending marker cannot be written"
    );
}

/// After a merge that had to keep ours (their side unusable), pending is still
/// set — the next turn must reconcile.
#[tokio::test]
async fn pending_is_set_after_keep_ours_merge() {
    let url = spawn_embeddings(3).await;
    let fx = Fixture::new();
    fx.build_ours(&url, &[("ours.txt", "ours body")]).await;
    std::fs::write(&fx.b, b"garbage").unwrap();

    let ok = driver_ok(
        fx.o.to_str().unwrap(),
        fx.a.to_str().unwrap(),
        fx.b.to_str().unwrap(),
        fx.p.to_str().unwrap(),
    )
    .await;
    assert!(ok);
    assert_pending(&fx.p, "keep-ours merge");
}
