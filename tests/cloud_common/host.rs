//! Host-core helpers shared by the PR-C tests (host edition only): boot a
//! full `WebUi` behind axum with the CloudServe inbound hook, and enroll a
//! two-device account over the fake relay.

use std::net::SocketAddr;
use std::path::Path;

use peakbot::boot::{self, WebBoot, WebSession};
use peakbot::cloud::{CloudState, InboundHook};
use peakbot::config::{Config, ParseCtx, Source};
use peakbot::skills::SkillRegistry;
use peakbot::ui::WebUi;
use peakbot::ui::web::settings::TokenSource;

use super::devices::id_a;
use super::fake_relay::FakeRelay;
use super::rig::{self, Dev, spawn_device};

/// The fixture config: two LOCAL profiles (`dev`, `alt`) and one `remote:`
/// profile (`home-dev`) — the chaining target §E.3 must never expose.
pub const YAML: &str = r#"
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
  alt:
    agents:
      a1:
        model: local
      a2:
        model: local
  home-dev:
    remote:
      url: https://10.2.0.48:7823
      token: pito
      ca: system
      profile: dev
"#;

/// The same fixture with a 1 MB upload cap (the §E.4 oversize case).
pub fn yaml_small_uploads() -> String {
    YAML.replacen(
        "general:\n",
        "general:\n  uploads:\n    max_file_mb: 1\n",
        1,
    )
}

pub struct Core {
    pub addr: SocketAddr,
    /// Keeps the WebUi (and its registry/deps) alive for the router.
    _ui: WebUi,
    #[allow(dead_code)]
    _dir: tempfile::TempDir,
}

/// Boot the host web core the way `main` does (`boot::web_boot`), serve its
/// router on a random loopback port, and return the CloudServe inbound hook
/// (host-only seam, §E) for wiring into the core's CloudLink.
pub async fn boot_core(yaml: &str) -> (Core, Option<InboundHook>) {
    let config = Config::parse(
        Source {
            path: Path::new("/global/config.yaml"),
            text: yaml,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .expect("fixture config must parse");
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
    let ui = boot::web_boot(web, WebSession::Ready(deps)).expect("web_boot");
    let hook = ui.cloud_inbound();
    let router = ui.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });
    (
        Core {
            addr,
            _ui: ui,
            _dir: dir,
        },
        hook,
    )
}

// ── the two-device chain over the fake relay ──────────────────────────────

pub async fn login(dev: &Dev, fake: &FakeRelay) {
    let fk = fake.clone();
    dev.handle
        .login(move |url| {
            tokio::spawn(async move { rig::drive_login(&fk, url).await });
        })
        .await
        .expect("login");
    let _ = rig::wait_state(dev.handle.state(), |s| {
        matches!(s, CloudState::Pending { .. } | CloudState::Active { .. })
    })
    .await;
}

/// A (host, CloudServe wired) and B (controller) on one fake relay:
/// A genesis, B enrolls, A approves — both Active.
pub async fn enroll_pair(fake: &FakeRelay, host_inbound: Option<InboundHook>) -> (Dev, Dev) {
    let a = spawn_device(1, "Laptop", fake, host_inbound);
    let b = spawn_device(2, "Desktop", fake, None);
    login(&a, fake).await;
    a.handle.genesis().await.expect("genesis");
    login(&b, fake).await;
    let st = rig::wait_state(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { requests, .. } if !requests.is_empty()),
    )
    .await;
    let letter = match st {
        CloudState::Active { requests, .. } => requests[0].letter_id,
        _ => panic!("A must see B's enroll request, got {st:?}"),
    };
    a.handle.approve(letter).await.expect("approve");
    let _ = rig::wait_state(b.handle.state(), |s| {
        matches!(s, CloudState::Active { devices, .. }
                if devices.iter().any(|d| d.id == id_a()))
    })
    .await;
    (a, b)
}
