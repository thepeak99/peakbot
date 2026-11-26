//! Universal v2 vector store contract (TDD: written before implementation).
//!
//! Every vector store is v2 with the `peakbot.vector` metadata key, and the
//! config surface is just `db_path` + `embeddings`: behavior is derived from
//! where the DB file lives. The contract has two anchor modes (both v2, same
//! key/schema):
//!
//!  * Repo-contained: DB inside a git repo → repo-relative source IDs,
//!    reconcile on turn start even when DB is gitignored/untracked.
//!  * Local (outside git): DB outside any git repo → absolute local IDs,
//!    no state/pending, no git work, plain discovery (ignores .gitignore).
//!
//! Legacy v1 stores (no `peakbot.vector` metadata) are readable but write-
//! protected: search works, index_file/doc_index refuses with a migration
//! error before any write. Zero-row empty stores are adopted into v2 on
//! first write.
//!
//! Uses the universal `VectorSpec` interface: there is no mode field. The tests
//! verify behavior, not configuration.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use peakbot::config::EmbeddingsConfig;
use peakbot::config::resolve::{DbPathSpec, VectorSpec};
use peakbot::vector::VectorStore;
use serde_json::{Value, json};

use ruvector_core::types::{DbOptions, DistanceMetric, HnswConfig, VectorEntry};
use ruvector_core::vector_db::VectorDB;

/// The new universal v2 metadata key (replaces peakbot.vector).
const META_KEY: &str = "peakbot.vector";

// ── Helpers ────────────────────────────────────────────────────────────

fn spec(db_path: PathBuf, base_url: &str) -> VectorSpec {
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

async fn embed_handler(State(dimensions): State<usize>, Json(body): Json<Value>) -> Json<Value> {
    let inputs = body["input"].as_array().cloned().unwrap_or_default();
    let data: Vec<Value> = inputs
        .iter()
        .map(|input| {
            let text = input.as_str().unwrap_or_default();
            json!({ "embedding": fake_vector(text, dimensions) })
        })
        .collect();
    Json(json!({ "data": data }))
}

fn fake_vector(text: &str, dimensions: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; dimensions];
    for (i, b) in text.bytes().enumerate() {
        v[i % dimensions] += f32::from(b);
    }
    if v.iter().all(|v| *v == 0.0) {
        v[0] = 1.0;
    }
    v
}

/// Mark `dir` as a git repo root (creates `.git` marker directory).
fn mark_repo(dir: &Path) {
    std::fs::create_dir_all(dir.join(".git")).expect("create .git marker");
}

/// Run a git subcommand in `dir`.
fn run_git(dir: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git")
}

/// Initialize a REAL git repository (needed when a test exercises git-backed
/// reconcile/`git ls-files`, not just the `.git` marker).
fn init_git_repo(dir: &Path) {
    assert!(run_git(dir, &["init", "-q"]).status.success(), "git init");
    assert!(
        run_git(dir, &["config", "user.email", "test@example.com"])
            .status
            .success(),
        "git config email"
    );
    assert!(
        run_git(dir, &["config", "user.name", "Test User"])
            .status
            .success(),
        "git config name"
    );
}

fn git_commit_all(dir: &Path, message: &str) {
    assert!(run_git(dir, &["add", "-A"]).status.success(), "git add -A");
    let out = run_git(dir, &["commit", "-m", message]);
    assert!(
        out.status.success(),
        "git commit {message:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Read the v2 metadata record from an already-indexed store via handle.
async fn metadata_json(store: &VectorStore) -> Value {
    let handle = store.handle_for_test(Path::new("")).await.unwrap();
    let raw = handle
        .load_config_value(META_KEY)
        .expect("read config value")
        .expect("peakbot.vector metadata present");
    serde_json::from_str(&raw).expect("metadata is valid JSON")
}

/// Build a genuine v1 store directly via ruvector (no metadata key at all).
/// Inserts rows with absolute source paths so it mimics a legacy local store.
async fn build_v1_fixture(db_path: &Path, content: &str) {
    let dims = 3;
    let opts = DbOptions {
        dimensions: dims,
        distance_metric: DistanceMetric::Cosine,
        storage_path: db_path.display().to_string(),
        hnsw_config: Some(HnswConfig {
            max_elements: 100_000,
            ..HnswConfig::default()
        }),
        ..Default::default()
    };
    let db = VectorDB::new(opts).expect("create raw ruvector db");
    let mut metadata = HashMap::new();
    // A genuine v1 local store keys rows by an absolute source path.
    let legacy_source = db_path
        .parent()
        .unwrap_or_else(|| Path::new("/"))
        .join("legacy.txt");
    metadata.insert("source".into(), json!(legacy_source.display().to_string()));
    metadata.insert("chunk_index".into(), json!(0));
    metadata.insert("text".into(), json!(content));
    metadata.insert("sha256".into(), json!("legacy-hash"));
    db.insert_batch(&[VectorEntry {
        id: None,
        vector: fake_vector(content, dims),
        metadata: Some(metadata),
    }])
    .expect("insert legacy row");
}

fn state_dir_for(db_path: &Path) -> PathBuf {
    db_path.with_extension(format!(
        "{}.peakbot",
        db_path
            .extension()
            .map_or("db".to_string(), |e| e.to_string_lossy().into_owned())
    ))
}

fn hit_sources(hits: &[peakbot::vector::Hit]) -> Vec<String> {
    hits.iter().map(|h| h.source.clone()).collect()
}

fn hit_texts(hits: &[peakbot::vector::Hit]) -> Vec<String> {
    hits.iter().map(|h| h.text.clone()).collect()
}

// ══════════════════════════════════════════════════════════════════════
// Acceptance case 1: repo-contained DB writes v2 metadata, repo-relative
// IDs/roots, reconciles even when gitignored/untracked
// ══════════════════════════════════════════════════════════════════════

/// A fresh DB inside a git repo must write the `peakbot.vector` metadata
/// (schema=2), use repo-relative source IDs, and reconcile even when the
/// DB is gitignored/untracked.
#[tokio::test]
async fn fresh_repo_db_writes_v2_metadata_repo_relative_ids() {
    let base = spawn_embeddings(3).await;
    let dir = tempfile::tempdir().unwrap();
    mark_repo(dir.path());
    let db_path = dir.path().join("v.db");
    let spec = spec(db_path, &base);
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));

    // Materialize DB by indexing a test file inside the repo.
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "test content").unwrap();
    store
        .index_file(&resolved, &file, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Reopen and check for v2 metadata.
    let store2 = VectorStore::open(&spec).unwrap();
    let meta = metadata_json(&store2).await;
    assert_eq!(meta["schema"], 2, "top-level schema is 2");
    assert_eq!(meta["fingerprint"]["schema"], 2, "fingerprint schema is 2");
    assert!(
        meta["manifest"].get("anchored_root").is_none(),
        "the manifest must not persist an absolute repo path (clone portability); got: {meta}"
    );
    assert!(
        meta["manifest"]["sources"].is_array(),
        "sources is an array, got: {meta}"
    );

    // Verify repo-relative source ID.
    let hits = store2.search(&resolved, "test", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].source, "test.txt",
        "source ID is repo-relative with '/' separators"
    );
}

/// Reconcile must keep a repo-contained store fresh even when the DB file is
/// gitignored and untracked: a `HEAD` change brings a new tracked file under a
/// recorded root into the store, independent of the DB's own git status.
#[tokio::test]
async fn repo_db_reconciles_when_db_is_gitignored_and_untracked() {
    use peakbot::tools::DocIndexTool;
    use rig_core::tool::Tool;

    let base = spawn_embeddings(3).await;
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());

    // The conventional repo-contained DB, gitignored and never committed.
    std::fs::write(dir.path().join(".gitignore"), ".peakbot/vectors.db\n").unwrap();
    std::fs::create_dir_all(dir.path().join(".peakbot")).unwrap();
    std::fs::create_dir_all(dir.path().join("docs")).unwrap();
    std::fs::write(dir.path().join("docs/a.txt"), "alpha body").unwrap();
    git_commit_all(dir.path(), "base");
    let db_path = dir.path().join(".peakbot/vectors.db");

    // Index the `docs` root (records a root in the v2 manifest).
    let store = VectorStore::open(&spec(db_path.clone(), &base)).unwrap();
    let tool = DocIndexTool::new(store.clone()).with_session_cwd(dir.path().to_path_buf());
    let args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(json!({
        "path": "docs",
        "recursive": true,
    }))
    .unwrap();
    tool.call(args).await.unwrap();

    // Precondition: the DB is genuinely ignored/untracked.
    let ignored = run_git(dir.path(), &["check-ignore", ".peakbot/vectors.db"]);
    assert!(
        ignored.status.success(),
        "test precondition: the DB must be gitignored"
    );
    let tracked = run_git(
        dir.path(),
        &["ls-files", "--error-unmatch", ".peakbot/vectors.db"],
    );
    assert!(
        !tracked.status.success(),
        "test precondition: the DB must be untracked"
    );

    // A new tracked file under the recorded root, committed (HEAD moves).
    std::fs::write(dir.path().join("docs/b.txt"), "bravo body").unwrap();
    git_commit_all(dir.path(), "add docs/b.txt");

    // A fresh store + search must reconcile (HEAD changed) and bring b.txt in.
    let store2 = VectorStore::open(&spec(db_path.clone(), &base)).unwrap();
    let resolved = store2.db_path_for(dir.path());
    let hits = store2.search(&resolved, "body", 10).await.unwrap();
    let mut sources: Vec<String> = hit_sources(&hits);
    sources.sort();
    sources.dedup();
    assert!(
        sources.contains(&"docs/b.txt".to_string()),
        "reconcile must discover the new tracked file under the recorded root, \
         even with the DB gitignored/untracked; sources: {sources:?}"
    );
    assert!(
        sources.contains(&"docs/a.txt".to_string()),
        "the unchanged source must stay indexed; sources: {sources:?}"
    );
}

// ══════════════════════════════════════════════════════════════════════
// Acceptance case 2: DB outside any git repo writes v2 metadata, absolute
// local IDs/roots, no state/pending, no git work, ignores .gitignore
// ══════════════════════════════════════════════════════════════════════

/// A fresh DB outside any git repo writes v2 metadata, uses absolute local
/// source IDs, creates no state/pending directory, and performs no git work.
#[tokio::test]
async fn fresh_local_db_writes_v2_metadata_absolute_ids() {
    let base = spawn_embeddings(3).await;
    let dir = tempfile::tempdir().unwrap();
    // NO .git marker — this is outside any repo.
    let db_path = dir.path().join("local.db");
    let spec = spec(db_path.clone(), &base);
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));

    let file = dir.path().join("test.txt");
    std::fs::write(&file, "local test").unwrap();
    store
        .index_file(&resolved, &file, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Reopen and check for v2 metadata.
    let store2 = VectorStore::open(&spec).unwrap();
    let meta = metadata_json(&store2).await;
    assert_eq!(meta["schema"], 2, "top-level schema is 2");
    assert_eq!(meta["fingerprint"]["schema"], 2, "fingerprint schema is 2");

    // Verify absolute local source ID (not repo-relative).
    let hits = store2.search(&resolved, "local", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert!(
        Path::new(&hits[0].source).is_absolute(),
        "a local (outside-git) source ID must be an absolute path, got: {}",
        hits[0].source
    );
    assert!(
        hits[0].source.ends_with("test.txt"),
        "the absolute source ID must name the indexed file, got: {}",
        hits[0].source
    );

    // Verify no state/pending directory created.
    assert!(
        !state_dir_for(&db_path).exists(),
        "local store must not create a state/pending directory"
    );
}

/// In a local (outside-git) store, plain discovery must NOT apply
/// `.gitignore`: a file listed there is still indexed. This is the local-mode
/// counterpart of repo-mode git-aware discovery. The store must also be v2
/// with an absolute root.
#[tokio::test]
async fn local_doc_index_ignores_gitignore_and_writes_v2() {
    use peakbot::tools::DocIndexTool;
    use rig_core::tool::Tool;

    let base = spawn_embeddings(3).await;
    let dir = tempfile::tempdir().unwrap();
    // No `.git` marker: this directory is outside any repo.
    let db_path = dir.path().join("local.db");

    // `.gitignore` names `ignored.txt`, which a plain walk still visits.
    std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(dir.path().join("visible.txt"), "visible body").unwrap();
    std::fs::write(dir.path().join("ignored.txt"), "ignored body").unwrap();

    let store = VectorStore::open(&spec(db_path.clone(), &base)).unwrap();
    let tool = DocIndexTool::new(store.clone()).with_session_cwd(dir.path().to_path_buf());
    let args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(json!({
        "path": ".",
        "recursive": true,
    }))
    .unwrap();
    tool.call(args).await.unwrap();

    let resolved = store.db_path_for(dir.path());
    let hits = store.search(&resolved, "body", 10).await.unwrap();
    let sources = hit_sources(&hits);
    assert!(
        sources.iter().any(|s| s.ends_with("visible.txt")),
        "the plain walk must index visible.txt; sources: {sources:?}"
    );
    assert!(
        sources.iter().any(|s| s.ends_with("ignored.txt")),
        "local plain discovery must NOT apply .gitignore: ignored.txt must be indexed; sources: {sources:?}"
    );

    // Local stores are still v2 with an absolute root.
    let meta = metadata_json(&store).await;
    assert_eq!(meta["schema"], 2, "local store is v2 (schema=2)");
    let roots = meta["manifest"]["roots"].as_array().expect("roots array");
    assert_eq!(roots.len(), 1, "one absolute root recorded, got: {meta}");
    let root = roots[0]["root"].as_str().expect("root is a string");
    assert!(
        Path::new(root).is_absolute(),
        "a local store records an absolute root, got: {root}"
    );
}

// ══════════════════════════════════════════════════════════════════════
// Acceptance case 3: hand-built genuine v1 rows with NO metadata key
// ══════════════════════════════════════════════════════════════════════

/// A genuine v1 store (hand-built via ruvector, no metadata key) is readable
/// via search, but any write (index_file/doc_index) refuses with a migration
/// error BEFORE any write. No metadata/state/pending created. Original bytes
/// preserved.
#[tokio::test]
async fn genuine_v1_fixture_search_reads_write_refuses_migration() {
    let base = spawn_embeddings(3).await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("legacy.db");

    // Build a genuine v1 store directly via ruvector (no metadata key).
    build_v1_fixture(&db_path, "legacy content").await;

    // Search reads.
    let spec = spec(db_path.clone(), &base);
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    let hits = store.search(&resolved, "legacy", 10).await.unwrap();
    assert!(
        hits.iter().any(|h| h.text.contains("legacy content")),
        "v1 rows must be readable via search; hits: {:?}",
        hit_texts(&hits)
    );

    // Capture the v1 file's bytes and mtime so we can prove a refused write
    // leaves it byte-identical and untouched.
    let before_bytes = std::fs::read(&db_path).expect("read v1 bytes");
    let before_mtime = std::fs::metadata(&db_path)
        .expect("stat v1 db")
        .modified()
        .expect("mtime");

    // Write refuses with a migration error BEFORE any write.
    let file = dir.path().join("new.txt");
    std::fs::write(&file, "new content").unwrap();
    let err = store
        .index_file(&resolved, &file, &HashMap::new())
        .await
        .expect_err("index_file on a v1 store must refuse with a migration error");
    let err_debug = format!("{err:?}");
    assert!(
        err_debug.contains("MigrationRequired")
            || err_debug.to_lowercase().contains("migration")
            || err_debug.to_lowercase().contains("migrate"),
        "expected a migration-required error, got: {err_debug}"
    );

    // The v1 file must be byte-identical and have the same mtime.
    assert_eq!(
        std::fs::read(&db_path).expect("re-read v1 bytes"),
        before_bytes,
        "a refused write must not modify the v1 DB bytes"
    );
    assert_eq!(
        std::fs::metadata(&db_path)
            .expect("stat v1 db")
            .modified()
            .expect("mtime"),
        before_mtime,
        "a refused write must not touch the v1 DB mtime"
    );

    // No metadata key created.
    let handle = store.handle_for_test(Path::new("")).await.unwrap();
    let meta = handle
        .load_config_value(META_KEY)
        .expect("read config value");
    assert!(
        meta.is_none(),
        "index_file must not create v2 metadata on a v1 store before writing"
    );

    // No state/pending directory created.
    assert!(
        !state_dir_for(&db_path).exists(),
        "index_file must not create state/pending on a v1 store before writing"
    );

    // doc_index (the tool layer) must likewise refuse before writing anything.
    use peakbot::tools::DocIndexTool;
    use rig_core::tool::Tool;
    let tool = DocIndexTool::new(store.clone()).with_session_cwd(dir.path().to_path_buf());
    let doc_args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(json!({
        "path": "new.txt",
    }))
    .unwrap();
    let doc_err = tool
        .call(doc_args)
        .await
        .expect_err("doc_index on a v1 store must refuse with a migration error");
    let doc_err_msg = format!("{doc_err:?}").to_lowercase();
    assert!(
        doc_err_msg.contains("migration") || doc_err_msg.contains("migrate"),
        "doc_index must refuse with a migration error, got: {doc_err:?}"
    );
}

// ══════════════════════════════════════════════════════════════════════
// Acceptance case 4: zero-row empty legacy store adopted into v2
// ══════════════════════════════════════════════════════════════════════

/// A zero-row empty store (no rows, no metadata) is adopted into v2 on
/// first write, gaining the `peakbot.vector` metadata key.
#[tokio::test]
async fn empty_zero_row_store_adopted_into_v2_on_first_write() {
    let base = spawn_embeddings(3).await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("empty.db");

    // Create an empty ruvector DB with no rows and no metadata (like a
    // previously abandoned or manually created DB file).
    {
        let opts = DbOptions {
            dimensions: 3,
            distance_metric: DistanceMetric::Cosine,
            storage_path: db_path.display().to_string(),
            hnsw_config: Some(HnswConfig {
                max_elements: 100_000,
                ..HnswConfig::default()
            }),
            ..Default::default()
        };
        let db = VectorDB::new(opts).expect("create empty raw db");
        drop(db);
    }

    // Precondition: a real, redb-backed, row-less store with NO metadata key.
    let bytes = std::fs::read(&db_path).expect("empty db materialized on disk");
    assert!(
        bytes.starts_with(b"redb\x1A\x0A\xA9\x0D\x0A"),
        "precondition: a valid redb file, got {} bytes",
        bytes.len()
    );
    {
        let opts = DbOptions {
            dimensions: 3,
            distance_metric: DistanceMetric::Cosine,
            storage_path: db_path.display().to_string(),
            hnsw_config: Some(HnswConfig {
                max_elements: 100_000,
                ..HnswConfig::default()
            }),
            ..Default::default()
        };
        let db = VectorDB::new(opts).expect("reopen empty raw db");
        assert!(db.is_empty().expect("is_empty"), "precondition: zero rows");
        assert!(
            db.load_config_value(META_KEY)
                .expect("load config")
                .is_none(),
            "precondition: no v2 metadata key"
        );
        drop(db);
    }

    let spec = spec(db_path.clone(), &base);
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));

    // First write: indexing a file should adopt the store into v2.
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "first content").unwrap();
    store
        .index_file(&resolved, &file, &HashMap::new())
        .await
        .unwrap();

    // Now the store must have v2 metadata.
    let meta = metadata_json(&store).await;
    assert_eq!(meta["schema"], 2, "adopted store has schema=2");
    assert!(
        meta["manifest"]["sources"].is_array(),
        "adopted store has a sources array"
    );
}

// ══════════════════════════════════════════════════════════════════════
// Acceptance case 5: anchor mismatch errors (moved outside/inside repo)
// ══════════════════════════════════════════════════════════════════════

/// A repo-contained v2 DB moved outside any git repo must refuse writes with
/// a clear anchor Metadata error rather than mixing.
#[tokio::test]
async fn repo_db_moved_outside_git_refuses_write_with_anchor_error() {
    let base = spawn_embeddings(3).await;

    // Phase 1: create a repo-contained v2 store.
    let repo_dir = tempfile::tempdir().unwrap();
    mark_repo(repo_dir.path());
    let repo_db_path = repo_dir.path().join("v.db");
    let repo_spec = spec(repo_db_path.clone(), &base);
    let store = VectorStore::open(&repo_spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    let file = repo_dir.path().join("test.txt");
    std::fs::write(&file, "repo content").unwrap();
    store
        .index_file(&resolved, &file, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Phase 2: move the DB to an outside-git directory. Copy then remove the
    // original so the new path is a fresh inode (no live redb handle on the
    // same file), while still simulating a relocation.
    let outside_dir = tempfile::tempdir().unwrap();
    let outside_db_path = outside_dir.path().join("v.db");
    std::fs::copy(&repo_db_path, &outside_db_path).expect("copy db outside git");
    std::fs::remove_file(&repo_db_path).expect("remove original db");

    // Phase 3: attempt a write (index_file). Must fail with anchor error.
    let outside_spec = spec(outside_db_path.clone(), &base);
    let store2 = VectorStore::open(&outside_spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let file2 = outside_dir.path().join("new.txt");
    std::fs::write(&file2, "new content").unwrap();
    let err = store2
        .index_file(&resolved2, &file2, &HashMap::new())
        .await
        .expect_err("repo db moved outside git must refuse writes");
    let err_msg = format!("{err:?}").to_lowercase();
    assert!(
        err_msg.contains("anchor"),
        "error must mention anchor, got: {err:?}"
    );
    assert!(
        err_msg.contains("metadata") || err_msg.contains("repository") || err_msg.contains("repo"),
        "error must mention metadata or repository, got: {err:?}"
    );
}

/// A local (outside-git) v2 DB moved into a git repo must refuse writes with
/// a clear anchor Metadata error rather than mixing.
#[tokio::test]
async fn local_db_moved_into_git_refuses_write_with_anchor_error() {
    let base = spawn_embeddings(3).await;

    // Phase 1: create a local (outside-git) v2 store.
    let outside_dir = tempfile::tempdir().unwrap();
    let outside_db_path = outside_dir.path().join("local.db");
    let outside_spec = spec(outside_db_path.clone(), &base);
    let store = VectorStore::open(&outside_spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    let file = outside_dir.path().join("test.txt");
    std::fs::write(&file, "local content").unwrap();
    store
        .index_file(&resolved, &file, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Phase 2: move the DB into a git repo. Copy then remove the original so
    // the new path is a fresh inode (no live redb handle on the same file),
    // while still simulating a relocation.
    let repo_dir = tempfile::tempdir().unwrap();
    mark_repo(repo_dir.path());
    let repo_db_path = repo_dir.path().join("local.db");
    std::fs::copy(&outside_db_path, &repo_db_path).expect("copy db into git");
    std::fs::remove_file(&outside_db_path).expect("remove original db");

    // Phase 3: attempt a write (index_file). Must fail with anchor error.
    let repo_spec = spec(repo_db_path.clone(), &base);
    let store2 = VectorStore::open(&repo_spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let file2 = repo_dir.path().join("new.txt");
    std::fs::write(&file2, "new content").unwrap();
    let err = store2
        .index_file(&resolved2, &file2, &HashMap::new())
        .await
        .expect_err("local db moved into git must refuse writes");
    let err_msg = format!("{err:?}").to_lowercase();
    assert!(
        err_msg.contains("anchor"),
        "error must mention anchor, got: {err:?}"
    );
    assert!(
        err_msg.contains("metadata") || err_msg.contains("repository") || err_msg.contains("repo"),
        "error must mention metadata or repository, got: {err:?}"
    );
}

// ══════════════════════════════════════════════════════════════════════
// Acceptance case 6: metadata schema 3 refused as newer
// ══════════════════════════════════════════════════════════════════════

/// A DB with `peakbot.vector` metadata at schema=3 (newer than supported)
/// must be refused with a "newer" error, not treated as legacy/migration.
#[tokio::test]
async fn schema_3_metadata_refused_as_newer_not_legacy() {
    let base = spawn_embeddings(3).await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("future.db");

    // Write a DB with peakbot.vector metadata at schema=3.
    {
        let opts = DbOptions {
            dimensions: 3,
            distance_metric: DistanceMetric::Cosine,
            storage_path: db_path.display().to_string(),
            hnsw_config: Some(HnswConfig {
                max_elements: 100_000,
                ..HnswConfig::default()
            }),
            ..Default::default()
        };
        let db = VectorDB::new(opts).expect("create raw db");
        let future_meta = json!({
            "schema": 3,
            "fingerprint": {
                "schema": 3,
                "model": "test-model",
                "dimensions": 3,
                "metric": "cosine",
                "chunk_chars": 1000,
                "chunk_overlap": 200
            },
            "manifest": {
                "roots": [],
                "sources": ["future.txt"]
            }
        });
        db.save_config_value(META_KEY, &future_meta.to_string())
            .expect("write future metadata");
        drop(db);
    }

    // Try to read/write. Must fail with "newer" error, not legacy/migration.
    let spec = spec(db_path.clone(), &base);
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));

    // Search should fail (newer schema).
    let err = match store.search(&resolved, "test", 10).await {
        Ok(hits) => panic!(
            "schema 3 store must fail on search, got {} hits: {:?}",
            hits.len(),
            hit_sources(&hits)
        ),
        Err(err) => err,
    };
    let err_msg = format!("{err:?}").to_lowercase();
    assert!(
        err_msg.contains("newer")
            || err_msg.contains("version")
            || err_msg.contains("incompatible"),
        "expected 'newer' or version mismatch error, got: {err:?}"
    );
    assert!(
        !err_msg.contains("migration"),
        "schema 3 must not be treated as legacy/migration, got: {err:?}"
    );
}

// ══════════════════════════════════════════════════════════════════════
// Acceptance case 8: merge driver derives anchor from working %P
// ══════════════════════════════════════════════════════════════════════

/// The merge driver must derive its validation anchor from the working %P
/// path, not from the temporary %A/%B paths.
///
/// To make the distinction observable, both git temp inputs (`%A`, `%B`) are
/// placed in a directory **outside any git repo**, while `%P` points at a
/// worktree file **inside** a git repo. A correct driver resolves the anchor
/// from `%P` (repo) and merges; a driver that looks at the temp paths would
/// see "outside a repo" and refuse to mix, keeping ours only.
#[tokio::test]
async fn merge_driver_derives_anchor_from_working_p_not_temp_paths() {
    let base = spawn_embeddings(3).await;

    // The real worktree lives inside a git repo.
    let repo_dir = tempfile::tempdir().unwrap();
    mark_repo(repo_dir.path());
    let worktree = repo_dir.path().join("vectors.db");

    // Build two repo-anchored v2 stores (repo-relative ids) inside the repo.
    let ours_src = repo_dir.path().join("ours-src.db");
    let theirs_src = repo_dir.path().join("theirs-src.db");

    let ours_spec = spec(ours_src.clone(), &base);
    let store = VectorStore::open(&ours_spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    let file = repo_dir.path().join("ours.txt");
    std::fs::write(&file, "ours body").unwrap();
    store
        .index_file(&resolved, &file, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    let theirs_spec = spec(theirs_src.clone(), &base);
    let store2 = VectorStore::open(&theirs_spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let file2 = repo_dir.path().join("theirs.txt");
    std::fs::write(&file2, "theirs body").unwrap();
    store2
        .index_file(&resolved2, &file2, &HashMap::new())
        .await
        .unwrap();
    drop(store2);

    // Git's temp files (%A/%B) live OUTSIDE any repo.
    let temp_dir = tempfile::tempdir().unwrap();
    assert!(!temp_dir.path().join(".git").exists());
    let a = temp_dir.path().join("a.db"); // %A — current/ours
    let b = temp_dir.path().join("b.db"); // %B — theirs
    std::fs::copy(&ours_src, &a).expect("copy ours to %A");
    std::fs::copy(&theirs_src, &b).expect("copy theirs to %B");

    // The working file at %P is inside the repo, as git would pass it.
    let ok = peakbot::vector::merge_driver::run([
        "/nonexistent-base.db",     // %O ignored
        a.to_str().unwrap(),        // %A outside any repo
        b.to_str().unwrap(),        // %B outside any repo
        worktree.to_str().unwrap(), // %P inside the repo
    ])
    .await
    .is_ok();
    assert!(
        ok,
        "the driver must derive its anchor from %P (inside a repo) and succeed, \
         not from the out-of-repo %A/%B"
    );

    // Git replaces the working-tree file with the driver's %A output. Do that
    // explicitly so we inspect exactly what the user would get at %P.
    std::fs::copy(&a, &worktree).expect("git would move %A into %P");

    // The merged result must be at %P with both sources (repo-relative ids).
    let worktree_spec = spec(worktree.clone(), &base);
    let worktree_store = VectorStore::open(&worktree_spec).unwrap();
    let worktree_resolved = worktree_store.db_path_for(Path::new(""));
    let hits = worktree_store
        .search(&worktree_resolved, "body", 10)
        .await
        .unwrap();
    let mut sources: Vec<String> = hit_sources(&hits);
    sources.sort();
    assert_eq!(
        sources,
        vec!["ours.txt".to_string(), "theirs.txt".to_string()],
        "the merged DB at %P must contain both ours and theirs relative ids"
    );
}
