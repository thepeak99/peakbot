//! Multi-root repo-store regression tests (TDD: written before implementation).
//!
//! These encode the two contract gaps the builder report surfaced:
//!
//!  1. `VectorManifest` must hold an ordered/deterministic **collection** of
//!     doc_index roots — each with its own `recursive` flag and `metadata` —
//!     instead of a single `canonical_root`/`recursive`/`metadata` triple.
//!  2. The v2 DB is anchored to **one** repo root (derived from the DB
//!     location / session repo), not re-derived per source file. Indexing a
//!     source from a different repo therefore errors instead of silently
//!     colliding on repo-relative source ids.
//!
//! Assumed interface (no Architect doc names it explicitly — stated here so the
//! builder can align or tell us to adapt):
//!
//!  * `peakbot.vector.manifest.roots` is an ordered collection. Each entry
//!    carries a root path plus `recursive` and `metadata`. The tests accept an
//!    array of objects (`{path, recursive, metadata}`) or a map keyed by path
//!    (`{<path>: {recursive, metadata}}`) so a reasonable shape choice is not
//!    rejected; a flat single-root manifest fails, as intended.
//!  * Roots are recorded per `doc_index` call (root + recursive + metadata),
//!    which is why the multi-root tests drive `DocIndexTool` — it is the public
//!    API that carries all three values. Error-path tests stay on
//!    `VectorStore::index_file` to assert the store-level contract.
//!
//! Style mirrors `tests/vector_store.rs`: a local axum mock embeddings
//! endpoint (no network), plain `assert!`s, no shared mutable state.

use std::collections::HashMap;
use std::path::Path;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use peakbot::config::EmbeddingsConfig;
use peakbot::config::resolve::{DbPathSpec, VectorSpec};
use peakbot::vector::{VectorStore, VectorStoreError};
use serde_json::{Value, json};

#[cfg(feature = "host")]
use peakbot::tools::DocIndexTool;
#[cfg(feature = "host")]
use rig_core::tool::Tool;

/// ruvector config key holding the v2 record.
const META_KEY: &str = "peakbot.vector";

fn spec(db_path: std::path::PathBuf, base_url: &str) -> VectorSpec {
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

/// Mark `dir` as a git repo root. `repo_root_of` keys on a `.git` marker, so a
/// plain directory is a legitimate repo root for these tests.
fn mark_repo(dir: &Path) {
    std::fs::create_dir_all(dir.join(".git")).expect("create .git marker");
}

/// Deterministic user metadata: `{author: <value>}`.
fn user_meta(author: &str) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    m.insert("author".to_string(), json!(author));
    m
}

/// Local OpenAI-compatible embeddings endpoint returning deterministic vectors
/// (identical text → identical vector).
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

/// Read and parse the v2 metadata record from an already-indexed store.
async fn metadata_json(store: &VectorStore, session_cwd: &Path) -> Value {
    let handle = store.handle_for_test(session_cwd).await.unwrap();
    let raw = handle
        .load_config_value(META_KEY)
        .expect("read config value")
        .expect("peakbot.vector metadata present");
    serde_json::from_str(&raw).expect("metadata is valid JSON")
}

/// Extract the recorded root entries from the manifest, accepting the two
/// reasonable collection shapes. A flat single-root manifest yields an empty
/// vec, which the multi-root assertions reject with a clear message.
fn root_entries(manifest: &Value) -> Vec<Value> {
    // Array form: `roots: [{path, recursive, metadata}, …]`.
    for key in ["roots", "index_roots"] {
        if let Some(arr) = manifest.get(key).and_then(Value::as_array) {
            return arr.clone();
        }
    }
    // Map form: `roots: { <path>: {recursive, metadata}, … }`.
    for key in ["roots", "canonical_roots", "index_roots"] {
        if let Some(obj) = manifest.get(key).and_then(Value::as_object) {
            return obj
                .iter()
                .map(|(k, v)| {
                    let mut m = v.as_object().cloned().unwrap_or_else(serde_json::Map::new);
                    m.insert("path".to_string(), json!(k));
                    Value::Object(m)
                })
                .collect();
        }
    }
    Vec::new()
}

/// The root's path string (array element, object field, or bare string).
fn entry_path(entry: &Value) -> String {
    if let Some(s) = entry.as_str() {
        return s.to_string();
    }
    for key in ["path", "root", "canonical_root", "name"] {
        if let Some(s) = entry.get(key).and_then(Value::as_str) {
            return s.to_string();
        }
    }
    String::new()
}

fn entry_recursive(entry: &Value) -> Option<bool> {
    entry.get("recursive").and_then(Value::as_bool)
}

fn entry_metadata<'a>(entry: &'a Value, key: &str) -> Option<&'a Value> {
    entry.get("metadata").and_then(|m| m.get(key))
}

/// True when `path` has `component` as one of its path components (so a repo
/// named "src" does not masquerade as the "src" index root).
fn path_has_component(path: &str, component: &str) -> bool {
    path.replace('\\', "/").split('/').any(|c| c == component)
}

fn find_root<'a>(roots: &'a [Value], component: &str) -> Option<&'a Value> {
    roots
        .iter()
        .find(|r| path_has_component(&entry_path(r), component))
}

/// Drive `doc_index` the way the agent does: one call = one root, carrying the
/// recursive flag and per-call metadata.
#[cfg(feature = "host")]
async fn run_doc_index(
    store: &VectorStore,
    session_cwd: &Path,
    path: &str,
    recursive: bool,
    metadata: HashMap<String, Value>,
) {
    let tool = DocIndexTool::new(store.clone()).with_session_cwd(session_cwd.to_path_buf());
    let args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(json!({
        "path": path,
        "recursive": recursive,
        "metadata": metadata,
    }))
    .expect("doc_index args deserialize");
    tool.call(args).await.expect("doc_index call must succeed");
}

// ============================================================================
// CONTRACT 1 — one anchored repo root; a source from another repo errors
// ============================================================================

/// The v2 DB is anchored to the repo that contains it. Indexing a file
/// from a *different* repo into that DB must error (its repo-relative source
/// id would otherwise collide with the anchored repo's own `src/lib.rs`).
#[tokio::test]
async fn store_rejects_source_from_another_repo() {
    let base = spawn_embeddings(3).await;

    // Repo A contains the DB → it is the anchored root.
    let repo_a = tempfile::tempdir().unwrap();
    mark_repo(repo_a.path());
    let spec = spec(repo_a.path().join(".peakbot-vectors.db"), &base);
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));

    let file_a = repo_a.path().join("src/lib.rs");
    std::fs::create_dir_all(file_a.parent().unwrap()).unwrap();
    std::fs::write(&file_a, "fn a() {}").unwrap();
    store
        .index_file(&resolved, &file_a, &HashMap::new())
        .await
        .expect("file inside the anchored repo must index");

    // Repo B is a different repo with a colliding repo-relative path.
    let repo_b = tempfile::tempdir().unwrap();
    mark_repo(repo_b.path());
    let file_b = repo_b.path().join("src/lib.rs");
    std::fs::create_dir_all(file_b.parent().unwrap()).unwrap();
    std::fs::write(&file_b, "fn b() {}").unwrap();

    let err = store
        .index_file(&resolved, &file_b, &HashMap::new())
        .await
        .expect_err("a source from another repo must be rejected");
    assert!(
        matches!(err, VectorStoreError::OutsideRoot { .. }),
        "expected OutsideRoot for a source outside the anchored repo, got: {err:?}"
    );
}

// ============================================================================
// CONTRACT 2 — two roots in one repo persist independently
// ============================================================================

/// Two different doc_index roots in the same repo both persist, each with its
/// own `recursive` flag and its own `metadata`.
#[cfg(feature = "host")]
#[tokio::test]
async fn store_two_roots_keep_their_own_recursive_and_metadata() {
    let base = spawn_embeddings(3).await;
    let repo = tempfile::tempdir().unwrap();
    mark_repo(repo.path());
    let spec = spec(repo.path().join(".peakbot-vectors.db"), &base);
    let store = VectorStore::open(&spec).unwrap();

    // Root 1: src/ recursive=true, author=alice.
    let src = repo.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("mod.rs"), "pub mod a;").unwrap();
    run_doc_index(&store, repo.path(), "src", true, user_meta("alice")).await;

    // Root 2: docs/ recursive=false, author=bob.
    let docs = repo.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("guide.md"), "# Guide").unwrap();
    run_doc_index(&store, repo.path(), "docs", false, user_meta("bob")).await;

    let store2 = VectorStore::open(&spec).unwrap();
    let meta = metadata_json(&store2, Path::new("")).await;
    let roots = root_entries(&meta["manifest"]);

    assert!(
        roots.len() >= 2,
        "manifest must persist both doc_index roots, found {roots:?} — manifest: {}",
        meta["manifest"]
    );

    let src_entry =
        find_root(&roots, "src").unwrap_or_else(|| panic!("src root missing from {roots:?}"));
    let docs_entry =
        find_root(&roots, "docs").unwrap_or_else(|| panic!("docs root missing from {roots:?}"));

    assert_eq!(
        entry_recursive(src_entry),
        Some(true),
        "src root must keep recursive=true: {src_entry}"
    );
    assert_eq!(
        entry_recursive(docs_entry),
        Some(false),
        "docs root must keep recursive=false: {docs_entry}"
    );
    assert_eq!(
        entry_metadata(src_entry, "author"),
        Some(&json!("alice")),
        "src root must keep its own metadata: {src_entry}"
    );
    assert_eq!(
        entry_metadata(docs_entry, "author"),
        Some(&json!("bob")),
        "docs root must keep its own metadata: {docs_entry}"
    );
}

/// Recording roots is order-independent (deterministic), not insertion-ordered.
#[cfg(feature = "host")]
#[tokio::test]
async fn store_roots_are_deterministically_ordered() {
    let base = spawn_embeddings(3).await;
    let repo = tempfile::tempdir().unwrap();
    mark_repo(repo.path());
    let spec = spec(repo.path().join(".peakbot-vectors.db"), &base);
    let store = VectorStore::open(&spec).unwrap();

    for dir in ["src", "docs"] {
        let d = repo.path().join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("f.txt"), dir).unwrap();
    }
    // Record in reverse-sorted order on purpose.
    run_doc_index(&store, repo.path(), "src", true, user_meta("alice")).await;
    run_doc_index(&store, repo.path(), "docs", true, user_meta("bob")).await;

    let store2 = VectorStore::open(&spec).unwrap();
    let meta = metadata_json(&store2, Path::new("")).await;
    let roots = root_entries(&meta["manifest"]);
    assert!(
        roots.len() >= 2,
        "expected two recorded roots, found {roots:?}"
    );

    // A deterministic collection is sorted (like `sources`); "docs" precedes
    // "src" regardless of call order.
    let paths: Vec<String> = roots.iter().map(entry_path).collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(
        paths, sorted,
        "roots must be ordered deterministically, got: {paths:?}"
    );
}

// ============================================================================
// CONTRACT 3 — re-recording a root updates in place, no duplication
// ============================================================================

/// Re-recording the same canonical root updates its entry (no duplicate) and
/// leaves the other root's entry untouched.
#[cfg(feature = "host")]
#[tokio::test]
async fn store_rerecording_root_updates_without_duplicating() {
    let base = spawn_embeddings(3).await;
    let repo = tempfile::tempdir().unwrap();
    mark_repo(repo.path());
    let spec = spec(repo.path().join(".peakbot-vectors.db"), &base);
    let store = VectorStore::open(&spec).unwrap();

    let src = repo.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.rs"), "fn a() {}").unwrap();
    run_doc_index(&store, repo.path(), "src", true, user_meta("alice")).await;

    let docs = repo.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("g.md"), "# g").unwrap();
    run_doc_index(&store, repo.path(), "docs", false, user_meta("bob")).await;

    // Re-record `src` with a new file and new metadata.
    std::fs::write(src.join("b.rs"), "fn b() {}").unwrap();
    run_doc_index(&store, repo.path(), "src", true, user_meta("alice-v2")).await;

    let store2 = VectorStore::open(&spec).unwrap();
    let meta = metadata_json(&store2, Path::new("")).await;
    let roots = root_entries(&meta["manifest"]);

    let src_count = roots
        .iter()
        .filter(|r| path_has_component(&entry_path(r), "src"))
        .count();
    let docs_count = roots
        .iter()
        .filter(|r| path_has_component(&entry_path(r), "docs"))
        .count();

    assert_eq!(
        src_count, 1,
        "re-recording must not duplicate the src root: {roots:?}"
    );
    assert_eq!(
        docs_count, 1,
        "re-recording src must preserve docs exactly once: {roots:?}"
    );

    let src_entry = find_root(&roots, "src").unwrap();
    let docs_entry = find_root(&roots, "docs").unwrap();
    assert_eq!(
        entry_metadata(src_entry, "author"),
        Some(&json!("alice-v2")),
        "src root metadata must be updated in place: {src_entry}"
    );
    assert_eq!(
        entry_metadata(docs_entry, "author"),
        Some(&json!("bob")),
        "docs root metadata must be preserved: {docs_entry}"
    );
}

// ============================================================================
// CONTRACT 4 — per-root metadata differences are not lost
// ============================================================================

/// Multiple roots with several distinct metadata keys keep every key on its own
/// root — nothing merges into one shared metadata object.
#[cfg(feature = "host")]
#[tokio::test]
async fn store_root_metadata_differences_not_lost() {
    let base = spawn_embeddings(3).await;
    let repo = tempfile::tempdir().unwrap();
    mark_repo(repo.path());
    let spec = spec(repo.path().join(".peakbot-vectors.db"), &base);
    let store = VectorStore::open(&spec).unwrap();

    let src = repo.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "fn x() {}").unwrap();
    let mut src_meta = HashMap::new();
    src_meta.insert("author".to_string(), json!("alice"));
    src_meta.insert("lang".to_string(), json!("rust"));
    run_doc_index(&store, repo.path(), "src", true, src_meta).await;

    let docs = repo.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("guide.md"), "# g").unwrap();
    let mut docs_meta = HashMap::new();
    docs_meta.insert("author".to_string(), json!("bob"));
    docs_meta.insert("lang".to_string(), json!("markdown"));
    run_doc_index(&store, repo.path(), "docs", false, docs_meta).await;

    let store2 = VectorStore::open(&spec).unwrap();
    let meta = metadata_json(&store2, Path::new("")).await;
    let roots = root_entries(&meta["manifest"]);

    let src_entry =
        find_root(&roots, "src").unwrap_or_else(|| panic!("src root missing from {roots:?}"));
    let docs_entry =
        find_root(&roots, "docs").unwrap_or_else(|| panic!("docs root missing from {roots:?}"));

    assert_eq!(entry_metadata(src_entry, "author"), Some(&json!("alice")));
    assert_eq!(entry_metadata(src_entry, "lang"), Some(&json!("rust")));
    assert_eq!(entry_metadata(docs_entry, "author"), Some(&json!("bob")));
    assert_eq!(entry_metadata(docs_entry, "lang"), Some(&json!("markdown")));
}

// ============================================================================
// CONTRACT 5 — a DB outside any repo is a valid local store
// ============================================================================

/// A DB that lives outside any git repo is a valid *local* store: universal v2
/// metadata with `anchor: local`, absolute source IDs, and no state directory.
#[tokio::test]
async fn db_outside_any_repo_is_a_valid_local_store() {
    let base = spawn_embeddings(3).await;

    // A source file, outside any repo.
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("src/lib.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "fn main() {}").unwrap();

    // The DB also lives outside any repo (no `.git` ancestor).
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("local.db");
    let spec = spec(db_path.clone(), &base);
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));

    store
        .index_file(&resolved, &file, &HashMap::new())
        .await
        .expect("local store indexes an outside-repo file");

    let hits = store.search(&resolved, "fn", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert!(
        Path::new(&hits[0].source).is_absolute(),
        "local source IDs are absolute, got: {}",
        hits[0].source
    );

    let meta = metadata_json(&store, Path::new("")).await;
    assert_eq!(meta["schema"], 2, "local stores are universal v2");
    assert_eq!(meta["anchor"], "local");
    assert!(
        !db_path.with_extension("db.peakbot").exists(),
        "local stores create no state directory"
    );
}

// ============================================================================
// CONTRACT 6 — local source identity is absolute, not repo-scoped
// ============================================================================

/// A local store indexes files from different repos into one DB: outside a
/// repo there is no single root, so IDs are absolute paths.
#[tokio::test]
async fn local_store_indexes_across_repos() {
    let base = spawn_embeddings(3).await;
    let dir = tempfile::tempdir().unwrap();
    let spec = spec(dir.path().join("local.db"), &base);
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));

    let repo1 = tempfile::tempdir().unwrap();
    mark_repo(repo1.path());
    let file1 = repo1.path().join("src/a.rs");
    std::fs::create_dir_all(file1.parent().unwrap()).unwrap();
    std::fs::write(&file1, "fn a() {}").unwrap();

    let repo2 = tempfile::tempdir().unwrap();
    mark_repo(repo2.path());
    let file2 = repo2.path().join("src/b.rs");
    std::fs::create_dir_all(file2.parent().unwrap()).unwrap();
    std::fs::write(&file2, "fn b() {}").unwrap();

    store
        .index_file(&resolved, &file1, &HashMap::new())
        .await
        .expect("local store indexes repo1 file");
    store
        .index_file(&resolved, &file2, &HashMap::new())
        .await
        .expect("local store indexes repo2 file");

    let hits = store.search(&resolved, "fn", 10).await.unwrap();
    assert_eq!(
        hits.len(),
        2,
        "local store must keep both chunks; got sources: {:?}",
        hits.iter().map(|h| &h.source).collect::<Vec<_>>()
    );
    // Local stores keep absolute source paths (today's behaviour).
    assert!(
        hits.iter().all(|h| Path::new(&h.source).is_absolute()),
        "local source ids stay absolute: {:?}",
        hits.iter().map(|h| &h.source).collect::<Vec<_>>()
    );
}
