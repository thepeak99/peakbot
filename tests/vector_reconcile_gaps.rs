//! Vector DB reconcile: safety-gap contract tests.
//!
//! Written **before** the implementation, against the interface the Architect
//! specified (`VectorStore::index_file` / `search` + `ensure_fresh` inside
//! `src/vector/reconcile.rs`). Each test names the behaviour it pins and is
//! expected to be RED only where `reconcile.rs` does not yet honour the
//! contract:
//!
//! 1. `missing_state_replays_manifest_and_reembeds_changed_source` —
//!    a missing local state directory must replay the recorded manifest and
//!    validate per-file hashes; a changed working-tree file must be re-embedded
//!    *before* serving. (Green guard: `missing_state_unchanged_source_*`.)
//! 2. `corrupt_replaced_db_*` / `replaced_db_without_manifest_*` — a corrupt or
//!    manifest-less replacement DB must surface a clear stale/corrupt error,
//!    keep the pending marker, embed **zero** documents, and never delete the
//!    file or rescan the anchored repo (so unrelated files are never indexed).
//! 3. `direct_index_file_does_not_cover_unrelated_sibling` — a bare
//!    `VectorStore::index_file` covers exactly that file; a newly added sibling
//!    in the same parent must not be embedded by reconcile (conservative
//!    per-file coverage, or a safe failure on the missing manifest).
//! 4. `unmerged_rebase_conflict_defers_with_stale_error` — a real rebase
//!    conflict (`git ls-files -u` non-empty, **no** `.git/MERGE_HEAD`) must
//!    defer with a stale error instead of reconciling a half-merged worktree.
//! 5. `valid_replacement_v2_db_without_state_reopens_with_hash_reuse` — a valid
//!    replacement v2 DB with no local state is reopened and validated with hash
//!    reuse, not classified as corrupt.
//!
//! ## Embedding accounting
//!
//! The mock endpoint records the *exact input strings* of every request.
//! Query embeddings are identified by an exact sentinel match, so a file
//! embedding can never hide behind a fixed "+1 for the query" offset. Every
//! document-embedding assertion inspects the request payloads, not a counter
//! delta.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use peakbot::config::EmbeddingsConfig;
use peakbot::config::resolve::{DbPathSpec, VectorSpec};
use peakbot::vector::{VectorStore, VectorStoreError};
use serde_json::Value;

/// ruvector config key holding the v2 record.
const META_KEY: &str = "peakbot.vector";

// ── Embedding recorder (request-text based accounting) ─────────────────────

/// Records the exact `input` array of every embeddings request, in order.
#[derive(Clone)]
struct EmbedRecorder {
    requests: Arc<Mutex<Vec<Vec<String>>>>,
}

impl EmbedRecorder {
    fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Every request's input strings, in call order.
    fn all(&self) -> Vec<Vec<String>> {
        self.requests.lock().unwrap().clone()
    }

    fn total(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// Forget everything recorded so far. Called after the test's own setup
    /// indexing so assertions see only the requests made by the operation under
    /// test (never the setup's legitimate document embeddings).
    fn clear(&self) {
        self.requests.lock().unwrap().clear();
    }

    /// True when this request is the query embedding for one of `queries`.
    /// A query is embedded as exactly one input whose text is the query.
    fn is_query_request(inputs: &[String], queries: &[&str]) -> bool {
        inputs.len() == 1 && queries.iter().any(|q| inputs[0] == *q)
    }

    /// Requests that are *not* the known query embedding — i.e. document /
    /// file-chunk embeddings. This is the only classification the tests use;
    /// an extra file embedding cannot be masked by an expected query call.
    fn document_requests(&self, queries: &[&str]) -> Vec<Vec<String>> {
        self.all()
            .into_iter()
            .filter(|inputs| !Self::is_query_request(inputs, queries))
            .collect()
    }

    /// Flattened document-embedding inputs (one per embedded chunk).
    fn document_inputs(&self, queries: &[&str]) -> Vec<String> {
        self.document_requests(queries)
            .into_iter()
            .flatten()
            .collect()
    }

    fn query_requests(&self, queries: &[&str]) -> usize {
        self.all()
            .iter()
            .filter(|inputs| Self::is_query_request(inputs, queries))
            .count()
    }
}

async fn spawn_recording_embeddings() -> (String, EmbedRecorder) {
    let recorder = EmbedRecorder::new();
    let app = Router::new()
        .route("/embeddings", post(record_embed))
        .with_state(recorder.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock embeddings");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), recorder)
}

async fn record_embed(
    State(recorder): State<EmbedRecorder>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let inputs: Vec<String> = body["input"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    recorder.requests.lock().unwrap().push(inputs.clone());

    let data: Vec<Value> = inputs
        .iter()
        .map(|text| {
            let mut v = vec![0.0f32; 3];
            for (i, b) in text.bytes().enumerate() {
                v[i % 3] += f32::from(b);
            }
            if v.iter().all(|x| *x == 0.0) {
                v[0] = 1.0;
            }
            serde_json::json!({ "embedding": v })
        })
        .collect();
    Json(serde_json::json!({ "data": data }))
}

// ── Specs ──────────────────────────────────────────────────────────────────

fn vector_spec(db_path: PathBuf, base_url: &str) -> VectorSpec {
    VectorSpec {
        db_path: DbPathSpec::Fixed(db_path),
        embeddings: EmbeddingsConfig {
            base_url: base_url.to_string(),
            api_key: None,
            model: "test-model".into(),
            dimensions: 3,
        },
    }
}

// ── Reconcile state helpers (mirror src/vector/reconcile.rs layout) ────────

fn state_dir_for(db_path: &Path) -> PathBuf {
    let ext = db_path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "db".to_string());
    db_path.with_extension(format!("{ext}.peakbot"))
}

fn pending_marker(db_path: &Path) -> PathBuf {
    state_dir_for(db_path).join("pending")
}

fn has_pending(db_path: &Path) -> bool {
    pending_marker(db_path).exists()
}

// ── Git helpers ────────────────────────────────────────────────────────────

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git")
}

fn init_git_repo(dir: &Path) {
    assert!(git(dir, &["init", "-q"]).status.success(), "git init");
    assert!(
        git(dir, &["config", "user.email", "test@example.com"])
            .status
            .success(),
        "git config email"
    );
    assert!(
        git(dir, &["config", "user.name", "Test User"])
            .status
            .success(),
        "git config name"
    );
    // Force the merge backend: it leaves real unmerged index entries
    // (`git ls-files -u`) and a `.git/rebase-merge` directory, but *no*
    // `.git/MERGE_HEAD` — exactly the state gap #4 is about.
    assert!(
        git(dir, &["config", "rebase.backend", "merge"])
            .status
            .success(),
        "git config rebase backend"
    );
}

/// Build a genuine v1 store directly via ruvector: a row with an absolute
/// source path and **no** `peakbot.vector` metadata key.
fn build_v1_store(db_path: &Path, source: &Path, content: &str) {
    let opts = ruvector_core::types::DbOptions {
        dimensions: 3,
        distance_metric: ruvector_core::types::DistanceMetric::Cosine,
        storage_path: db_path.display().to_string(),
        hnsw_config: Some(ruvector_core::types::HnswConfig {
            max_elements: 100_000,
            ..ruvector_core::types::HnswConfig::default()
        }),
        ..Default::default()
    };
    let raw = ruvector_core::vector_db::VectorDB::new(opts).expect("create v1 db");
    let mut v = vec![0.0f32; 3];
    for (i, b) in content.bytes().enumerate() {
        v[i % 3] += f32::from(b);
    }
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
        vector: v,
        metadata: Some(metadata),
    }])
    .expect("insert v1 row");
}

fn commit_all(dir: &Path, message: &str) {
    assert!(
        git(dir, &["add", "-A"]).status.success(),
        "git add -A failed"
    );
    let out = git(dir, &["commit", "-m", message]);
    assert!(
        out.status.success(),
        "git commit {message:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_head(dir: &Path) -> String {
    let out = git(dir, &["rev-parse", "HEAD"]);
    assert!(out.status.success(), "git rev-parse HEAD failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn current_branch(dir: &Path) -> String {
    let out = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert!(out.status.success(), "git rev-parse branch failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// `git ls-files -u` output: non-empty iff the index has unmerged entries.
fn unmerged_index_entries(dir: &Path) -> String {
    let out = git(dir, &["ls-files", "-u"]);
    assert!(out.status.success(), "git ls-files -u failed");
    String::from_utf8(out.stdout).unwrap()
}

fn merge_head_exists(dir: &Path) -> bool {
    dir.join(".git").join("MERGE_HEAD").exists()
}

// ── Error-message helpers ──────────────────────────────────────────────────

/// A corrupt/replacement DB must fail with a message an operator can act on —
/// not a silent rebuild and not an opaque panic.
fn assert_corrupt_error(err: &VectorStoreError, context: &str) {
    let msg = err.to_string().to_lowercase();
    let clear = [
        "corrupt",
        "stale",
        "unreadable",
        "invalid",
        "failed to open",
        "not a vector",
        "manifest",
        "metadata",
    ]
    .iter()
    .any(|needle| msg.contains(needle));
    assert!(
        clear,
        "{context}: expected a clear stale/corrupt error, got: {msg}"
    );
}

/// A worktree with unmerged index entries must defer with a stale error.
fn assert_stale_error(err: &VectorStoreError, context: &str) {
    let msg = err.to_string().to_lowercase();
    let deferred = [
        "stale",
        "unmerged",
        "rebase",
        "merge in progress",
        "defer",
        "interrupted",
    ]
    .iter()
    .any(|needle| msg.contains(needle));
    assert!(
        deferred,
        "{context}: expected a stale/deferred error, got: {msg}"
    );
}

fn hit_texts(hits: &[peakbot::vector::Hit]) -> Vec<String> {
    hits.iter().map(|h| h.text.clone()).collect()
}

fn hit_sources(hits: &[peakbot::vector::Hit]) -> Vec<String> {
    hits.iter().map(|h| h.source.clone()).collect()
}

// ══════════════════════════════════════════════════════════════════════════
// Gap 1 — missing state must replay the manifest and validate hashes
// ══════════════════════════════════════════════════════════════════════════

/// State is NOT the source of truth for content. When the local state
/// directory is lost but the DB/manifest survive, reconcile must replay the
/// manifest and re-embed a working-tree change even though HEAD and the DB
/// stamp are unchanged — otherwise search serves stale rows forever.
#[tokio::test]
async fn missing_state_replays_manifest_and_reembeds_changed_source() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, recorder) = spawn_recording_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "alpha original body").unwrap();
    commit_all(dir.path(), "base");

    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    assert!(
        state_dir_for(&db_path).exists(),
        "index_file must record reconcile state (test precondition)"
    );
    drop(store);

    // Uncommitted working-tree edit: HEAD, DB bytes/mtime and the manifest all
    // stay exactly as they were. Only the local state directory disappears.
    std::fs::write(&doc, "omega replacement body").unwrap();
    std::fs::remove_dir_all(state_dir_for(&db_path)).unwrap();
    assert!(!state_dir_for(&db_path).exists());

    let query = "zz-sentinel-missing-state";
    // Baseline: ignore the setup indexing so only the operation under test is
    // accounted for.
    recorder.clear();
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let hits = store2.search(&resolved2, query, 5).await.unwrap();

    let doc_requests = recorder.document_requests(&[query]);
    assert_eq!(
        doc_requests.len(),
        1,
        "missing state must replay the manifest, validate hashes, and re-embed \
         exactly the one changed file before serving; all requests: {:?}",
        recorder.all()
    );
    assert_eq!(
        doc_requests[0],
        vec!["omega replacement body".to_string()],
        "the re-embedded document must be the changed working-tree text"
    );
    assert!(
        hits.iter()
            .any(|h| h.text.contains("omega replacement body")),
        "search served stale rows after state loss: {:?}",
        hit_texts(&hits)
    );
    assert!(
        hits.iter().all(|h| !h.text.contains("alpha original body")),
        "pre-change chunks must be replaced after the state-loss replay: {:?}",
        hit_texts(&hits)
    );
}

/// Guard (must stay green): an unchanged source with missing state validates
/// the hashes and embeds ZERO documents. The only request is the query itself,
/// identified by its exact input text — not by a fixed "+1" arithmetic trick.
#[tokio::test]
async fn missing_state_unchanged_source_reuses_hash_with_zero_document_embeddings() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, recorder) = spawn_recording_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "stable unchanged body").unwrap();
    commit_all(dir.path(), "base");

    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    std::fs::remove_dir_all(state_dir_for(&db_path)).unwrap();

    let query = "zz-sentinel-missing-state-unchanged";
    // Baseline: ignore the setup indexing so only the operation under test is
    // accounted for.
    recorder.clear();
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let hits = store2.search(&resolved2, query, 5).await.unwrap();

    assert!(
        hits.iter()
            .any(|h| h.text.contains("stable unchanged body")),
        "hash reuse must keep the unchanged document searchable: {:?}",
        hit_texts(&hits)
    );
    assert_eq!(
        recorder.total(),
        1,
        "missing state + unchanged source must cost exactly one query embedding; \
         all requests: {:?}",
        recorder.all()
    );
    assert!(
        recorder.document_requests(&[query]).is_empty(),
        "missing state + unchanged source must emit zero document embeddings; \
         requests: {:?}",
        recorder.all()
    );
}

// ══════════════════════════════════════════════════════════════════════════
// Embedding-accounting audit — robust (request-text) file/query split
// ══════════════════════════════════════════════════════════════════════════

/// A reconcile that re-embeds one changed file makes exactly one document
/// request (the changed chunk) plus exactly one query request. The query is
/// recognised by its input text, so a second hidden file embedding would make
/// this fail instead of being absorbed into a "+1" expectation.
#[tokio::test]
async fn head_change_reconcile_embeds_exactly_one_document_request() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, recorder) = spawn_recording_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc1.txt");
    std::fs::write(&doc, "first version body").unwrap();
    commit_all(dir.path(), "base");

    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    std::fs::write(&doc, "second version body").unwrap();
    commit_all(dir.path(), "change doc1");

    let query = "zz-sentinel-changed-file";
    // Baseline: ignore the setup indexing so only the operation under test is
    // accounted for.
    recorder.clear();
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    store2.search(&resolved2, query, 5).await.unwrap();

    let docs = recorder.document_requests(&[query]);
    assert_eq!(
        docs.len(),
        1,
        "one changed file must produce exactly one document request; requests: {:?}",
        recorder.all()
    );
    assert_eq!(
        docs[0],
        vec!["second version body".to_string()],
        "the document request must carry the changed text"
    );
    assert_eq!(
        recorder.query_requests(&[query]),
        1,
        "the query must be embedded exactly once; requests: {:?}",
        recorder.all()
    );
    assert_eq!(
        recorder.total(),
        2,
        "changed-file reconcile = 1 document request + 1 query request; requests: {:?}",
        recorder.all()
    );
}

/// A HEAD change with no changed sources embeds zero documents; only the query
/// leaves the process. (`notes.bin` is unsupported, so it is not coverage.)
#[tokio::test]
async fn head_change_unchanged_source_embeds_no_documents() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, recorder) = spawn_recording_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "stable reconcile body").unwrap();
    commit_all(dir.path(), "base");

    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    std::fs::write(dir.path().join("notes.bin"), "not indexable").unwrap();
    commit_all(dir.path(), "unrelated commit");

    let query = "zz-sentinel-unchanged-file";
    // Baseline: ignore the setup indexing so only the operation under test is
    // accounted for.
    recorder.clear();
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let hits = store2.search(&resolved2, query, 5).await.unwrap();

    assert!(
        hits.iter()
            .any(|h| h.text.contains("stable reconcile body")),
        "unchanged document must stay searchable: {:?}",
        hit_texts(&hits)
    );
    assert_eq!(
        recorder.total(),
        1,
        "unchanged sources must not be re-embedded; requests: {:?}",
        recorder.all()
    );
    assert!(
        recorder.document_requests(&[query]).is_empty(),
        "unchanged sources must emit zero document embeddings; requests: {:?}",
        recorder.all()
    );
}

// ══════════════════════════════════════════════════════════════════════════
// Gap 2 — corrupt / replacement DB: error, do not delete, do not rescan
// ══════════════════════════════════════════════════════════════════════════

/// Replace `db_path` with a garbage inode (the "file swapped under us" case).
fn replace_db_with_garbage(db_path: &Path, bytes: &[u8]) {
    let incoming = db_path.with_extension("incoming");
    std::fs::write(&incoming, bytes).unwrap();
    std::fs::rename(&incoming, db_path).unwrap();
}

/// A garbage replacement DB must produce a clear stale/corrupt error and must
/// NOT be deleted, rewritten, or "repaired" by scanning the anchored repo. The
/// pending marker stays so the next run retries; zero documents are embedded,
/// so no unrelated repo file can slip into the store.
#[tokio::test]
async fn corrupt_replaced_db_errors_without_deleting_or_rescanning() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, recorder) = spawn_recording_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "indexed document body").unwrap();
    std::fs::write(dir.path().join("unrelated.txt"), "unrelated decoy body").unwrap();
    commit_all(dir.path(), "base");

    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    let garbage = b"not a vector db: replaced by garbage bytes".to_vec();
    replace_db_with_garbage(&db_path, &garbage);
    assert_eq!(std::fs::read(&db_path).unwrap(), garbage);

    let query = "zz-sentinel-corrupt-db";
    // Baseline: ignore the setup indexing so only the operation under test is
    // accounted for.
    recorder.clear();
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let err = match store2.search(&resolved2, query, 5).await {
        Ok(hits) => panic!(
            "corrupt replacement DB must error, not serve a rebuild; got {} hits: {:?}",
            hits.len(),
            hit_sources(&hits)
        ),
        Err(err) => err,
    };
    assert_corrupt_error(&err, "corrupt replacement DB");

    assert_eq!(
        std::fs::read(&db_path).unwrap(),
        garbage,
        "production must NOT delete or rewrite the corrupt replacement DB"
    );
    assert!(
        has_pending(&db_path),
        "a corrupt replacement DB must leave the pending repair marker set"
    );
    assert!(
        recorder.document_requests(&[query]).is_empty(),
        "corrupt replacement DB must trigger ZERO file embeddings (no repo rescan); \
         requests: {:?}",
        recorder.all()
    );
    assert!(
        recorder
            .all()
            .iter()
            .flatten()
            .all(|t| !t.contains("unrelated decoy body")),
        "must never index unrelated repo files while repairing a corrupt DB; \
         requests: {:?}",
        recorder.all()
    );
}

/// A pre-existing pending marker (interrupted previous reconcile) must survive a
/// corrupt-DB failure: the store stays flagged for repair instead of being
/// silently cleared or rebuilt.
#[tokio::test]
async fn corrupt_db_failure_keeps_pending_marker() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, recorder) = spawn_recording_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "pending corpus body").unwrap();
    std::fs::write(dir.path().join("unrelated.txt"), "unrelated decoy body").unwrap();
    commit_all(dir.path(), "base");

    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Simulate a reconcile that was interrupted last run.
    std::fs::create_dir_all(state_dir_for(&db_path)).unwrap();
    std::fs::write(pending_marker(&db_path), "pending").unwrap();

    let garbage = b"garbage where the database should be".to_vec();
    replace_db_with_garbage(&db_path, &garbage);

    let query = "zz-sentinel-pending-corrupt";
    // Baseline: ignore the setup indexing so only the operation under test is
    // accounted for.
    recorder.clear();
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let err = match store2.search(&resolved2, query, 5).await {
        Ok(hits) => panic!(
            "corrupt DB with a pending marker must error, not rebuild; got {} hits: {:?}",
            hits.len(),
            hit_sources(&hits)
        ),
        Err(err) => err,
    };
    assert_corrupt_error(&err, "corrupt DB with pending marker");

    assert!(
        has_pending(&db_path),
        "a failed reconcile must never clear the pending repair marker"
    );
    assert_eq!(
        std::fs::read(&db_path).unwrap(),
        garbage,
        "production must NOT delete the corrupt DB on failed reconcile"
    );
    assert!(
        recorder.document_requests(&[query]).is_empty(),
        "a failed reconcile must embed zero documents (no repo rescan); requests: {:?}",
        recorder.all()
    );
}

/// A *valid* replacement DB that carries no v2 manifest is not silently
/// rebuilt by walking the anchored root. Rewriting the manifest from the whole
/// repo would index files the user never opted into.
#[tokio::test]
async fn replaced_db_without_manifest_does_not_rescan_anchored_root() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, recorder) = spawn_recording_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "surviving document body").unwrap();
    std::fs::write(dir.path().join("unrelated.txt"), "unrelated decoy body").unwrap();
    commit_all(dir.path(), "base");

    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    assert!(state_dir_for(&db_path).exists(), "state recorded");
    drop(store);

    // Build a valid-but-empty ruvector DB in a scratch dir (no v2 manifest)
    // and swap its bytes in as a new inode. Bytes are copied, not renamed, so
    // the scratch DB's pooled handle is never aliased onto this path.
    let scratch = tempfile::tempdir().unwrap();
    let empty_path = scratch.path().join("empty.db");
    let empty_store = VectorStore::open(&vector_spec(empty_path.clone(), &url)).unwrap();
    let _empty_handle = empty_store.handle_for_test(Path::new("")).await.unwrap();
    let empty_bytes = std::fs::read(&empty_path).unwrap();
    assert!(!empty_bytes.is_empty(), "empty DB file materialized");
    drop(empty_store);

    let incoming = db_path.with_extension("incoming");
    std::fs::write(&incoming, &empty_bytes).unwrap();
    std::fs::rename(&incoming, &db_path).unwrap();

    let query = "zz-sentinel-manifest-missing";
    // Baseline: ignore the setup indexing so only the operation under test is
    // accounted for.
    recorder.clear();
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let err = match store2.search(&resolved2, query, 5).await {
        Ok(hits) => panic!(
            "a replacement DB with no v2 manifest must error, not rescan the repo; \
             got {} hits: {:?}",
            hits.len(),
            hit_sources(&hits)
        ),
        Err(err) => err,
    };
    assert_corrupt_error(&err, "replacement DB with no manifest");

    assert!(
        db_path.exists(),
        "the replacement DB must be preserved for inspection, not deleted"
    );
    assert!(
        recorder.document_requests(&[query]).is_empty(),
        "missing manifest must not trigger a whole-root rescan/embed; requests: {:?}",
        recorder.all()
    );
    assert!(
        recorder
            .all()
            .iter()
            .flatten()
            .all(|t| !t.contains("unrelated decoy body")),
        "never index unrelated repo files when the manifest is missing; requests: {:?}",
        recorder.all()
    );
}

/// Corrupt metadata must fail the read, must not prune existing rows,
/// and must not mix in new sources.
#[tokio::test]
async fn corrupt_metadata_never_prunes_or_mixes_sources() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, recorder) = spawn_recording_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "guarded corpus body").unwrap();
    std::fs::write(dir.path().join("unrelated.txt"), "unrelated decoy body").unwrap();
    commit_all(dir.path(), "base");

    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();

    // Corrupt the metadata record in place (public DB handle seam).
    let db = store.handle_for_test(Path::new("")).await.unwrap();
    db.save_config_value(META_KEY, "{ this is not json")
        .unwrap();
    drop(db);
    drop(store);

    let query = "zz-sentinel-corrupt-metadata";
    // Baseline: ignore the setup indexing so only the operation under test is
    // accounted for.
    recorder.clear();
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let err = match store2.search(&resolved2, query, 5).await {
        Ok(hits) => panic!(
            "corrupt metadata must error, got {} hits: {:?}",
            hits.len(),
            hit_sources(&hits)
        ),
        Err(err) => err,
    };
    assert_corrupt_error(&err, "corrupt metadata");
    assert!(
        recorder.document_requests(&[query]).is_empty(),
        "corrupt metadata must not embed/prune anything; requests: {:?}",
        recorder.all()
    );

    // Rows survive: read them through the raw handle (no reconcile/classify),
    // proving the corrupt record caused no prune and mixed in no new source.
    let reader = VectorStore::open(&vector_spec(db_path.clone(), &url)).unwrap();
    let handle = reader.handle_for_test(Path::new("")).await.unwrap();
    let mut rows = Vec::new();
    for id in handle.keys().unwrap() {
        if let Some(entry) = handle.get(&id).unwrap() {
            rows.push(entry.metadata.unwrap_or_default());
        }
    }
    assert!(
        rows.iter()
            .any(|md| md.get("text").and_then(|v| v.as_str()) == Some("guarded corpus body")),
        "corrupt metadata must not prune existing rows"
    );
    assert!(
        rows.iter().all(|md| md
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .ends_with("doc.txt")),
        "corrupt metadata must not mix in new sources"
    );
}

/// Legacy v1 compatibility: a populated store with no v2 manifest and a
/// stale leftover state file must never trigger a whole-root rebuild.
#[tokio::test]
async fn legacy_v1_with_stale_state_never_rebuilds_whole_root() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("legacy.db");
    let (url, recorder) = spawn_recording_embeddings().await;

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "legacy corpus body").unwrap();
    commit_all(dir.path(), "base");

    // Build a populated v1 store by hand: absolute source path, no v2 record.
    build_v1_store(&db_path, &doc, "legacy corpus body");

    // A new tracked source on disk plus a stale leftover reconcile state.
    std::fs::write(dir.path().join("unrelated.txt"), "unrelated decoy body").unwrap();
    commit_all(dir.path(), "add unrelated");
    std::fs::create_dir_all(state_dir_for(&db_path)).unwrap();
    std::fs::write(
        state_dir_for(&db_path).join("state.json"),
        r#"{"last_head":"0000000000000000000000000000000000000000","db_size":1,"db_mtime":1,"last_reconciled":0}"#,
    )
    .unwrap();

    let query = "zz-sentinel-legacy-v1";
    // Baseline: ignore the setup indexing so only the operation under test is
    // accounted for.
    recorder.clear();
    let store = VectorStore::open(&vector_spec(db_path.clone(), &url)).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    let hits = store.search(&resolved, query, 10).await.unwrap();

    assert!(
        hits.iter().any(|h| h.text.contains("legacy corpus body")),
        "legacy v1 rows must remain readable in a repo-anchored store: {:?}",
        hit_texts(&hits)
    );
    assert!(
        hits.iter().all(|h| !h.source.ends_with("unrelated.txt")),
        "legacy v1 must not gain whole-root coverage; sources: {:?}",
        hit_sources(&hits)
    );
    assert!(
        recorder.document_requests(&[query]).is_empty(),
        "legacy v1 compatibility must never rescan/embed the anchored root; \
         requests: {:?}",
        recorder.all()
    );
}

// ══════════════════════════════════════════════════════════════════════════
// Gap 3 — no implicit parent-directory coverage from a direct index_file
// ══════════════════════════════════════════════════════════════════════════

/// `index_file` covers one file. Adding an unrelated valid file in the same
/// parent afterwards must not cause it to be embedded on reconcile: production
/// either records conservative per-file coverage or fails safely on the missing
/// manifest. A parent-directory fallback is forbidden.
#[tokio::test]
async fn direct_index_file_does_not_cover_unrelated_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, recorder) = spawn_recording_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc1 = dir.path().join("doc1.txt");
    std::fs::write(&doc1, "primary document body").unwrap();
    commit_all(dir.path(), "base");

    // Direct store-level index: no doc_index root, so no directory coverage.
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc1, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Unrelated, valid, same parent.
    std::fs::write(dir.path().join("doc2.txt"), "sibling decoy body").unwrap();
    commit_all(dir.path(), "add sibling");

    let query = "zz-sentinel-direct-index-file";
    // Baseline: ignore the setup indexing so only the operation under test is
    // accounted for.
    recorder.clear();
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let result = store2.search(&resolved2, query, 10).await;

    let docs = recorder.document_inputs(&[query]);
    assert!(
        docs.is_empty(),
        "a direct index_file must not gain parent-directory coverage: the \
         unrelated sibling was embedded ({docs:?}); all requests: {:?}",
        recorder.all()
    );
    assert!(
        docs.iter().all(|t| !t.contains("sibling decoy body")),
        "the unrelated sibling must never be embedded; inputs: {docs:?}"
    );

    match result {
        // Conservative per-file coverage: search works but does not serve doc2.
        Ok(hits) => assert!(
            hits.iter().all(|h| !h.source.ends_with("doc2.txt")),
            "unrelated sibling must not be served; sources: {:?}",
            hit_sources(&hits)
        ),
        // The other permitted behaviour: fail safely on the missing manifest.
        Err(err) => {
            let msg = err.to_string().to_lowercase();
            assert!(
                msg.contains("manifest") || msg.contains("coverage") || msg.contains("stale"),
                "failing safely on missing coverage must say so; got: {msg}"
            );
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════
// Gap 4 — unmerged index entries defer (no reliance on .git/MERGE_HEAD)
// ══════════════════════════════════════════════════════════════════════════

/// A real rebase conflict leaves unmerged index entries (`git ls-files -u`)
/// and a `.git/rebase-merge` directory, but NO `.git/MERGE_HEAD`. reconcile must
/// still detect the unstable worktree and defer with a stale error.
#[tokio::test]
async fn unmerged_rebase_conflict_defers_with_stale_error() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, recorder) = spawn_recording_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "base body").unwrap();
    commit_all(dir.path(), "base");
    let base_head = git_head(dir.path());
    let default_branch = current_branch(dir.path());

    // Index the base version so state records the base HEAD.
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Two conflicting edits, then a real rebase → conflict.
    assert!(
        git(dir.path(), &["checkout", "-q", "-b", "feature"])
            .status
            .success(),
        "create feature branch"
    );
    std::fs::write(&doc, "feature body").unwrap();
    commit_all(dir.path(), "feature edit");
    assert!(
        git(dir.path(), &["checkout", "-q", &default_branch])
            .status
            .success(),
        "back to default branch"
    );
    std::fs::write(&doc, "default body").unwrap();
    commit_all(dir.path(), "default edit");
    assert!(
        git(dir.path(), &["checkout", "-q", "feature"])
            .status
            .success(),
        "checkout feature"
    );

    let rebase = git(dir.path(), &["rebase", &default_branch]);
    assert!(
        !rebase.status.success(),
        "test precondition: rebase must conflict (stdout: {}, stderr: {})",
        String::from_utf8_lossy(&rebase.stdout),
        String::from_utf8_lossy(&rebase.stderr)
    );

    // Authentic unmerged state, and proof we are NOT relying on MERGE_HEAD.
    let unmerged = unmerged_index_entries(dir.path());
    assert!(
        !unmerged.trim().is_empty(),
        "test precondition: git ls-files -u must report unmerged entries"
    );
    assert!(
        !merge_head_exists(dir.path()),
        "test precondition: the merge backend rebase leaves no .git/MERGE_HEAD"
    );
    // HEAD moved (detached at the upstream commit), so the freshness check is
    // definitely triggered on top of the unmerged index state.
    assert_ne!(
        git_head(dir.path()),
        base_head,
        "test precondition: the rebase must have moved HEAD away from the indexed commit"
    );

    let query = "zz-sentinel-unmerged-index";
    // Baseline: ignore the setup indexing so only the operation under test is
    // accounted for.
    recorder.clear();
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let err = match store2.search(&resolved2, query, 5).await {
        Ok(hits) => panic!(
            "unmerged index entries must defer, not reconcile a half-merged worktree; \
             got {} hits: {:?}",
            hits.len(),
            hit_texts(&hits)
        ),
        Err(err) => err,
    };
    assert_stale_error(&err, "rebase conflict");
    assert!(
        recorder.document_requests(&[query]).is_empty(),
        "deferring on unmerged entries must not embed the conflicted worktree; \
         requests: {:?}",
        recorder.all()
    );
}

// ══════════════════════════════════════════════════════════════════════════
// Gap 5 — valid replacement v2 DB without local state: reopen + hash reuse
// ══════════════════════════════════════════════════════════════════════════

/// Absent local state is not corruption. A valid v2 DB copied into place must
/// be reopened, its manifest replayed, and its hashes reused (zero document
/// embeddings) — not rejected or rebuilt.
#[tokio::test]
async fn valid_replacement_v2_db_without_state_reopens_with_hash_reuse() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, recorder) = spawn_recording_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "durable corpus body").unwrap();
    commit_all(dir.path(), "base");

    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Snapshot the valid DB, drop the local state, and swap the snapshot in as a
    // new inode (a fresh clone / checkout of a checked-in store).
    let snapshot = dir.path().join("snapshot.db");
    std::fs::copy(&db_path, &snapshot).unwrap();
    std::fs::remove_dir_all(state_dir_for(&db_path)).unwrap();
    std::fs::rename(&snapshot, &db_path).unwrap();

    let query = "zz-sentinel-valid-replacement";
    // Baseline: ignore the setup indexing so only the operation under test is
    // accounted for.
    recorder.clear();
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let hits = store2.search(&resolved2, query, 5).await.unwrap();

    assert!(
        hits.iter().any(|h| h.text.contains("durable corpus body")),
        "a valid replacement v2 DB must remain searchable: {:?}",
        hit_texts(&hits)
    );
    assert!(
        recorder.document_requests(&[query]).is_empty(),
        "valid replacement + missing state must reuse stored hashes (zero document \
         embeddings); requests: {:?}",
        recorder.all()
    );
    assert_eq!(
        recorder.total(),
        1,
        "only the query embedding is expected; requests: {:?}",
        recorder.all()
    );
}
