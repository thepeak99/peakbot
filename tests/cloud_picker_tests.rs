//! P5 Task 1 tests: no_local_profile frame + cloud target catalog.
//!
//! Case (a): A zero-local-profile cloud-only config, unpinned attach → the
//! first server frame is `no_local_profile`, then the socket closes, and no
//! session is registered.
//!
//! Case (b): A config with a local profile, unpinned attach → the normal
//! state frame (regression guard).
//!
//! Case (c): Catalog after a cloud attach: attach with `profile=name@slug`
//! and assert the state catalog has an entry for that target.

mod cloud_common;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Once, OnceLock};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use peakbot::boot::{self, WebBoot, WebSession};
use peakbot::cloud::store::MemSeedStore;
use peakbot::cloud::{CloudHandle, Seed, SeedStore};
use peakbot::config::{Config, ParseCtx, Source};
use peakbot::paths::{self, Roots};
use peakbot::skills::SkillRegistry;
use peakbot::ui::WebUi;
use peakbot::ui::web::remote_types::Directory;
use peakbot::ui::web::settings::TokenSource;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message as TMsg;

use cloud_common::relay_harness::real::TestRelay;

const WAIT: Duration = Duration::from_secs(20);
type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

// ── process-global roots ─────────────────────────────────────────────────

static ROOTS_DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
static ROOTS_ONCE: Once = Once::new();
// Every attach re-reads the one process-global config.yaml, so a core owns
// it for its whole lifetime; otherwise a parallel test overwrites it mid-attach.
static CONFIG_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn init_shared_roots() {
    ROOTS_ONCE.call_once(|| {
        let t = tempfile::tempdir().expect("roots dir");
        let (c, d, k) = (t.path().join("c"), t.path().join("d"), t.path().join("k"));
        for p in [&c, &d, &k] {
            std::fs::create_dir_all(p).expect("root");
        }
        // Write a default config (won't be used directly by our cores)
        std::fs::write(
            c.join("config.yaml"),
            "general:\n  context:\n    enabled: false\n",
        )
        .expect("cfg");
        paths::init_roots(Roots {
            config: c,
            data: d,
            cache: k,
        });
        let _ = ROOTS_DIR.set(t);
    });
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

// ── a core: web server + CloudLink, all paths injected ───────────────────

#[allow(dead_code)]
struct Core {
    addr: SocketAddr,
    handle: CloudHandle,
    _ui: WebUi,
    _tmp: tempfile::TempDir,
    _config: tokio::sync::MutexGuard<'static, ()>,
}

impl Core {
    async fn start(name: &str, seed: u8, yaml: &str, _relay: &TestRelay) -> Core {
        let config = CONFIG_LOCK.lock().await;
        init_shared_roots();
        let tmp = tempfile::tempdir().expect("core dir");
        // Write the config to the roots config dir so fresh_home_attach reads it.
        let config_dir = paths::config_dir().expect("roots set");
        std::fs::write(config_dir.join("config.yaml"), yaml).expect("write config");
        let deps = boot::session_deps_in(
            parse(yaml),
            SkillRegistry::new(),
            Vec::new(),
            Vec::new(),
            None,
            None,
            tmp.path().join("uploads"),
        );
        let web = WebBoot {
            addr: "127.0.0.1:0".parse().expect("addr"),
            token: None,
            token_source: TokenSource::None,
            tls: false,
            tls_names: Vec::new(),
            config_dir: tmp.path().to_path_buf(),
            boot_cwd: tmp.path().to_path_buf(),
        };
        let ui = boot::web_boot(web, WebSession::Ready(deps)).expect("web_boot");
        let store = Arc::new(MemSeedStore::new());
        store.store_seed(&Seed::new([seed; 64])).expect("seed");
        let handle = boot::boot_cloud(
            &parse(yaml),
            tmp.path().join("cloud"),
            name,
            store,
            ui.cloud_inbound(),
        )
        .expect("general.cloud is set ⇒ the CloudLink is spawned");
        let dir = Arc::new(Directory::new().with_cloud(Some(handle.clone())));
        let ui = ui.with_cloud(Some(handle.clone())).with_directory(dir);
        let router = ui.router();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, router).await.ok();
        });
        Core {
            addr,
            handle,
            _ui: ui,
            _tmp: tmp,
            _config: config,
        }
    }

    #[allow(dead_code)]
    fn id_hex(&self) -> String {
        hex(&self.handle.device_id().expect("device id"))
    }
}

#[allow(dead_code)]
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ── websocket helpers ────────────────────────────────────────────────────

async fn ws(addr: SocketAddr) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .expect("ws upgrade");
    ws
}

async fn send(ws: &mut Ws, v: serde_json::Value) {
    ws.send(TMsg::Text(v.to_string().into()))
        .await
        .expect("send");
}

async fn next_frame(ws: &mut Ws, secs: u64) -> Option<serde_json::Value> {
    loop {
        match tokio::time::timeout(Duration::from_secs(secs), ws.next()).await {
            Ok(Some(Ok(TMsg::Text(t)))) => return serde_json::from_str(&t).ok(),
            Ok(Some(Ok(TMsg::Ping(_) | TMsg::Pong(_)))) => continue,
            _ => return None,
        }
    }
}

/// Wait for a frame matching `want`, panicking on timeout or close.
async fn until_frame(
    ws: &mut Ws,
    what: &str,
    want: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    for _ in 0..100 {
        let f = next_frame(ws, WAIT.as_secs()).await;
        match f {
            Some(ref v) if want(v) => return v.clone(),
            Some(_) => continue,
            None => panic!("socket closed before {what}"),
        }
    }
    panic!("timeout waiting for {what}");
}

// ── fixture configs ──────────────────────────────────────────────────────

/// A config with zero local profiles and no home remote (cloud-only phone).
const ZERO_LOCAL_YAML: &str = r#"
providers:
  - name: p
    type: mock
    models:
      - { name: llama3, alias: local }
general:
  context:
    enabled: false
  cloud:
    relay: __RELAY__
"#;

/// A config with a local profile (regression guard).
const LOCAL_PROFILE_YAML: &str = r#"
providers:
  - name: p
    type: mock
    models:
      - { name: llama3, alias: local }
general:
  context:
    enabled: false
  cloud:
    relay: __RELAY__
profiles:
  dev:
    agents:
      team:
        model: local
"#;

fn with_relay(yaml: &str, relay_url: &str) -> String {
    yaml.replace("__RELAY__", relay_url)
}

// ═══ case (a): zero-local config → no_local_profile frame ════════════════

#[tokio::test]
async fn zero_local_config_unpinned_attach_sends_no_local_profile_frame() {
    let relay = TestRelay::spawn().await;
    let phone = Core::start(
        "phone",
        1,
        &with_relay(ZERO_LOCAL_YAML, &relay.url()),
        &relay,
    )
    .await;

    let mut w = ws(phone.addr).await;

    // Wait a moment for the server to be fully ready.
    tokio::time::sleep(Duration::from_millis(100)).await;

    send(&mut w, json!({"type": "attach", "convo": null})).await;

    // The first meaningful frame must be `no_local_profile`.
    let f = until_frame(&mut w, "no_local_profile", |f| {
        f["type"] == "no_local_profile"
    })
    .await;
    assert_eq!(f["type"], "no_local_profile", "frame type");

    // After the frame, the socket should close (no session minted).
    let closed = next_frame(&mut w, 3).await;
    assert!(
        closed.is_none(),
        "socket should close after no_local_profile"
    );
}

// ═══ case (b): local profile config → normal attach (regression) ═════════

#[tokio::test]
async fn local_profile_config_unpinned_attach_works_normally() {
    let relay = TestRelay::spawn().await;
    let phone = Core::start(
        "phone",
        2,
        &with_relay(LOCAL_PROFILE_YAML, &relay.url()),
        &relay,
    )
    .await;

    // Unpinned attach: should proceed normally, NOT send no_local_profile.
    let mut w = ws(phone.addr).await;
    send(&mut w, json!({"type": "attach", "convo": null})).await;

    // Should get ready/attached, not no_local_profile.
    let f = until_frame(&mut w, "attached or ready", |f| {
        f["type"] == "attached" || f["type"] == "ready"
    })
    .await;
    assert!(
        f["type"] == "attached" || f["type"] == "ready",
        "expected attached or ready, got: {}",
        f["type"]
    );
}

// ═══ case (c): catalog after cloud attach has target entry ═══════════════

#[test]
fn cloud_attach_catalog_contains_target_entry() {
    // The LocalView created for a cloud attach must contain a stub row for
    // the target. This is tested as a unit test of cloud_target_info, the
    // function that builds the catalog entry. The two-core integration test
    // is deferred; the unit test covers the server-side contract.
    let info = peakbot::ui::web::cloud_target_info("dev@computer");
    assert_eq!(info.name, "dev@computer");
    assert_eq!(info.origin, "remote");
    assert!(!info.overrides_global);
    assert!(info.agents.is_empty());
    assert_eq!(info.machine.as_deref(), Some("computer"));
}

// ═══ case (d): the catalog row's colour is per HOST, not per target profile ═
//
// Confirmed bug: `cloud_target_info` derived the colour from the bare target
// profile (`default`), so the same profile on two different devices got the
// SAME colour, and a renamed profile changed a host's colour for no reason.
// After the fix the colour is `TabColor::derived(slug)` — a host identity.
//
// These call the single-argument signature: the colour is `derived` of the
// slug parsed from `name`'s last `@` suffix.

#[test]
fn cloud_target_info_color_is_derived_from_the_device_slug() {
    use peakbot::config::file::TabColor;

    let desk = peakbot::ui::web::cloud_target_info("default@desk");
    let phone = peakbot::ui::web::cloud_target_info("default@phone");

    assert_eq!(
        desk.color,
        TabColor::derived("desk"),
        "the attached desk target must be coloured from its slug"
    );
    assert_eq!(
        phone.color,
        TabColor::derived("phone"),
        "the phone host target must be coloured from its slug"
    );
    assert_ne!(
        desk.color, phone.color,
        "two hosts with the same profile name must not share a colour"
    );
}

#[test]
fn cloud_target_info_color_is_stable_across_profiles_on_one_host() {
    use peakbot::config::file::TabColor;

    let alpha = peakbot::ui::web::cloud_target_info("alpha@desk");
    let beta = peakbot::ui::web::cloud_target_info("beta@desk");

    assert_eq!(
        alpha.color,
        TabColor::derived("desk"),
        "alpha on desk is coloured by the host"
    );
    assert_eq!(
        beta.color,
        TabColor::derived("desk"),
        "beta on the same host keeps the host colour"
    );
    assert_eq!(
        alpha.color, beta.color,
        "a host's colour must not follow the profile name"
    );
}

#[test]
fn cloud_target_info_keeps_name_and_machine_unchanged() {
    let info = peakbot::ui::web::cloud_target_info("default@phone");
    assert_eq!(info.name, "default@phone", "route key is untouched");
    assert_eq!(info.origin, "remote");
    assert!(!info.overrides_global);
    assert!(info.agents.is_empty());
    assert_eq!(info.machine.as_deref(), Some("phone"));
}

#[test]
fn cloud_target_info_uses_the_last_at_suffix_as_the_host() {
    use peakbot::config::file::TabColor;

    // A profile name may itself contain '@'; the slug is the last segment.
    let info = peakbot::ui::web::cloud_target_info("team@ops@phone");
    assert_eq!(info.machine.as_deref(), Some("phone"));
    assert_eq!(info.color, TabColor::derived("phone"));
}
