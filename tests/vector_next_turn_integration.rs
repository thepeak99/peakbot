//! TDD tests for the **next-turn integration** slice.
//!
//! ## Contract
//!
//! After a git merge leaves a pending marker (`<P>.peakbot/pending`), the very
//! next user turn reconciles the v2 store *without* any explicit doc tool
//! call: the first `search`/`index` on the store triggers `ensure_fresh`, which
//! sees the marker and replays the manifest.
//!
//! * unchanged sources are reused (hash match, zero re-embedding);
//! * on failure the turn continues, `pending` stays set, and a later
//!   `doc_search` errors rather than serving stale rows;
//! * a local-only store does no git/state work at all.
//!
//! These drive the store exactly as the turn would — the store API is the
//! reconciliation seam (there is no session-level "start of turn" hook exposed
//! to tests yet; that wiring is flagged as a GAP in the report). The
//! end-to-end driver+merge property lives in `vector_merge_end_to_end.rs`.

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

// ── Embedding recorder (exact-request accounting) ──────────────────────

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

    fn clear(&self) {
        self.requests.lock().unwrap().clear();
    }

    fn all(&self) -> Vec<Vec<String>> {
        self.requests.lock().unwrap().clone()
    }

    fn document_requests(&self, query: &str) -> Vec<Vec<String>> {
        self.all()
            .into_iter()
            .filter(|inputs| !(inputs.len() == 1 && inputs[0] == query))
            .collect()
    }

    fn query_requests(&self, query: &str) -> usize {
        self.all()
            .iter()
            .filter(|inputs| inputs.len() == 1 && inputs[0] == query)
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
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let inputs: Vec<String> = body["input"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    recorder.requests.lock().unwrap().push(inputs.clone());
    let data: Vec<serde_json::Value> = inputs
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

// ── Stores / git / state helpers ───────────────────────────────────────

fn spec_at(db: PathBuf, url: &str) -> VectorSpec {
    VectorSpec {
        db_path: DbPathSpec::Fixed(db),
        embeddings: EmbeddingsConfig {
            base_url: url.to_string(),
            api_key: None,
            model: "test-model".into(),
            dimensions: 3,
        },
    }
}

fn init_git_repo(dir: &Path) {
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "test@example.com"][..],
        &["config", "user.name", "Test User"][..],
    ] {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
    }
}

fn git_commit_all(dir: &Path, message: &str) {
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .unwrap();
    let out = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn state_dir_for(db: &Path) -> PathBuf {
    let ext = db
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "db".to_string());
    db.with_extension(format!("{ext}.peakbot"))
}

fn has_pending(db: &Path) -> bool {
    state_dir_for(db).join("pending").exists()
}

fn set_pending(db: &Path) {
    let dir = state_dir_for(db);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pending"), "pending").unwrap();
}

// ══════════════════════════════════════════════════════════════════════════
// 1. Next turn: a pending marker auto-reconciles on the first store touch
// ══════════════════════════════════════════════════════════════════════════

/// The turn after a merge: the driver has set `pending`. The first search
/// reconciles without a doc tool call, clears the marker, and re-embeds nothing
/// beyond the query itself (the source is unchanged).
#[tokio::test]
async fn next_turn_search_auto_reconciles_on_pending_marker() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("v.db");
    let (url, rec) = spawn_recording_embeddings().await;
    let spec = spec_at(db.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "initial content").unwrap();
    git_commit_all(dir.path(), "initial");

    let store = VectorStore::open(&spec).unwrap();
    let dp = store.db_path_for(Path::new(""));
    store.index_file(&dp, &doc, &HashMap::new()).await.unwrap();
    drop(store);

    // The merge driver left a pending marker.
    set_pending(&db);

    let query = "zz-next-turn-sentinel";
    rec.clear();
    let store2 = VectorStore::open(&spec).unwrap();
    let dp2 = store2.db_path_for(Path::new(""));
    let hits = store2.search(&dp2, query, 5).await.unwrap();

    assert!(
        hits.iter().any(|h| h.text.contains("initial content")),
        "next-turn search must serve the reconciled doc"
    );
    assert!(
        !has_pending(&db),
        "a successful auto-reconcile clears the pending marker"
    );
    assert!(
        rec.document_requests(query).is_empty(),
        "an unchanged source must not be re-embedded; requests: {:?}",
        rec.all()
    );
    assert_eq!(
        rec.query_requests(query),
        1,
        "the query itself is embedded exactly once"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 2. Reconcile failure: turn continues, pending stays, search errors
// ══════════════════════════════════════════════════════════════════════════

/// If embeddings are unreachable during a pending reconcile, the search errors
/// (never serving stale rows) and the pending marker remains for the next try.
#[tokio::test]
async fn next_turn_reconcile_failure_keeps_pending_and_search_errors() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("v.db");
    let (url, _rec) = spawn_recording_embeddings().await;

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "content").unwrap();
    git_commit_all(dir.path(), "initial");

    let store = VectorStore::open(&spec_at(db.clone(), &url)).unwrap();
    let dp = store.db_path_for(Path::new(""));
    store.index_file(&dp, &doc, &HashMap::new()).await.unwrap();
    drop(store);

    // Trigger a reconcile with a changed file, but point embeddings at a dead
    // endpoint so the reconcile must fail.
    std::fs::write(&doc, "changed content").unwrap();
    git_commit_all(dir.path(), "change");
    set_pending(&db);

    let dead = VectorSpec {
        db_path: DbPathSpec::Fixed(db.clone()),
        embeddings: EmbeddingsConfig {
            base_url: "http://127.0.0.1:59999".to_string(),
            api_key: None,
            model: "test-model".into(),
            dimensions: 3,
        },
    };
    let store2 = VectorStore::open(&dead).unwrap();
    let dp2 = store2.db_path_for(Path::new(""));
    let result = store2.search(&dp2, "content", 5).await;

    assert!(
        result.is_err(),
        "a failed reconcile must surface an error to the turn, not serve stale rows"
    );
    assert!(
        has_pending(&db),
        "the pending marker must remain set for the next retry"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 3. Local-only stores do no git/state work on any turn
// ══════════════════════════════════════════════════════════════════════════

/// A local (outside-git) store creates no state directory and needs no git;
/// every turn is a plain store access.
#[tokio::test]
async fn next_turn_local_store_performs_no_git_state_work() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("local.db");
    let (url, _rec) = spawn_recording_embeddings().await;
    let spec = spec_at(db.clone(), &url);

    // No git repo needed.
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "content").unwrap();

    let store = VectorStore::open(&spec).unwrap();
    let dp = store.db_path_for(Path::new(""));
    store.index_file(&dp, &doc, &HashMap::new()).await.unwrap();
    drop(store);

    assert!(
        !state_dir_for(&db).exists(),
        "local stores must not create a reconcile state directory"
    );

    let store2 = VectorStore::open(&spec).unwrap();
    let dp2 = store2.db_path_for(Path::new(""));
    let hits = store2.search(&dp2, "content", 5).await.unwrap();
    assert!(!hits.is_empty(), "local store search works on any turn");
    assert!(
        !state_dir_for(&db).exists(),
        "local store still performs no state work on the next turn"
    );
}
