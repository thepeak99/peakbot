//! END-TO-END property: after the locked merge driver + the final source
//! merge, a next-turn reconciliation must **reuse** unchanged and theirs-only
//! (adopted) vectors and embed **only** the finally-changed documents.
//!
//! This ties the two slices together:
//!  1. build OUR store (`docs/keep.txt` unchanged, `docs/changed.txt` = v1) and
//!     THEIR store (`docs/keep.txt` same, theirs-only `docs/added.txt` = C);
//!  2. run the real merge driver (`peakbot vectors merge-driver %O %A %B %P`
//!     semantics) to produce the merged DB;
//!  3. model the final source merge (`keep` unchanged, `added` = C adopted,
//!     `changed` -> v2);
//!  4. reconcile the merged store and assert exactly one document is embedded
//!     (the changed v2), while `keep` and `added` are hash-reused.
//!
//! RED until `peakbot::vector::merge_driver` exists. Assumed interface:
//! `pub async fn run(args: [&str; 4]) -> Result<(), MergeDriverError>;` with
//! `%O %A %B %P` order (see `vector_merge_driver.rs`).

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

// ── Embedding recorder ─────────────────────────────────────────────────

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

    fn document_inputs(&self, query: &str) -> Vec<String> {
        self.document_requests(query)
            .into_iter()
            .flatten()
            .collect()
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

// ── Helpers ────────────────────────────────────────────────────────────

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

// ══════════════════════════════════════════════════════════════════════════
// The end-to-end property
// ══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn end_to_end_merge_then_reconcile_embeds_only_the_changed_document() {
    let repo = tempfile::tempdir().unwrap();
    let repo_path = repo.path();
    init_git_repo(repo_path); // real git repo so anchoring/reconcile work

    let docs = repo_path.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("keep.txt"), "keep body unchanged").unwrap();
    std::fs::write(docs.join("changed.txt"), "changed body v1").unwrap();
    git_commit_all(repo_path, "base docs");

    let (url, rec) = spawn_recording_embeddings().await;
    let ours_db = repo_path.join(".ours.db");
    let theirs_db = repo_path.join(".theirs.db");
    let incoming = repo_path.join(".incoming.db"); // %A temp
    let final_db = repo_path.join(".vectors.db"); // %P final path

    // ── OUR store: keep + changed(v1), rooted at docs/ recursively ──
    let ours = VectorStore::open(&spec_at(ours_db.clone(), &url)).unwrap();
    let ours_dp = ours.db_path_for(Path::new(""));
    ours.index_file(&ours_dp, &docs.join("keep.txt"), &HashMap::new())
        .await
        .unwrap();
    ours.index_file(&ours_dp, &docs.join("changed.txt"), &HashMap::new())
        .await
        .unwrap();
    ours.record_root(&ours_dp, docs.clone(), true, &HashMap::new())
        .await
        .unwrap();
    drop(ours);

    // ── THEIR store: keep (same) + theirs-only added(C), rooted at docs/ ──
    std::fs::write(docs.join("added.txt"), "added body theirs-only").unwrap();
    let theirs = VectorStore::open(&spec_at(theirs_db.clone(), &url)).unwrap();
    let theirs_dp = theirs.db_path_for(Path::new(""));
    theirs
        .index_file(&theirs_dp, &docs.join("keep.txt"), &HashMap::new())
        .await
        .unwrap();
    theirs
        .index_file(&theirs_dp, &docs.join("added.txt"), &HashMap::new())
        .await
        .unwrap();
    theirs
        .record_root(&theirs_dp, docs.clone(), true, &HashMap::new())
        .await
        .unwrap();
    drop(theirs);

    // ── Run the real merge driver ──
    // %A is a byte copy of ours (a fresh inode the driver owns); %B is theirs;
    // %P is the working-tree DB path that receives the pending marker.
    std::fs::copy(&ours_db, &incoming).expect("copy ours to %A");
    peakbot::vector::merge_driver::run([
        "/nonexistent-base.db", // %O ignored
        incoming.to_str().unwrap(),
        theirs_db.to_str().unwrap(),
        final_db.to_str().unwrap(),
    ])
    .await
    .expect("merge driver succeeds");
    // git finalizes %A into place. We copy (new inode) to emulate the
    // cross-process finalization and avoid aliasing the driver's temp handle.
    std::fs::copy(&incoming, &final_db).expect("finalize %A -> %P");
    assert!(
        has_pending(&final_db),
        "the driver must have marked <P>.peakbot/pending"
    );

    // The merged manifest must carry all three sources.
    let manifest = {
        let s = VectorStore::open(&spec_at(final_db.clone(), &url)).unwrap();
        let handle = s.handle_for_test(Path::new("")).await.unwrap();
        let raw = handle
            .load_config_value("peakbot.vector")
            .unwrap()
            .expect("v2 metadata");
        serde_json::from_str::<serde_json::Value>(&raw).unwrap()
    };
    let sources: Vec<String> = manifest["manifest"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        sources,
        vec![
            "docs/added.txt".to_string(),
            "docs/changed.txt".to_string(),
            "docs/keep.txt".to_string()
        ],
        "merged manifest sources are the sorted union"
    );

    // ── Final source merge: keep unchanged, added = C, changed -> v2 ──
    // (added was written above and stays; only `changed` diverges.)
    std::fs::write(docs.join("changed.txt"), "changed body v2 after merge").unwrap();

    // ── Reconcile via the first store touch of the next turn ──
    let query = "zz-end-to-end-sentinel";
    rec.clear();
    let store = VectorStore::open(&spec_at(final_db.clone(), &url)).unwrap();
    let dp = store.db_path_for(Path::new(""));
    let hits = store.search(&dp, query, 10).await.unwrap();

    // Exactly one document request, and it is the changed v2 text.
    let doc_requests = rec.document_requests(query);
    assert_eq!(
        doc_requests.len(),
        1,
        "reconcile must embed exactly one changed document; all requests: {:?}",
        rec.all()
    );
    assert_eq!(
        doc_requests[0],
        vec!["changed body v2 after merge".to_string()],
        "the single re-embed must be the changed v2 text"
    );
    assert!(
        rec.document_inputs(query)
            .iter()
            .all(|t| !t.contains("keep body unchanged")),
        "the unchanged source must be reused, not re-embedded"
    );
    assert!(
        rec.document_inputs(query)
            .iter()
            .all(|t| !t.contains("added body theirs-only")),
        "the adopted theirs-only source must be reused, not re-embedded"
    );

    // All three documents are served after reconcile.
    let texts: Vec<&str> = hits.iter().map(|h| h.text.as_str()).collect();
    assert!(
        texts.iter().any(|t| t.contains("keep body unchanged")),
        "unchanged source still served: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.contains("changed body v2 after merge")),
        "changed source served at its new version: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("added body theirs-only")),
        "adopted theirs-only source still served: {texts:?}"
    );
    assert!(
        texts.iter().all(|t| !t.contains("changed body v1")),
        "the pre-merge changed version must be gone: {texts:?}"
    );

    // Success clears pending.
    assert!(
        !has_pending(&final_db),
        "a completed reconcile clears the pending marker"
    );
}
