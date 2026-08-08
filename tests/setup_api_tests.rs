//! T1 — Setup HTTP API tests (plan §B / §D track S, tasks S2 / S3 / S4).
//!
//! **Status: compile-fail until S2-S4 land.** This file targets the planned
//! public API in `peakbot::ui::web::setup`. None of the symbols referenced
//! below exist today; the file fails to compile and `cargo test` stops at
//! this integration target. That is the RED state we want.
//!
//! Per plan §B / §D track S the locked surface is:
//!
//! - `GET  /api/setup`              → `SetupInfo` (paths, os/arch/exe,
//!   builtin_tools, install, existing)
//! - `POST /api/setup/config`       body `{ yaml }` → on 200 `{ path,
//!   backup, restart_required: true }`, on 422 `{ error, problems? }`
//! - `POST /api/setup/config` 415 when Content-Type is not JSON.
//! - 401 when a token is configured and not presented (existing layer).
//!
//! Test strategy: spawn the real axum router on a random loopback port via
//! `spawn_app` (the same pattern as `src/ui/web/mod.rs::tests`), then
//! drive it with `reqwest`. This exercises actual TCP, the token layer,
//! the JSON Content-Type gate, and the validator pipeline.
//!
//! All HTTP tests run against `SetupState` with a caller-injected config
//! path (no `$HOME` mutation), so the suite is hermetic. `save_master_config`
//! in turn writes through `save_config_at` to a tempdir we control.

use axum::Router;
use axum::middleware::from_fn_with_state;
use peakbot::ui::web::require_token;
use peakbot::ui::web::setup::{
    InstallFn, InstallPath, ServiceFn, SetupInfo, SetupOpError, SetupState, WriteOutcome, router,
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

/// Spawn the setup router behind the existing token layer on a random
/// loopback port. Mirrors the `spawn_app` helper in `src/ui/web/mod.rs::tests`.
///
/// Assumption: the impl exposes `peakbot::ui::web::setup::router(state) -> Router`
/// and the existing `peakbot::ui::web::require_token` helper is reachable
/// from tests. The token layer is attached by the test directly via
/// `from_fn_with_state` to keep this file independent of whether S3
/// exposes a wrapper.
async fn spawn_setup(config_path: PathBuf, token: Option<&str>) -> (SocketAddr, TempDir) {
    let dir = TempDir::new().unwrap();
    let _ = dir.path(); // tmpdir lives for the test

    let state = SetupState {
        config_path,
        facts_base: peakbot::ui::web::setup::FactsBase::current(),
        needs_setup: false,
        // The `install` and `service` seams are exercised separately in
        // tests/install_render_tests.rs and tests/setup_service_tests.rs.
        install: peakbot::ui::web::setup::InstallFn::default_for_tests(),
        service: peakbot::ui::web::setup::ServiceFn::default_for_tests(),
    };
    let mut app: Router = router(state);
    if let Some(t) = token {
        // The plan pins the token layer as the existing helper; we import
        // it from `peakbot::ui::web` and apply it here so the test exercises
        // the same auth gate every other route sees.
        let secret: Arc<str> = t.into();
        app = app.layer(from_fn_with_state(secret, require_token));
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (addr, dir)
}

/// Spawn the setup router with the install / service adapters set to
/// the test fakes. The fakes are process-global; the caller queues
/// their responses (via [`queue_install`] / [`queue_service`]) BEFORE
/// spawning and the spawn returns the bound address. Tests that need
/// the default no-op (e.g. for /api/setup GET) should call this with
/// `use_fake_install = false`.
async fn spawn_setup_with_fakes(
    config_path: PathBuf,
    use_fake_install: bool,
    use_fake_service: bool,
    token: Option<&str>,
) -> (SocketAddr, TempDir) {
    let dir = TempDir::new().unwrap();
    let _ = dir.path();
    let state = SetupState {
        config_path,
        facts_base: peakbot::ui::web::setup::FactsBase::current(),
        needs_setup: false,
        install: if use_fake_install {
            fake_install()
        } else {
            peakbot::ui::web::setup::InstallFn::default_for_tests()
        },
        service: if use_fake_service {
            fake_service()
        } else {
            peakbot::ui::web::setup::ServiceFn::default_for_tests()
        },
    };
    let mut app: Router = router(state);
    if let Some(t) = token {
        let secret: Arc<str> = t.into();
        app = app.layer(from_fn_with_state(secret, require_token));
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (addr, dir)
}

/// Build a bare reqwest client that doesn't follow redirects.
fn bare_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

// ===========================================================================
// S2 — GET /api/setup
// ===========================================================================

#[tokio::test]
async fn get_api_setup_returns_expected_shape() {
    let cfg_dir = TempDir::new().unwrap();
    let (addr, _t) = spawn_setup(cfg_dir.path().join("config.yaml"), None).await;

    let resp = bare_client()
        .get(format!("http://{addr}/api/setup"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp.headers()[reqwest::header::CONTENT_TYPE]
        .to_str()
        .unwrap();
    assert!(ct.starts_with("application/json"), "got content-type {ct}");

    let body: SetupInfo = resp.json().await.expect("body must parse as SetupInfo");
    let _ = body;
}

#[tokio::test]
async fn get_api_setup_config_path_equals_injected_path() {
    let cfg_dir = TempDir::new().unwrap();
    let injected: PathBuf = cfg_dir.path().join("config.yaml");
    let (addr, _t) = spawn_setup(injected.clone(), None).await;

    let resp = bare_client()
        .get(format!("http://{addr}/api/setup"))
        .send()
        .await
        .unwrap();
    let body: SetupInfo = resp.json().await.unwrap();
    assert_eq!(
        body.config_path,
        injected.to_string_lossy(),
        "config_path must equal the injected tempdir path"
    );
}

#[tokio::test]
async fn get_api_setup_reports_existing_absent_when_no_file() {
    let cfg_dir = TempDir::new().unwrap();
    let (addr, _t) = spawn_setup(cfg_dir.path().join("config.yaml"), None).await;

    let resp = bare_client()
        .get(format!("http://{addr}/api/setup"))
        .send()
        .await
        .unwrap();
    let body: SetupInfo = resp.json().await.unwrap();

    match body.existing {
        peakbot::ui::web::setup::ExistingConfig::Absent => {}
        other => panic!("expected Absent, got {other:?}"),
    }
}

#[tokio::test]
async fn get_api_setup_reports_existing_ok_when_valid_file_present() {
    let cfg_dir = TempDir::new().unwrap();
    let yaml = "provider:\n  type: openrouter\n  config:\n    model: x\n";
    std::fs::write(cfg_dir.path().join("config.yaml"), yaml).unwrap();
    let (addr, _t) = spawn_setup(cfg_dir.path().join("config.yaml"), None).await;

    let resp = bare_client()
        .get(format!("http://{addr}/api/setup"))
        .send()
        .await
        .unwrap();
    let body: SetupInfo = resp.json().await.unwrap();

    match body.existing {
        peakbot::ui::web::setup::ExistingConfig::Ok { config } => {
            // The transcoded JSON should carry the keys the file contained
            // — no defaults pollution (§A-Q3).
            assert!(
                config.get("provider").is_some(),
                "imported config must carry the provider key"
            );
        }
        other => panic!("expected Ok, got {other:?}"),
    }
}

#[tokio::test]
async fn get_api_setup_reports_existing_error_on_garbage_file_but_returns_200() {
    let cfg_dir = TempDir::new().unwrap();
    std::fs::write(
        cfg_dir.path().join("config.yaml"),
        "this: is: not: valid: yaml: at: all:\n",
    )
    .unwrap();
    let (addr, _t) = spawn_setup(cfg_dir.path().join("config.yaml"), None).await;

    let resp = bare_client()
        .get(format!("http://{addr}/api/setup"))
        .send()
        .await
        .unwrap();
    // A malformed config can never break the facts fetch (plan §A-Q3).
    assert_eq!(resp.status(), 200);
    let body: SetupInfo = resp.json().await.unwrap();
    match body.existing {
        peakbot::ui::web::setup::ExistingConfig::Error { message } => {
            assert!(!message.is_empty(), "error message must be non-empty");
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

// ===========================================================================
// S3 — JSON Content-Type + token gating.
// ===========================================================================

#[tokio::test]
async fn post_config_with_wrong_content_type_returns_415() {
    let cfg_dir = TempDir::new().unwrap();
    let (addr, _t) = spawn_setup(cfg_dir.path().join("config.yaml"), None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .header(reqwest::header::CONTENT_TYPE, "text/plain")
        .body("yaml: 1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 415, "non-JSON body must return 415");
}

#[tokio::test]
async fn post_config_with_missing_content_type_returns_415() {
    let cfg_dir = TempDir::new().unwrap();
    let (addr, _t) = spawn_setup(cfg_dir.path().join("config.yaml"), None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .body(r#"{"yaml":"provider:\n  type: openrouter\n  config:\n    model: x\n"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 415, "missing content-type must return 415");
}

#[tokio::test]
async fn post_config_with_malformed_json_returns_400() {
    let cfg_dir = TempDir::new().unwrap();
    let (addr, _t) = spawn_setup(cfg_dir.path().join("config.yaml"), None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("this is not json")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "malformed JSON body must return 400");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body.get("error").is_some(),
        "malformed-JSON response must use the ApiError envelope"
    );
}

#[tokio::test]
async fn setup_routes_require_token_when_one_is_configured() {
    let cfg_dir = TempDir::new().unwrap();
    let (addr, _t) = spawn_setup(cfg_dir.path().join("config.yaml"), Some("s3cret")).await;

    // No token → 401 on every setup route.
    let resp = bare_client()
        .get(format!("http://{addr}/api/setup"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(r#"{"yaml":"provider:\n  type: openrouter\n  config:\n    model: x\n"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // With the right query token → 200.
    let resp = bare_client()
        .get(format!("http://{addr}/api/setup?token=s3cret"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

// ===========================================================================
// S4 — POST /api/setup/config validate-then-write pipeline.
// ===========================================================================

#[tokio::test]
async fn post_valid_config_returns_200_writes_exact_bytes_and_flags_restart() {
    let cfg_dir = TempDir::new().unwrap();
    let (addr, _t) = spawn_setup(cfg_dir.path().join("config.yaml"), None).await;
    let yaml = "provider:\n  type: openrouter\n  config:\n    model: anthropic/claude-3.7-sonnet\n";

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "yaml": yaml }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: WriteOutcome = resp.json().await.unwrap();
    assert!(
        body.restart_required,
        "lock: restart_required must be literal true"
    );
    assert!(
        body.backup.is_none(),
        "first write must report backup = null"
    );

    // File on disk is byte-identical to the input.
    let on_disk = std::fs::read_to_string(&body.path).unwrap();
    assert_eq!(on_disk, yaml, "writer must write the input bytes verbatim");
}

#[tokio::test]
async fn post_valid_multiline_persona_round_trips_byte_for_byte() {
    // §A-Q7 + §A-Q4 S4: a YAML carrying a multi-line `persona:` block
    // round-trips byte-for-byte. Combined coverage of P1 + S4 in one HTTP.
    let cfg_dir = TempDir::new().unwrap();
    let (addr, _t) = spawn_setup(cfg_dir.path().join("config.yaml"), None).await;
    let yaml = "\
persona: |2-
  You are a coding agent working in the user's local filesystem.

  State what you are about to do in one line, do it, then report what changed.
provider:
  type: openrouter
  config:
    model: anthropic/claude-3.7-sonnet
";

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "yaml": yaml }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200, "wizard-shaped YAML must round-trip");
    let body: WriteOutcome = resp.json().await.unwrap();
    let on_disk = std::fs::read_to_string(&body.path).unwrap();
    assert_eq!(
        on_disk, yaml,
        "post-write bytes must match pre-write bytes byte-for-byte"
    );
}

#[tokio::test]
async fn post_invalid_yaml_returns_422_and_does_not_write_file() {
    let cfg_dir = TempDir::new().unwrap();
    let cfg_path = cfg_dir.path().join("config.yaml");
    let (addr, _t) = spawn_setup(cfg_path.clone(), None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "yaml": "provider:\n  type: openrouter\n  config:\n    model: x\nthis_is_definitely_not_a_real_key: 1\n" }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 422, "unknown key must produce 422");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("error").is_some(), "envelope must have `error`");
    assert!(
        body.get("problems").is_some(),
        "422 envelope must carry a `problems` array"
    );
    assert!(
        !cfg_path.exists(),
        "422 must NOT write to disk; target file must be unchanged/absent"
    );
}

#[tokio::test]
async fn post_yaml_with_reserved_alias_unknown_returns_422_and_does_not_write() {
    let cfg_dir = TempDir::new().unwrap();
    let cfg_path = cfg_dir.path().join("config.yaml");
    let (addr, _t) = spawn_setup(cfg_path.clone(), None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "yaml": "providers:\n  - name: openrouter\n    type: openrouter\n    api_key: k\n    models:\n      - name: anthropic/claude-3.7-sonnet\n        alias: unknown\ndefault_model: unknown\n" }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        422,
        "reserved alias `unknown` must be rejected"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    let problems = body
        .get("problems")
        .and_then(|p| p.as_array())
        .expect("problems[]");
    let joined = problems
        .iter()
        .map(|p| p.as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        joined.contains("unknown") || joined.contains("reserved"),
        "problems[] must name the offending alias or the rule; got: {joined}"
    );
    assert!(
        !cfg_path.exists(),
        "422 must NOT write to disk; target file must be absent"
    );
}

#[tokio::test]
async fn post_yaml_with_default_model_referencing_undeclared_alias_returns_422() {
    let cfg_dir = TempDir::new().unwrap();
    let cfg_path = cfg_dir.path().join("config.yaml");
    let (addr, _t) = spawn_setup(cfg_path.clone(), None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "yaml": "providers:\n  - name: openrouter\n    type: openrouter\n    api_key: k\n    models:\n      - name: anthropic/claude-3.7-sonnet\n        alias: sonnet\ndefault_model: sonet\n" }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        422,
        "default_model pointing at undeclared alias must be 422"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    let problems = body
        .get("problems")
        .and_then(|p| p.as_array())
        .expect("problems[]");
    let joined = problems
        .iter()
        .map(|p| p.as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        joined.contains("sonet") || joined.contains("default_model"),
        "problems[] must mention the bad alias or default_model; got: {joined}"
    );
    assert!(!cfg_path.exists(), "422 must NOT write to disk");
}

#[tokio::test]
async fn post_yaml_with_tools_disabled_and_only_returns_422() {
    let cfg_dir = TempDir::new().unwrap();
    let cfg_path = cfg_dir.path().join("config.yaml");
    let (addr, _t) = spawn_setup(cfg_path.clone(), None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "yaml": "tools:\n  disabled: [\"bash_bg\"]\n  only: [\"file_read\"]\n" }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        422,
        "tools.disabled + tools.only must be rejected with 422 (XOR)"
    );
    assert!(!cfg_path.exists(), "422 must NOT write to disk");
}

// ---------------------------------------------------------------------------
// Stage 3 — the `pipelines:` validator (plan §6.6). The endpoint runs the
// same `PipelineSet::build` the binary runs at boot, so a team the binary
// would refuse to start with never reaches disk.
// ---------------------------------------------------------------------------

/// `providers:` + `default_model:` prefix shared by the pipeline fixtures.
const MODELS_YAML: &str = "providers:\n  - name: openrouter\n    type: openrouter\n    api_key: k\n    models:\n      - name: anthropic/claude-3.7-sonnet\n        alias: sonnet\ndefault_model: sonnet\n";

/// Collect the `problems[]` array of an error envelope into one string.
async fn problems_joined(resp: reqwest::Response) -> String {
    let body: serde_json::Value = resp.json().await.unwrap();
    body.get("problems")
        .and_then(|p| p.as_array())
        .expect("problems[]")
        .iter()
        .map(|p| p.as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" | ")
}

#[tokio::test]
async fn post_valid_pipelines_list_returns_200() {
    let cfg_dir = TempDir::new().unwrap();
    let cfg_path = cfg_dir.path().join("config.yaml");
    let (addr, _t) = spawn_setup(cfg_path.clone(), None).await;
    let yaml = format!(
        "{MODELS_YAML}pipelines:\n  - name: review-team\n    orchestrator:\n      model: sonnet\n      prompt: |2-\n        You lead a small team.\n    agents:\n      reviewer:\n        model: sonnet\n        prompt: |2-\n          Review diffs.\n"
    );

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "yaml": yaml }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        200,
        "a valid pipelines list must be accepted"
    );
    assert_eq!(
        std::fs::read_to_string(&cfg_path).unwrap(),
        yaml,
        "the reviewed bytes are what lands"
    );
}

#[tokio::test]
async fn post_pipeline_name_with_spaces_returns_200() {
    // `/pipeline` takes the rest of the line, so a spaced name is legal and
    // the endpoint must not refuse what the binary now boots with.
    let cfg_dir = TempDir::new().unwrap();
    let cfg_path = cfg_dir.path().join("config.yaml");
    let (addr, _t) = spawn_setup(cfg_path.clone(), None).await;
    let yaml = format!(
        "{MODELS_YAML}pipelines:\n  - name: \"Generic Dev Team\"\n    orchestrator:\n      model: sonnet\n    agents:\n      reviewer:\n        prompt: Review diffs.\n"
    );

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "yaml": yaml }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        200,
        "a pipeline name with spaces must be accepted"
    );
}

#[tokio::test]
async fn post_pipelines_with_unknown_orchestrator_alias_returns_422_with_pipeline_message() {
    let cfg_dir = TempDir::new().unwrap();
    let cfg_path = cfg_dir.path().join("config.yaml");
    let (addr, _t) = spawn_setup(cfg_path.clone(), None).await;
    let yaml = format!(
        "{MODELS_YAML}pipelines:\n  - name: review-team\n    orchestrator:\n      model: ghost\n    agents:\n      reviewer:\n        prompt: Review diffs.\n"
    );

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "yaml": yaml }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 422, "unknown orchestrator alias must be 422");
    let joined = problems_joined(resp).await;
    assert!(
        joined.contains("pipeline 'review-team'")
            && joined.contains("orchestrator names unknown model alias 'ghost'"),
        "problems[] must carry the PipelineSet message verbatim; got: {joined}"
    );
    assert!(!cfg_path.exists(), "422 must NOT write to disk");
}

#[tokio::test]
async fn post_pipeline_with_no_members_returns_422() {
    let cfg_dir = TempDir::new().unwrap();
    let cfg_path = cfg_dir.path().join("config.yaml");
    let (addr, _t) = spawn_setup(cfg_path.clone(), None).await;
    let yaml = format!(
        "{MODELS_YAML}pipelines:\n  - name: solo\n    orchestrator: {{}}\n    agents: {{}}\n"
    );

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "yaml": yaml }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 422, "a memberless pipeline must be 422");
    let joined = problems_joined(resp).await;
    assert!(
        joined.contains("pipeline 'solo'") && joined.contains("at least one sub-agent"),
        "problems[] must name the empty team; got: {joined}"
    );
    assert!(!cfg_path.exists(), "422 must NOT write to disk");
}

#[tokio::test]
async fn post_legacy_pipeline_block_returns_422_with_migration_hint() {
    // Amendment 5: the legacy block is a hard boot error, so the wizard
    // must refuse it here rather than write a config that cannot start.
    let cfg_dir = TempDir::new().unwrap();
    let cfg_path = cfg_dir.path().join("config.yaml");
    let (addr, _t) = spawn_setup(cfg_path.clone(), None).await;
    let yaml = format!(
        "{MODELS_YAML}pipeline:\n  enabled: true\n  orchestrator_prompt: You lead a small team.\n  agents:\n    reviewer:\n      prompt: Review diffs.\n"
    );

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "yaml": yaml }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 422, "the legacy pipeline block must be 422");
    let joined = problems_joined(resp).await;
    assert!(
        joined.contains("legacy 'pipeline:' block is no longer supported")
            && joined.contains("pipelines:"),
        "problems[] must carry the migration recipe; got: {joined}"
    );
    assert!(!cfg_path.exists(), "422 must NOT write to disk");
}

#[tokio::test]
async fn post_when_config_exists_creates_backup_with_old_bytes() {
    let cfg_dir = TempDir::new().unwrap();
    let cfg_path = cfg_dir.path().join("config.yaml");
    let first = "provider:\n  type: openrouter\n  config:\n    model: A\n";
    std::fs::write(&cfg_path, first).unwrap();
    let (addr, _t) = spawn_setup(cfg_path.clone(), None).await;

    let second = "provider:\n  type: openrouter\n  config:\n    model: B\n";
    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/config"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "yaml": second }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: WriteOutcome = resp.json().await.unwrap();
    let backup = body
        .backup
        .as_ref()
        .expect("second write must report a backup");
    let backup_bytes = std::fs::read_to_string(backup).unwrap();
    assert_eq!(backup_bytes, first, ".bak must hold the old bytes");
    assert_eq!(std::fs::read_to_string(&body.path).unwrap(), second);
}

// ===========================================================================
// I6 — track I HTTP wiring (plan §B / §D I6).
//
// Strategy: swap the production `InstallFn` / `ServiceFn` for fakes that
// return canned `Result<JsonValue, SetupOpError>` values. The fakes let
// us exercise the *handler* (status mapping, JSON shape, error
// envelope) without ever copying a binary or shelling out to
// `systemctl` — both of which fail in the CI container (§E.9).
//
// The fn-pointer seam cannot capture state. We work around this with
// two `tokio::sync::Mutex` cells:
//   * `SERIAL` — the test holds it for the entire test body, so
//     parallel tests cannot race for the response slot.
//   * `RESPONSE` — the dispatch (running in `spawn_blocking`) reads
//     the canned response from here.
// Because `SERIAL` is held by the test for the whole async body, two
// I6 tests cannot overlap; the dispatch reads `RESPONSE` without
// touching `SERIAL`, so no deadlock.
// ===========================================================================

// A single global `Arc<Mutex<()>>` used to serialise the I6 tests.
// Stored in a `OnceLock` so the init is one-shot; cloned into each
// test that needs the lock, the test holds the `OwnedMutexGuard`
// across its entire async body to keep parallel tests from racing
// for the response slot.
static SERIAL: std::sync::OnceLock<std::sync::Arc<tokio::sync::Mutex<()>>> =
    std::sync::OnceLock::new();
static INSTALL_RESPONSE: tokio::sync::Mutex<Option<Result<serde_json::Value, SetupOpError>>> =
    tokio::sync::Mutex::const_new(None);
static SERVICE_RESPONSE: tokio::sync::Mutex<Option<Result<serde_json::Value, SetupOpError>>> =
    tokio::sync::Mutex::const_new(None);

fn serial_lock() -> std::sync::Arc<tokio::sync::Mutex<()>> {
    SERIAL
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

async fn serial() -> tokio::sync::OwnedMutexGuard<()> {
    serial_lock().lock_owned().await
}

fn fake_install_dispatch(_req: serde_json::Value) -> Result<serde_json::Value, SetupOpError> {
    // The dispatch is called from `spawn_blocking`, so a sync mutex
    // is fine. `try_lock` because we cannot .await inside this fn —
    // the slot must already be populated by the test before the
    // handler invokes us. (If the test forgot to queue, we return a
    // 500 with a diagnostic.)
    match INSTALL_RESPONSE.try_lock() {
        Ok(mut g) => g.take().unwrap_or_else(|| {
            Err(SetupOpError::internal(
                "no install fake queued (test forgot to call queue_install)",
                Vec::new(),
            ))
        }),
        Err(_) => Err(SetupOpError::internal(
            "install fake mutex contention",
            Vec::new(),
        )),
    }
}

fn fake_service_dispatch(_req: serde_json::Value) -> Result<serde_json::Value, SetupOpError> {
    match SERVICE_RESPONSE.try_lock() {
        Ok(mut g) => g.take().unwrap_or_else(|| {
            Err(SetupOpError::internal(
                "no service fake queued (test forgot to call queue_service)",
                Vec::new(),
            ))
        }),
        Err(_) => Err(SetupOpError::internal(
            "service fake mutex contention",
            Vec::new(),
        )),
    }
}

fn fake_install() -> InstallFn {
    InstallFn(fake_install_dispatch)
}
fn fake_service() -> ServiceFn {
    ServiceFn(fake_service_dispatch)
}

/// Park a response on the install fake. The serial guard from
/// `serial()` must already be held so the slot is private to this
/// test; the dispatch will `try_lock` + take the response exactly
/// once.
fn queue_install(response: Result<serde_json::Value, SetupOpError>) {
    *INSTALL_RESPONSE
        .try_lock()
        .expect("install cell not writable under serial") = Some(response);
}
fn queue_service(response: Result<serde_json::Value, SetupOpError>) {
    *SERVICE_RESPONSE
        .try_lock()
        .expect("service cell not writable under serial") = Some(response);
}

// ── I6c — GET /api/setup install block (pure data) ─────────────────────

#[tokio::test]
async fn get_api_setup_install_block_has_real_pure_fields() {
    // The install block is computed from `install_target()` and
    // `path_state()` — both pure. No fake needed; we drive the real
    // install adapter and assert the wire shape.
    let cfg_dir = TempDir::new().unwrap();
    let (addr, _t) = spawn_setup(cfg_dir.path().join("config.yaml"), None).await;

    let resp = bare_client()
        .get(format!("http://{addr}/api/setup"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: SetupInfo = resp.json().await.unwrap();

    // §B `install.target` is a non-empty path on every host with a
    // home dir (Linux/macOS/Windows all resolve one).
    assert!(
        !body.install.target.is_empty(),
        "install.target must be a real path"
    );
    // §B `install.state` is one of three wire strings.
    assert!(
        matches!(body.install.state.as_str(), "current" | "absent" | "other"),
        "install.state must be one of the §B wire strings, got {:?}",
        body.install.state
    );
    // §B `install.path` is the same tagged union as the install
    // response's `path`. Round-trips through `InstallPath` since the
    // handler is built on top of it.
    let _ = body.install.path.clone();
    // Validate the tagged-union shape by serialising — §B pins it
    // exactly: status is one of `on_path` / `shadowed` / `absent`.
    let raw = serde_json::to_value(&body.install.path).unwrap();
    let status = raw.get("status").and_then(|s| s.as_str()).unwrap_or("");
    assert!(
        matches!(status, "on_path" | "shadowed" | "absent"),
        "install.path.status must be a §B wire string, got {status:?}"
    );
}

#[tokio::test]
async fn install_path_from_core_round_trips_all_variants() {
    // The in-module `InstallPath::from_core` is the only place the
    // core enum is converted to wire JSON. Exercise every variant
    // here so the GET install block can't silently drop a case.
    use peakbot::install::PathState;
    let on = InstallPath::from_core(&PathState::OnPath);
    let sh = InstallPath::from_core(&PathState::Shadowed {
        by: PathBuf::from("/opt/other/peakbot"),
    });
    let ab = InstallPath::from_core(&PathState::NotOnPath {
        hint: "add me".to_string(),
    });
    let v_on = serde_json::to_value(&on).unwrap();
    let v_sh = serde_json::to_value(&sh).unwrap();
    let v_ab = serde_json::to_value(&ab).unwrap();
    assert_eq!(v_on["status"], "on_path");
    assert_eq!(v_sh["status"], "shadowed");
    assert_eq!(v_sh["by"], "/opt/other/peakbot");
    assert_eq!(v_ab["status"], "absent");
    assert_eq!(v_ab["hint"], "add me");
}

// ── I6a — POST /api/setup/install ─────────────────────────────────────

#[tokio::test]
async fn post_install_success_returns_200_with_full_wire_shape() {
    let _serial = serial().await;
    let cfg_dir = TempDir::new().unwrap();
    queue_install(Ok(serde_json::json!({
        "source": "/x/peakbot",
        "target": "/home/u/.local/bin/peakbot",
        "action": "installed",
        "path": {"status": "on_path"},
        "notes": ["restart me"]
    })));
    let (addr, _t) =
        spawn_setup_with_fakes(cfg_dir.path().join("config.yaml"), true, false, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/install"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["action"], "installed");
    assert_eq!(body["source"], "/x/peakbot");
    assert_eq!(body["target"], "/home/u/.local/bin/peakbot");
    assert_eq!(body["path"]["status"], "on_path");
    assert_eq!(body["notes"][0], "restart me");
}

#[tokio::test]
async fn post_install_unsupported_maps_to_501() {
    let _serial = serial().await;
    let cfg_dir = TempDir::new().unwrap();
    queue_install(Err(SetupOpError::unsupported("no systemd here")));
    let (addr, _t) =
        spawn_setup_with_fakes(cfg_dir.path().join("config.yaml"), true, false, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/install"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 501, "Unsupported must map to 501 per §B");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "no systemd here");
}

#[tokio::test]
async fn post_install_io_error_maps_to_500() {
    let _serial = serial().await;
    let cfg_dir = TempDir::new().unwrap();
    queue_install(Err(SetupOpError::internal(
        "install I/O error: permission denied",
        Vec::new(),
    )));
    let (addr, _t) =
        spawn_setup_with_fakes(cfg_dir.path().join("config.yaml"), true, false, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/install"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("install I/O error"),
        "500 must surface the underlying error message verbatim"
    );
}

#[tokio::test]
async fn post_install_command_failed_maps_to_500_with_problems() {
    let _serial = serial().await;
    let cfg_dir = TempDir::new().unwrap();
    // §B: a CommandFailed-like failure must surface stderr in `problems`.
    queue_install(Err(SetupOpError::internal(
        "`systemctl --user enable` failed: bus not reachable",
        vec!["bus not reachable".to_string()],
    )));
    let (addr, _t) =
        spawn_setup_with_fakes(cfg_dir.path().join("config.yaml"), true, false, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/install"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.unwrap();
    let problems = body
        .get("problems")
        .and_then(|p| p.as_array())
        .expect("problems[]");
    assert_eq!(problems[0], "bus not reachable");
}

// ── I6b — GET|POST|DELETE /api/setup/service ──────────────────────────

#[tokio::test]
async fn get_service_status_returns_200_with_service_report_shape() {
    let _serial = serial().await;
    let cfg_dir = TempDir::new().unwrap();
    queue_service(Ok(serde_json::json!({
        "manager": "systemd-user",
        "name": "peakbot.service",
        "artifact": "/home/u/.config/systemd/user/peakbot.service",
        "installed": true,
        "exe": "/home/u/.local/bin/peakbot",
        "run_state": "running",
        "survives_logout": false,
        "commands": ["systemctl --user is-active peakbot.service"],
        "notes": ["linger is off"]
    })));
    let (addr, _t) =
        spawn_setup_with_fakes(cfg_dir.path().join("config.yaml"), false, true, None).await;

    let resp = bare_client()
        .get(format!("http://{addr}/api/setup/service"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    // §B field-for-field: every key must be present.
    for key in [
        "manager",
        "name",
        "artifact",
        "installed",
        "exe",
        "run_state",
        "survives_logout",
        "commands",
        "notes",
    ] {
        assert!(
            body.get(key).is_some(),
            "ServiceResponse missing field {key}"
        );
    }
    assert_eq!(body["manager"], "systemd-user");
    assert_eq!(body["run_state"], "running");
}

#[tokio::test]
async fn post_service_install_returns_200_with_bind_and_token_passed_through() {
    let _serial = serial().await;
    let cfg_dir = TempDir::new().unwrap();
    queue_service(Ok(serde_json::json!({
        "manager": "systemd-user",
        "name": "peakbot.service",
        "artifact": "/home/u/.config/systemd/user/peakbot.service",
        "installed": true,
        "exe": "/home/u/.local/bin/peakbot",
        "run_state": "unknown",
        "survives_logout": false,
        "commands": [],
        "notes": []
    })));
    let (addr, _t) =
        spawn_setup_with_fakes(cfg_dir.path().join("config.yaml"), false, true, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/service"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(
            serde_json::json!({
                "op": "install",
                "bind": "127.0.0.1:7823",
                "token": "s3cret",
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["installed"], true);
}

#[tokio::test]
async fn post_service_lan_without_token_maps_to_422() {
    let _serial = serial().await;
    let cfg_dir = TempDir::new().unwrap();
    // Adapter surfaces 422 on PlanError::TokenRequired; the fake
    // returns one to confirm the handler passes it through unchanged.
    queue_service(Err(SetupOpError::validation(
        "refusing to plan a service on non-loopback 0.0.0.0:7823: a token is required \
         (set --token or PEAKBOT_WEB_TOKEN)",
    )));
    let (addr, _t) =
        spawn_setup_with_fakes(cfg_dir.path().join("config.yaml"), false, true, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/service"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({"op":"install", "bind":"0.0.0.0:7823"}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        422,
        "PlanError::TokenRequired must map to 422 per §B"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    let msg = body["error"].as_str().unwrap_or("");
    assert!(
        msg.contains("0.0.0.0:7823") && msg.contains("token"),
        "422 must echo the rejected bind and name the missing token; got: {msg}"
    );
}

#[tokio::test]
async fn post_service_unsupported_maps_to_501() {
    let _serial = serial().await;
    let cfg_dir = TempDir::new().unwrap();
    queue_service(Err(SetupOpError::unsupported(
        "no service manager on this platform",
    )));
    let (addr, _t) =
        spawn_setup_with_fakes(cfg_dir.path().join("config.yaml"), false, true, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/service"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({"op":"install"}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 501);
}

#[tokio::test]
async fn delete_service_returns_200_with_notes_including_token_file_notice() {
    let _serial = serial().await;
    let cfg_dir = TempDir::new().unwrap();
    // The handler synthesises `{"op":"uninstall"}` and the fake
    // returns a pre-shaped report that includes the §E.5 token-file
    // note. The real adapter is what actually appends the note —
    // here we exercise the handler + the wire shape.
    queue_service(Ok(serde_json::json!({
        "manager": "systemd-user",
        "name": "peakbot.service",
        "artifact": null,
        "installed": false,
        "exe": null,
        "run_state": "unknown",
        "survives_logout": false,
        "commands": ["systemctl --user disable --now peakbot.service"],
        "notes": [
            "service unit removed",
            "the web-token file at /home/u/.config/peakbot/web-token was NOT deleted"
        ]
    })));
    let (addr, _t) =
        spawn_setup_with_fakes(cfg_dir.path().join("config.yaml"), false, true, None).await;

    let resp = bare_client()
        .delete(format!("http://{addr}/api/setup/service"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let notes: Vec<String> = body["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_string())
        .collect();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("web-token") && n.contains("NOT deleted")),
        "delete response must carry the §E.5 'web-token NOT deleted' note; got: {notes:?}"
    );
}

#[tokio::test]
async fn post_service_unknown_op_maps_to_422() {
    let _serial = serial().await;
    let cfg_dir = TempDir::new().unwrap();
    // The fake stays queued unused — the handler validates first.
    queue_service(Ok(serde_json::json!({})));
    let (addr, _t) =
        spawn_setup_with_fakes(cfg_dir.path().join("config.yaml"), false, true, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/service"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({"op": "freeze"}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 422);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("unknown service op")
    );
}

#[tokio::test]
async fn post_service_missing_op_defaults_to_install_with_200() {
    // Plan §B: POST body is `{bind?, token?}` — no `op` field. The
    // handler synthesises `op:"install"` and the install runs, so
    // missing `op` is *not* an error. The fake returns success.
    let _serial = serial().await;
    let cfg_dir = TempDir::new().unwrap();
    queue_service(Ok(serde_json::json!({
        "manager": "systemd-user",
        "name": "peakbot.service",
        "artifact": null,
        "installed": false,
        "exe": null,
        "run_state": "unknown",
        "survives_logout": false,
        "commands": [],
        "notes": []
    })));
    let (addr, _t) =
        spawn_setup_with_fakes(cfg_dir.path().join("config.yaml"), false, true, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/service"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "POST with no `op` defaults to install per §B"
    );
}

#[tokio::test]
async fn post_service_with_wrong_content_type_returns_415() {
    let _serial = serial().await;
    let cfg_dir = TempDir::new().unwrap();
    queue_service(Ok(serde_json::json!({})));
    let (addr, _t) =
        spawn_setup_with_fakes(cfg_dir.path().join("config.yaml"), false, true, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/setup/service"))
        .header(reqwest::header::CONTENT_TYPE, "text/plain")
        .body("hello")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        415,
        "POST /api/setup/service must reject non-JSON"
    );
}

#[tokio::test]
async fn install_routes_require_token_when_one_is_configured() {
    let _serial = serial().await;
    let cfg_dir = TempDir::new().unwrap();
    queue_install(Ok(serde_json::json!({})));
    queue_service(Ok(serde_json::json!({})));
    let (addr, _t) = spawn_setup_with_fakes(
        cfg_dir.path().join("config.yaml"),
        true,
        true,
        Some("s3cret"),
    )
    .await;

    for verb in ["get", "post", "delete"] {
        let url = format!("http://{addr}/api/setup/service");
        let req = match verb {
            "get" => bare_client().get(url).send(),
            "post" => bare_client()
                .post(url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body("{}")
                .send(),
            _ => bare_client().delete(url).send(),
        };
        let resp = req.await.unwrap();
        assert_eq!(
            resp.status(),
            401,
            "{verb} /api/setup/service must 401 without token"
        );
    }
}
