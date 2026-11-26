//! PR1 vector DB universal v2: reconciliation and state management.
//!
//! Tests the locked reconciliation contract:
//! - ensure_fresh triggers on: pending marker, HEAD differs, DB stamp differs
//! - Missing state validates once with zero embeddings
//! - Reconcile: fingerprint validation, manifest replay incl. new-file discovery,
//!   unchanged=zero embeds, changed-only embed, shrink/delete/empty handling
//! - Provider failure blocks stale search, resumes without re-embedding valid docs
//! - Replaced-inode reopen on ensure_fresh
//! - Fingerprint mismatch / corrupt metadata never prunes or mixes
//! - Legacy nonempty v1 stays readable, no migrate
//! - Local stores perform no git/state work
//! - State recorded and pending cleared only after complete success
//! - Real two-branch git merge conflict defers with stale error
//!
//! Uses a mock embeddings server for deterministic vectors.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use peakbot::config::EmbeddingsConfig;
use peakbot::config::resolve::{DbPathSpec, VectorSpec};
use peakbot::vector::{VectorStore, VectorStoreError};

/// Shared test helper: create an in-memory mock embeddings server via a simple TCP
/// listener that returns 3D vectors. We count calls to verify embed count behavior.
/// For simplicity this uses a local HTTP server that returns deterministic vectors
/// based on the input text.
async fn spawn_mock_embeddings() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};

    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count_clone = call_count.clone();

    let app = Router::new()
        .route(
            "/embeddings",
            post(
                move |State(count): State<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
                      Json(body): Json<serde_json::Value>| async move {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let inputs = body["input"].as_array().cloned().unwrap_or_default();
                    let data: Vec<serde_json::Value> = inputs
                        .iter()
                        .map(|input| {
                            let text = input.as_str().unwrap_or_default();
                            let mut v = vec![0.0f32; 3];
                            for (i, b) in text.bytes().enumerate() {
                                v[i % 3] += f32::from(b);
                            }
                            serde_json::json!({ "embedding": v })
                        })
                        .collect();
                    Json(serde_json::json!({ "data": data }))
                },
            ),
        )
        .with_state(call_count);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    (format!("http://{addr}"), count_clone)
}

/// Create a VectorSpec for a repo-anchored store.
fn vector_spec(db_path: PathBuf, url: &str) -> VectorSpec {
    VectorSpec {
        db_path: DbPathSpec::Fixed(db_path),
        embeddings: EmbeddingsConfig {
            base_url: url.to_string(),
            api_key: None,
            model: "test-model".into(),
            dimensions: 3,
        },
    }
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

/// Initialize a git repo with config and initial commit of a file.
fn init_git_repo(dir: &Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(dir)
        .output()
        .unwrap();
}

/// Commit all changes.
fn git_commit_all(dir: &Path, message: &str) {
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .unwrap();
    let output = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "git commit failed: {:?}", output);
}

/// Get current HEAD sha.
fn git_head(dir: &Path) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Check if a git merge is in progress (MERGE_HEAD exists).
fn git_merge_in_progress(dir: &Path) -> bool {
    dir.join(".git").join("MERGE_HEAD").exists()
}

// ── ensure_fresh trigger tests ─────────────────────────────────────────────

/// ensure_fresh is triggered by a pending marker.
#[tokio::test]
async fn ensure_fresh_pending_marker_triggers() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, _count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "initial content").unwrap();
    git_commit_all(dir.path(), "initial");

    // Index a file first to establish state
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Create a pending marker to simulate interrupted reconcile
    let state_dir = dir.path().join("v.db.peakbot");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("pending"), "pending").unwrap();

    // Search should trigger ensure_fresh, which should reconcile
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let hits = store2.search(&resolved2, "initial", 5).await.unwrap();
    assert!(
        !hits.is_empty(),
        "reconciled search should find the indexed doc"
    );
    // Pending marker should be cleared after successful reconcile
    assert!(
        !state_dir.join("pending").exists(),
        "ensure_fresh should clear pending marker after successful reconcile"
    );
}

/// ensure_fresh is triggered when HEAD differs from recorded state, and
/// reconcile discovers a new file under a recorded root. A bare `index_file`
/// records no directory coverage, so this drives DocIndexTool (the legitimate
/// root-recording seam) instead of expecting sibling pickup.
#[tokio::test]
async fn ensure_fresh_head_differs_triggers() {
    use peakbot::tools::DocIndexTool;
    use rig_core::tool::Tool;

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let docs = dir.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("doc.txt"), "initial content").unwrap();
    git_commit_all(dir.path(), "initial");

    // Record docs/ as a recursive root via the tool layer.
    let store = VectorStore::open(&spec).unwrap();
    let tool = DocIndexTool::new(store.clone()).with_session_cwd(dir.path().to_path_buf());
    let args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(serde_json::json!({
        "path": "docs",
        "recursive": true,
    }))
    .unwrap();
    tool.call(args).await.unwrap();
    drop(store);

    // Make a change and commit (HEAD now differs).
    std::fs::write(docs.join("new_doc.txt"), "new content here").unwrap();
    git_commit_all(dir.path(), "add new doc");

    // Search should trigger reconcile due to HEAD change.
    let embeds_before = count.load(std::sync::atomic::Ordering::SeqCst);
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let hits = store2.search(&resolved2, "new content", 5).await.unwrap();

    // New file should have been indexed.
    assert!(
        hits.iter().any(|h| h.source == "docs/new_doc.txt"),
        "reconcile should discover new files under the recorded root after HEAD \
         change; hits: {:?}",
        hits.iter().map(|h| &h.source).collect::<Vec<_>>()
    );
    assert!(
        count.load(std::sync::atomic::Ordering::SeqCst) > embeds_before,
        "reconcile should embed newly discovered files"
    );
}

/// ensure_fresh is triggered when the DB stamp differs: a valid replacement DB
/// (new inode, different manifest) is reopened and reconciled with hash reuse,
/// rather than served from the stale pooled handle.
#[tokio::test]
async fn ensure_fresh_db_stamp_differs_triggers() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let replacement_path = dir.path().join("other").join("replacement.db");
    let (url, _count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    let replacement_doc = dir.path().join("doc2.txt");
    std::fs::write(&doc, "initial content").unwrap();
    std::fs::write(&replacement_doc, "replacement content").unwrap();
    git_commit_all(dir.path(), "initial");

    // Index doc.txt into the real DB, recording HEAD + stamp state.
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Build a *valid* v2 DB with a different manifest in the same repo.
    let replacement_spec = vector_spec(replacement_path.clone(), &url);
    let replacement = VectorStore::open(&replacement_spec).unwrap();
    let replacement_resolved = replacement.db_path_for(Path::new(""));
    replacement
        .index_file(&replacement_resolved, &replacement_doc, &HashMap::new())
        .await
        .unwrap();
    drop(replacement);

    // Swap the replacement in as a new inode (same path, different DB). Local
    // state still describes the old file, so the stamp differs.
    let incoming = db_path.with_extension("incoming");
    std::fs::copy(&replacement_path, &incoming).unwrap();
    std::fs::rename(&incoming, &db_path).unwrap();

    // Search must reopen the on-disk inode and reconcile its manifest.
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let hits = store2
        .search(&resolved2, "replacement content", 5)
        .await
        .unwrap();

    assert!(
        hits.iter().any(|h| h.source == "doc2.txt"),
        "the replacement DB's document must be served after the stamp-triggered \
         reopen; hits: {:?}",
        hits.iter().map(|h| &h.source).collect::<Vec<_>>()
    );
    assert!(
        hits.iter().all(|h| h.source != "doc.txt"),
        "rows from the replaced (old inode) DB must not be served; hits: {:?}",
        hits.iter().map(|h| &h.source).collect::<Vec<_>>()
    );

    // State must be re-recorded to match the reopened DB.
    let state = std::fs::read_to_string(dir.path().join("v.db.peakbot/state.json")).unwrap();
    assert!(
        state.contains("last_head"),
        "state.json must be rewritten after a stamp-triggered reconcile: {state}"
    );
}

/// A corrupted replacement DB must be refused — never deleted, never rebuilt
/// from the anchored repo, never served from a stale inode — and must leave
/// the pending repair marker set.
///
/// (Safety-contract update: this test was `replaced_db_file_reopened_on_ensure_fresh`
/// and previously asserted that a garbage file was silently rebuilt by
/// rescanning the repo root, which indexed unrelated files the user never opted
/// into.)
#[tokio::test]
async fn corrupt_replaced_db_refused_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "initial content").unwrap();
    std::fs::write(dir.path().join("unrelated.txt"), "unrelated decoy body").unwrap();
    git_commit_all(dir.path(), "initial");

    // Index the legitimate source, establishing state.
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Replace the DB file on disk with garbage (new inode).
    let garbage = b"replaced by something that is not a vector db".to_vec();
    let tmp = db_path.with_extension("tmp");
    std::fs::write(&tmp, &garbage).unwrap();
    std::fs::rename(&tmp, &db_path).unwrap();

    let embeds_before = count.load(std::sync::atomic::Ordering::SeqCst);

    // Search must fail with a clear stale/corrupt error instead of rebuilding.
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let err = match store2.search(&resolved2, "initial", 5).await {
        Ok(hits) => panic!(
            "a corrupt replacement DB must error, not be rebuilt; got {} hits",
            hits.len()
        ),
        Err(err) => err,
    };
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("corrupt") || msg.contains("stale") || msg.contains("unreadable"),
        "expected a clear corrupt/stale error, got: {msg}"
    );

    // The replacement must be preserved byte-for-byte and flagged for repair.
    assert_eq!(
        std::fs::read(&db_path).unwrap(),
        garbage,
        "the corrupt replacement DB must not be deleted or rewritten"
    );
    assert!(
        dir.path().join("v.db.peakbot").join("pending").exists(),
        "a corrupt replacement must leave the pending repair marker set"
    );
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        embeds_before,
        "refusing a corrupt DB must not embed any file (no repo rescan)"
    );
}

// ── Reconcile behavior tests ───────────────────────────────────────────────

/// Reconcile replays manifest roots and discovers new files. Driving
/// DocIndexTool records `src/` as a root; a bare `index_file` deliberately does
/// not gain parent-directory coverage (see the safety-gap suite).
#[tokio::test]
async fn reconcile_discovers_new_files_under_roots() {
    use peakbot::tools::DocIndexTool;
    use rig_core::tool::Tool;

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, _count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let src_dir = dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let doc1 = src_dir.join("doc1.txt");
    std::fs::write(&doc1, "content one").unwrap();
    git_commit_all(dir.path(), "initial");

    // Record src/ as a recursive root.
    let store = VectorStore::open(&spec).unwrap();
    let tool = DocIndexTool::new(store.clone()).with_session_cwd(dir.path().to_path_buf());
    let args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(serde_json::json!({
        "path": "src",
        "recursive": true,
    }))
    .unwrap();
    tool.call(args).await.unwrap();
    drop(store);

    // Add a new file under the recorded root.
    let doc2 = src_dir.join("doc2.txt");
    std::fs::write(&doc2, "content two").unwrap();
    git_commit_all(dir.path(), "add doc2");

    // Search should discover doc2 via manifest replay.
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let hits = store2.search(&resolved2, "content two", 5).await.unwrap();
    assert!(
        hits.iter().any(|h| h.source == "src/doc2.txt"),
        "reconcile should discover new files under previously indexed roots; \
         hits: {:?}",
        hits.iter().map(|h| &h.source).collect::<Vec<_>>()
    );
}

/// Reconcile embeds only changed files (unchanged produce zero embeddings).
#[tokio::test]
async fn reconcile_unchanged_files_produce_zero_embeddings() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc1 = dir.path().join("doc1.txt");
    let doc2 = dir.path().join("doc2.txt");
    std::fs::write(&doc1, "content one").unwrap();
    std::fs::write(&doc2, "content two").unwrap();
    git_commit_all(dir.path(), "initial");

    // Index both files
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc1, &HashMap::new())
        .await
        .unwrap();
    store
        .index_file(&resolved, &doc2, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Trigger a reconcile with no changes (recreate state to force it)
    let state_dir = dir.path().join("v.db.peakbot");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("pending"), "pending").unwrap();

    let embeds_before = count.load(std::sync::atomic::Ordering::SeqCst);
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    store2.search(&resolved2, "content", 5).await.unwrap();

    // Search embeds the query (1 call); no files changed, so no file embeds.
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst) - embeds_before,
        1,
        "unchanged files should produce zero embedding calls during reconcile"
    );
}

/// Reconcile embeds only changed files.
#[tokio::test]
async fn reconcile_changed_file_embeds_only_that_file() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc1 = dir.path().join("doc1.txt");
    let doc2 = dir.path().join("doc2.txt");
    std::fs::write(&doc1, "content one").unwrap();
    std::fs::write(&doc2, "content two").unwrap();
    git_commit_all(dir.path(), "initial");

    // Index both files
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc1, &HashMap::new())
        .await
        .unwrap();
    store
        .index_file(&resolved, &doc2, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Change only doc1
    std::fs::write(&doc1, "changed content one").unwrap();
    git_commit_all(dir.path(), "change doc1");

    // Force reconcile
    let state_dir = dir.path().join("v.db.peakbot");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("pending"), "pending").unwrap();

    let embeds_before = count.load(std::sync::atomic::Ordering::SeqCst);
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    store2.search(&resolved2, "changed", 5).await.unwrap();

    // Search embeds the query (1 call); only doc1 changed, so 1 file embed.
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst) - embeds_before,
        2,
        "only changed file should be embedded; unchanged file skipped"
    );
}

/// Reconcile handles shrunk files (deletes trailing chunks).
#[tokio::test]
async fn reconcile_shrunk_file_deletes_trailing_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, _count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("large.txt");
    // Create content that will be multiple chunks (default is 1000 chars per chunk)
    let large = "This is line ".repeat(100) + "\n" + "unique old content at the end";
    std::fs::write(&doc, large).unwrap();
    git_commit_all(dir.path(), "initial large file");

    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Shrink to small content
    std::fs::write(&doc, "short content").unwrap();
    git_commit_all(dir.path(), "shrink file");

    // Force reconcile
    let state_dir = dir.path().join("v.db.peakbot");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("pending"), "pending").unwrap();

    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    store2.search(&resolved2, "short", 5).await.unwrap();

    // Old unique content should no longer be searchable. The new chunk (index 0,
    // "short content") remains; only the stale trailing chunks must be gone.
    let old_hits = store2
        .search(&resolved2, "unique old content", 5)
        .await
        .unwrap();
    assert!(
        old_hits
            .iter()
            .all(|h| !h.text.contains("unique old content")),
        "trailing chunks from larger version should be deleted after shrink; got: {:?}",
        old_hits.iter().map(|h| &h.text).collect::<Vec<_>>()
    );
}

/// Reconcile prunes deleted files.
#[tokio::test]
async fn reconcile_deleted_file_is_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, _count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("to_delete.txt");
    std::fs::write(&doc, "content to be deleted").unwrap();
    git_commit_all(dir.path(), "initial");

    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Delete the file
    std::fs::remove_file(&doc).unwrap();
    git_commit_all(dir.path(), "delete file");

    // Force reconcile
    let state_dir = dir.path().join("v.db.peakbot");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("pending"), "pending").unwrap();

    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    store2.search(&resolved2, "anything", 5).await.unwrap();

    // Deleted file should not be searchable
    let hits = store2
        .search(&resolved2, "content to be deleted", 5)
        .await
        .unwrap();
    assert!(
        hits.is_empty(),
        "deleted file should be pruned from search results after reconcile"
    );
}

/// Reconcile handles empty files.
#[tokio::test]
async fn reconcile_empty_file_handled() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, _count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("empty.txt");
    std::fs::write(&doc, "").unwrap();
    git_commit_all(dir.path(), "initial empty file");

    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Force reconcile
    let state_dir = dir.path().join("v.db.peakbot");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("pending"), "pending").unwrap();

    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    // Should not error on empty file
    let result = store2.search(&resolved2, "anything", 5).await;
    assert!(
        result.is_ok(),
        "reconcile should handle empty files without error"
    );
}

// ── State management tests ─────────────────────────────────────────────────

/// Missing state validates once with zero embeddings.
#[tokio::test]
async fn missing_state_validates_once_zero_embeddings() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "content").unwrap();
    git_commit_all(dir.path(), "initial");

    // Index the file
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    let embeds_after_index = count.load(std::sync::atomic::Ordering::SeqCst);

    // Delete state dir (simulate fresh clone with valid DB)
    let state_dir = dir.path().join("v.db.peakbot");
    if state_dir.exists() {
        std::fs::remove_dir_all(&state_dir).unwrap();
    }

    // Search should validate but not re-embed any files.
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    store2.search(&resolved2, "content", 5).await.unwrap();

    // Search embeds the query itself (1 call), but validate-once must not
    // re-embed any already-indexed file.
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst) - embeds_after_index,
        1,
        "missing state should validate once with zero file embedding calls"
    );
}

/// State is recorded only after complete successful reconcile.
#[tokio::test]
async fn state_recorded_only_after_complete_success() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, _count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "initial").unwrap();
    git_commit_all(dir.path(), "initial");

    // Index
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Make a change and trigger reconcile
    std::fs::write(&doc, "changed").unwrap();
    git_commit_all(dir.path(), "change");

    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    store2.search(&resolved2, "changed", 5).await.unwrap();

    // State should now be updated with the new HEAD
    let state_dir = dir.path().join("v.db.peakbot");
    let state_file = state_dir.join("state.json");
    assert!(
        state_file.exists(),
        "state.json should exist after successful reconcile"
    );
    let state = std::fs::read_to_string(&state_file).unwrap();
    let current_head = git_head(dir.path());
    assert!(
        state.contains(&current_head),
        "state.json should record the current HEAD after successful reconcile"
    );
    // Success clears the pending marker — state only lands on complete success.
    assert!(
        !state_dir.join("pending").exists(),
        "pending marker must be cleared after a complete reconcile"
    );
}

/// Local stores perform no git/state work.
#[tokio::test]
async fn local_store_performs_no_git_state_work() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("local.db");
    let (url, _count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    // No git repo
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "content").unwrap();

    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // No state dir should be created
    let state_dir = dir.path().join("local.db.peakbot");
    assert!(
        !state_dir.exists(),
        "local stores should not create state directories"
    );
}

/// Provider failure blocks search and leaves pending marker.
#[tokio::test]
async fn provider_failure_blocks_search_leaves_pending() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, _count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "content").unwrap();
    git_commit_all(dir.path(), "initial");

    // Index
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // The state recorded the initial HEAD before the change.
    let state_dir = dir.path().join("v.db.peakbot");
    let stale_head = git_head(dir.path());

    // Change the file to trigger reconcile
    std::fs::write(&doc, "changed content").unwrap();
    git_commit_all(dir.path(), "change");

    // Make provider fail by using a dead URL
    let dead_spec = VectorSpec {
        db_path: spec.db_path.clone(),
        embeddings: EmbeddingsConfig {
            base_url: "http://127.0.0.1:59999".to_string(),
            api_key: None,
            model: "test-model".into(),
            dimensions: 3,
        },
    };

    let store2 = VectorStore::open(&dead_spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let result = store2.search(&resolved2, "content", 5).await;

    // Search should fail due to provider error
    assert!(
        result.is_err(),
        "search should fail when provider is unreachable"
    );

    // A failed reconcile must leave the pending marker set and must NOT advance
    // the recorded state (no "fresh" claim on failure).
    assert!(
        state_dir.join("pending").exists(),
        "provider failure must leave the pending marker set"
    );
    let state = std::fs::read_to_string(state_dir.join("state.json")).unwrap();
    assert!(
        state.contains(&stale_head),
        "failed reconcile must not advance state.json past the last success: {state}"
    );
    assert!(
        !state.contains(&git_head(dir.path())),
        "failed reconcile must never record the new HEAD: {state}"
    );
}

// ── Real git merge conflict test ───────────────────────────────────────────

/// Real two-branch merge conflict: ensure_fresh detects it and defers with stale error.
#[tokio::test]
async fn real_git_merge_conflict_defers_with_stale_error() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, _count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    // Initialize repo and create base content
    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "base content").unwrap();
    git_commit_all(dir.path(), "base commit");

    // Index the base version
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    store
        .index_file(&resolved, &doc, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Create two branches with conflicting changes
    // Determine the default branch name (main or master).
    let default_branch = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let default_branch = String::from_utf8(default_branch.stdout)
        .unwrap()
        .trim()
        .to_string();

    // Create branch-a and switch to it.
    Command::new("git")
        .args(["checkout", "-b", "branch-a"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Modify on branch-a
    std::fs::write(&doc, "change on branch A").unwrap();
    git_commit_all(dir.path(), "change on branch A");

    // Switch back to the default branch, modify differently.
    Command::new("git")
        .args(["checkout", &default_branch])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::fs::write(&doc, "change on master").unwrap();
    git_commit_all(dir.path(), "change on master");

    // Now merge branch-a into the default branch - this will conflict.
    let merge_output = Command::new("git")
        .args(["merge", "branch-a"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // The merge should have failed with conflicts
    // (git reports exit code 1 on merge conflict)
    let has_conflict = !merge_output.status.success();
    if !has_conflict {
        // If git didn't conflict, check if we're in a merge state anyway
        panic!("expected merge conflict but got success");
    }

    // Verify we're in a merge-in-progress state
    assert!(
        git_merge_in_progress(dir.path()),
        "MERGE_HEAD should exist indicating merge-in-progress"
    );

    // Now search - ensure_fresh should detect the merge conflict
    // and defer with a stale error
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let result = store2.search(&resolved2, "content", 5).await;

    // Search should fail because we're in a merge-in-progress state
    assert!(
        result.is_err(),
        "search should fail with stale error when in git merge-in-progress state"
    );
}

// ── Legacy v1 compatibility tests ──────────────────────────────────────────

/// A populated v1 store (no v2 record) stays readable; writes require an
/// explicit migration and never run implicitly.
#[tokio::test]
async fn legacy_v1_store_stays_readable_and_writes_require_migration() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("legacy.db");
    let (url, _count) = spawn_mock_embeddings().await;

    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "legacy content").unwrap();
    build_v1_store(&db_path, &doc, "legacy content");

    let store = VectorStore::open(&vector_spec(db_path.clone(), &url)).unwrap();
    let resolved = store.db_path_for(Path::new(""));

    // Original data is readable.
    let hits = store.search(&resolved, "legacy", 5).await.unwrap();
    assert!(!hits.is_empty(), "legacy rows stay readable");

    // A write refuses before touching anything.
    let more = dir.path().join("more.txt");
    std::fs::write(&more, "more content").unwrap();
    let err = store
        .index_file(&resolved, &more, &HashMap::new())
        .await
        .expect_err("legacy stores require a migration before writing");
    assert!(
        matches!(err, VectorStoreError::MigrationRequired { .. }),
        "expected MigrationRequired, got: {err:?}"
    );

    let handle = store.handle_for_test(Path::new("")).await.unwrap();
    assert!(
        handle
            .load_config_value("peakbot.vector")
            .unwrap()
            .is_none(),
        "legacy store must not gain v2 metadata"
    );
}

// ── Multi-root manifest replay ─────────────────────────────────────────────

/// Reconcile replays the recorded multi-root manifest *including per-root
/// metadata*: a new file discovered under a metadata-bearing root inherits that
/// root's metadata (recursive walk + metadata, not just the file's raw text).
#[tokio::test]
async fn reconcile_applies_root_metadata_to_new_files() {
    use peakbot::tools::DocIndexTool;
    use rig_core::tool::Tool;

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v.db");
    let (url, _count) = spawn_mock_embeddings().await;
    let spec = vector_spec(db_path.clone(), &url);

    init_git_repo(dir.path());
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("doc1.txt"), "content one").unwrap();
    git_commit_all(dir.path(), "initial");

    // Record src/ as recursive with author=alice (doc_index carries the root).
    let store = VectorStore::open(&spec).unwrap();
    let mut md = HashMap::new();
    md.insert("author".to_string(), serde_json::json!("alice"));
    let tool = DocIndexTool::new(store.clone()).with_session_cwd(dir.path().to_path_buf());
    let args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(serde_json::json!({
        "path": "src",
        "recursive": true,
        "metadata": md,
    }))
    .unwrap();
    tool.call(args).await.unwrap();
    drop(store);

    // Add a new file under src/ and commit — reconcile discovers it.
    std::fs::write(src.join("doc2.txt"), "fresh content two").unwrap();
    git_commit_all(dir.path(), "add doc2");

    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let hits = store2
        .search(&resolved2, "fresh content two", 8)
        .await
        .unwrap();
    let hit = hits
        .iter()
        .find(|h| h.source == "src/doc2.txt")
        .expect("reconcile should discover src/doc2.txt under the recorded root");
    assert_eq!(
        hit.metadata.get("author"),
        Some(&serde_json::json!("alice")),
        "new file under a metadata-bearing root must inherit that root's metadata"
    );
}
