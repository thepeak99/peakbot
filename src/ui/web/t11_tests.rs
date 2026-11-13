//! P3 PR-A — PINNING tests for `Principal::Token` (p3-design §E.5): the
//! token-holder path keeps TODAY's behaviour, so these are GREEN now and
//! must stay green after A1 moves `handle_socket` into
//! `serve_socket(…, principal)`. Marked PIN per the PR-A brief.
//!
//! - PIN: `shutdown` is still obeyed for the token principal (the socket
//!   closes; only a CloudPeer's shutdown is refused server-side).
//! - PIN: chaining semantics are unchanged — a `relayed` attach naming a
//!   REMOTE profile is refused with the locked E-CHAIN text, and a
//!   `relayed` attach naming a LOCAL profile binds normally.
//!
//! (The t4 tests cover the same contract over the same harness; these pin
//! it at the PR-A seam explicitly and are independent of A1's internals.)

use super::ws_handler;
use crate::config::resolve::{Config, ParseCtx, Source};
use crate::config::CONFIG_ENV_LOCK;
use crate::session::{SessionDeps, test_support};
use crate::storage::{ConversationStorage, InMemoryStorage};
use crate::ui::web::WsState;
use crate::ui::web::registry::SessionRegistry;
use axum::extract::State;
use axum::routing::get;
use axum::Router;
use futures::SinkExt;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::sync::{Arc, MutexGuard};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as TMsg;

const LOCAL_YAML: &str = r#"
providers:
  - name: ollama
    type: ollama
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
"#;

/// `LOCAL_YAML` + a REMOTE profile (the E-CHAIN target).
const WITH_REMOTE_YAML: &str = r#"
providers:
  - name: ollama
    type: ollama
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

/// Points `XDG_CONFIG_HOME` at a tempdir holding `yaml` as the global
/// config, under the crate-wide env lock; restores on drop (t4 pattern).
struct XdgHome {
    prev: Option<OsString>,
    _dir: tempfile::TempDir,
    _lock: MutexGuard<'static, ()>,
}

impl XdgHome {
    fn new(yaml: &str) -> Self {
        let lock = CONFIG_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().expect("XDG tempdir");
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let cfg_dir = crate::config::get_config_dir().expect("config dir");
        std::fs::create_dir_all(&cfg_dir).expect("config dir");
        std::fs::write(cfg_dir.join("config.yaml"), yaml).expect("write config");
        Self {
            prev,
            _dir: dir,
            _lock: lock,
        }
    }
}

impl Drop for XdgHome {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
    }
}

fn state_for(yaml: &str) -> (XdgHome, WsState) {
    let home = XdgHome::new(yaml);
    let storage = Arc::new(InMemoryStorage::new());
    let mut deps = test_support::test_deps(Some(storage as Arc<dyn ConversationStorage>));
    deps.config = Arc::new(
        Config::parse(
            Source {
                path: std::path::Path::new("/global/config.yaml"),
                text: yaml,
            },
            None,
            &ParseCtx { known_skills: &[] },
        )
        .expect("fixture config must parse"),
    );
    let uploads = deps.uploads.clone();
    let deps: Arc<SessionDeps> = Arc::new(deps);
    let state = WsState {
        registry: Some(SessionRegistry::new(deps)),
        models: Arc::new(Vec::new()),
        active_alias: "local".into(),
        session_ttl: Duration::from_secs(3600),
        uploads: uploads,
    };
    (home, state)
}

async fn spawn_ws(state: &WsState) -> std::net::SocketAddr {
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    addr
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_connect(addr: std::net::SocketAddr) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .expect("ws upgrade");
    ws
}

async fn ws_send(ws: &mut Ws, v: Value) {
    ws.send(TMsg::Text(v.to_string().into()))
        .await
        .expect("send");
}

async fn next_frame(ws: &mut Ws, secs: u64) -> Result<Value, bool> {
    use futures::StreamExt;
    loop {
        match tokio::time::timeout(Duration::from_secs(secs), ws.next()).await {
            Err(_) => return Err(false),
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(TMsg::Close(_)))) => return Err(true),
            Ok(Some(Ok(TMsg::Text(t)))) => {
                return Ok(serde_json::from_str(&t).expect("server frames are JSON"));
            }
            Ok(Some(Ok(_))) => continue, // ping / pong / binary
        }
    }
}

/// Read until an `error` frame; returns its message.
async fn wait_error(ws: &mut Ws) -> String {
    for _ in 0..200 {
        match next_frame(ws, 15).await {
            Ok(f) if f["type"] == "error" => {
                return f["message"].as_str().unwrap_or_default().to_string();
            }
            Ok(_) => continue,
            Err(closed) => panic!(
                "socket {} before any error frame",
                if closed { "closed" } else { "timed out" }
            ),
        }
    }
    panic!("no error frame");
}

async fn assert_closes(ws: &mut Ws) {
    for _ in 0..200 {
        match next_frame(ws, 15).await {
            Err(true) => return,
            Err(false) => panic!("socket neither closed nor sent anything within 15s"),
            Ok(_) => continue,
        }
    }
    panic!("socket never closed");
}

/// Read frames until a `state` carries `(profile, agent)`.
async fn wait_selection(ws: &mut Ws, profile: &str, agent: &str) {
    for _ in 0..200 {
        match next_frame(ws, 15).await {
            Ok(f) => {
                if f["type"] == "error" {
                    panic!("unexpected error frame: {f}");
                }
                if f["type"] == "state" {
                    let s = &f["state"]["selection"];
                    if s["profile"] == profile && s["agent"] == agent {
                        return;
                    }
                }
            }
            Err(closed) => panic!(
                "socket {} before selection ({profile},{agent})",
                if closed { "closed" } else { "timed out" }
            ),
        }
    }
    panic!("selection ({profile},{agent}) never arrived");
}

fn attach(profile: &str, agent: &str, relayed: bool) -> Value {
    json!({
        "type": "attach",
        "profile": profile,
        "agent": agent,
        "relayed": relayed,
    })
}

// ═══════════════════════════════════════════════════════════════════════
// PIN (E.5): Principal::Token keeps today's behaviour
// ═══════════════════════════════════════════════════════════════════════

/// PIN: for the token principal `shutdown` is STILL obeyed — the socket
/// closes (a CloudPeer's shutdown is the one refused inbound message).
#[tokio::test]
async fn pin_token_shutdown_is_still_obeyed() {
    let (_home, state) = state_for(LOCAL_YAML);
    let addr = spawn_ws(&state).await;
    let mut ws = ws_connect(addr).await;
    ws_send(&mut ws, attach("dev", "team", false)).await;
    wait_selection(&mut ws, "dev", "team").await;

    ws_send(&mut ws, json!({"type": "shutdown"})).await;
    assert_closes(&mut ws).await;
}

/// PIN: chaining semantics are unchanged for the token principal — a
/// `relayed` attach naming a REMOTE profile is refused with the locked
/// E-CHAIN text and no session is minted.
#[tokio::test]
async fn pin_token_relayed_remote_attach_still_refused_as_chaining() {
    let (_home, state) = state_for(WITH_REMOTE_YAML);
    let addr = spawn_ws(&state).await;
    let mut ws = ws_connect(addr).await;
    ws_send(&mut ws, attach("home-dev", "dev", true)).await;
    let msg = wait_error(&mut ws).await;
    assert_eq!(
        msg,
        "profile 'home-dev' on this server is itself a remote profile — \
         chaining remote profiles isn't supported",
        "E-CHAIN text (design §5.9) unchanged for Principal::Token"
    );
    assert_closes(&mut ws).await;
}

/// PIN: the `relayed` marker only forbids chaining — a `relayed` attach
/// naming a LOCAL profile binds normally.
#[tokio::test]
async fn pin_token_relayed_local_attach_binds_normally() {
    let (_home, state) = state_for(LOCAL_YAML);
    let addr = spawn_ws(&state).await;
    let mut ws = ws_connect(addr).await;
    ws_send(&mut ws, attach("alt", "a2", true)).await;
    wait_selection(&mut ws, "alt", "a2").await;
}
