//! Real-Git integration: build two branches with divergent v2 v2 DBs,
//! invoke the actual merge driver via `.gitattributes`, and verify the
//! complete driver contract end-to-end.
//!
//! The test creates a real temp git repo, branches with v2 v2 DB files,
//! commits a `.gitattributes` line selecting `peakbot-vectors`, configures
//! `merge.peakbot-vectors.driver` to invoke the actual built binary via
//! `CARGO_BIN_EXE_peakbot` with `%O %A %B %P`, then runs a real `git merge`
//! as a separate process with an isolated environment.
//!
//! No in-process handle is ever opened on the live DB path before git runs;
//! scratch DBs are byte-copied to the live path so redb locking cannot make
//! the test flaky.
//!
//! Requires the `peakbot` binary (built by Cargo for integration tests).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use peakbot::config::EmbeddingsConfig;
use peakbot::config::resolve::{DbPathSpec, VectorSpec};
use peakbot::vector::VectorStore;

fn peakbot_bin() -> &'static str {
    match option_env!("CARGO_BIN_EXE_peakbot") {
        Some(bin) => bin,
        None => panic!("peakbot binary not built for tests"),
    }
}

fn spec_at(db: PathBuf, url: &str) -> VectorSpec {
    VectorSpec {
        db_path: DbPathSpec::Fixed(db),
        embeddings: EmbeddingsConfig {
            base_url: url.to_string(),
            api_key: None,
            model: "test-model".to_string(),
            dimensions: 3,
        },
    }
}

fn git(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    cmd
}

fn run_git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = git(dir, args).output().expect("git failed to execute");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[derive(Clone)]
struct EmbedRecorder {
    requests: Arc<Mutex<Vec<String>>>,
}

impl EmbedRecorder {
    fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn clear(&self) {
        self.requests.lock().unwrap().clear();
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

async fn spawn_recording_embeddings(rec: EmbedRecorder) -> String {
    let app = Router::new()
        .route("/embeddings", post(record_handler))
        .with_state(rec.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock embeddings");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://{addr}")
}

async fn record_handler(
    State(rec): State<EmbedRecorder>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    if let Some(arr) = body["input"].as_array() {
        for item in arr {
            if let Some(text) = item.as_str() {
                rec.requests.lock().unwrap().push(text.to_string());
            }
        }
    }
    Json(serde_json::json!({ "data": [{ "embedding": [0.0, 0.0, 0.0] }] }))
}

/// Build a v2 DB from the current repo working tree, copying it to the
/// live path, committing, and returning the DB path. Scratch DB is a temp
/// sibling so no handle is ever opened on the live path.
static SCRATCH_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

async fn build_and_commit(
    repo: &Path,
    live_db: &Path,
    url: &str,
    index_files: &[&str],
    root_metadata: &HashMap<String, serde_json::Value>,
    commit_msg: &str,
) {
    let seq = SCRATCH_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let scratch = repo.join(format!(".scratch-{}.db", seq));
    let store = VectorStore::open(&spec_at(scratch.clone(), url)).unwrap();
    let dp = store.db_path_for(Path::new(""));

    for rel in index_files {
        let p = repo.join(rel);
        store
            .index_file(&dp, &p, &HashMap::new())
            .await
            .expect("index file");
    }

    let docs_dir = repo.join("docs");
    store
        .record_root(&dp, docs_dir.clone(), true, root_metadata)
        .await
        .expect("record root");

    // Explicitly drop the store so the scratch file is not held open when we
    // copy it. The DB handle is still in the process-wide registry keyed by
    // the scratch path, but that is a different inode from the live path —
    // no redb lock on the live path is created.
    drop(store);

    // Verify the scratch file was actually created.
    assert!(
        scratch.exists(),
        "scratch DB file should exist at {} after indexing",
        scratch.display()
    );

    // Create the destination parent directory if needed.
    if let Some(parent) = live_db.parent() {
        std::fs::create_dir_all(parent).expect("create live_db parent directory");
    }
    std::fs::copy(&scratch, live_db).expect("copy scratch DB to live path");
    std::fs::remove_file(&scratch).ok();

    run_git(
        repo,
        &[
            "add",
            ".gitattributes",
            &format!("{}", live_db.display()),
            "docs",
        ],
    );
    run_git(repo, &["commit", "-m", commit_msg]);
}

/// A theirs-only source is adopted, ours-only is retained, and an overlapping
/// whole source group resolves with ours winning (including its metadata).
/// The DB conflict resolves cleanly (exit 0) via the real merge driver invoked
/// by git. The driver must not embed or reach the network.
#[cfg(feature = "host")]
#[tokio::test]
async fn real_git_merge_drives_the_vector_merge_driver() {
    let repo = tempfile::tempdir().unwrap();
    let repo_path = repo.path();

    // Initialize real git repo with user config
    run_git(repo_path, &["init", "-q"]);
    run_git(repo_path, &["config", "user.email", "test@example.com"]);
    run_git(repo_path, &["config", "user.name", "Test User"]);

    let docs = repo_path.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("keep.txt"), "keep body").unwrap();
    std::fs::write(docs.join("both.txt"), "both body").unwrap();

    // Configure the custom merge driver for .peakbot/vectors.db
    let driver_cmd = format!("{} vectors merge-driver %O %A %B %P", peakbot_bin());
    run_git(
        repo_path,
        &["config", "merge.peakbot-vectors.driver", &driver_cmd],
    );

    std::fs::write(
        repo_path.join(".gitattributes"),
        ".peakbot/vectors.db merge=peakbot-vectors\n",
    )
    .unwrap();

    let live_db = repo_path.join(".peakbot/vectors.db");
    let rec = EmbedRecorder::new();
    let url = spawn_recording_embeddings(rec.clone()).await;

    // === BASE commit on master ===
    let base_metadata = HashMap::new();
    build_and_commit(
        repo_path,
        &live_db,
        &url,
        &["docs/keep.txt", "docs/both.txt"],
        &base_metadata,
        "base: index keep and both",
    )
    .await;

    // Create `theirs` branch from base
    run_git(repo_path, &["branch", "theirs"]);

    // === OURS commit on master ===
    std::fs::write(docs.join("ours-only.txt"), "ours only body").unwrap();
    // both.txt stays identical on both branches — the DBs differ by
    // recorded root metadata, which is how the merge driver's ours-wins
    // overlap is exercised without a text-file conflict.

    let ours_metadata = HashMap::from([("author".to_string(), serde_json::json!("alice"))]);
    build_and_commit(
        repo_path,
        &live_db,
        &url,
        &["docs/keep.txt", "docs/both.txt", "docs/ours-only.txt"],
        &ours_metadata,
        "ours: update both and add ours-only",
    )
    .await;

    // === THEIRS commit on theirs branch ===
    run_git(repo_path, &["checkout", "theirs"]);
    std::fs::write(docs.join("theirs-only.txt"), "theirs only body").unwrap();
    // both.txt stays identical to ours — the DBs differ by root metadata

    let theirs_metadata = HashMap::from([("author".to_string(), serde_json::json!("bob"))]);
    build_and_commit(
        repo_path,
        &live_db,
        &url,
        &["docs/keep.txt", "docs/both.txt", "docs/theirs-only.txt"],
        &theirs_metadata,
        "theirs: update both and add theirs-only",
    )
    .await;

    // === Merge theirs into master ===
    run_git(repo_path, &["checkout", "master"]);

    // Clear embed requests before merge to prove the driver doesn't embed.
    rec.clear();
    let out = git(repo_path, &["merge", "theirs"])
        .output()
        .expect("git merge");
    assert!(
        out.status.success(),
        "git merge failed with exit code {:?}, stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    // Verify no embedding requests were made during the merge.
    assert_eq!(
        rec.count(),
        0,
        "the merge driver must not embed or reach the network"
    );

    // Merge should exit 0 with no unmerged (conflict) entries.
    let out = run_git(repo_path, &["ls-files", "-u"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "",
        "expected no unmerged (conflict) entries after merge"
    );

    // Verify the merged DB by reading rows directly — no reconcile, no
    // network. Inspect the manifest to confirm sources are the sorted union of
    // both sides, and that the overlapping root resolved with ours' metadata
    // (ours-wins overlap).
    let local_store = VectorStore::open(&spec_at(live_db.clone(), &url)).unwrap();
    let handle = local_store.handle_for_test(Path::new("")).await.unwrap();

    let mut ours_only = false;
    let mut theirs_only = false;
    let mut both_found = false;

    for id in handle.keys().unwrap() {
        if let Some(entry) = handle.get(&id).unwrap()
            && let Some(md) = &entry.metadata
        {
            let source = md
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if source == "docs/ours-only.txt" {
                ours_only = true;
            }
            if source == "docs/theirs-only.txt" {
                theirs_only = true;
            }
            if source == "docs/both.txt" {
                both_found = true;
            }
        }
    }

    assert!(
        ours_only,
        "ours-only source docs/ours-only.txt should be retained after merge"
    );
    assert!(
        theirs_only,
        "theirs-only source docs/theirs-only.txt should be adopted after merge"
    );
    assert!(
        both_found,
        "the overlapping source docs/both.txt should exist after merge"
    );

    // Verify manifest roots: the overlapping docs root should have ours'
    // metadata (author=alice), proving ours-wins for the root.
    let raw = handle
        .load_config_value("peakbot.vector")
        .unwrap()
        .expect("v2 metadata should exist after merge");
    let manifest: serde_json::Value = serde_json::from_str(&raw).unwrap();

    // Verify sources are the sorted union of both sides.
    let mut sources: Vec<String> = manifest["manifest"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    sources.sort();
    assert_eq!(
        sources,
        vec![
            "docs/both.txt".to_string(),
            "docs/keep.txt".to_string(),
            "docs/ours-only.txt".to_string(),
            "docs/theirs-only.txt".to_string(),
        ],
        "manifest sources should be the sorted union of both sides"
    );

    // Find the docs root and verify ours' metadata (author=alice) won.
    let roots = manifest["manifest"]["roots"]
        .as_array()
        .expect("manifest roots array should exist");
    let docs_root = roots
        .iter()
        .find(|r| r["root"].as_str() == Some("docs"))
        .expect("docs root should exist in merged manifest");
    let author = docs_root["metadata"]["author"]
        .as_str()
        .expect("docs root should have author metadata");
    assert_eq!(
        author, "alice",
        "overlapping docs root should have ours' metadata (author=alice), not theirs'"
    );

    // Verify the pending marker was created at the live %P path.
    let pending_path = repo_path.join(".peakbot/vectors.db.peakbot/pending");
    assert!(
        pending_path.exists(),
        "pending marker should exist at {} after merge",
        pending_path.display()
    );

    drop(handle);
    drop(local_store);
}
