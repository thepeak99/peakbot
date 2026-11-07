//! `boot.edition` + Local `415 images_only` (mobile plan §2.5/§2.8 / §4.3
//! test 2) — RED.
//!
//! **RED today by BEHAVIOUR** (both compile fine in every edition):
//!   - `GET /api/settings` has no `boot.edition` field yet (L4 adds
//!     `"edition": "full" | "local"` to the `boot` block, in BOTH normal and
//!     needs_setup modes, derived from `cfg!(feature = "host")`);
//!   - the Local edition accepts a PDF upload today (L4 adds the
//!     `415 {"error":"images_only"}` gate — QL7: Local attachments are images
//!     only).
//!
//! **No `LocalCore` dependency:** the settings and uploads routers are driven
//! directly on loopback (the `settings_api_tests` / `uploads_http_tests`
//! pattern), so this target compiles and runs in BOTH editions —
//! `cargo test --locked` (host ⇒ `edition:"full"`) and
//! `cargo test --locked --no-default-features` (local ⇒ `edition:"local"`).
//! That is what makes it a true edition table, not a compile-fail.

use axum::Router;
// `png_bytes` (the Local upload fixture) is the only use — gate the import
// with the same cfg so the host build has no unused import.
#[cfg(not(feature = "host"))]
use base64::Engine;
use peakbot::SkillRegistry;
use peakbot::attachments::UploadStore;
use peakbot::config::UploadsConfig;
use peakbot::config::resolve::{Config, ParseCtx, Source};
use peakbot::session::SessionDeps;
use peakbot::ui::web::registry::SessionRegistry;
use peakbot::ui::web::settings::{BootWeb, SettingsState, TokenSource, router as settings_router};
use peakbot::ui::web::uploads::router as uploads_router;
use std::net::SocketAddr;
use std::sync::Arc;
use uuid::Uuid;

/// The edition this build is: `full` with `host`, `local` without.
const EDITION: &str = if cfg!(feature = "host") {
    "full"
} else {
    "local"
};

/// Valid 0.24 global with an offline Ollama provider (never contacted).
const GLOBAL_024: &str = "providers:\n  - name: local\n    type: ollama\n    models:\n      - { name: llama3, alias: local }\nprofiles:\n  dev:\n    agents:\n      main:\n        model: local\n";

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn parse_global(text: &str) -> Config {
    Config::parse(
        Source {
            path: std::path::Path::new("/global/config.yaml"),
            text,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .expect("fixture global must parse")
}

fn deps_with_config(cfg: Config) -> Arc<SessionDeps> {
    Arc::new(SessionDeps {
        config: Arc::new(cfg),
        skills: SkillRegistry::new(),
        mcp_handles: Arc::new(Vec::new()),
        boot_notices: std::sync::Mutex::new(Vec::new()),
        shell_kind: None,
        storage: None,
        profile_pin: None,
        uploads: UploadStore::new(UploadStore::default_root(), UploadsConfig::default()),
    })
}

fn state(deps: Option<Arc<SessionDeps>>) -> SettingsState {
    let config_dir = tempfile::tempdir().expect("config tempdir");
    let boot_cwd = tempfile::tempdir().expect("cwd tempdir");
    // Keep the tempdirs alive for the server's lifetime via leaked guard.
    std::mem::forget(config_dir);
    std::mem::forget(boot_cwd);
    SettingsState {
        deps: deps.clone(),
        registry: deps.map(SessionRegistry::new),
        config_dir: std::env::temp_dir().join(format!("pb-edition-cfg-{}", Uuid::new_v4())),
        boot_cwd: std::env::temp_dir().join(format!("pb-edition-cwd-{}", Uuid::new_v4())),
        migrate_error: None,
        boot_web: BootWeb {
            bind: "127.0.0.1:7823".parse().expect("bind"),
            token_source: TokenSource::Config,
            tls: false,
            token: None,
            extra_sans: Vec::new(),
            tls_dir: None,
        },
    }
}

/// Mount the settings router on a random loopback port (no token layer —
/// `boot.edition` is read from the body, not gated on auth).
async fn spawn_settings(s: SettingsState) -> SocketAddr {
    let app: Router = settings_router(s);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    addr
}

async fn get_settings(addr: SocketAddr) -> serde_json::Value {
    let resp = client()
        .get(format!("http://{addr}/api/settings"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "GET /api/settings must be 200");
    resp.json().await.unwrap()
}

// ═══ boot.edition ════════════════════════════════════════════════════════

/// needs_setup mode (deps None): `boot.edition` is the build's edition.
#[tokio::test]
async fn settings_reports_the_edition_in_needs_setup_mode() {
    let addr = spawn_settings(state(None)).await;
    let v = get_settings(addr).await;
    assert_eq!(v["needs_setup"], true, "deps None ⇒ needs_setup; got: {v}");
    assert_eq!(
        v["boot"]["edition"], EDITION,
        "boot.edition must be {EDITION:?} in needs_setup mode; boot = {}",
        v["boot"]
    );
}

/// Normal mode (deps present): `boot.edition` is the build's edition.
#[tokio::test]
async fn settings_reports_the_edition_in_normal_mode() {
    let deps = deps_with_config(parse_global(GLOBAL_024));
    let addr = spawn_settings(state(Some(deps))).await;
    let v = get_settings(addr).await;
    assert_eq!(
        v["needs_setup"], false,
        "deps present ⇒ not needs_setup; got: {v}"
    );
    assert_eq!(
        v["boot"]["edition"], EDITION,
        "boot.edition must be {EDITION:?} in normal mode; boot = {}",
        v["boot"]
    );
}

/// Cross-side pin (L7): the web suite
/// (`web/src/settings/editionGating.test.tsx`) feeds `getSettings()` the
/// `boot` block of the shared fixture `fixtures/settings_boot_local.json`.
/// The live response's `boot` key set must equal the fixture's, so a server
/// that moves/renames a boot key (e.g. `edition` to top-level) breaks both
/// sides at once — the L7 bug where the web read a top-level `edition` the
/// server never sent.
#[tokio::test]
async fn boot_block_shape_matches_the_shared_fixture() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/settings_boot_local.json"))
            .expect("the shared fixture must be valid JSON");
    // The fixture itself is the locked shape: `edition` in the `boot`
    // block, never top-level.
    assert!(
        fixture.get("edition").is_none(),
        "the fixture must not carry a top-level edition: {fixture}"
    );
    assert_eq!(fixture["boot"]["edition"], "local", "fixture: {fixture}");

    let addr = spawn_settings(state(None)).await;
    let v = get_settings(addr).await;
    let mut live: Vec<&str> = v["boot"]
        .as_object()
        .expect("the live boot block must be an object")
        .keys()
        .map(|k| k.as_str())
        .collect();
    let mut fixture_keys: Vec<&str> = fixture["boot"]
        .as_object()
        .expect("the fixture boot block must be an object")
        .keys()
        .map(|k| k.as_str())
        .collect();
    live.sort_unstable();
    fixture_keys.sort_unstable();
    assert_eq!(
        live, fixture_keys,
        "the live boot block's keys must match the shared fixture — the web \
         suite reads its shape from there; live boot = {}",
        v["boot"]
    );
}

// ═══ Local upload: images only (QL7) ═════════════════════════════════════

#[cfg(not(feature = "host"))]
fn png_bytes() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")
        .expect("PNG fixture base64 must decode")
}

async fn spawn_uploads() -> SocketAddr {
    let dir = tempfile::tempdir().expect("uploads tempdir");
    let store = UploadStore::new(dir.path().to_path_buf(), UploadsConfig::default());
    std::mem::forget(dir);
    let app: Router = uploads_router(store);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    addr
}

async fn post_upload(addr: SocketAddr, name: &str, body: Vec<u8>) -> reqwest::Response {
    client()
        .post(format!("http://{addr}/api/uploads"))
        .query(&[
            ("convo", Uuid::new_v4().to_string()),
            ("name", name.to_string()),
        ])
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(body)
        .send()
        .await
        .unwrap()
}

/// Local edition: a PDF (non-image) upload is a `415` with the EXACT body
/// `{"error":"images_only"}` (QL7).
#[cfg(not(feature = "host"))]
#[tokio::test]
async fn local_pdf_upload_is_415_images_only() {
    let addr = spawn_uploads().await;
    let resp = post_upload(addr, "spec.pdf", vec![b'%'; 1024]).await;
    assert_eq!(
        resp.status(),
        415,
        "a PDF upload in the Local edition must be a 415"
    );
    // The exact body — no prose, no extra keys (the SPA matches it verbatim).
    let text = resp.text().await.unwrap();
    assert_eq!(
        text, r#"{"error":"images_only"}"#,
        "the 415 body must be exactly {{\"error\":\"images_only\"}}; got: {text}"
    );
}

/// Local edition: an image upload still succeeds (200).
#[cfg(not(feature = "host"))]
#[tokio::test]
async fn local_image_upload_is_ok() {
    let addr = spawn_uploads().await;
    let resp = post_upload(addr, "cat.png", png_bytes()).await;
    assert_eq!(
        resp.status(),
        200,
        "an image upload in the Local edition must succeed; body: {}",
        resp.text().await.unwrap()
    );
}

/// Full edition: a PDF upload is allowed (200) — the images-only gate is
/// Local-only. This row is a guard that the L4 gate does not leak into host.
#[cfg(feature = "host")]
#[tokio::test]
async fn full_edition_pdf_upload_is_ok() {
    let addr = spawn_uploads().await;
    let resp = post_upload(addr, "spec.pdf", vec![b'%'; 1024]).await;
    assert_eq!(
        resp.status(),
        200,
        "the full edition accepts any file; a PDF must not be a 415"
    );
}
