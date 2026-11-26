//! PR1 vector DB universal v2 behavior tests (TDD: written before implementation).
//!
//! Tests the locked PR1 contract:
//! - Fresh v2 stores use v2 schema with `peakbot.vector` metadata
//! - Fingerprint includes {schema, model, dimensions, metric, chunk_chars, chunk_overlap}
//! - Fingerprint excludes api_key and base_url
//! - Canonical source IDs are repo-relative (path-independent)
//! - Fingerprint mismatch on reopen errors without writes
//! - Existing nonempty v1 stores stay readable; writes require an explicit migration
//! - Local stores write v2 metadata with absolute IDs and keep no state directory
//!
//! Uses only the public API: index_file, search, db_path_for, handle_for_test.
//! DB materialization is triggered via index_file.
//!
//! The embeddings endpoint is a local axum server (no external network), so the
//! real parse → chunk → embed → store path runs end to end.

use std::path::{Path, PathBuf};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use peakbot::config::EmbeddingsConfig;
use peakbot::config::resolve::{DbPathSpec, VectorSpec};
use peakbot::vector::{VectorStore, VectorStoreError};
use serde_json::{Value, json};

/// ruvector config key holding the v2 record.
const META_KEY: &str = "peakbot.vector";

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

/// Mark `dir` as a git repo root. `canonical_source` keys on a `.git` marker,
/// so a plain directory is a legitimate repo root for these tests.
fn mark_repo(dir: &Path) {
    std::fs::create_dir_all(dir.join(".git")).expect("create .git marker");
}

/// Local OpenAI-compatible embeddings endpoint returning deterministic
/// vectors (identical text → identical vector).
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
    let mut metadata = std::collections::HashMap::new();
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

/// Read and parse the v2 metadata record from an already-indexed store.
async fn metadata_json(store: &VectorStore) -> Value {
    let handle = store.handle_for_test(Path::new("")).await.unwrap();
    let raw = handle
        .load_config_value(META_KEY)
        .expect("read config value")
        .expect("peakbot.vector metadata present");
    serde_json::from_str(&raw).expect("metadata is valid JSON")
}

/// Fresh v2 store writes `peakbot.vector` metadata with schema=2 and an
/// empty roots array (roots are recorded by doc_index). It persists **no**
/// absolute repo path — the repo root is derived locally from the DB location,
/// which is what keeps the store portable across clones.
#[tokio::test]
async fn fresh_store_writes_v2_metadata() {
    let base = spawn_embeddings(3).await;
    let dir = tempfile::tempdir().unwrap();
    mark_repo(dir.path());
    let db_path = dir.path().join("v.db");
    let spec = spec(db_path, &base);
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));

    // Materialize DB by indexing a test file.
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "test content").unwrap();
    store
        .index_file(&resolved, &file, &std::collections::HashMap::new())
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
        meta["manifest"]["roots"].is_array(),
        "roots is an array, got: {meta}"
    );
    assert!(
        meta["manifest"]["roots"].as_array().unwrap().is_empty(),
        "roots is empty until doc_index records a root, got: {meta}"
    );
}

/// Fresh v2 store fingerprint includes all required fields.
#[tokio::test]
async fn store_fingerprint_contains_required_fields() {
    let base = spawn_embeddings(3).await;
    let dir = tempfile::tempdir().unwrap();
    mark_repo(dir.path());
    let db_path = dir.path().join("v.db");
    let spec = spec(db_path, &base);
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));

    let file = dir.path().join("test.txt");
    std::fs::write(&file, "test content").unwrap();
    store
        .index_file(&resolved, &file, &std::collections::HashMap::new())
        .await
        .unwrap();
    drop(store);

    let store2 = VectorStore::open(&spec).unwrap();
    let fp = &metadata_json(&store2).await["fingerprint"];
    assert_eq!(fp["schema"], 2);
    assert_eq!(fp["model"], "test-model");
    assert_eq!(fp["dimensions"], 3);
    assert_eq!(fp["metric"], "cosine");
    assert_eq!(fp["chunk_chars"], 1000);
    assert_eq!(fp["chunk_overlap"], 200);
}

/// Different api_key/base_url do not change the fingerprint.
#[tokio::test]
async fn fingerprint_ignores_api_key_and_base_url() {
    let base1 = spawn_embeddings(3).await;
    let base2 = spawn_embeddings(3).await;
    let dir1 = tempfile::tempdir().unwrap();
    let dir2 = tempfile::tempdir().unwrap();
    mark_repo(dir1.path());
    mark_repo(dir2.path());
    let db1 = dir1.path().join("a.db");
    let db2 = dir2.path().join("b.db");

    let spec1 = VectorSpec {
        db_path: DbPathSpec::Fixed(db1),
        embeddings: EmbeddingsConfig {
            base_url: base1,
            api_key: Some("key1".into()),
            model: "test-model".into(),
            dimensions: 3,
        },
    };
    let spec2 = VectorSpec {
        db_path: DbPathSpec::Fixed(db2),
        embeddings: EmbeddingsConfig {
            base_url: base2,
            api_key: Some("key2".into()),
            model: "test-model".into(),
            dimensions: 3,
        },
    };

    let store1 = VectorStore::open(&spec1).unwrap();
    let resolved1 = store1.db_path_for(Path::new(""));
    let file1 = dir1.path().join("test.txt");
    std::fs::write(&file1, "test").unwrap();
    store1
        .index_file(&resolved1, &file1, &std::collections::HashMap::new())
        .await
        .unwrap();
    drop(store1);

    let store2 = VectorStore::open(&spec2).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let file2 = dir2.path().join("test.txt");
    std::fs::write(&file2, "test").unwrap();
    store2
        .index_file(&resolved2, &file2, &std::collections::HashMap::new())
        .await
        .unwrap();
    drop(store2);

    let s1 = VectorStore::open(&spec1).unwrap();
    let s2 = VectorStore::open(&spec2).unwrap();
    let fp1 = metadata_json(&s1).await["fingerprint"].clone();
    let fp2 = metadata_json(&s2).await["fingerprint"].clone();
    assert_eq!(fp1, fp2, "fingerprint must ignore api_key and base_url");
}

/// Same repo-relative path indexed in two separate v2 stores (one per
/// clone) yields the same source id — path independence holds. Under the
/// single-repo anchoring contract, a v2 DB cannot index files from two
/// different repos, so this test uses two stores.
#[tokio::test]
async fn same_repo_file_different_paths_same_source_id() {
    let base = spawn_embeddings(3).await;
    let clone1 = tempfile::tempdir().unwrap();
    let clone2 = tempfile::tempdir().unwrap();
    mark_repo(clone1.path());
    mark_repo(clone2.path());

    let file1 = clone1.path().join("src/foo.rs");
    let file2 = clone2.path().join("src/foo.rs");
    std::fs::create_dir_all(file1.parent().unwrap()).unwrap();
    std::fs::create_dir_all(file2.parent().unwrap()).unwrap();
    std::fs::write(&file1, "fn foo() {}").unwrap();
    std::fs::write(&file2, "fn foo() {}").unwrap();

    // Store 1: anchored to clone1.
    let spec1 = spec(clone1.path().join("v.db"), &base);
    let store1 = VectorStore::open(&spec1).unwrap();
    let db1 = store1.db_path_for(Path::new(""));
    store1
        .index_file(&db1, &file1, &std::collections::HashMap::new())
        .await
        .unwrap();
    let hits1 = store1.search(&db1, "foo", 10).await.unwrap();
    assert_eq!(hits1.len(), 1);
    assert_eq!(
        hits1[0].source, "src/foo.rs",
        "source ID is repo-relative with '/' separators (clone1)"
    );

    // Store 2: anchored to clone2.
    let spec2 = spec(clone2.path().join("v.db"), &base);
    let store2 = VectorStore::open(&spec2).unwrap();
    let db2 = store2.db_path_for(Path::new(""));
    store2
        .index_file(&db2, &file2, &std::collections::HashMap::new())
        .await
        .unwrap();
    let hits2 = store2.search(&db2, "foo", 10).await.unwrap();
    assert_eq!(hits2.len(), 1);
    assert_eq!(
        hits2[0].source, "src/foo.rs",
        "source ID is repo-relative with '/' separators (clone2)"
    );
}

/// Different files in same repo yield different source IDs.
#[tokio::test]
async fn different_files_same_repo_different_source_ids() {
    let base = spawn_embeddings(3).await;
    let clone = tempfile::tempdir().unwrap();
    mark_repo(clone.path());
    let file1 = clone.path().join("src/foo.rs");
    let file2 = clone.path().join("src/bar.rs");
    std::fs::create_dir_all(file1.parent().unwrap()).unwrap();
    std::fs::write(&file1, "fn foo() {}").unwrap();
    std::fs::write(&file2, "fn bar() {}").unwrap();

    // DB lives inside the repo (anchoring requires it).
    let db_path = clone.path().join("v.db");
    let spec = spec(db_path, &base);
    let store = VectorStore::open(&spec).unwrap();
    let db_path = store.db_path_for(Path::new(""));

    store
        .index_file(&db_path, &file1, &std::collections::HashMap::new())
        .await
        .unwrap();
    store
        .index_file(&db_path, &file2, &std::collections::HashMap::new())
        .await
        .unwrap();

    // Both chunks are present, each with its own repo-relative source id.
    let hits = store.search(&db_path, "anything", 10).await.unwrap();
    let mut sources: Vec<String> = hits.iter().map(|h| h.source.clone()).collect();
    sources.sort();
    sources.dedup();
    assert_eq!(
        sources,
        vec!["src/bar.rs".to_string(), "src/foo.rs".to_string()],
        "different files must have different source IDs"
    );

    let meta = metadata_json(&store).await;
    assert_eq!(
        meta["manifest"]["sources"],
        json!(["src/bar.rs", "src/foo.rs"])
    );
}

/// Reopening v2 store with mismatched fingerprint errors.
#[tokio::test]
async fn store_fingerprint_mismatch_errors_on_reopen() {
    let base = spawn_embeddings(3).await;
    let dir = tempfile::tempdir().unwrap();
    mark_repo(dir.path());
    let db_path = dir.path().join("v.db");

    let spec1 = spec(db_path.clone(), &base);
    let store1 = VectorStore::open(&spec1).unwrap();
    let resolved = store1.db_path_for(Path::new(""));
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "test").unwrap();
    store1
        .index_file(&resolved, &file, &std::collections::HashMap::new())
        .await
        .unwrap();
    drop(store1);

    let spec2 = VectorSpec {
        db_path: DbPathSpec::Fixed(db_path.clone()),
        embeddings: EmbeddingsConfig {
            base_url: base,
            api_key: None,
            model: "different-model".into(),
            dimensions: 3,
        },
    };
    let store2 = VectorStore::open(&spec2).unwrap();
    let resolved = store2.db_path_for(Path::new(""));
    // Search triggers reopen — should error on fingerprint mismatch.
    let result = store2.search(&resolved, "test", 10).await;
    assert!(
        result.is_err(),
        "reopening with mismatched fingerprint should error"
    );
}

/// A populated v1 store (no v2 metadata) stays readable, but writes refuse
/// with a migration error before touching any bytes or creating state.
#[tokio::test]
async fn existing_v1_store_reads_but_writes_require_migration() {
    let base = spawn_embeddings(3).await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("legacy.db");

    let file = dir.path().join("test.txt");
    std::fs::write(&file, "test content").unwrap();
    build_v1_store(&db_path, &file, "test content");

    let spec = spec(db_path.clone(), &base);
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));
    let hits = store.search(&resolved, "test", 10).await.unwrap();
    assert_eq!(hits.len(), 1, "v1 rows stay readable");

    // Capture after the read (opening redb can update its header) and before
    // the refused write.
    let before = std::fs::read(&db_path).unwrap();

    // Writing refuses before any write: the v1 file is unchanged and never
    // gains v2 metadata.
    let more = dir.path().join("more.txt");
    std::fs::write(&more, "more content").unwrap();
    let err = store
        .index_file(&resolved, &more, &std::collections::HashMap::new())
        .await
        .expect_err("v1 stores must require a migration before writing");
    assert!(
        matches!(err, VectorStoreError::MigrationRequired { .. }),
        "expected MigrationRequired, got: {err:?}"
    );
    assert_eq!(std::fs::read(&db_path).unwrap(), before, "bytes unchanged");

    let handle = store.handle_for_test(Path::new("")).await.unwrap();
    assert!(
        handle.load_config_value(META_KEY).unwrap().is_none(),
        "v1 store must not gain peakbot.vector metadata"
    );
    assert!(
        !dir.path().join("legacy.db.peakbot").exists(),
        "v1 refusal must not create state"
    );
}

/// Local (outside-git) stores are universal v2 too: absolute source IDs and a
/// `peakbot.vector` record (with `anchor: local`), but no state directory.
#[tokio::test]
async fn local_store_writes_universal_v2_metadata() {
    let base = spawn_embeddings(3).await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("local.db");
    let spec = spec(db_path, &base);
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));

    let file = dir.path().join("test.txt");
    std::fs::write(&file, "test").unwrap();
    store
        .index_file(&resolved, &file, &std::collections::HashMap::new())
        .await
        .unwrap();
    drop(store);

    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));
    let hits = store2.search(&resolved2, "test", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].source,
        file.canonicalize().unwrap().display().to_string(),
        "local stores use the absolute source path"
    );

    let handle = store2.handle_for_test(Path::new("")).await.unwrap();
    let raw = handle
        .load_config_value(META_KEY)
        .unwrap()
        .expect("local store has v2 metadata");
    let meta: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(meta["schema"], 2);
    assert_eq!(meta["anchor"], "local");
}

/// Local stores create no state directory.
#[tokio::test]
async fn local_store_creates_no_state_directory() {
    let base = spawn_embeddings(3).await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("local.db");
    let spec = spec(db_path.clone(), &base);
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));

    let file = dir.path().join("test.txt");
    std::fs::write(&file, "test").unwrap();
    store
        .index_file(&resolved, &file, &std::collections::HashMap::new())
        .await
        .unwrap();
    drop(store);

    let state_dir = dir.path().join("local.db.peakbot");
    assert!(
        !state_dir.exists(),
        "local store should not create a state directory"
    );
}

/// The v2 manifest is deterministic: roots are sorted, each with its own
/// recursive flag and metadata; sources are sorted globally. This test drives
/// DocIndexTool (the layer that carries root + recursive + metadata), not
/// index_file directly.
#[tokio::test]
async fn store_manifest_is_deterministic() {
    use peakbot::tools::DocIndexTool;
    use rig_core::tool::Tool;

    let base = spawn_embeddings(3).await;
    let dir = tempfile::tempdir().unwrap();
    mark_repo(dir.path());
    let db_path = dir.path().join("v.db");
    let spec = spec(db_path, &base);
    let store = VectorStore::open(&spec).unwrap();

    // Two doc_index roots recorded in reverse-sorted order to prove
    // deterministic ordering.
    let src = dir.path().join("src");
    let docs = dir.path().join("docs");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(src.join("a.rs"), "fn a() {}").unwrap();
    std::fs::write(docs.join("guide.md"), "# Guide").unwrap();

    // Record `src` first (reverse-sorted on purpose).
    let tool_src = DocIndexTool::new(store.clone()).with_session_cwd(dir.path().to_path_buf());
    let src_args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(json!({
        "path": "src",
        "recursive": true,
    }))
    .unwrap();
    tool_src.call(src_args).await.unwrap();

    // Record `docs` second.
    let tool_docs = DocIndexTool::new(store.clone()).with_session_cwd(dir.path().to_path_buf());
    let docs_args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(json!({
        "path": "docs",
        "recursive": false,
    }))
    .unwrap();
    tool_docs.call(docs_args).await.unwrap();

    drop(store);

    // Reopen and check the manifest.
    let store2 = VectorStore::open(&spec).unwrap();
    let meta = metadata_json(&store2).await;
    let manifest = &meta["manifest"];

    // Sources are sorted globally, not insertion-ordered.
    assert_eq!(
        manifest["sources"],
        json!(["docs/guide.md", "src/a.rs"]),
        "sources are sorted, not insertion-ordered"
    );

    // Roots are sorted by path; `docs` precedes `src`.
    let roots = manifest["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    assert!(
        roots[0]["root"].as_str().unwrap().ends_with("docs"),
        "docs root comes first (deterministic sort), got: {}",
        roots[0]["root"]
    );
    assert!(
        roots[1]["root"].as_str().unwrap().ends_with("src"),
        "src root comes second, got: {}",
        roots[1]["root"]
    );
    assert_eq!(roots[0]["recursive"], false);
    assert_eq!(roots[1]["recursive"], true);
}

/// A repo-anchored store rejects sources outside its repository: relative IDs
/// have no meaning for a file the repo does not contain.
#[tokio::test]
async fn repo_store_rejects_source_outside_repo() {
    let base = spawn_embeddings(3).await;
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("loose.txt");
    std::fs::write(&file, "loose").unwrap();

    let dir = tempfile::tempdir().unwrap();
    mark_repo(dir.path());
    let db_path = dir.path().join("v.db");
    let spec = spec(db_path, &base);
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));

    let err = store
        .index_file(&resolved, &file, &std::collections::HashMap::new())
        .await
        .expect_err("a source outside any repo must be rejected");
    assert!(
        matches!(err, VectorStoreError::OutsideRoot { .. }),
        "expected OutsideRoot, got: {err:?}"
    );
}
