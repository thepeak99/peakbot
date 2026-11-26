//! TDD tests for the vectors-merge turn-start reconciliation seam.
//!
//! These drive the *real* AgentRunner turn paths (`run_loop` user arm and
//! `run_bg_synthetic_turn_if_any` background arm) with a VectorStore injected
//! via `StateManager::with_vector`. They prove that the turn loop itself — not
//! just a doc tool call — reconciles a drifted repo-anchored store at the start of
//! every turn, before any agent work.
//!
//! The turn-start `ensure_fresh` call lives in
//! `AgentRunner::process_message_internal` (the shared seam between the user
//! arm and `run_bg_synthetic_turn_if_any`), reading the store from
//! `state_manager.vector_store()`.

#[cfg(feature = "host")]
use crate::bg_processes::StartParams;
#[cfg(feature = "host")]
use crate::config::EmbeddingsConfig;
#[cfg(feature = "host")]
use crate::config::resolve::{DbPathSpec, VectorSpec};
#[cfg(feature = "mock")]
use crate::hooks::SessionHook;
#[cfg(feature = "mock")]
use crate::mock::{MockCompletionModel, MockResponse};
#[cfg(feature = "mock")]
use crate::providers::DynAgent;
#[cfg(feature = "host")]
use crate::state::StateManager;
#[cfg(feature = "host")]
use crate::tools::ShellKind;
#[cfg(feature = "host")]
use crate::vector::VectorStore;
#[cfg(feature = "mock")]
use crate::{AgentRunner, ProviderInfo, SkillRegistry, UiAction};
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

#[cfg(feature = "mock")]
use crate::config::resolve::{Config, ParseCtx, Source};
#[cfg(feature = "mock")]
use crate::config::{ProviderType, ResolvedModel};

/// Minimal global config for AgentRunner construction (test fixture).
fn test_config() -> Config {
    const YAML: &str = r#"
providers:
  - name: mock
    type: openai
    base_url: http://localhost:1
    api_key: test
    models:
      - { name: mock-model, alias: mock-model }
general:
  context:
    enabled: false
profiles:
  default:
    agents:
      agent:
        model: mock-model
"#;
    Config::parse(
        Source {
            path: std::path::Path::new("/test/config.yaml"),
            text: YAML,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .expect("test config must parse")
}

/// The wire identity the mock-agent tests boot on.
fn mock_resolved_model() -> ResolvedModel {
    ResolvedModel {
        alias: "mock".to_string(),
        model_name: "mock-model".to_string(),
        provider_name: "mock".to_string(),
        provider_kind: ProviderType::OpenRouter,
        provider_config: crate::config::ProviderConfig::default(),
        context_size: 100_000,
    }
}

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

    fn hits(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn reset(&self) {
        self.requests.lock().unwrap().clear();
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

/// A mock that fails every request but records that it was hit — for the
/// "reconcile failure does not fail the turn" test.
#[derive(Clone)]
struct FailingRecorder {
    hit_count: Arc<AtomicU64>,
}

impl FailingRecorder {
    fn new() -> Self {
        Self {
            hit_count: Arc::new(AtomicU64::new(0)),
        }
    }

    fn hits(&self) -> u64 {
        self.hit_count.load(Ordering::Relaxed)
    }
}

async fn spawn_failing_embeddings() -> (String, FailingRecorder) {
    let rec = FailingRecorder::new();
    let app = Router::new()
        .route("/embeddings", post(fail_embed))
        .with_state(rec.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind failing embeddings");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), rec)
}

async fn fail_embed(State(rec): State<FailingRecorder>) -> Json<serde_json::Value> {
    rec.hit_count.fetch_add(1, Ordering::Relaxed);
    Json(serde_json::json!({ "error": { "message": "mock 500" } }))
}

// ── Git / store helpers ─────────────────────────────────────────────────

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
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("pending"), "pending").unwrap();
}

/// Build the turn-loop test fixture: a temp git repo, repo-anchored DB, indexed doc,
/// drift created (changed source + HEAD change + pending marker).
/// Returns the tempdir, DB path, live embeddings URL + recorder, and the
/// repo store spec.
async fn build_fixture() -> (
    tempfile::TempDir,
    PathBuf,
    String,
    EmbedRecorder,
    VectorSpec,
) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("v.db");
    let (url, rec) = spawn_recording_embeddings().await;
    let spec = spec_at(db.clone(), &url);

    init_git_repo(dir.path());
    let doc = dir.path().join("doc.txt");
    fs::write(&doc, "initial content").unwrap();
    git_commit_all(dir.path(), "initial");

    let store = VectorStore::open(&spec).unwrap();
    let dp = store.db_path_for(Path::new(""));
    store.index_file(&dp, &doc, &HashMap::new()).await.unwrap();
    drop(store);

    // Create drift: new commit changes HEAD, plus pending marker (the merge
    // driver would set this).
    fs::write(&doc, "changed content").unwrap();
    git_commit_all(dir.path(), "change");
    set_pending(&db);

    (dir, db, url, rec, spec)
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(all(feature = "mock", feature = "host"))]
#[tokio::test(flavor = "multi_thread")]
async fn turn_start_reconciles_repo_store_without_doc_tool_call() {
    // Drives the real `run_loop` user QueueMessage arm. The mock agent
    // returns a plain text response (no doc tool calls), so if reconciliation
    // happens at all it must be at turn start — not triggered by a tool.
    let (dir, db, _url, rec, spec) = build_fixture().await;

    // Reset request accounting so the count reflects only the turn itself:
    // the agent makes no doc tool call, so any embedding after this point can
    // only come from turn-start reconciliation of the changed source.
    rec.reset();

    let store = VectorStore::open(&spec).unwrap();
    let sm = Arc::new(StateManager::new().with_vector(store));
    sm.ensure_boot_conversation(dir.path(), "mock-model");

    let mock = MockCompletionModel::new();
    mock.add_response(MockResponse::text("Turn started."));
    let (ev_tx, events) = tokio::sync::mpsc::unbounded_channel();
    let hook = SessionHook::with_context_tracking(Some(ev_tx), sm.stats_arc())
        .with_state_manager(&sm)
        .with_wire_label("mock".to_string(), "mock-model".to_string());

    let agent = rig_core::agent::AgentBuilder::new(mock)
        .preamble("test prompt")
        .max_tokens(1024)
        .default_max_turns(4)
        .hook(hook.clone())
        .build();

    let info = ProviderInfo {
        name: "mock".to_string(),
        model: "mock-model".to_string(),
        supports_vision: false,
        preserve_reasoning: true,
        display_reasoning: false,
    };

    let config = Arc::new(test_config());
    let model = mock_resolved_model();

    let mut runner = AgentRunner::new(
        DynAgent::Mock(agent),
        config,
        info,
        SkillRegistry::default(),
        Some(events),
        Some(sm.clone()),
        Arc::new(hook),
        &model,
    )
    .expect("runner builds");

    let (tx, rx) = mpsc::unbounded_channel();
    let h = tokio::spawn(async move { runner.run_loop(rx).await });

    tx.send(UiAction::SendMessage {
        text: "Start turn.".to_string(),
        attachments: Vec::new(),
    })
    .expect("channel open");

    // Wait for the assistant row (≤10s).
    let got = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let msgs = sm.get_state().chat.messages;
            if let Some(row) = msgs.iter().find(|m| {
                matches!(m.role, crate::ui::app_state::MessageRole::Agent)
                    && m.content.contains("Turn started.")
            }) {
                return Some(row.content.clone());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("turn must finish within 10s");
    assert!(got.is_some(), "mock model must return normally");

    drop(tx);
    tokio::time::timeout(Duration::from_secs(5), h)
        .await
        .expect("run_loop must exit after channel closes")
        .expect("run_loop task must not panic");

    // The turn loop should have reconciled the store at turn start.
    assert!(
        !has_pending(&db),
        "turn-start reconciliation must clear the pending marker"
    );
    assert!(
        rec.hits() > 0,
        "the turn must re-embed the changed source at turn start (no doc tool \
         was called, so the only possible embed is the turn-start reconcile)"
    );

    // Changed source must be indexed. Search with a fresh store (triggers
    // ensure_fresh, but pending is already cleared, so this is a pure query).
    let store2 = VectorStore::open(&spec).unwrap();
    let dp2 = store2.db_path_for(Path::new(""));
    let hits = store2.search(&dp2, "changed content", 5).await.unwrap();
    assert!(
        hits.iter().any(|h| h.text.contains("changed content")),
        "reconciled store must contain changed content; hits: {:?}",
        hits.iter().map(|h| h.text.clone()).collect::<Vec<_>>()
    );
}

#[cfg(all(feature = "mock", feature = "host"))]
#[tokio::test(flavor = "multi_thread")]
async fn background_turn_start_reconciles_repo_store() {
    // Drives the real `run_bg_synthetic_turn_if_any` path: a background
    // process emits output, the agent loop turns it into a synthetic user
    // turn, and the turn must reconcile before running.
    let (dir, db, _url, _rec, spec) = build_fixture().await;

    let store = VectorStore::open(&spec).unwrap();
    let sm = Arc::new(StateManager::new().with_vector(store));
    sm.ensure_boot_conversation(dir.path(), "mock-model");

    // Spawn a bg process with cooldown 0 so the synthetic turn can be drained
    // immediately. `start_bg` requires the bg-notify bridge to be attached
    // (the reader pings it); a bare channel is enough — `run_bg_synthetic_turn_if_any`
    // drains synchronously without the loop.
    let (bg_ping_tx, _bg_ping_rx) = mpsc::unbounded_channel::<()>();
    sm.attach_bg_notify(bg_ping_tx);
    let entry = sm
        .start_bg(StartParams {
            command: "echo hi from bg; sleep 0.05".to_string(),
            capture_cap: 10,
            cwd: Some(dir.path().display().to_string()),
            label: None,
            cooldown: Duration::ZERO,
            env: None,
            shell: ShellKind::Bash {
                path: "sh".to_string(),
            },
        })
        .expect("start_bg succeeds");

    // Wait for output in the buffer (≤3s).
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        let rows = sm.list_bg();
        if let Some(r) = rows.iter().find(|r| r.id == entry.id)
            && r.buffer_len > 0
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Drain into a synthetic turn payload.
    // (Do NOT drain here — `run_bg_synthetic_turn_if_any` performs the drain
    // itself. Pre-draining would empty the buffer and make the seam a no-op.)

    // Call the real bg turn seam directly.
    let mock = MockCompletionModel::new();
    mock.add_response(MockResponse::text("Processing background output."));
    let (ev_tx, _events) = tokio::sync::mpsc::unbounded_channel();
    let hook = SessionHook::with_context_tracking(Some(ev_tx), sm.stats_arc())
        .with_state_manager(&sm)
        .with_wire_label("mock".to_string(), "mock-model".to_string());

    let agent = rig_core::agent::AgentBuilder::new(mock)
        .preamble("test prompt")
        .max_tokens(1024)
        .default_max_turns(4)
        .hook(hook.clone())
        .build();

    let config = Arc::new(test_config());

    let (completion_tx, _rx) = broadcast::channel(16);
    AgentRunner::run_bg_synthetic_turn_if_any(
        &Some(sm.clone()),
        &Arc::new(DynAgent::Mock(agent)),
        &config,
        &completion_tx,
    )
    .await;

    // The bg synthetic turn must have actually run (the mock model responded).
    let ran = sm.get_state().chat.messages.iter().any(|m| {
        matches!(m.role, crate::ui::app_state::MessageRole::Agent)
            && m.content.contains("Processing background output.")
    });
    assert!(
        ran,
        "the bg synthetic turn must run through process_message_internal"
    );

    // Background turn should have reconciled.
    assert!(
        !has_pending(&db),
        "background turn-start reconciliation must clear the pending marker"
    );

    let store2 = VectorStore::open(&spec).unwrap();
    let dp2 = store2.db_path_for(Path::new(""));
    let hits = store2.search(&dp2, "changed content", 5).await.unwrap();
    assert!(
        hits.iter().any(|h| h.text.contains("changed content")),
        "reconciled store must contain changed content after bg turn"
    );
}

#[cfg(all(feature = "mock", feature = "host"))]
#[tokio::test(flavor = "multi_thread")]
async fn turn_start_reconcile_failure_does_not_fail_turn() {
    // A store with drift; the embeddings endpoint fails on every request.
    // The turn must survive, pending stays set, and the failing endpoint must
    // show a request was made (proof the turn attempted to reconcile).
    let (dir, db, _url, _rec, _spec) = build_fixture().await;

    let (fail_url, fail_rec) = spawn_failing_embeddings().await;
    let fail_spec = spec_at(db.clone(), &fail_url);
    let store = VectorStore::open(&fail_spec).unwrap();
    let sm = Arc::new(StateManager::new().with_vector(store));
    sm.ensure_boot_conversation(dir.path(), "mock-model");

    let mock = MockCompletionModel::new();
    mock.add_response(MockResponse::text("Turn survived reconcile failure."));
    let (ev_tx, events) = tokio::sync::mpsc::unbounded_channel();
    let hook = SessionHook::with_context_tracking(Some(ev_tx), sm.stats_arc())
        .with_state_manager(&sm)
        .with_wire_label("mock".to_string(), "mock-model".to_string());

    let agent = rig_core::agent::AgentBuilder::new(mock)
        .preamble("test prompt")
        .max_tokens(1024)
        .default_max_turns(4)
        .hook(hook.clone())
        .build();

    let info = ProviderInfo {
        name: "mock".to_string(),
        model: "mock-model".to_string(),
        supports_vision: false,
        preserve_reasoning: true,
        display_reasoning: false,
    };

    let config = Arc::new(test_config());
    let model = mock_resolved_model();

    let mut runner = AgentRunner::new(
        DynAgent::Mock(agent),
        config,
        info,
        SkillRegistry::default(),
        Some(events),
        Some(sm.clone()),
        Arc::new(hook),
        &model,
    )
    .expect("runner builds");

    let (tx, rx) = mpsc::unbounded_channel();
    let h = tokio::spawn(async move { runner.run_loop(rx).await });

    tx.send(UiAction::SendMessage {
        text: "Survive.".to_string(),
        attachments: Vec::new(),
    })
    .expect("channel open");

    let got = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let msgs = sm.get_state().chat.messages;
            if let Some(row) = msgs.iter().find(|m| {
                matches!(m.role, crate::ui::app_state::MessageRole::Agent)
                    && m.content.contains("Turn survived")
            }) {
                return Some(row.content.clone());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("turn must finish within 10s");
    assert!(got.is_some(), "turn must survive the reconcile failure");

    drop(tx);
    tokio::time::timeout(Duration::from_secs(5), h)
        .await
        .expect("run_loop must exit after channel closes")
        .expect("run_loop task must not panic");

    // The turn attempted reconcile (failing endpoint hit) and it failed.
    assert!(
        fail_rec.hits() > 0,
        "the turn loop must attempt reconcile before dispatching the prompt \
         (the failing endpoint must record at least one request)"
    );
    assert!(
        has_pending(&db),
        "failed turn-start reconcile must leave the pending marker set for the next retry"
    );

    // Subsequent doc_search with the failing store refuses to serve stale rows.
    let store2 = VectorStore::open(&fail_spec).unwrap();
    let dp2 = store2.db_path_for(Path::new(""));
    let result = store2.search(&dp2, "content", 5).await;
    assert!(
        result.is_err(),
        "doc_search must error rather than serve stale rows when the store \
         is in a needs-reconcile state"
    );
}

#[cfg(all(feature = "mock", feature = "host"))]
#[tokio::test(flavor = "multi_thread")]
async fn local_store_turn_does_no_git_state_work() {
    // Local (outside-git) store: no reconcile state directory, no git ops,
    // regardless of turn activity.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("local.db");
    let (url, _rec) = spawn_recording_embeddings().await;
    let spec = spec_at(db.clone(), &url);

    let doc = dir.path().join("doc.txt");
    fs::write(&doc, "content").unwrap();

    let store = VectorStore::open(&spec).unwrap();
    let dp = store.db_path_for(Path::new(""));
    store.index_file(&dp, &doc, &HashMap::new()).await.unwrap();
    drop(store);

    assert!(
        !state_dir_for(&db).exists(),
        "local stores must not create a reconcile state directory"
    );

    let store2 = VectorStore::open(&spec).unwrap();
    let sm = Arc::new(StateManager::new().with_vector(store2));
    sm.ensure_boot_conversation(dir.path(), "mock-model");

    let mock = MockCompletionModel::new();
    mock.add_response(MockResponse::text("Local store turn."));
    let (ev_tx, events) = tokio::sync::mpsc::unbounded_channel();
    let hook = SessionHook::with_context_tracking(Some(ev_tx), sm.stats_arc())
        .with_state_manager(&sm)
        .with_wire_label("mock".to_string(), "mock-model".to_string());

    let agent = rig_core::agent::AgentBuilder::new(mock)
        .preamble("test prompt")
        .max_tokens(1024)
        .default_max_turns(4)
        .hook(hook.clone())
        .build();

    let info = ProviderInfo {
        name: "mock".to_string(),
        model: "mock-model".to_string(),
        supports_vision: false,
        preserve_reasoning: true,
        display_reasoning: false,
    };

    let config = Arc::new(test_config());
    let model = mock_resolved_model();

    let mut runner = AgentRunner::new(
        DynAgent::Mock(agent),
        config,
        info,
        SkillRegistry::default(),
        Some(events),
        Some(sm.clone()),
        Arc::new(hook),
        &model,
    )
    .expect("runner builds");

    let (tx, rx) = mpsc::unbounded_channel();
    let h = tokio::spawn(async move { runner.run_loop(rx).await });

    tx.send(UiAction::SendMessage {
        text: "Local turn.".to_string(),
        attachments: Vec::new(),
    })
    .expect("channel open");

    let got = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let msgs = sm.get_state().chat.messages;
            if let Some(row) = msgs.iter().find(|m| {
                matches!(m.role, crate::ui::app_state::MessageRole::Agent)
                    && m.content.contains("Local store turn.")
            }) {
                return Some(row.content.clone());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("turn must finish within 10s");
    assert!(got.is_some(), "agent must respond on a local-store turn");

    drop(tx);
    tokio::time::timeout(Duration::from_secs(5), h)
        .await
        .expect("run_loop must exit after channel closes")
        .expect("run_loop task must not panic");

    assert!(
        !state_dir_for(&db).exists(),
        "local store must still perform no state work after a turn"
    );
}
