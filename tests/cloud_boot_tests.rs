//! P3 PR-C RED tests — boot wiring: with `general.cloud` configured,
//! CloudLink is spawned at boot and `/api/cloud/state` no longer answers
//! `cloud_disabled`; without it, the state stays disabled. Both editions
//! wire the spawn (Local: `inbound = None`).
//!
//! RED today: `boot::boot_cloud` is a `todo!()` skeleton — the configured
//! cases fail on the panic (feature absent); the unconfigured case already
//! pins the existing `cloud_disabled` behaviour and stays green.

mod cloud_common;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use peakbot::boot::{self, WebBoot, WebSession};
use peakbot::cloud::CloudStateKind;
use peakbot::cloud::store::MemSeedStore;
use peakbot::config::{Config, ParseCtx, Source};
use peakbot::skills::SkillRegistry;
use peakbot::ui::web::settings::TokenSource;

use cloud_common::fake_relay::FakeRelay;

const NO_CLOUD_YAML: &str = r#"
providers:
  - name: p
    type: mock
    models:
      - { name: llama3, alias: local }
general:
  context:
    enabled: false
profiles:
  dev:
    agents:
      team:
        model: local
"#;

/// `general.cloud` pointing at the fake relay (loopback plain is a legal
/// endpoint, RP3).
fn with_cloud_yaml(relay_url: &str) -> String {
    format!(
        r#"
{NO_CLOUD_YAML}
  cloud:
    relay: {relay_url}
"#
    )
}

fn parse(yaml: &str) -> Config {
    Config::parse(
        Source {
            path: Path::new("/global/config.yaml"),
            text: yaml,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .expect("fixture config must parse")
}

async fn boot_ui(config: Config, cloud: Option<peakbot::cloud::CloudHandle>) -> SocketAddr {
    let deps = boot::session_deps(
        config,
        SkillRegistry::new(),
        Vec::new(),
        Vec::new(),
        None,
        None,
    );
    let dir = tempfile::tempdir().expect("boot dir");
    let web = WebBoot {
        addr: "127.0.0.1:0".parse().expect("addr"),
        token: None,
        token_source: TokenSource::None,
        tls: false,
        tls_names: Vec::new(),
        config_dir: dir.path().to_path_buf(),
        boot_cwd: dir.path().to_path_buf(),
    };
    let ui = boot::web_boot(web, WebSession::Ready(deps))
        .expect("web_boot")
        .with_cloud(cloud);
    let router = ui.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });
    // Keep the WebUi (and the tempdir) alive for the test's duration.
    std::mem::forget(ui);
    std::mem::forget(dir);
    addr
}

/// `general.cloud` configured ⇒ boot wiring spawns the CloudLink: the
/// handle is live and its state leaves `Disabled` (it goes connecting to
/// the relay).
#[tokio::test]
async fn boot_cloud_spawns_when_general_cloud_is_configured() {
    let fake = FakeRelay::spawn().await;
    let config = parse(&with_cloud_yaml(&fake.url()));
    let dir = tempfile::tempdir().expect("state dir");
    let store: Arc<dyn peakbot::cloud::SeedStore> = Arc::new(MemSeedStore::new());

    let handle =
        peakbot::boot::boot_cloud(&config, dir.path().to_path_buf(), "Laptop", store, None)
            .expect("general.cloud present ⇒ the CloudLink is spawned at boot");

    let mut rx = handle.state();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if rx.borrow().kind() != CloudStateKind::Disabled {
                return;
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
                _ = rx.changed() => {}
            }
        }
    })
    .await
    .expect("the state leaves Disabled within 10 s");
    assert_ne!(
        rx.borrow().kind(),
        CloudStateKind::Disabled,
        "a configured CloudLink must not stay Disabled"
    );
}

/// No `general.cloud` ⇒ no actor: boot wiring returns `None`.
#[tokio::test]
async fn boot_cloud_is_none_without_general_cloud() {
    let config = parse(NO_CLOUD_YAML);
    let dir = tempfile::tempdir().expect("state dir");
    let store: Arc<dyn peakbot::cloud::SeedStore> = Arc::new(MemSeedStore::new());

    let handle =
        peakbot::boot::boot_cloud(&config, dir.path().to_path_buf(), "Laptop", store, None);
    assert!(
        handle.is_none(),
        "without general.cloud the CloudLink stays off"
    );
}

/// The web surface: with a boot-spawned handle, `GET /api/cloud/state`
/// answers the live state — no longer `cloud_disabled`.
#[tokio::test]
async fn api_cloud_state_no_longer_answers_cloud_disabled() {
    let fake = FakeRelay::spawn().await;
    let config = parse(&with_cloud_yaml(&fake.url()));
    let dir = tempfile::tempdir().expect("state dir");
    let store: Arc<dyn peakbot::cloud::SeedStore> = Arc::new(MemSeedStore::new());

    let handle =
        peakbot::boot::boot_cloud(&config, dir.path().to_path_buf(), "Laptop", store, None)
            .expect("configured ⇒ spawned");
    let addr = boot_ui(config, Some(handle)).await;

    let resp = reqwest::get(format!("http://{addr}/api/cloud/state"))
        .await
        .expect("GET /api/cloud/state");
    assert_eq!(resp.status(), 200, "a live cloud state is a 200");
    let body: serde_json::Value = resp.json().await.expect("JSON body");
    assert_ne!(
        body["state"].as_str(),
        Some("disabled"),
        "with the handle the state must not be cloud_disabled: {body}"
    );
}

/// Without the config the API stays disabled (pins the PR-B behaviour —
/// this one is green today and must stay green).
#[tokio::test]
async fn api_cloud_state_stays_disabled_without_the_config() {
    let config = parse(NO_CLOUD_YAML);
    let addr = boot_ui(config, None).await;

    let resp = reqwest::get(format!("http://{addr}/api/cloud/state"))
        .await
        .expect("GET /api/cloud/state");
    assert_eq!(resp.status(), 404, "unconfigured ⇒ 404");
    let body: serde_json::Value = resp.json().await.expect("JSON body");
    assert_eq!(
        body["error"].as_str(),
        Some("cloud_disabled"),
        "unconfigured ⇒ cloud_disabled: {body}"
    );
}
