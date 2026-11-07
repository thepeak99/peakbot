//! L3 contract (mobile plan §2.11 / §4.3 test 1) — RED.
//!
//! **RED today by COMPILE ERROR:** `peakbot::embed` does not exist yet. L3
//! ("boot extraction — the key remaining engineering item") moves the full
//! web boot sequence out of `main.rs` (storage, uploads, `SessionDeps`,
//! models snapshot) into ONE lib fn — **`peakbot::boot::web_boot`** — that
//! both `main` and `embed` call, and exposes:
//!
//! ```ignore
//! pub mod boot {
//!     /// The ONE web boot sequence: storage, uploads, `SessionDeps`,
//!     /// models snapshot. `main` (web mode) and `embed::LocalCore::boot`
//!     /// both call exactly this — never a copy of it.
//!     pub async fn web_boot(/* token, roots-scoped deps … */) -> /* WebUi */;
//! }
//! pub mod embed {
//!     pub struct LocalCore;   // opaque; owns sessions, storage, SessionDeps
//!     impl LocalCore {
//!         /// Full chat-session boot from `<config root>/config.yaml`
//!         /// (RepoMode::Ignore, no skills, no MCP). Missing/invalid config
//!         /// ⇒ a needs_setup core (never an error). Err only on IO.
//!         /// MUST call `peakbot::boot::web_boot`.
//!         pub async fn boot(token: String) -> anyhow::Result<LocalCore>;
//!         /// Serve the full router on `listener` until dropped; callable
//!         /// again (iOS re-serve).
//!         pub async fn serve(&self, listener: tokio::net::TcpListener) -> std::io::Result<()>;
//!     }
//! }
//! ```
//!
//! This is the S15/S16 1006 regression lock: in the spike only the
//! *needs_setup* server booted, so a chat `/ws` closed `1006`. L3 boots a
//! **full chat session**, so a user message over `/ws` completes with the
//! provider's reply.
//!
//! **Edition:** Local only — `#![cfg(not(feature = "host"))]` keeps the
//! default (host) build green (the self dev-dependency keeps `mock` on for
//! every test build, so the cfg, not `required-features`, does the gating
//! there). Run:
//! `cargo test --locked --no-default-features --features mock --test local_core`
//!
//! **The mock provider seam (builder must add, documented here):** there is
//! no config-addressable mock today (`MockCompletionModel` is test-only,
//! behind `feature = "mock"`, scripted FIFO). The builder adds
//! `type: mock` — a `ProviderType::Mock` variant compiled ONLY under
//! `feature = "mock"` — that `create_provider` maps to an agent backed by
//! `MockCompletionModel` (or an equivalent fixed-text model). The mock
//! answers EVERY prompt with fixed text containing `MOCK_REPLY` (below), so
//! the `/ws` turn is deterministic and needs no API key.
//!
//! **Process-global OnceLock:** `paths::init_roots` is a process-global
//! OnceLock, so the boot scenarios run sequentially in ONE `#[tokio::test]`
//! (each scenario re-boots against the same config root, writing the config
//! file between them). The source-scan test is separate and never touches
//! roots.

#![cfg(not(feature = "host"))]

use futures::{SinkExt, StreamExt};
use peakbot::embed::LocalCore;
use peakbot::paths::{self, Roots};
use serde_json::json;
use std::net::SocketAddr;
use tokio_tungstenite::tungstenite::Message as TMsg;

/// The config the builder's `mock` provider type must parse: `type: mock`,
/// one model aliased `mock`, one profile.
const MOCK_CONFIG: &str = r#"
providers:
  - name: mock
    type: mock
    models:
      - { name: mock-model, alias: mock }
profiles:
  local:
    agents:
      main:
        model: mock
"#;

const TOKEN: &str = "l3-contract-token";

/// The mock provider's fixed reply marker. The builder's config-addressable
/// mock (compiled only under `feature = "mock"`) must answer every prompt
/// with text containing this marker.
const MOCK_REPLY: &str = "MOCK_REPLY";

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// `GET /api/settings?token=…` → the parsed JSON body.
async fn get_settings(addr: SocketAddr) -> serde_json::Value {
    let resp = client()
        .get(format!("http://{addr}/api/settings?token={TOKEN}"))
        .send()
        .await
        .expect("GET /api/settings");
    assert_eq!(
        resp.status(),
        200,
        "GET /api/settings with the boot token must be 200"
    );
    resp.json().await.expect("settings is JSON")
}

async fn ws_connect(addr: SocketAddr) -> Ws {
    // The `?token=` query establishes the auth cookie for the /ws upgrade.
    // A 1006 close here is the S15/S16 regression (only needs_setup boots).
    let (ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/ws?token={TOKEN}"
    ))
    .await
    .expect("ws upgrade must succeed (S15/S16: a full chat session is booted, not only needs_setup)");
    ws
}

/// Next JSON text frame within `secs`; `Err(true)` = closed, `Err(false)` =
/// timeout.
async fn next_frame(ws: &mut Ws, secs: u64) -> Result<serde_json::Value, bool> {
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(secs), ws.next()).await {
            Err(_) => return Err(false),
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(TMsg::Close(_)))) => return Err(true),
            Ok(Some(Ok(TMsg::Text(t)))) => {
                return Ok(serde_json::from_str(&t).expect("server frames are JSON"));
            }
            Ok(Some(Ok(_))) => continue, // ping / pong / binary
        }
    }
}

async fn send_text(ws: &mut Ws, v: serde_json::Value) {
    ws.send(TMsg::Text(v.to_string().into()))
        .await
        .expect("ws send");
}

/// A full chat turn over `/ws`: attach → ready → send a user message →
/// receive the mock's reply in a `state` frame. Panics (the 1006 regression)
/// if the socket closes before the reply.
async fn full_chat_turn(addr: SocketAddr) {
    let mut ws = ws_connect(addr).await;

    send_text(&mut ws, json!({ "type": "attach", "convo": null })).await;

    // Wait for `ready` (the live handshake).
    let mut saw_ready = false;
    for _ in 0..200 {
        match next_frame(&mut ws, 15).await {
            Ok(f) if f["type"] == "ready" => {
                saw_ready = true;
                break;
            }
            Ok(f) if f["type"] == "error" => panic!("error frame before ready: {f}"),
            Ok(_) => continue,
            Err(closed) => panic!(
                "socket {} before ready — the S15/S16 1006 regression",
                if closed { "closed" } else { "timed out" }
            ),
        }
    }
    assert!(saw_ready, "a `ready` frame must arrive on a full chat boot");

    // Send the user message and wait for the mock's reply in a state frame.
    send_text(&mut ws, json!({ "type": "send_message", "text": "hi" })).await;
    for _ in 0..300 {
        match next_frame(&mut ws, 30).await {
            Ok(f) if f["type"] == "state" => {
                if let Some(msgs) = f["state"]["chat"]["messages"].as_array() {
                    for m in msgs {
                        if m["role"] == "agent"
                            && m["content"]
                                .as_str()
                                .is_some_and(|c| c.contains(MOCK_REPLY))
                        {
                            return; // full turn completed
                        }
                    }
                }
            }
            Ok(f) if f["type"] == "error" => panic!("error frame mid-turn: {f}"),
            Ok(_) => continue,
            Err(closed) => panic!(
                "socket {} before the mock reply — the S15/S16 1006 regression",
                if closed { "closed" } else { "timed out" }
            ),
        }
    }
    panic!("the mock's reply ({MOCK_REPLY}) never reached a state frame");
}

/// One full chat turn is a live session on the registry — the needs_setup
/// server (deps None) would 503 `/ws` and never get this far.
#[tokio::test]
async fn local_core_contract() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_root = tmp.path().join("config");
    std::fs::create_dir_all(&config_root).expect("config root");
    // One data root per the §2.6 layout: config/data/cache under the tempdir.
    paths::init_roots(Roots {
        config: config_root.clone(),
        data: tmp.path().join("data"),
        cache: tmp.path().join("cache"),
    });

    // ── Scenario A: empty config root ⇒ needs_setup server ────────────────
    // No config.yaml yet: boot must succeed (never an error) and serve the
    // settings surface reporting `needs_setup: true`.
    let core = LocalCore::boot(TOKEN.to_string())
        .await
        .expect("boot on an empty config root");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let task_a = tokio::spawn(core.serve(listener));

    let settings = get_settings(addr).await;
    assert_eq!(
        settings["needs_setup"], true,
        "an empty config root must boot a needs_setup server; got: {settings}"
    );

    // ── Scenario B: a valid mock config ⇒ a full chat turn over /ws ───────
    // Write the mock-provider config, re-boot, and drive a complete chat turn
    // (the S15/S16 regression: this used to 1006).
    std::fs::write(config_root.join("config.yaml"), MOCK_CONFIG)
        .expect("write mock config");
    let core = LocalCore::boot(TOKEN.to_string())
        .await
        .expect("boot with the mock config");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    task_a.abort(); // drop the first serve before the second
    let task_b = tokio::spawn(core.serve(listener));

    let settings = get_settings(addr).await;
    assert_eq!(
        settings["needs_setup"], false,
        "a valid mock config must boot a full (not needs_setup) server; got: {settings}"
    );
    full_chat_turn(addr).await;

    // ── Scenario C: serve twice on two listeners from ONE core ────────────
    // The iOS re-serve: drop the first serve future, then serve the same core
    // on a new listener — the second serve must answer.
    task_b.abort();
    let l2 = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind #2");
    let addr2 = l2.local_addr().expect("addr #2");
    let task_c = tokio::spawn(core.serve(l2));
    let settings = get_settings(addr2).await;
    assert_eq!(
        settings["needs_setup"], false,
        "the re-serve on a new listener must serve the same (full) core; got: {settings}"
    );
    task_c.abort();
}

/// `main` and `embed` share ONE boot fn (plan §2.11): the shared symbol is
/// **`peakbot::boot::web_boot`**. The CLI's web-mode boot calls it instead of
/// building the web UI inline, and `embed::LocalCore::boot` calls it too.
/// This source-scan lock pins that `src/main.rs` actually calls
/// `boot::web_boot(…)` (a web-mode boot that bypasses it would let the CLI
/// and the embed diverge — the R9 regression the plan names). The embed side
/// is exercised at runtime by `local_core_contract` above.
#[test]
fn main_calls_the_shared_web_boot_fn() {
    let main_src = std::fs::read_to_string("src/main.rs").expect("read src/main.rs");
    assert!(
        main_src.contains("boot::web_boot("),
        "src/main.rs must call the shared boot fn `peakbot::boot::web_boot` \
         (the one L3 extracts so main and embed never diverge)"
    );
}
