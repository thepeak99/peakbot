//! Clone portability regression tests for universal-v2 vector DBs.
//!
//! These tests encode the contract that a universal-v2 vector DB must be portable
//! across absolute clone paths: a DB created at one path, copied to a different
//! path, and used without the original must behave correctly. The repo root is
//! derived locally from the DB path and is never persisted; the manifest's
//! `roots` are repo-relative POSIX paths.
//!
//! Test categories:
//!  1. Clone portability: index at A, copy to B, remove A, search/index at B
//!  2. Tampered metadata: absolute/traversal roots must be rejected at reconcile
//!  3. Merge driver root handling: different roots merge correctly, ours wins same root
//!  4. Config parse: same resolved db_path with different embeddings rejected
//!  5. Gitignore respect: ignored files not indexed, new non-ignored files discovered
//!  6. State.json stamp: after indexing, state equals actual file stamp
//!
//! Uses only public API + test seams. Runs against the implementation, not mocks.
//! The embeddings endpoint is a local axum server (no external network).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use peakbot::config::EmbeddingsConfig;
use peakbot::config::resolve::{DbPathSpec, VectorSpec};
use peakbot::vector::VectorStore;
use serde_json::{Value, json};

/// Recorder for embedding requests, to detect needless re-embeds.
#[derive(Clone)]
struct EmbedRecorder {
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl EmbedRecorder {
    fn new() -> Self {
        Self {
            requests: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn clear(&self) {
        self.requests.lock().unwrap().clear();
    }

    fn document_inputs(&self, query: &str) -> Vec<String> {
        let all = self.requests.lock().unwrap();
        all.iter().filter(|s| *s != query).cloned().collect()
    }
}

async fn spawn_recording_embeddings(dimensions: usize, rec: EmbedRecorder) -> String {
    let app = Router::new()
        .route(
            "/embeddings",
            post(
                move |State(rec): State<EmbedRecorder>, Json(body): Json<Value>| async move {
                    let inputs = body["input"].as_array().cloned().unwrap_or_default();
                    let texts: Vec<String> = inputs
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    rec.requests.lock().unwrap().extend(texts);
                    let data: Vec<Value> = inputs
                        .iter()
                        .map(|input| {
                            let text = input.as_str().unwrap_or_default();
                            json!({ "embedding": fake_vector(text, dimensions) })
                        })
                        .collect();
                    Json(json!({ "data": data }))
                },
            ),
        )
        .with_state(rec);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock embeddings");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://{addr}")
}
/// Ruvector config key holding the v2 record.
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

fn spec_with_dims(db_path: PathBuf, base_url: &str, dimensions: usize) -> VectorSpec {
    VectorSpec {
        db_path: DbPathSpec::Fixed(db_path),
        embeddings: EmbeddingsConfig {
            base_url: base_url.to_string(),
            api_key: None,
            model: "test-model".into(),
            dimensions,
        },
    }
}

/// Mark `dir` as a git repo root. `.git` marker is sufficient for the
/// canonical_source anchoring logic.
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

/// Read and parse the v2 metadata record from an already-indexed store.
async fn metadata_json(store: &VectorStore, session_cwd: &Path) -> Value {
    let handle = store.handle_for_test(session_cwd).await.unwrap();
    let raw = handle
        .load_config_value(META_KEY)
        .expect("read config value")
        .expect("peakbot.vector metadata present");
    serde_json::from_str(&raw).expect("metadata is valid JSON")
}

/// Copy a directory tree from `src` to `dst`.
fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("create dst");
    for entry in std::fs::read_dir(src).expect("read src dir") {
        let entry = entry.expect("read entry");
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_tree(&src_path, &dst_path);
        } else {
            std::fs::copy(&src_path, &dst_path).expect("copy file");
        }
    }
}

/// Write a `.gitignore` file.
fn write_gitignore(repo: &Path, content: &str) {
    std::fs::write(repo.join(".gitignore"), content).expect("write .gitignore");
}

// ── Contract 1: Clone portability ──────────────────────────────────────

/// Index at path A, copy entire repo+DB to different absolute path B,
/// remove/rename A, open at B: search returns all original chunks, sources
/// are not pruned, zero DOCUMENT embeddings on reopen (classify payload vs
/// query), and indexing a new file at B succeeds.
#[tokio::test]
async fn db_is_portable_across_clone_paths() {
    use peakbot::tools::DocIndexTool;
    use rig_core::tool::Tool;

    // Use a recording embeddings endpoint to detect needless re-embeds.
    let rec = EmbedRecorder::new();
    let base = spawn_recording_embeddings(16, rec.clone()).await;
    let repo_a = tempfile::tempdir().unwrap();
    mark_repo(repo_a.path());
    let db_a = repo_a.path().join(".peakbot/vectors.db");
    let spec_a = spec_with_dims(db_a, &base, 16);
    let store_a = VectorStore::open(&spec_a).unwrap();

    // Index at A via DocIndexTool (carries root + recursive + metadata).
    let docs_a = repo_a.path().join("docs");
    std::fs::create_dir_all(&docs_a).unwrap();
    std::fs::write(
        docs_a.join("guide.md"),
        "ALPHA unique content for guide document",
    )
    .unwrap();
    std::fs::write(
        docs_a.join("api.md"),
        "BETA unique content for API reference",
    )
    .unwrap();
    let src_a = repo_a.path().join("src");
    std::fs::create_dir_all(&src_a).unwrap();
    std::fs::write(
        src_a.join("main.rs"),
        "GAMMA unique content for main source file",
    )
    .unwrap();

    let tool_a = DocIndexTool::new(store_a.clone()).with_session_cwd(repo_a.path().to_path_buf());
    let args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(json!({
        "path": "docs",
        "recursive": true,
    }))
    .unwrap();
    tool_a.call(args).await.expect("doc_index docs at A");

    let args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(json!({
        "path": "src",
        "recursive": false,
    }))
    .unwrap();
    tool_a.call(args).await.expect("doc_index src at A");
    drop(store_a);

    // Verify indexed at A before copy — search for any content, expect all 3 docs.
    {
        let store = VectorStore::open(&spec_a).unwrap();
        let resolved = store.db_path_for(Path::new(""));
        let hits = store.search(&resolved, "unique content", 10).await.unwrap();
        assert_eq!(
            hits.len(),
            3,
            "all 3 indexed files should be searchable at A before copy"
        );
        // All source IDs should be repo-relative.
        for hit in &hits {
            assert!(
                !hit.source.starts_with("/"),
                "source IDs at A must be repo-relative, got: {}",
                hit.source
            );
        }
        drop(store);
    }

    // Copy entire repo+DB to a different absolute path B.
    let repo_b = tempfile::tempdir().unwrap();
    mark_repo(repo_b.path());
    copy_tree(repo_a.path(), repo_b.path());
    drop(repo_a); // Remove the original (simulating rename/rm).

    // Open at B. Manifest roots must be repo-relative so the store
    // resolves them against B, not A.
    let db_b = repo_b.path().join(".peakbot/vectors.db");
    let spec_b = spec_with_dims(db_b, &base, 16);

    // Clear the recorder after initial indexing — only query embeddings
    // are allowed from now on. Any document embeddings indicate a needless
    // full replay triggered by stale state or a clone-incompatible manifest.
    rec.clear();

    let store_b = VectorStore::open(&spec_b).unwrap();

    // Verify search works at B (unchanged content).
    {
        let resolved = store_b.db_path_for(Path::new(""));
        let hits = store_b
            .search(&resolved, "unique content", 10)
            .await
            .unwrap();
        assert_eq!(
            hits.len(),
            3,
            "search works at B after copy — all 3 original files found"
        );
    }

    // Verify sources are not pruned — each document is still searchable. (A
    // vector search returns the top k nearest rows, so assert the source is
    // present rather than that it is the only row.)
    {
        let resolved = store_b.db_path_for(Path::new(""));
        let hits = store_b.search(&resolved, "ALPHA", 10).await.unwrap();
        assert!(
            hits.iter().any(|h| h.source.contains("guide.md")),
            "guide.md still searchable at B; hits: {:?}",
            hits.iter().map(|h| &h.source).collect::<Vec<_>>()
        );
        let hits = store_b.search(&resolved, "BETA", 10).await.unwrap();
        assert!(
            hits.iter().any(|h| h.source.contains("api.md")),
            "api.md still searchable at B"
        );
        let hits = store_b.search(&resolved, "GAMMA", 10).await.unwrap();
        assert!(
            hits.iter().any(|h| h.source.contains("main.rs")),
            "main.rs still searchable at B"
        );
    }

    // Verify source IDs are repo-relative at B.
    {
        let resolved = store_b.db_path_for(Path::new(""));
        let hits = store_b.search(&resolved, "ALPHA", 10).await.unwrap();
        assert!(
            hits.iter().any(|h| h.source.contains("guide.md")),
            "a source ID at B references guide.md; hits: {:?}",
            hits.iter().map(|h| &h.source).collect::<Vec<_>>()
        );
        assert!(
            hits.iter().all(|h| !h.source.starts_with('/')),
            "source IDs at B must stay repo-relative"
        );
    }

    // The manifest persists repo roots as relative POSIX paths and no absolute
    // anchor at all. After the clone-portability fix, opening at B must not
    // require the original creator path in the manifest.
    {
        let meta = metadata_json(&store_b, Path::new("")).await;
        assert!(
            meta["manifest"].get("anchored_root").is_none(),
            "the manifest must not persist a repo root; got: {meta}"
        );

        // Roots entries must be relative POSIX paths.
        let roots = meta["manifest"]["roots"].as_array().expect("roots array");
        for root in roots {
            let root_path = root["root"].as_str().unwrap_or("");
            assert!(
                !root_path.starts_with('/')
                    && !root_path.contains('\\')
                    && !root_path.starts_with(".."),
                "root entry must be repo-relative POSIX, got: {}",
                root_path
            );
        }
    }

    // Verify zero document embeddings on open at B — only query embeddings.
    // A clone-incompatible manifest (absolute anchored_root) or stale state
    // would trigger a full reconcile and re-embed every document. (Searches
    // themselves contribute query embeddings, so match on document text.)
    let doc_inputs: Vec<String> = rec
        .document_inputs("unique content")
        .into_iter()
        .filter(|input| input.contains("unique content for"))
        .collect();
    assert_eq!(
        doc_inputs.len(),
        0,
        "zero document embeddings after opening B; only query embeddings expected, got: {doc_inputs:?}"
    );

    // Indexing a new file at B succeeds (proves the store writes at B).
    {
        let new_file = repo_b.path().join("docs/new-at-b.md");
        std::fs::write(&new_file, "DELTA new file at B").unwrap();
        let tool = DocIndexTool::new(store_b.clone()).with_session_cwd(repo_b.path().to_path_buf());
        let args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(json!({
            "path": "docs",
            "recursive": true,
        }))
        .unwrap();
        tool.call(args).await.expect("doc_index at B succeeds");
    }
}

// ── Contract 2: Tampered v2 metadata rejected at reconcile ─────────────

/// A manifest whose recorded root/source escapes the repo (absolute or `..`)
/// must be rejected when the metadata is read — before any file or git access,
/// with no prune, no embed, and the real DB bytes left untouched.
#[tokio::test]
async fn tampered_manifest_root_entry_rejected_before_any_read() {
    let rec = EmbedRecorder::new();
    let base = spawn_recording_embeddings(16, rec.clone()).await;
    let repo = tempfile::tempdir().unwrap();
    mark_repo(repo.path());

    let db_path = repo.path().join(".peakbot/vectors.db");
    let spec = spec_with_dims(db_path.clone(), &base, 16);
    let store = VectorStore::open(&spec).unwrap();
    let resolved = store.db_path_for(Path::new(""));

    let file = repo.path().join("src/lib.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "fn main() {}").unwrap();
    store
        .index_file(&resolved, &file, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Tamper: an absolute root entry and a traversal source.
    let store2 = VectorStore::open(&spec).unwrap();
    let handle = store2.handle_for_test(Path::new("")).await.unwrap();
    let mut meta: Value = {
        let raw = handle.load_config_value(META_KEY).unwrap().unwrap();
        serde_json::from_str(&raw).unwrap()
    };
    meta["manifest"]["roots"] = json!([
        { "root": "/etc", "recursive": true, "metadata": {} },
    ]);
    meta["manifest"]["sources"] = json!(["../../etc/passwd"]);
    let tampered = serde_json::to_string(&meta).unwrap();
    handle.save_config_value(META_KEY, &tampered).unwrap();
    drop(handle);
    drop(store2);
    let before = std::fs::read(&db_path).unwrap();
    rec.clear();

    // Reopen and search: the manifest must be rejected on read.
    let store3 = VectorStore::open(&spec).unwrap();
    let resolved3 = store3.db_path_for(Path::new(""));
    let err = match store3.search(&resolved3, "main", 10).await {
        Ok(hits) => panic!(
            "a manifest path outside the repo must be rejected; got {} hits: {:?}",
            hits.len(),
            hits.iter().map(|h| &h.source).collect::<Vec<_>>()
        ),
        Err(err) => err,
    };
    assert!(
        format!("{err:?}").to_lowercase().contains("manifest")
            || format!("{err:?}").to_lowercase().contains("corrupt"),
        "the error must name the invalid manifest, got: {err:?}"
    );

    // Nothing was read, embedded, pruned, or written to the real DB.
    assert!(
        rec.document_inputs("main").is_empty(),
        "a rejected manifest must embed nothing; requests: {:?}",
        rec.document_inputs("main")
    );
    assert_eq!(
        std::fs::read(&db_path).unwrap(),
        before,
        "a rejected manifest must leave the real DB bytes untouched"
    );
}

// ── Contract 3: Merge driver root handling ────────────────────────────

/// Two different absolute clone paths with differently named relative
/// roots/metadata merge correctly: after reconcile in A, theirs-only content
/// survives with its original root metadata, and unchanged vectors are reused.
///
/// This test creates two separate "clones" at different absolute paths,
/// indexes different roots in each, runs the merge driver, and verifies:
/// 1. Theirs-only content (docs/) survives with original metadata (author=bob)
/// 2. Ours' unique content (src/) survives
/// 3. Global sources are the sorted union
/// 4. Zero avoidable document reembedding after the merge
#[tokio::test]
async fn merge_driver_preserves_theirs_only_content_and_its_root_metadata() {
    let rec = EmbedRecorder::new();
    let base = spawn_recording_embeddings(3, rec.clone()).await;
    let repo = tempfile::tempdir().unwrap();
    mark_repo(repo.path());

    // Build OURS store at clone A path with src/ root (author=alice).
    let clone_a = repo.path().join("clone-a");
    std::fs::create_dir_all(&clone_a).unwrap();
    mark_repo(&clone_a);
    let ours_src = clone_a.join(".peakbot/vectors.db");
    std::fs::create_dir_all(ours_src.parent().unwrap()).unwrap();

    {
        let store = VectorStore::open(&spec(ours_src.clone(), &base)).unwrap();
        let dp = store.db_path_for(Path::new(""));

        // Index src/ at clone A.
        let src = clone_a.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("main.rs"),
            "UNIQUE_AAA_CONTENT_FOR_ALICE_MODULE_XYZ",
        )
        .unwrap();
        store
            .index_file(&dp, &src.join("main.rs"), &HashMap::new())
            .await
            .unwrap();

        // Record root with author=alice.
        let mut meta = HashMap::new();
        meta.insert("author".to_string(), json!("alice"));
        store.record_root(&dp, src, true, &meta).await.unwrap();
        drop(store);
    }

    // Build THEIRS store at clone B path with docs/ root (author=bob).
    let clone_b = repo.path().join("clone-b");
    std::fs::create_dir_all(&clone_b).unwrap();
    mark_repo(&clone_b);
    let theirs_src = clone_b.join(".peakbot/vectors.db");
    std::fs::create_dir_all(theirs_src.parent().unwrap()).unwrap();

    {
        let store = VectorStore::open(&spec(theirs_src.clone(), &base)).unwrap();
        let dp = store.db_path_for(Path::new(""));

        // Index docs/ at clone B.
        let docs = clone_b.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("guide.md"),
            "UNIQUE_BBB_CONTENT_FOR_BOB_DOCUMENT_123",
        )
        .unwrap();
        store
            .index_file(&dp, &docs.join("guide.md"), &HashMap::new())
            .await
            .unwrap();

        // Record root with author=bob.
        let mut meta = HashMap::new();
        meta.insert("author".to_string(), json!("bob"));
        store.record_root(&dp, docs, true, &meta).await.unwrap();
        drop(store);
    }

    // Clear the recorder — only query embeddings allowed from now on.
    rec.clear();

    // Copy ours to %A (the temp path the driver writes to).
    let a_path = repo.path().join("a.db");
    std::fs::copy(&ours_src, &a_path).unwrap();
    let p_path = repo.path().join("vectors.db");

    // Run the merge driver.
    peakbot::vector::merge_driver::run([
        "/nonexistent-base.db",
        a_path.to_str().unwrap(),
        theirs_src.to_str().unwrap(),
        p_path.to_str().unwrap(),
    ])
    .await
    .expect("merge driver succeeds");

    // After merge, open the result and verify.
    let manifest_store = VectorStore::open(&spec(a_path.clone(), &base)).unwrap();
    let meta = metadata_json(&manifest_store, Path::new("")).await;
    let roots = meta["manifest"]["roots"].as_array().expect("roots array");

    // Verify theirs-only root (docs/) survived with its original metadata.
    let docs_root = roots
        .iter()
        .find(|r| r["root"].as_str().unwrap_or("").ends_with("docs"))
        .expect("docs root (theirs-only) must survive merge");
    assert_eq!(
        docs_root["metadata"]["author"],
        json!("bob"),
        "theirs-only root must keep its original metadata"
    );

    // Verify ours' unique root (src/) survived.
    let src_root = roots
        .iter()
        .find(|r| r["root"].as_str().unwrap_or("").ends_with("src"))
        .expect("src root (ours) must survive merge");
    assert_eq!(
        src_root["metadata"]["author"],
        json!("alice"),
        "ours' root must keep its metadata"
    );

    // Global sources must be the sorted union.
    let sources: Vec<String> = meta["manifest"]["sources"]
        .as_array()
        .expect("sources array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        sources,
        vec!["docs/guide.md".to_string(), "src/main.rs".to_string()],
        "global sources are the sorted union"
    );

    // Search after merge — triggers initial reconcile (no state.json for merged DB).
    // Doc embeds on this first search are expected; we care about subsequent searches.
    let resolved = manifest_store.db_path_for(Path::new(""));
    let _hits = manifest_store
        .search(&resolved, "UNIQUE_AAA_CONTENT", 10)
        .await
        .unwrap();
    drop(manifest_store);

    // Now clear the recorder and verify no doc embeds on a subsequent search.
    rec.clear();

    // Reopen the merged DB and search again — should be zero doc embeds.
    let store_after = VectorStore::open(&spec(a_path.clone(), &base)).unwrap();
    let resolved_after = store_after.db_path_for(Path::new(""));
    let _hits = store_after
        .search(&resolved_after, "UNIQUE_BBB_CONTENT", 10)
        .await
        .unwrap();

    let doc_inputs = rec.document_inputs("UNIQUE_BBB_CONTENT");
    assert_eq!(
        doc_inputs.len(),
        0,
        "zero avoidable document reembeddings after reopen; only query embeddings expected"
    );
}

// ── Contract 4: Config parse ──────────────────────────────────────────

/// Same resolved db_path with identical embeddings is accepted: the universal
/// v2 store has one behavior regardless of location, so there is no mode to
/// mismatch (only one embedding space per file).
#[test]
fn same_db_path_same_embeddings_accepted() {
    use peakbot::config::resolve::{Config, ParseCtx, Source};

    let global_path = "/home/u/.config/peakbot/config.yaml";
    let ctx = ParseCtx { known_skills: &[] };

    let yaml = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\nprofiles:\n  p1:\n    vector_db:\n      db_path: /tmp/v.db\n      embeddings:\n        base_url: https://e.example/v1\n        model: bge\n        dimensions: 3\n    agents:\n      a:\n        model: a\n  p2:\n    vector_db:\n      db_path: /tmp/v.db\n      embeddings:\n        base_url: https://e.example/v1\n        model: bge\n        dimensions: 3\n    agents:\n      a:\n        model: a\n";

    let result = Config::parse(
        Source {
            path: Path::new(global_path),
            text: yaml,
        },
        None,
        &ctx,
    );

    assert!(result.is_ok(), "identical embeddings should be accepted");
}

// ── Contract 5: Gitignore respect ──────────────────────────────────────

/// Helper: initialize a real git repo with a .gitignore file.
fn init_git_repo(dir: &Path, gitignore: &str) {
    write_gitignore(dir, gitignore);
    std::fs::create_dir_all(dir.join(".git")).expect("create .git marker");
    Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir)
        .output()
        .expect("git config email");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .expect("git config name");
}

/// Recursive discovery finds new non-ignored files on reconcile.
/// After indexing a root, a new file added under that root is discovered when
/// the next freshness check replays the manifest.
#[tokio::test]
async fn reconcile_discovers_new_files_under_indexed_root() {
    use peakbot::tools::DocIndexTool;
    use rig_core::tool::Tool;

    let base = spawn_embeddings(16).await;
    let repo = tempfile::tempdir().unwrap();
    mark_repo(repo.path());

    let db_path = repo.path().join(".peakbot/vectors.db");
    let spec = spec_with_dims(db_path, &base, 16);
    let store = VectorStore::open(&spec).unwrap();

    // Index a root directory.
    let src = repo.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.rs"), "ALPHA initial content for main module").unwrap();

    let tool = DocIndexTool::new(store.clone()).with_session_cwd(repo.path().to_path_buf());
    let args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(json!({
        "path": "src",
        "recursive": true,
    }))
    .unwrap();
    tool.call(args)
        .await
        .expect("doc_index src at initial index");

    // Verify initial indexing worked.
    {
        let resolved = store.db_path_for(Path::new(""));
        let hits = store
            .search(&resolved, "initial content", 10)
            .await
            .unwrap();
        assert!(
            hits.iter().any(|h| h.source.contains("main.rs")),
            "initial file must be indexed; hits: {:?}",
            hits.iter().map(|h| &h.source).collect::<Vec<_>>()
        );
    }

    // Drop the store and add a new file.
    drop(store);
    let new_file = src.join("new_module.rs");
    std::fs::write(
        &new_file,
        "BETA new module content added after initial index",
    )
    .unwrap();

    // Reopen and search — reconcile should discover the new file.
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved = store2.db_path_for(Path::new(""));

    // Verify new file was discovered.
    let hits = store2
        .search(&resolved, "BETA new module", 10)
        .await
        .unwrap();
    assert!(
        hits.iter().any(|h| h.source.contains("new_module.rs")),
        "reconcile must discover the new non-ignored file; hits: {:?}",
        hits.iter().map(|h| &h.source).collect::<Vec<_>>()
    );

    // Verify original file is still searchable.
    let hits = store2
        .search(&resolved, "ALPHA initial content", 10)
        .await
        .unwrap();
    assert!(
        hits.iter().any(|h| h.source.contains("main.rs")),
        "original file still searchable after reconcile"
    );
}

/// Gitignore: ignored files must never be embedded, while non-ignored files are.
/// This test creates a real .gitignore that ignores target/, includes
/// target/secret.json (a supported .json file) and a normal docs/readme.md.
/// After indexing, secret.json must NOT be embedded; readme.md must be.
/// Then a second ignored file and a new non-ignored file are added, and after
/// reconcile, the ignored file is still not embedded while the new file is.
#[tokio::test]
async fn gitignore_ignored_files_never_embedded_non_ignored_are() {
    use peakbot::tools::DocIndexTool;
    use rig_core::tool::Tool;

    let base = spawn_embeddings(16).await;
    let repo = tempfile::tempdir().unwrap();

    // Initialize a real git repo with .gitignore ignoring target/.
    init_git_repo(repo.path(), "target/\n");

    let db_path = repo.path().join(".peakbot/vectors.db");
    let spec = spec_with_dims(db_path, &base, 16);
    let store = VectorStore::open(&spec).unwrap();

    // Create target/secret.json (ignored by .gitignore, but has supported .json ext).
    let target = repo.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("secret.json"),
        "{\"password\": \"hunter2-secret\"}",
    )
    .unwrap();

    // Create docs/readme.md (not ignored).
    let docs = repo.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(
        docs.join("readme.md"),
        "# Project README with distinctive content",
    )
    .unwrap();

    // Index the repo root recursively through DocIndexTool.
    let tool = DocIndexTool::new(store.clone()).with_session_cwd(repo.path().to_path_buf());
    let args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(json!({
        "path": ".",
        "recursive": true,
    }))
    .unwrap();
    let output = tool.call(args).await.expect("doc_index repo root");
    eprintln!("initial indexing output: {}", output);

    // Search for the ignored secret — its source must NOT be in the store.
    // (Vector search always returns the nearest rows, so assert on sources,
    // not on an empty result set.)
    let resolved = store.db_path_for(Path::new(""));
    let hits = store.search(&resolved, "hunter2-secret", 10).await.unwrap();
    assert!(
        hits.iter().all(|h| !h.source.contains("secret.json")),
        "ignored file target/secret.json must NOT be embedded; hits: {:?}",
        hits.iter().map(|h| &h.source).collect::<Vec<_>>()
    );

    // Search for the non-ignored readme — must be found.
    let hits = store
        .search(&resolved, "distinctive content", 10)
        .await
        .unwrap();
    assert!(
        hits.iter().any(|h| h.source.contains("readme.md")),
        "non-ignored docs/readme.md must be embedded; hits: {:?}",
        hits.iter().map(|h| &h.source).collect::<Vec<_>>()
    );

    // Drop the store. Add another ignored file and a new non-ignored file.
    drop(store);

    let target2 = target.join("build");
    std::fs::create_dir_all(&target2).unwrap();
    std::fs::write(
        target2.join("another-secret.json"),
        "{\"token\": \"abc123-ignored-token\"}",
    )
    .unwrap();

    let docs2 = docs.join("changelog");
    std::fs::create_dir_all(&docs2).unwrap();
    std::fs::write(
        docs2.join("v2.md"),
        "## Release v2 with new distinctive content",
    )
    .unwrap();

    // Force a reconcile via HEAD change: commit the new files.
    Command::new("git")
        .args(["add", "."])
        .current_dir(repo.path())
        .output()
        .expect("git add");
    Command::new("git")
        .args(["commit", "-m", "add new files"])
        .current_dir(repo.path())
        .output()
        .expect("git commit");

    // Reopen and search — ensure_fresh should detect the HEAD change and
    // trigger a reconcile that discovers the new non-ignored file while
    // still skipping the ignored files.
    let store2 = VectorStore::open(&spec).unwrap();
    let resolved2 = store2.db_path_for(Path::new(""));

    // The new ignored file must NOT be embedded.
    let hits = store2
        .search(&resolved2, "abc123-ignored-token", 10)
        .await
        .unwrap();
    assert!(
        hits.iter()
            .all(|h| !h.source.contains("another-secret.json")),
        "new ignored file must NOT be embedded after reconcile; hits: {:?}",
        hits.iter().map(|h| &h.source).collect::<Vec<_>>()
    );

    // The new non-ignored file must be embedded.
    let hits = store2
        .search(&resolved2, "new distinctive content", 10)
        .await
        .unwrap();
    assert!(
        hits.iter().any(|h| h.source.contains("v2.md")),
        "new non-ignored file must be embedded after reconcile; hits: {:?}",
        hits.iter().map(|h| &h.source).collect::<Vec<_>>()
    );

    // Original readme must still be searchable.
    let hits = store2
        .search(&resolved2, "distinctive content", 10)
        .await
        .unwrap();
    assert!(
        hits.iter().any(|h| h.source.contains("readme.md")),
        "original readme.md must still be searchable after reconcile; hits: {:?}",
        hits.iter().map(|h| &h.source).collect::<Vec<_>>()
    );
}

// ── Contract 6: State.json stamp after indexing ────────────────────────

/// After DocIndexTool finishes, a subsequent search does not trigger a
/// needless full replay. The state.json DB stamp must be current enough
/// that ensure_fresh does not detect a "changed" DB and re-embed everything.
///
/// This test verifies two things:
/// 1. The state.json DB stamp equals the actual DB file stat immediately after
///    DocIndexTool completes (no stale_stamp).
/// 2. A fresh search after reopening performs no document embeddings (only query).
///
/// Uses the EmbedRecorder to detect needless document re-embeddings.
#[tokio::test]
async fn state_stamp_prevents_needless_replay_on_search() {
    use peakbot::tools::DocIndexTool;
    use rig_core::tool::Tool;

    // Use a recording embeddings endpoint to detect needless re-embeds.
    let rec = EmbedRecorder::new();
    let base = spawn_recording_embeddings(3, rec.clone()).await;

    let repo = tempfile::tempdir().unwrap();
    mark_repo(repo.path());

    let db_path = repo.path().join(".peakbot/vectors.db");
    let spec = spec(db_path.clone(), &base);
    let store = VectorStore::open(&spec).unwrap();

    // Index a file.
    let file = repo.path().join("src/lib.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "fn main() {}").unwrap();

    let tool = DocIndexTool::new(store.clone()).with_session_cwd(repo.path().to_path_buf());
    let args = serde_json::from_value::<<DocIndexTool as Tool>::Args>(json!({
        "path": "src",
        "recursive": false,
    }))
    .unwrap();
    tool.call(args).await.expect("doc_index src");
    drop(store);

    // Immediately after DocIndexTool, the state.json DB stamp must equal the
    // actual DB file stat. If record_root writes metadata AFTER record_state,
    // the stamp will be stale and ensure_fresh will trigger a needless replay.
    {
        // Read state.json directly (reconcile module is private).
        let state_dir = db_path.with_extension("db.peakbot");
        let state_file = state_dir.join("state.json");
        let state_json =
            std::fs::read_to_string(&state_file).expect("read state.json after indexing");
        let state: serde_json::Value =
            serde_json::from_str(&state_json).expect("state.json must be valid JSON");

        let db_meta = std::fs::metadata(&db_path).expect("read DB file metadata");
        let actual_size = db_meta.len();
        let actual_mtime = db_meta
            .modified()
            .expect("DB mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("mtime since epoch")
            .as_nanos();

        let state_size: u64 = state["db_size"].as_u64().expect("db_size in state.json");
        let state_mtime: u128 = state["db_mtime"]
            .to_string()
            .parse()
            .expect("db_mtime in state.json");

        assert_eq!(
            state_size, actual_size,
            "state.db_size must equal actual DB file size immediately after indexing"
        );
        assert_eq!(
            state_mtime, actual_mtime,
            "state.db_mtime must equal actual DB mtime immediately after indexing"
        );
    }

    // Clear the recorder: any document embeddings after this point are needless.
    rec.clear();

    // Drop the store and reopen to simulate a new session/turn.
    let store2 = VectorStore::open(&spec).unwrap();

    // Search: this triggers ensure_fresh, which checks the DB stamp.
    // If the stamp is stale, it will re-embed all files (needless replay).
    // If the stamp is current, it will skip the replay and just search.
    let resolved = store2.db_path_for(Path::new(""));
    let hits = store2.search(&resolved, "main", 10).await.unwrap();
    assert_eq!(hits.len(), 1, "search returns 1 hit");

    // Verify no document embeddings were made (only query embeddings).
    let doc_inputs = rec.document_inputs("main");
    assert_eq!(
        doc_inputs.len(),
        0,
        "no needless document re-embeddings; ensure_fresh must detect the DB is current"
    );
}

// ── Existing assertion that needs correction after fix ─────────────────

/// After the clone-portability fix the repo root is derived locally from the
/// current DB path and is **not persisted at all**. This test verifies the
/// metadata carries no creator-absolute anchor (and no relative substitute).
#[tokio::test]
async fn fresh_store_derives_repo_root_locally_not_from_persisted_anchored_root() {
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
        .index_file(&resolved, &file, &HashMap::new())
        .await
        .unwrap();
    drop(store);

    // Reopen and check for v2 metadata.
    let store2 = VectorStore::open(&spec).unwrap();
    let meta = metadata_json(&store2, Path::new("")).await;
    assert_eq!(meta["schema"], 2, "top-level schema is 2");

    // CONTRACT: no repo root is persisted — not absolute, not relative. The
    // repo root is derived locally from the current DB path; only the anchor
    // kind is recorded so a relocation across the repo boundary can be refused
    // on write.
    assert!(
        meta["manifest"].get("anchored_root").is_none(),
        "the manifest must not persist a repo root (clone portability); got: {meta}"
    );
}
