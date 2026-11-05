//! Remote-profiles T6 — the relay (design §5.1, §5.2, §5.3, §5.6, §5.7,
//! §5.12, §6.3, §7, §9-T6). RED until the implementation lands.
//!
//! Written against the interface the design specifies, NOT an existing impl:
//!
//! * `ui::web::handle_socket` routes an attach naming a REMOTE profile to
//!   `relay::run` and RETURNS — the registry is never touched (§5.1, G7).
//! * `relay::run` dials the remote (`Directory::client` → `Client::dial`),
//!   re-attaches with `attach{convo, profile: P, agent: pin‖client, relayed:
//!   true}` (§5.2), reads until `ready` — exact version match, §5.12
//!   E-VERSION — then pumps: inbound frames classified by
//!   `relay::classify_inbound` (§6.3 table), outbound frames parsed, `state`
//!   localized by `relay::localize_state` and republished, anything
//!   unparseable/unknown dropped.
//! * `relay::classify_inbound(text: &str, view: &LocalView) ->
//!   InboundDisposition` — pure, the §6.3 inbound table.
//! * `relay::localize_state(state: AppState, view: &LocalView) -> AppState`
//!   — pure; changes EXACTLY four things: (1) root-relative markdown image
//!   URLs `/…` → `/r/<L>/…`, (2) `catalog := view.catalog` with entry L's
//!   `agents :=` the remote catalogue's entry P's agents (pin-filtered,
//!   empty if P is missing), (3) `selection.profile: P → L`, (4)
//!   `profile_pinned := false`.
//!
//! The module the implementer must provide: `crate::ui::web::remote::relay`
//! with `LocalView`, `InboundDisposition`, `classify_inbound`,
//! `localize_state` (and `run`, called from `handle_socket`). The pure
//! functions below compile-RED until it exists; the E2E tests below
//! fail on their assertions until `handle_socket` routes to it.
//!
//! Locked strings asserted here (L = the local remote-profile name,
//! M = `PROTOCOL_VERSION`):
//! * E-VERSION: `Remote profile 'L': remote PeakBot speaks protocol vN,
//!   this PeakBot speaks vM — upgrade one side so they match.`
//! * E-SHUTDOWN: `shutdown isn't available in a remote profile.`
//! * E-LOST: `Lost the connection to remote profile 'L'. Reconnecting…`
//! * E-DOWN: `Can't reach remote profile 'L' at H: <reason>.` (§5.12)
//! * E-TLS-PIN / E-AUTH: the §5.12 strings as `RemoteError::Display`
//!   (T5-locked; the relay forwards them verbatim as `error` frames).
//! * select on a foreign profile: `switch profiles from the chip` (§6.3).
//!
//! Harness: a REAL local `/ws` (T4-style `WsState` + `ws_handler`, token
//! gated with `T_LOCAL`) pointed at a SCRIPTED fake remote — an axum TLS
//! server (fresh test CA) that records every WS frame it receives, answers
//! scripted outbound frames, and (via `test_support::Recorder`) records
//! every HTTP request line + header, so the §7 token isolation is asserted
//! on the wire.

#![allow(clippy::await_holding_lock)] // one E2E test at a time (ENV_LOCK)

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::extract::ws::{Message as AxMsg, WebSocket, WebSocketUpgrade};
use axum::http::{Request, StatusCode};
use axum::middleware::{self, Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message as TMsg;

use super::WsStream;
use super::test_support::{self, TestCa};
use crate::config::file::TabColor;
use crate::config::resolve::{AgentInfo, Config, ParseCtx, ProfileInfo, Source};
use crate::session::{SessionDeps, test_support as deps_ts};
use crate::storage::{ConversationStorage, InMemoryStorage};
use crate::ui::app_state::AppState;
use crate::ui::web::registry::SessionRegistry;
use crate::ui::web::remote::relay;
use crate::ui::web::{WsState, require_token, ws_handler};
use crate::ui::wire::PROTOCOL_VERSION;

// ── Contract constants (locked by the design) ──────────────────────────

/// L — the local remote-profile name (the tab's profile).
const L: &str = "home-dev";
/// P — the profile on the remote server (the relay's attach target).
const P: &str = "dev";
/// The local web token — must NEVER reach the remote (§7 / §9-T6(d)).
const T_LOCAL: &str = "LOCAL-TOKEN-RELAY-0a1b";
/// The remote's web token — travels only as the `peakbot_token` cookie.
const T_REMOTE: &str = "remote-token-relay-7c9d";
/// The remote's live conversation id (what its `attached` reports).
const CONVO: &str = "77777777-7777-7777-7777-777777777777";

/// The sentinel the scripted remote appends to its frame log when the
/// socket's handler returns (i.e. the socket is closed).
const SOCKET_CLOSED: &str = "\u{1}SOCKET_CLOSED\u{1}";

/// E2E tests share the process-global `XDG_CONFIG_HOME`; serialize them.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn eversion(remote_v: u32) -> String {
    format!(
        "Remote profile '{L}': remote PeakBot speaks protocol v{remote_v}, \
         this PeakBot speaks v{m} — upgrade one side so they match.",
        m = PROTOCOL_VERSION
    )
}

fn elost() -> String {
    format!("Lost the connection to remote profile '{L}'. Reconnecting…")
}

// ── The scripted fake remote ────────────────────────────────────────────

/// The remote's behaviour: what it answers after the attach, and which
/// outbound frames it sends in reply to which inbound `type`.
#[derive(Clone, Default)]
struct WsScript {
    /// Send `attached{convo}` before `ready` (the remote's live id).
    attached_convo: Option<String>,
    /// `Some(Some(v))` → `ready{v}`; `Some(None)` → `ready` with no `v`;
    /// `None` → no ready frame at all.
    ready_v: Option<Option<u32>>,
    /// Inbound frame `type` → outbound frames to send after receiving it.
    replies: BTreeMap<String, Vec<String>>,
    /// Close the socket after replying to this inbound `type`.
    close_after: Option<String>,
}

#[derive(Clone)]
struct RemoteState {
    token: String,
    ca_pem: String,
    script: WsScript,
    frames: Arc<Mutex<Vec<String>>>,
}

/// Push the close sentinel when the socket's handler unwinds.
struct ClosedMark(Arc<Mutex<Vec<String>>>);

impl Drop for ClosedMark {
    fn drop(&mut self) {
        self.0
            .lock()
            .expect("frames lock")
            .push(SOCKET_CLOSED.to_string());
    }
}

async fn scripted_ws(mut socket: WebSocket, script: WsScript, frames: Arc<Mutex<Vec<String>>>) {
    let _mark = ClosedMark(frames.clone());

    // First frame: the relay's re-attach. Record it, then answer.
    let first = match socket.recv().await {
        Some(Ok(AxMsg::Text(t))) => {
            frames.lock().expect("frames lock").push(t.to_string());
            true
        }
        _ => return,
    };
    if !first {
        return;
    }
    if let Some(convo) = &script.attached_convo {
        let _ = socket
            .send(AxMsg::Text(
                format!(r#"{{"type":"attached","convo":"{convo}"}}"#).into(),
            ))
            .await;
    }
    if let Some(v) = script.ready_v {
        let ready = match v {
            Some(n) => format!(r#"{{"type":"ready","v":{n}}}"#),
            None => r#"{"type":"ready"}"#.to_string(),
        };
        let _ = socket.send(AxMsg::Text(ready.into())).await;
    }

    loop {
        let Some(Ok(AxMsg::Text(t))) = socket.recv().await else {
            return;
        };
        let text = t.to_string();
        frames.lock().expect("frames lock").push(text.clone());
        let ty = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string));
        let Some(ty) = ty else { return };
        if let Some(replies) = script.replies.get(&ty) {
            for frame in replies {
                if socket
                    .send(AxMsg::Text(frame.clone().into()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
        if script.close_after.as_deref() == Some(ty.as_str()) {
            return;
        }
    }
}

async fn gate(State(st): State<RemoteState>, req: Request<Body>, next: Next) -> Response {
    if test_support::cookie_token(&req).as_deref() != Some(st.token.as_str()) {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    next.run(req).await
}

async fn ca_route(State(st): State<RemoteState>) -> (StatusCode, String) {
    (StatusCode::OK, st.ca_pem.clone())
}

async fn ws_route(ws: WebSocketUpgrade, State(st): State<RemoteState>) -> Response {
    ws.on_upgrade(move |socket| scripted_ws(socket, st.script.clone(), st.frames.clone()))
}

fn remote_app(st: RemoteState) -> Router {
    let authed = Router::new()
        .route("/ws", get(ws_route))
        .layer(middleware::from_fn_with_state(st.clone(), gate));
    // `/peakbot-ca.crt` is tokenless (the §5.8 bootstrap), everything else
    // is gated by the remote token — exactly like a real PeakBot.
    Router::new()
        .route("/peakbot-ca.crt", get(ca_route))
        .merge(authed)
        .with_state(st)
}

// ── The E2E environment: real local /ws ↔ scripted fake remote ─────────

struct RelayEnv {
    _home: test_support::XdgHome,
    registry: SessionRegistry,
    /// The local (token-gated) `/ws` endpoint.
    local: SocketAddr,
    /// Every HTTP request the fake remote saw (line + headers).
    rec: test_support::Recorder,
    /// Every WS frame the fake remote's `/ws` received.
    frames: Arc<Mutex<Vec<String>>>,
}

impl RelayEnv {
    /// The remote's received WS frames (the close sentinel excluded).
    fn ws_frames(&self) -> Vec<String> {
        self.frames
            .lock()
            .expect("frames lock")
            .iter()
            .filter(|f| f.as_str() != SOCKET_CLOSED)
            .cloned()
            .collect()
    }

    fn saw_remote_close(&self) -> bool {
        self.frames
            .lock()
            .expect("frames lock")
            .iter()
            .any(|f| f.as_str() == SOCKET_CLOSED)
    }
}

/// The local global config: two local profiles + the remote profile L.
fn local_yaml(url: &str, pin: &str, token: &str) -> String {
    format!(
        r#"
providers:
  - name: ollama
    type: ollama
    models:
      - {{ name: llama3, alias: local }}
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
  {L}:
    remote:
      url: {url}
      token: {token}
      ca: {pin}
      profile: {P}
"#
    )
}

/// Spin up the full environment.
///
/// * `remote_ca` = Some ⇒ a TLS fake remote is started (serving its CA at
///   `/peakbot-ca.crt`); the config's `ca` is `pin` and its token
///   `config_token`.
/// * `dead_url` = Some ⇒ no remote is started; the config points at that
///   (refused) URL — the E-DOWN row.
///
/// `remote_token` is what the fake remote's token gate accepts (it may
/// differ from `config_token` — the 401 row).
async fn relay_env(
    script: WsScript,
    remote_ca: Option<&TestCa>,
    dead_url: Option<String>,
    pin: &str,
    config_token: &str,
    remote_token: &str,
) -> RelayEnv {
    let frames = Arc::new(Mutex::new(Vec::new()));
    let rec = test_support::Recorder::new();
    let url = match (remote_ca, dead_url) {
        (Some(ca), None) => {
            test_support::start_tls_app(
                ca,
                &["127.0.0.1"],
                remote_app(RemoteState {
                    token: remote_token.to_string(),
                    ca_pem: ca.ca_pem.clone(),
                    script,
                    frames: frames.clone(),
                }),
                rec.clone(),
            )
            .await
        }
        (None, Some(url)) => url,
        _ => panic!("relay_env: exactly one of remote_ca / dead_url"),
    };
    let yaml = local_yaml(&url, pin, config_token);
    let home = test_support::XdgHome::new(&yaml);

    // The real local server: T4-style deps over the SAME yaml.
    let storage = Arc::new(InMemoryStorage::new());
    let mut deps = deps_ts::test_deps(Some(storage.clone() as Arc<dyn ConversationStorage>));
    deps.config = Arc::new(
        Config::parse(
            Source {
                path: std::path::Path::new("/global/config.yaml"),
                text: &yaml,
            },
            None,
            &ParseCtx { known_skills: &[] },
        )
        .expect("fixture config must parse"),
    );
    let deps: Arc<SessionDeps> = Arc::new(deps);
    let registry = SessionRegistry::new(deps.clone());
    let state = WsState {
        registry: Some(registry.clone()),
        models: Arc::new(Vec::new()),
        active_alias: "local".into(),
        session_ttl: Duration::from_secs(3600),
        uploads: deps.uploads.clone(),
    };
    let local = serve_local_ws(&state).await;
    RelayEnv {
        _home: home,
        registry,
        local,
        rec,
        frames,
    }
}

/// The local `/ws`, wrapped in the SAME token layer `WebUi::run` applies.
async fn serve_local_ws(state: &WsState) -> SocketAddr {
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .with_state(state.clone())
        .layer(from_fn_with_state(Arc::<str>::from(T_LOCAL), require_token));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local listener");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    addr
}

/// The happy-path environment: a healthy remote (attached + ready{v:1}).
fn base_script() -> WsScript {
    WsScript {
        attached_convo: Some(CONVO.to_string()),
        ready_v: Some(Some(PROTOCOL_VERSION)),
        replies: BTreeMap::new(),
        close_after: None,
    }
}

async fn ready_env() -> RelayEnv {
    let ca = test_support::mint_ca();
    relay_env(
        base_script(),
        Some(&ca),
        None,
        &ca.fingerprint,
        T_REMOTE,
        T_REMOTE,
    )
    .await
}

// ── WS client helpers (the local browser's socket) ─────────────────────

type Ws = WsStream;

async fn ws_connect(addr: SocketAddr) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={T_LOCAL}"))
        .await
        .expect("local ws upgrade");
    ws
}

async fn ws_send(ws: &mut Ws, v: &Value) {
    ws.send(TMsg::Text(v.to_string().into()))
        .await
        .expect("ws send");
}

/// Next JSON text frame within `secs`; `Err(true)` = closed, `Err(false)` =
/// timeout.
async fn next_frame(ws: &mut Ws, secs: u64) -> Result<Value, bool> {
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

/// Collect every frame arriving within `ms` (stops early on close).
async fn collect_for(ws: &mut Ws, ms: u64) -> Vec<Value> {
    let end = tokio::time::Instant::now() + Duration::from_millis(ms);
    let mut out = Vec::new();
    loop {
        let left = end.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, next_frame(ws, 60)).await {
            Ok(Ok(v)) => out.push(v),
            _ => break,
        }
    }
    out
}

/// Collect frames until the socket closes (bounded).
async fn frames_until_close(ws: &mut Ws) -> Vec<Value> {
    let mut out = Vec::new();
    loop {
        match next_frame(ws, 10).await {
            Ok(v) => out.push(v),
            Err(true) => return out,
            Err(false) => panic!("the socket did not close within 10s; frames so far: {out:?}"),
        }
    }
}

/// Read until an `error` frame; returns its message. Panics on
/// timeout/close.
async fn wait_error(ws: &mut Ws) -> String {
    for _ in 0..200 {
        match next_frame(ws, 10).await {
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

/// The relay handshake: `attached` (the REMOTE id) then `ready{v:1}`.
/// Returns the attached convo.
async fn wait_handshake(ws: &mut Ws) -> String {
    let mut attached: Option<String> = None;
    let mut ready = false;
    for _ in 0..200 {
        match next_frame(ws, 10).await {
            Ok(f) => match f["type"].as_str() {
                Some("attached") => {
                    assert!(attached.is_none(), "two attached frames: {f}");
                    attached = Some(
                        f["convo"]
                            .as_str()
                            .expect("attached carries the convo id")
                            .to_string(),
                    );
                }
                Some("ready") => {
                    assert_eq!(f["v"], json!(PROTOCOL_VERSION), "ready v: {f}");
                    ready = true;
                }
                Some("error") => panic!("handshake answered with an error: {f}"),
                _ => {}
            },
            Err(closed) => panic!(
                "socket {} mid-handshake (attached={attached:?}, ready={ready})",
                if closed { "closed" } else { "timed out" }
            ),
        }
        if let (Some(convo), true) = (&attached, ready) {
            return convo.clone();
        }
    }
    panic!("handshake incomplete (attached={attached:?}, ready={ready})");
}

/// Attach + handshake; optionally send one more frame. Returns the socket.
async fn drive(env: &RelayEnv, trigger: Value) -> Ws {
    let mut ws = ws_connect(env.local).await;
    ws_send(&mut ws, &attach_frame(None, Some(L), None)).await;
    wait_handshake(&mut ws).await;
    if trigger.is_object() {
        ws_send(&mut ws, &trigger).await;
    }
    ws
}

/// Poll the remote's frame log until `pred` matches (5 s bound).
async fn wait_remote_frame(env: &RelayEnv, mut pred: impl FnMut(&str) -> bool) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(f) = env.ws_frames().into_iter().find(|f| pred(f)) {
            return f;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "the remote never saw the expected frame; got {:?}",
                env.ws_frames()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll until the fake remote's socket closed (5 s bound).
async fn wait_remote_close(env: &RelayEnv) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if env.saw_remote_close() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "the remote socket never closed; frames: {:?}",
                env.ws_frames()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Attach frame helper.
fn attach_frame(convo: Option<&str>, profile: Option<&str>, agent: Option<&str>) -> Value {
    let mut v = json!({ "type": "attach", "convo": convo });
    if let Some(p) = profile {
        v["profile"] = json!(p);
    }
    if let Some(a) = agent {
        v["agent"] = json!(a);
    }
    v
}

fn hit_cookie(hit: &test_support::Captured) -> Option<String> {
    hit.headers
        .iter()
        .find(|(name, _)| name == "cookie")
        .map(|(_, value)| value.clone())
}

/// An env whose scripted remote answers `replies` to an inbound frame of
/// `type` `key`.
async fn env_replies_to(key: &str, replies: Vec<Value>) -> RelayEnv {
    let ca = test_support::mint_ca();
    let mut script = base_script();
    script.replies.insert(
        key.to_string(),
        replies.iter().map(|v| v.to_string()).collect(),
    );
    relay_env(script, Some(&ca), None, &ca.fingerprint, T_REMOTE, T_REMOTE).await
}

/// Drive one trigger frame, collect what arrives within 1.5 s, and return
/// the (still open) socket with the env.
async fn drive_and_collect(env: &RelayEnv, trigger: Value) -> (Ws, Vec<Value>) {
    let mut ws = drive(env, trigger).await;
    let frames = collect_for(&mut ws, 1500).await;
    (ws, frames)
}

// ═══════════════════════════════════════════════════════════════════════
// P1 — routing and the hard security/correctness guarantees
// ═══════════════════════════════════════════════════════════════════════

// §5.1 / §9-T6(e): an attach naming a remote profile is ROUTED to the relay.
// The remote receives `attach{profile: P, relayed: true}`; the tab binds to
// the REMOTE id; NO local session is minted; the local agent never answers.
#[tokio::test]
async fn attach_remote_profile_is_routed_to_the_relay() {
    let _guard = env_guard();
    let env = ready_env().await;
    let mut ws = ws_connect(env.local).await;
    ws_send(&mut ws, &attach_frame(None, Some(L), None)).await;
    let convo = wait_handshake(&mut ws).await;
    assert_eq!(convo, CONVO, "the tab binds to the REMOTE id");

    // The remote saw the re-attach: relayed, the REMOTE target profile.
    let frames = env.ws_frames();
    let first = frames.first().expect("the remote received the attach");
    let a: Value = serde_json::from_str(first).expect("the attach frame is JSON");
    assert_eq!(a["type"], "attach");
    assert_eq!(
        a["profile"], P,
        "the remote target profile, not the local name"
    );
    assert_eq!(
        a["relayed"], true,
        "the relay always attaches with relayed: true"
    );
    assert!(
        a.get("agent").is_none() || a["agent"].is_null(),
        "no agent in the attach: {a}"
    );
    assert!(
        a.get("convo").is_none() || a["convo"].is_null(),
        "fresh attach: {a}"
    );

    // G7: the registry is never touched.
    assert!(
        env.registry.active_ids().is_empty(),
        "no local session may exist for a remote tab"
    );

    // The local agent never answers: a send_message reaches the remote and
    // gets no local state/error frame.
    ws_send(
        &mut ws,
        &json!({ "type": "send_message", "text": "hello remote" }),
    )
    .await;
    let _ = wait_remote_frame(&env, |f| f.contains("hello remote")).await;
    let frames = collect_for(&mut ws, 1200).await;
    assert!(
        frames
            .iter()
            .all(|f| f["type"] != "state" && f["type"] != "error"),
        "no local state or error answers a remote tab: {frames:?}"
    );
}

// §5.2 step 4: the client's `agent` rides the re-attach
// (`agent: L.agent.or(attach.agent)`).
#[tokio::test]
async fn attach_remote_profile_with_agent_forwards_the_agent() {
    let _guard = env_guard();
    let env = ready_env().await;
    let mut ws = ws_connect(env.local).await;
    ws_send(&mut ws, &attach_frame(None, Some(L), Some("team"))).await;
    wait_handshake(&mut ws).await;

    let first: Value =
        serde_json::from_str(env.ws_frames().first().expect("the attach frame")).expect("JSON");
    assert_eq!(
        first["agent"], "team",
        "the client's agent rides the re-attach"
    );
}

// ── §5.2 step 5 / §5.12 E-VERSION — exact match (T13) ──────────────────

async fn version_env(remote_v: Option<u32>) -> RelayEnv {
    let ca = test_support::mint_ca();
    let mut script = base_script();
    script.ready_v = Some(remote_v);
    relay_env(script, Some(&ca), None, &ca.fingerprint, T_REMOTE, T_REMOTE).await
}

fn assert_everion(frames: &[Value], remote_v: u32) {
    let err_idx = frames
        .iter()
        .position(|f| f["type"] == "error")
        .unwrap_or_else(|| panic!("no E-VERSION error frame: {frames:?}"));
    assert_eq!(
        frames[err_idx]["message"],
        json!(eversion(remote_v)),
        "the locked E-VERSION string names both versions"
    );
    assert_eq!(
        frames.len(),
        err_idx + 1,
        "nothing is forwarded after a version failure: {frames:?}"
    );
}

// Equal versions → the handshake is forwarded (attached → ready{v:1}).
#[tokio::test]
async fn remote_ready_v1_is_forwarded_to_the_local_client() {
    let _guard = env_guard();
    let env = version_env(Some(1)).await;
    let mut ws = ws_connect(env.local).await;
    ws_send(&mut ws, &attach_frame(None, Some(L), None)).await;
    let convo = wait_handshake(&mut ws).await;
    assert_eq!(convo, CONVO);
    assert!(env.registry.active_ids().is_empty());
}

// A NEWER remote (v = 2) → E-VERSION naming both versions, then close.
#[tokio::test]
async fn remote_ready_v2_is_everion_naming_both_versions_then_close() {
    let _guard = env_guard();
    let env = version_env(Some(2)).await;
    let mut ws = ws_connect(env.local).await;
    ws_send(&mut ws, &attach_frame(None, Some(L), None)).await;
    let frames = frames_until_close(&mut ws).await;
    assert_everion(&frames, 2);
    assert!(env.registry.active_ids().is_empty());
}

// An OLDER remote (v = 0) → the same E-VERSION error.
#[tokio::test]
async fn remote_ready_v0_is_everion_naming_both_versions_then_close() {
    let _guard = env_guard();
    let env = version_env(Some(0)).await;
    let mut ws = ws_connect(env.local).await;
    ws_send(&mut ws, &attach_frame(None, Some(L), None)).await;
    let frames = frames_until_close(&mut ws).await;
    assert_everion(&frames, 0);
    assert!(env.registry.active_ids().is_empty());
}

// A pre-version remote (no `v` field → reads as 0) → the same E-VERSION.
#[tokio::test]
async fn remote_ready_without_v_field_is_everion_then_close() {
    let _guard = env_guard();
    let env = version_env(None).await;
    let mut ws = ws_connect(env.local).await;
    ws_send(&mut ws, &attach_frame(None, Some(L), None)).await;
    let frames = frames_until_close(&mut ws).await;
    assert_everion(&frames, 0);
    assert!(env.registry.active_ids().is_empty());
}

// ── §5.12 failure rows the relay owns ──────────────────────────────────

// E-DOWN: the remote is unreachable at dial → the locked E-DOWN string as
// an error frame, then close; bounded; no local session; no local answer.
#[tokio::test]
async fn remote_down_is_edown_then_close() {
    let _guard = env_guard();
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("port probe");
        probe.local_addr().expect("port").port()
    }; // dropped here: the port is now refused
    let ca = test_support::mint_ca();
    let env = relay_env(
        base_script(),
        None,
        Some(format!("https://127.0.0.1:{port}")),
        &ca.fingerprint,
        T_REMOTE,
        T_REMOTE,
    )
    .await;

    let mut ws = ws_connect(env.local).await;
    ws_send(&mut ws, &attach_frame(None, Some(L), None)).await;
    let started = std::time::Instant::now();
    let frames = frames_until_close(&mut ws).await;
    let elapsed = started.elapsed();

    let errs: Vec<&Value> = frames.iter().filter(|f| f["type"] == "error").collect();
    assert_eq!(errs.len(), 1, "one E-DOWN error: {frames:?}");
    let msg = errs[0]["message"].as_str().unwrap_or("");
    assert!(
        msg.starts_with(&format!(
            "Can't reach remote profile '{L}' at 127.0.0.1:{port}: "
        )),
        "E-DOWN names the profile and host:port: {msg}"
    );
    assert!(
        msg.ends_with('.'),
        "locked string ends with a period: {msg}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "bounded, took {elapsed:?}"
    );
    assert!(env.registry.active_ids().is_empty(), "no local session");
}

// E-TLS-PIN: the CA bootstrap mismatch is a HARD error, forwarded verbatim
// (the T5-locked `RemoteError` string), then close.
#[tokio::test]
async fn tls_pin_mismatch_is_etls_pin_then_close() {
    let _guard = env_guard();
    let server_ca = test_support::mint_ca();
    let other_ca = test_support::mint_ca(); // the pin comes from a DIFFERENT CA
    let env = relay_env(
        base_script(),
        Some(&server_ca),
        None,
        &other_ca.fingerprint,
        T_REMOTE,
        T_REMOTE,
    )
    .await;

    let mut ws = ws_connect(env.local).await;
    ws_send(&mut ws, &attach_frame(None, Some(L), None)).await;
    let frames = frames_until_close(&mut ws).await;

    let errs: Vec<&Value> = frames.iter().filter(|f| f["type"] == "error").collect();
    assert_eq!(errs.len(), 1, "one E-TLS-PIN error: {frames:?}");
    assert_eq!(
        errs[0]["message"],
        json!(format!(
            "Remote profile '{L}': the server's CA is {}, but the config pins {}. \
             Refusing to connect. If the server's CA was rotated, update `ca` in Settings → Profiles.",
            server_ca.fingerprint, other_ca.fingerprint
        )),
        "E-TLS-PIN names both fingerprints (the T5-locked string, verbatim)"
    );
    assert!(env.registry.active_ids().is_empty());
}

// E-AUTH: a 401 on the WS upgrade → the locked auth string, then close.
#[tokio::test]
async fn remote_401_is_eauth_then_close() {
    let _guard = env_guard();
    let ca = test_support::mint_ca();
    // The fake remote only accepts `server-token-real`; the config carries a
    // stale one.
    let env = relay_env(
        base_script(),
        Some(&ca),
        None,
        &ca.fingerprint,
        "stale-token-from-config",
        "server-token-real",
    )
    .await;

    let mut ws = ws_connect(env.local).await;
    ws_send(&mut ws, &attach_frame(None, Some(L), None)).await;
    let frames = frames_until_close(&mut ws).await;

    let errs: Vec<&Value> = frames.iter().filter(|f| f["type"] == "error").collect();
    assert_eq!(errs.len(), 1, "one E-AUTH error: {frames:?}");
    assert_eq!(
        errs[0]["message"],
        json!(
            "Remote profile 'home-dev' rejected its token. \
               Update the token in Settings → Profiles."
        ),
        "the locked E-AUTH string"
    );
    assert!(env.registry.active_ids().is_empty());
}

// §7 / §9-T6(d): token isolation, end to end. The local web token (T_LOCAL)
// appears NOWHERE in the remote's capture — not in any HTTP header, URL or
// WS frame. The remote token appears only in `Cookie` headers.
#[tokio::test]
async fn local_token_never_reaches_the_remote() {
    let _guard = env_guard();
    let env = ready_env().await;
    let mut ws = ws_connect(env.local).await;
    ws_send(&mut ws, &attach_frame(None, Some(L), None)).await;
    wait_handshake(&mut ws).await;
    ws_send(
        &mut ws,
        &json!({ "type": "send_message", "text": "token-free" }),
    )
    .await;
    let _ = wait_remote_frame(&env, |f| f.contains("token-free")).await;

    let hits = env.rec.hits();
    assert!(
        !hits.is_empty(),
        "the remote saw HTTP requests (the CA fetch + the /ws upgrade)"
    );
    for hit in &hits {
        assert!(
            !hit.uri.contains(T_LOCAL),
            "the local token is not in any URL: {}",
            hit.uri
        );
        for (name, value) in &hit.headers {
            assert!(
                !value.contains(T_LOCAL),
                "the local token is not in any header ({name}): {value}"
            );
            if value.contains(T_REMOTE) {
                assert_eq!(
                    name, "cookie",
                    "the remote token appears only in the cookie header, not {name}"
                );
            }
        }
    }
    for frame in env.ws_frames() {
        assert!(
            !frame.contains(T_LOCAL),
            "the local token is not in any WS frame: {frame}"
        );
        assert!(
            !frame.contains(T_REMOTE),
            "the remote token is not in any WS frame: {frame}"
        );
    }
    // Sanity: the remote DID authenticate with its own token.
    let ws_hit = hits
        .iter()
        .find(|h| h.uri == "/ws")
        .expect("the /ws upgrade reached the remote");
    assert_eq!(
        hit_cookie(ws_hit),
        Some(format!("peakbot_token={T_REMOTE}")),
        "the /ws upgrade carries the remote token as the cookie"
    );
}

// §5.12 E-SHUTDOWN: `shutdown` on a relay socket is BLOCKED — the locked
// string, the socket stays open, and the frame never reaches the remote.
#[tokio::test]
async fn shutdown_is_blocked_with_eshutdown_and_the_socket_stays_open() {
    let _guard = env_guard();
    let env = ready_env().await;
    let mut ws = ws_connect(env.local).await;
    ws_send(&mut ws, &attach_frame(None, Some(L), None)).await;
    wait_handshake(&mut ws).await;

    ws_send(&mut ws, &json!({ "type": "shutdown" })).await;
    let msg = wait_error(&mut ws).await;
    assert_eq!(
        msg, "shutdown isn't available in a remote profile.",
        "the locked E-SHUTDOWN string"
    );
    assert!(
        env.ws_frames().iter().all(|f| !f.contains("shutdown")),
        "shutdown must not reach the remote: {:?}",
        env.ws_frames()
    );

    // The socket stays open: the next frame is still forwarded.
    ws_send(&mut ws, &json!({ "type": "stop" })).await;
    let _ = wait_remote_frame(&env, |f| f == r#"{"type":"stop"}"#).await;
}

// ═══════════════════════════════════════════════════════════════════════
// P2 — the §6.3 frame interception table
// ═══════════════════════════════════════════════════════════════════════

// ── classify_inbound — the pure inbound table (compile-RED) ────────────
//
// Contract: `relay::classify_inbound(text: &str, view: &LocalView) ->
// InboundDisposition` with
//   `LocalView { name: L, target_profile: P, target_agent: Option<pin>,
//                catalog: Vec<ProfileInfo> }`
// and dispositions: `Forward` (the 8 forward variants, raw text), `Ignore`
// (mid-stream attach), `ForwardRewritten(text)` (select L→P),
// `Refuse(message)` (select rejections), `InterceptLocal(…)`
// (request_conversations, kill_session — never forwarded),
// `ShutdownBlocked`, `Invalid` (bad JSON → the local `invalid inbound
// JSON: …` answer). `Debug` is required on the enum.

fn view() -> relay::LocalView {
    relay::LocalView {
        name: L.to_string(),
        target_profile: P.to_string(),
        target_agent: None,
        catalog: local_catalog(),
    }
}

fn local_catalog() -> Vec<ProfileInfo> {
    vec![
        ProfileInfo {
            name: P.to_string(),
            origin: "global".into(),
            overrides_global: false,
            agents: vec![agent_info("team")],
            color: TabColor::derived(P),
        },
        ProfileInfo {
            name: "alt".into(),
            origin: "global".into(),
            overrides_global: false,
            agents: vec![agent_info("a1"), agent_info("a2")],
            color: TabColor::derived("alt"),
        },
        ProfileInfo {
            name: L.to_string(),
            origin: "remote".into(),
            overrides_global: false,
            agents: vec![agent_info("placeholder")],
            color: TabColor::derived(L),
        },
    ]
}

fn agent_info(name: &str) -> AgentInfo {
    AgentInfo {
        name: name.into(),
        model: "local".into(),
        subagents: vec![],
    }
}

#[test]
fn classify_send_message_forwards_verbatim() {
    let d = relay::classify_inbound(
        r#"{"type":"send_message","text":"hi","attachments":["5b7c1a2e-0000-4000-8000-000000000001"]}"#,
        &view(),
    );
    assert!(
        matches!(d, relay::InboundDisposition::Forward),
        "send_message is forwarded raw, got {d:?}"
    );
}

#[test]
fn classify_stop_forwards_verbatim() {
    let d = relay::classify_inbound(r#"{"type":"stop"}"#, &view());
    assert!(
        matches!(d, relay::InboundDisposition::Forward),
        "stop is forwarded raw, got {d:?}"
    );
}

#[test]
fn classify_pause_forwards_verbatim() {
    let d = relay::classify_inbound(r#"{"type":"pause"}"#, &view());
    assert!(
        matches!(d, relay::InboundDisposition::Forward),
        "pause is forwarded raw, got {d:?}"
    );
}

#[test]
fn classify_resume_forwards_verbatim() {
    let d = relay::classify_inbound(r#"{"type":"resume"}"#, &view());
    assert!(
        matches!(d, relay::InboundDisposition::Forward),
        "resume is forwarded raw, got {d:?}"
    );
}

#[test]
fn classify_switch_model_forwards_verbatim() {
    let d = relay::classify_inbound(r#"{"type":"switch_model","alias":"local"}"#, &view());
    assert!(
        matches!(d, relay::InboundDisposition::Forward),
        "switch_model is forwarded raw, got {d:?}"
    );
}

#[test]
fn classify_switch_cwd_forwards_verbatim() {
    let d = relay::classify_inbound(r#"{"type":"switch_cwd","path":"~/proj"}"#, &view());
    assert!(
        matches!(d, relay::InboundDisposition::Forward),
        "switch_cwd is forwarded raw, got {d:?}"
    );
}

#[test]
fn classify_list_dir_forwards_verbatim() {
    let d = relay::classify_inbound(r#"{"type":"list_dir","path":"/tmp"}"#, &view());
    assert!(
        matches!(d, relay::InboundDisposition::Forward),
        "list_dir is forwarded raw, got {d:?}"
    );
}

#[test]
fn classify_request_recent_dirs_forwards_verbatim() {
    let d = relay::classify_inbound(r#"{"type":"request_recent_dirs"}"#, &view());
    assert!(
        matches!(d, relay::InboundDisposition::Forward),
        "request_recent_dirs is forwarded raw, got {d:?}"
    );
}

#[test]
fn classify_mid_stream_attach_is_ignored() {
    let d = relay::classify_inbound(
        r#"{"type":"attach","convo":"abc","profile":"home-dev"}"#,
        &view(),
    );
    assert!(
        matches!(d, relay::InboundDisposition::Ignore),
        "a mid-stream attach is ignored (row 1'), got {d:?}"
    );
}

// Row 10: `profile == L` → re-sent with `profile: P`, the rest preserved.
#[test]
fn classify_select_same_profile_rewrites_to_the_remote_target() {
    let d = relay::classify_inbound(
        r#"{"type":"select","profile":"home-dev","agent":"team","model":"opus","subagent_models":{"t":"haiku"}}"#,
        &view(),
    );
    match d {
        relay::InboundDisposition::ForwardRewritten(text) => {
            let v: Value = serde_json::from_str(&text).expect("the rewritten frame is JSON");
            assert_eq!(v["type"], "select");
            assert_eq!(v["profile"], P, "L is rewritten to the remote target");
            assert_eq!(v["agent"], "team", "the agent is preserved");
            assert_eq!(v["model"], "opus", "the model leg is preserved");
            assert_eq!(
                v["subagent_models"]["t"], "haiku",
                "the overrides are preserved"
            );
        }
        other => panic!("expected the rewritten select, got {other:?}"),
    }
}

// Row 10: an agent pin is set and the select names another agent → refuse.
#[test]
fn classify_select_agent_pin_conflict_is_refused() {
    let mut v = view();
    v.target_agent = Some("team".into());
    let d = relay::classify_inbound(
        r#"{"type":"select","profile":"home-dev","agent":"other"}"#,
        &v,
    );
    match d {
        relay::InboundDisposition::Refuse(msg) => {
            assert!(
                msg.contains("team"),
                "the refusal names the pinned agent: {msg:?}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

// Row 10: `profile ≠ L` → the locked chip refusal (the SPA re-attaches).
#[test]
fn classify_select_other_profile_is_refused_with_chip_message() {
    let d = relay::classify_inbound(r#"{"type":"select","profile":"alt","agent":"a1"}"#, &view());
    match d {
        relay::InboundDisposition::Refuse(msg) => {
            assert_eq!(
                msg, "switch profiles from the chip",
                "the locked chip message"
            );
        }
        other => panic!("expected the chip refusal, got {other:?}"),
    }
}

// Row 11: answered locally (T7's merged list) — NEVER forwarded.
#[test]
fn classify_request_conversations_is_intercepted_locally() {
    let d = relay::classify_inbound(r#"{"type":"request_conversations"}"#, &view());
    assert!(
        matches!(d, relay::InboundDisposition::InterceptLocal(_)),
        "request_conversations is intercepted, got {d:?}"
    );
}

// Row 12: routed by `profile` (T7's kill routing) — never forwarded.
#[test]
fn classify_kill_session_is_intercepted_locally() {
    let d = relay::classify_inbound(
        r#"{"type":"kill_session","convo":"77777777-7777-7777-7777-777777777777","profile":"home-dev"}"#,
        &view(),
    );
    assert!(
        matches!(d, relay::InboundDisposition::InterceptLocal(_)),
        "kill_session is intercepted, got {d:?}"
    );
}

// Row 13: blocked with E-SHUTDOWN; it would `/exit` the remote session.
#[test]
fn classify_shutdown_is_blocked() {
    let d = relay::classify_inbound(r#"{"type":"shutdown"}"#, &view());
    assert!(
        matches!(d, relay::InboundDisposition::ShutdownBlocked),
        "shutdown is blocked, got {d:?}"
    );
}

// Invalid JSON → the local `invalid inbound JSON: …` answer (same as today).
#[test]
fn classify_invalid_json_is_invalid() {
    let d = relay::classify_inbound(r#"{"type":"send_message""#, &view());
    assert!(
        matches!(d, relay::InboundDisposition::Invalid),
        "invalid JSON is answered locally, got {d:?}"
    );
}

// ── localize_state — the pure §6.3 rewrite (compile-RED) ───────────────
//
// Contract: `relay::localize_state(state: AppState, view: &LocalView) ->
// AppState`. Changes exactly four things; everything else is untouched.

/// The six URL shapes of §6.3 item 1 in one message: root-relative images,
/// uploads, the `/api/config` edge, and the three untouched families.
const IMGS: &str = "![shot](/images/abcd1234.png) \
                    ![up](/api/uploads/c1/a.png) \
                    ![cfg](/api/config) \
                    ![abs](https://x.example/i.png) \
                    ![data](data:image/png;base64,QUJD) \
                    ![proto](//cdn.example/i.png)";
const IMGS_REWRITTEN: &str = "![shot](/r/home-dev/images/abcd1234.png) \
                              ![up](/r/home-dev/api/uploads/c1/a.png) \
                              ![cfg](/r/home-dev/api/config) \
                              ![abs](https://x.example/i.png) \
                              ![data](data:image/png;base64,QUJD) \
                              ![proto](//cdn.example/i.png)";

/// A full remote `AppState`: the remote's catalogue, selection P, pinned,
/// image URLs, and every "everything else" field populated.
fn remote_state() -> AppState {
    serde_json::from_value(json!({
        "chat": {
            "messages": [
                { "role": "agent", "content": IMGS, "timestamp": "2026-10-31T10:00:00Z" },
                { "role": "user", "content": "no images here", "timestamp": "2026-10-31T10:01:00Z" }
            ],
            "auto_scroll": true,
            "scroll_offset": 0
        },
        "todo": { "visible": false, "items": [] },
        "input": { "buffer": "" },
        "stats": {
            "total_input_tokens": 100, "total_output_tokens": 200,
            "total_api_calls": 3, "total_cost": 0.01, "model": "llama3"
        },
        "context": { "current_usage": 5000, "window_size": 8192, "compaction_threshold": 0.8 },
        "conversation": null,
        "preferences": { "theme": "dark", "font_size": 14, "wrap_lines": true },
        "selection": { "profile": P, "agent": "team" },
        "profile_pinned": true,
        "catalog": [
            { "name": P, "origin": "global", "overrides_global": false,
              "agents": [ { "name": "team", "model": "local", "subagents": [] } ],
              "color": "teal" }
        ],
        "is_running": true,
        "visible_tabs": [ "tab-1" ],
        "welcome": {
            "provider_name": "ollama", "model": "llama3", "max_tokens": 4096,
            "builtin_tools_count": 10, "mcp_tools_count": 1, "skills_count": 2,
            "searxng_enabled": false, "searxng_url": null,
            "cost_tracking_enabled": true, "compaction_enabled": true,
            "compaction_threshold": 0.8, "compaction_keep_recent": 4,
            "conversation_persistence_enabled": true, "cwd": "/tmp",
            "peakbot_version": "0.25.0"
        }
    }))
    .expect("fixture AppState parses")
}

// §6.3 item 1: root-relative image URLs are re-rooted under `/r/<L>/` —
// incl. the `/api/config` edge (§9-T6(a)); http(s)/data:/`//` untouched.
#[test]
fn localize_state_reroots_root_relative_image_urls() {
    let out = relay::localize_state(remote_state(), &view());
    let content = &out.chat.messages[0].content;
    assert!(
        content.contains("![shot](/r/home-dev/images/abcd1234.png)"),
        "the /images/ link is re-rooted: {content}"
    );
    assert!(
        content.contains("![up](/r/home-dev/api/uploads/c1/a.png)"),
        "the /api/uploads/ link is re-rooted: {content}"
    );
    assert!(
        content.contains("![cfg](/r/home-dev/api/config)"),
        "even /api/config is re-rooted: {content}"
    );
    assert!(
        content.contains("![abs](https://x.example/i.png)"),
        "https URLs are untouched: {content}"
    );
    assert!(
        content.contains("![data](data:image/png;base64,QUJD)"),
        "data: URLs are untouched: {content}"
    );
    assert!(
        content.contains("![proto](//cdn.example/i.png)"),
        "protocol-relative URLs are untouched: {content}"
    );
    assert!(
        !content.contains("![shot](/images/abcd1234.png)"),
        "the original link is gone: {content}"
    );
    // A message without images is byte-identical.
    assert_eq!(out.chat.messages[1].content, "no images here");
}

// §6.3 item 2: `catalog := view.catalog` (the local catalogue, both kinds),
// and entry L's `agents :=` the REMOTE catalogue's entry P's agents.
#[test]
fn localize_state_swaps_the_catalogue_and_copies_remote_agents_into_l() {
    let out = relay::localize_state(remote_state(), &view());
    let names: Vec<&str> = out.catalog.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec![P, "alt", L], "the local catalogue, in order");

    let l = out
        .catalog
        .iter()
        .find(|e| e.name == L)
        .expect("entry L exists");
    assert_eq!(
        l.agents,
        vec![agent_info("team")],
        "L's agents come from the remote catalogue's P"
    );
    // The other entries keep the local catalogue's agents.
    let alt = out
        .catalog
        .iter()
        .find(|e| e.name == "alt")
        .expect("alt exists");
    assert_eq!(alt.agents.len(), 2, "alt keeps its own agents");
}

// §6.3 item 2: an agent pin filters L's agents to `[pin]`.
#[test]
fn localize_state_filters_l_agents_to_the_pin() {
    let mut v = view();
    v.target_agent = Some("team".into());
    let out = relay::localize_state(remote_state(), &v);
    let l = out
        .catalog
        .iter()
        .find(|e| e.name == L)
        .expect("entry L exists");
    assert_eq!(
        l.agents,
        vec![agent_info("team")],
        "with a pin, L's agents are exactly [pin]"
    );
}

// §6.3 item 2: P missing from the remote catalogue ⇒ L's agents are empty.
#[test]
fn localize_state_leaves_l_agents_empty_when_the_remote_profile_is_missing() {
    let mut state = remote_state();
    state.catalog = Vec::new(); // the remote catalogue has no entry P
    let out = relay::localize_state(state, &view());
    let l = out
        .catalog
        .iter()
        .find(|e| e.name == L)
        .expect("entry L exists");
    assert!(l.agents.is_empty(), "P missing ⇒ no agents: {:?}", l.agents);
}

// §6.3 item 3: `selection.profile: P → L` (the agent is untouched).
#[test]
fn localize_state_rewrites_selection_profile_to_l() {
    let out = relay::localize_state(remote_state(), &view());
    assert_eq!(out.selection.profile, L, "P → L");
    assert_eq!(out.selection.agent, "team", "the agent is untouched");
}

// §6.3 item 4: `profile_pinned := false` (the local chip must be able to
// leave the remote tab).
#[test]
fn localize_state_clears_profile_pinned() {
    let out = relay::localize_state(remote_state(), &view());
    assert!(!out.profile_pinned, "the pin is cleared on a relayed state");
}

// §9-T6(a): EVERYTHING else is unchanged — a deep JSON compare with exactly
// the four locked changes applied to the input.
#[test]
fn localize_state_changes_exactly_the_four_locked_things() {
    let view = view();
    let out = relay::localize_state(remote_state(), &view);

    let mut expected = serde_json::to_value(remote_state()).expect("serialize input");
    // 1. image re-root
    expected["chat"]["messages"][0]["content"] = json!(IMGS_REWRITTEN);
    // 2. catalogue := the view's, with L's agents := the remote P's agents
    let mut catalog = serde_json::to_value(&view.catalog).expect("view catalogue");
    if let Some(arr) = catalog.as_array_mut()
        && let Some(l) = arr.iter_mut().find(|e| e["name"] == L)
    {
        l["agents"] = json!([ { "name": "team", "model": "local", "subagents": [] } ]);
    }
    expected["catalog"] = catalog;
    // 3. selection P → L
    expected["selection"]["profile"] = json!(L);
    // 4. pin cleared
    expected["profile_pinned"] = json!(false);
    // …and its DERIVED wire twin (`settings_available = !profile_pinned`).
    expected["settings_available"] = json!(true);

    assert_eq!(
        serde_json::to_value(&out).expect("serialize output"),
        expected,
        "localize_state changes exactly the four locked things"
    );
}

// §9-T6(b): the AppState re-encode fidelity property — the relay reparses
// and re-serializes the snapshot, so `ser(de(ser(s))) == ser(s)` must hold
// (guards against a lossy custom `Serialize`).
#[test]
fn app_state_round_trips_through_serde_without_field_loss() {
    let s = remote_state();
    let ser1 = serde_json::to_value(&s).expect("ser");
    let de: AppState = serde_json::from_value(ser1.clone()).expect("de");
    let ser2 = serde_json::to_value(&de).expect("ser again");
    assert_eq!(ser1, ser2, "ser(de(ser(s))) == ser(s) — no field loss");
}

// ── The table end to end: local /ws → relay → scripted fake remote ─────

/// Send one inbound frame through the full stack and assert the remote
/// received it BYTE-FOR-BYTE (raw-text forward, §5.3).
async fn forward_frame_verbatim(frame: Value) {
    let env = ready_env().await;
    let mut ws = ws_connect(env.local).await;
    ws_send(&mut ws, &attach_frame(None, Some(L), None)).await;
    wait_handshake(&mut ws).await;
    let sent = frame.to_string();
    ws_send(&mut ws, &frame).await;
    let got = wait_remote_frame(&env, |f| f == sent).await;
    assert_eq!(
        got, sent,
        "the frame must arrive at the remote byte-for-byte"
    );
}

#[tokio::test]
async fn send_message_reaches_the_remote_verbatim() {
    let _guard = env_guard();
    forward_frame_verbatim(json!({
        "type": "send_message",
        "text": "what's in this?",
        "attachments": ["5b7c1a2e-0000-4000-8000-000000000001"]
    }))
    .await;
}

#[tokio::test]
async fn stop_reaches_the_remote_verbatim() {
    let _guard = env_guard();
    forward_frame_verbatim(json!({ "type": "stop" })).await;
}

#[tokio::test]
async fn pause_reaches_the_remote_verbatim() {
    let _guard = env_guard();
    forward_frame_verbatim(json!({ "type": "pause" })).await;
}

#[tokio::test]
async fn resume_reaches_the_remote_verbatim() {
    let _guard = env_guard();
    forward_frame_verbatim(json!({ "type": "resume" })).await;
}

#[tokio::test]
async fn switch_model_reaches_the_remote_verbatim() {
    let _guard = env_guard();
    forward_frame_verbatim(json!({ "type": "switch_model", "alias": "local" })).await;
}

#[tokio::test]
async fn switch_cwd_reaches_the_remote_verbatim() {
    let _guard = env_guard();
    forward_frame_verbatim(json!({ "type": "switch_cwd", "path": "~/proj" })).await;
}

#[tokio::test]
async fn list_dir_reaches_the_remote_verbatim() {
    let _guard = env_guard();
    forward_frame_verbatim(json!({ "type": "list_dir", "path": "/tmp" })).await;
}

#[tokio::test]
async fn request_recent_dirs_reaches_the_remote_verbatim() {
    let _guard = env_guard();
    forward_frame_verbatim(json!({ "type": "request_recent_dirs" })).await;
}

// Outbound row 3: the remote's `models_available` is forwarded (the local
// server sends none of its own on a relay socket).
#[tokio::test]
async fn outbound_models_available_is_forwarded() {
    let _guard = env_guard();
    let env = env_replies_to(
        "stop",
        vec![json!({
            "type": "models_available",
            "active": "local",
            "models": [ { "alias": "local", "provider_name": "ollama",
                          "model_name": "llama3", "context_size": 8192 } ]
        })],
    )
    .await;
    let (_ws, frames) = drive_and_collect(&env, json!({ "type": "stop" })).await;
    let m = frames
        .iter()
        .find(|f| f["type"] == "models_available")
        .expect("models_available reached the local client");
    assert_eq!(m["active"], "local");
    assert_eq!(m["models"][0]["alias"], "local");
}

// Outbound row 4: `state` passes through LOCALIZED — the four changes on
// the wire, everything else intact.
#[tokio::test]
async fn outbound_state_is_localized_end_to_end() {
    let _guard = env_guard();
    let env = env_replies_to("send_message", vec![remote_state_frame()]).await;
    let (_ws, frames) =
        drive_and_collect(&env, json!({ "type": "send_message", "text": "hi" })).await;
    let st = frames
        .iter()
        .find(|f| f["type"] == "state")
        .expect("a state frame reached the local client");
    let s = &st["state"];

    // The four locked changes:
    assert_eq!(s["selection"]["profile"], L, "selection P → L");
    assert_eq!(s["profile_pinned"], false, "the pin is cleared");
    assert!(
        s["chat"]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("/r/home-dev/images/abcd1234.png"),
        "the image URL is re-rooted: {}",
        s["chat"]["messages"][0]["content"]
    );
    let catalog = s["catalog"].as_array().expect("the catalogue is an array");
    let l = catalog
        .iter()
        .find(|e| e["name"] == L)
        .expect("the local catalogue carries entry L");
    assert_eq!(
        l["agents"][0]["name"], "team",
        "L's agents come from the remote catalogue's P"
    );
    assert!(
        catalog.iter().any(|e| e["name"] == "dev"),
        "the local entries are in the view catalogue"
    );

    // Untouched:
    assert_eq!(s["selection"]["agent"], "team");
    assert_eq!(s["chat"]["messages"][1]["content"], "no images here");
    assert_eq!(s["welcome"]["peakbot_version"], "0.25.0");
    assert_eq!(s["visible_tabs"][0], "tab-1");
    assert_eq!(s["is_running"], true);
}

/// The remote's own state snapshot, as an outbound frame (a full
/// `AppState` — the relay reparses and re-serializes it).
fn remote_state_frame() -> Value {
    json!({
        "type": "state",
        "state": {
            "chat": {
                "messages": [
                    { "role": "agent", "content": "shot: ![shot](/images/abcd1234.png)",
                      "timestamp": "2026-10-31T10:00:00Z" },
                    { "role": "user", "content": "no images here",
                      "timestamp": "2026-10-31T10:01:00Z" }
                ],
                "auto_scroll": true,
                "scroll_offset": 0
            },
            "todo": { "visible": false, "items": [] },
            "input": { "buffer": "" },
            "stats": {
                "total_input_tokens": 100, "total_output_tokens": 200,
                "total_api_calls": 3, "total_cost": 0.01, "model": "llama3"
            },
            "context": { "current_usage": 5000, "window_size": 8192, "compaction_threshold": 0.8 },
            "conversation": null,
            "preferences": { "theme": "dark", "font_size": 14, "wrap_lines": true },
            "selection": { "profile": P, "agent": "team" },
            "profile_pinned": true,
            "catalog": [
                { "name": P, "origin": "global", "overrides_global": false,
                  "agents": [ { "name": "team", "model": "local", "subagents": [] } ],
                  "color": "teal" }
            ],
            "is_running": true,
            "visible_tabs": [ "tab-1" ],
            "welcome": {
                "provider_name": "ollama", "model": "llama3", "max_tokens": 4096,
                "builtin_tools_count": 10, "mcp_tools_count": 1, "skills_count": 2,
                "searxng_enabled": false, "searxng_url": null,
                "cost_tracking_enabled": true, "compaction_enabled": true,
                "compaction_threshold": 0.8, "compaction_keep_recent": 4,
                "conversation_persistence_enabled": true, "cwd": "/tmp",
                "peakbot_version": "0.25.0"
            }
        }
    })
}

// Outbound row 5: `conversations_list` is DROPPED (it is never solicited —
// rows 11/12 are intercepted) and the socket stays open.
#[tokio::test]
async fn outbound_conversations_list_is_dropped() {
    let _guard = env_guard();
    let env = env_replies_to(
        "stop",
        vec![json!({ "type": "conversations_list", "items": [
            { "id": "11111111-1111-1111-1111-111111111111", "name": "one",
              "updated_at": "2026-10-30T12:00:00Z", "message_count": 3,
              "model": "llama3", "active": true, "running": false,
              "profile": "dev" }
        ] })],
    )
    .await;
    let (mut ws, frames) = drive_and_collect(&env, json!({ "type": "stop" })).await;
    assert!(
        !frames.iter().any(|f| f["type"] == "conversations_list"),
        "conversations_list must be dropped: {frames:?}"
    );
    // The same socket is still live.
    ws_send(&mut ws, &json!({ "type": "pause" })).await;
    let _ = wait_remote_frame(&env, |f| f == r#"{"type":"pause"}"#).await;
}

// Outbound row 6: `recent_dirs` is forwarded.
#[tokio::test]
async fn outbound_recent_dirs_is_forwarded() {
    let _guard = env_guard();
    let env = env_replies_to(
        "stop",
        vec![json!({ "type": "recent_dirs", "dirs": ["/a", "/b"] })],
    )
    .await;
    let (_ws, frames) = drive_and_collect(&env, json!({ "type": "stop" })).await;
    let r = frames
        .iter()
        .find(|f| f["type"] == "recent_dirs")
        .expect("recent_dirs reached the local client");
    assert_eq!(r["dirs"], json!(["/a", "/b"]));
}

// Outbound row 7: `dir_listing` is forwarded.
#[tokio::test]
async fn outbound_dir_listing_is_forwarded() {
    let _guard = env_guard();
    let env = env_replies_to(
        "stop",
        vec![json!({
            "type": "dir_listing", "path": "/tmp", "parent": null,
            "entries": [ { "name": "a", "is_dir": true, "full_path": "/tmp/a" } ],
            "error": null
        })],
    )
    .await;
    let (_ws, frames) = drive_and_collect(&env, json!({ "type": "stop" })).await;
    let d = frames
        .iter()
        .find(|f| f["type"] == "dir_listing")
        .expect("dir_listing reached the local client");
    assert_eq!(d["entries"][0]["name"], "a");
}

// Outbound row 8: `error` is forwarded (after ready).
#[tokio::test]
async fn outbound_error_is_forwarded() {
    let _guard = env_guard();
    let env = env_replies_to(
        "stop",
        vec![json!({ "type": "error", "message": "the remote says no" })],
    )
    .await;
    let (_ws, frames) = drive_and_collect(&env, json!({ "type": "stop" })).await;
    let e = frames
        .iter()
        .find(|f| f["type"] == "error")
        .expect("the remote's error reached the local client");
    assert_eq!(e["message"], "the remote says no");
}

// Outbound row —: an unknown `type` is dropped (and the socket stays open).
#[tokio::test]
async fn outbound_unknown_type_is_dropped() {
    let _guard = env_guard();
    let env = env_replies_to("stop", vec![json!({ "type": "brand_new_frame", "x": 1 })]).await;
    let (mut ws, frames) = drive_and_collect(&env, json!({ "type": "stop" })).await;
    assert!(
        frames.iter().all(|f| f["type"] != "brand_new_frame"),
        "the unknown type is dropped: {frames:?}"
    );
    // The same socket is still live.
    ws_send(&mut ws, &json!({ "type": "pause" })).await;
    let _ = wait_remote_frame(&env, |f| f == r#"{"type":"pause"}"#).await;
}

// Inbound row 11 (E2E): `request_conversations` is never forwarded to the
// remote (it is answered locally — T7's merged list) and the socket stays
// open.
#[tokio::test]
async fn request_conversations_is_never_forwarded_to_the_remote() {
    let _guard = env_guard();
    let env = ready_env().await;
    let mut ws = drive(&env, Value::Null).await;
    ws_send(&mut ws, &json!({ "type": "request_conversations" })).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        env.ws_frames()
            .iter()
            .all(|f| !f.contains("request_conversations")),
        "request_conversations must never reach the remote: {:?}",
        env.ws_frames()
    );
    ws_send(&mut ws, &json!({ "type": "pause" })).await;
    let _ = wait_remote_frame(&env, |f| f == r#"{"type":"pause"}"#).await;
}

// Inbound row 12 (E2E): `kill_session` is never forwarded verbatim (T7
// routes it by profile) and the socket stays open.
#[tokio::test]
async fn kill_session_is_never_forwarded_to_the_remote() {
    let _guard = env_guard();
    let env = ready_env().await;
    let mut ws = drive(&env, Value::Null).await;
    ws_send(
        &mut ws,
        &json!({ "type": "kill_session", "convo": CONVO, "profile": L }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        env.ws_frames().iter().all(|f| !f.contains("kill_session")),
        "kill_session must never be forwarded verbatim: {:?}",
        env.ws_frames()
    );
    ws_send(&mut ws, &json!({ "type": "pause" })).await;
    let _ = wait_remote_frame(&env, |f| f == r#"{"type":"pause"}"#).await;
}

// Inbound row 10 (E2E): a select on L is rewritten to P before forwarding.
#[tokio::test]
async fn select_same_profile_is_rewritten_before_forwarding() {
    let _guard = env_guard();
    let env = ready_env().await;
    let mut ws = drive(&env, Value::Null).await;
    ws_send(
        &mut ws,
        &json!({
            "type": "select", "profile": L, "agent": "team",
            "model": "opus", "subagent_models": { "t": "haiku" }
        }),
    )
    .await;
    let got = wait_remote_frame(&env, |f| f.contains("\"select\"")).await;
    let v: Value = serde_json::from_str(&got).expect("the select frame is JSON");
    assert_eq!(v["type"], "select");
    assert_eq!(v["profile"], P, "L is rewritten to the remote target");
    assert_eq!(v["agent"], "team");
    assert_eq!(v["model"], "opus");
    assert_eq!(v["subagent_models"]["t"], "haiku");
}

// Inbound row 10 (E2E): a select on a foreign profile is refused locally
// with the locked chip message, and nothing reaches the remote.
#[tokio::test]
async fn select_other_profile_is_refused_end_to_end() {
    let _guard = env_guard();
    let env = ready_env().await;
    let mut ws = drive(&env, Value::Null).await;
    ws_send(
        &mut ws,
        &json!({ "type": "select", "profile": "alt", "agent": "a1" }),
    )
    .await;
    let msg = wait_error(&mut ws).await;
    assert_eq!(
        msg, "switch profiles from the chip",
        "the locked chip message"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        env.ws_frames().iter().all(|f| !f.contains("\"select\"")),
        "no select reached the remote: {:?}",
        env.ws_frames()
    );
}

// ═══════════════════════════════════════════════════════════════════════
// P3 — lifecycle (§5.7)
// ═══════════════════════════════════════════════════════════════════════

// Browser socket drops → the relay ends: the remote socket closes, and the
// remote session is NOT killed (no kill frame, a clean close).
#[tokio::test]
async fn local_client_disconnect_closes_the_remote_socket_without_killing_the_session() {
    let _guard = env_guard();
    let env = ready_env().await;
    let mut ws = ws_connect(env.local).await;
    ws_send(&mut ws, &attach_frame(None, Some(L), None)).await;
    wait_handshake(&mut ws).await;

    drop(ws);
    wait_remote_close(&env).await;

    let frames = env.ws_frames();
    assert!(
        frames.iter().all(|f| !f.contains("kill_session")),
        "the relay must not kill the remote session: {frames:?}"
    );
}

// The remote leg ends → E-LOST (the locked string, it really reconnects via
// the SPA) → the browser socket closes.
#[tokio::test]
async fn remote_socket_drop_is_elost_then_close() {
    let _guard = env_guard();
    let ca = test_support::mint_ca();
    let mut script = base_script();
    script.close_after = Some("stop".to_string()); // the remote drops after stop
    let env = relay_env(script, Some(&ca), None, &ca.fingerprint, T_REMOTE, T_REMOTE).await;

    let mut ws = ws_connect(env.local).await;
    ws_send(&mut ws, &attach_frame(None, Some(L), None)).await;
    wait_handshake(&mut ws).await;
    ws_send(&mut ws, &json!({ "type": "stop" })).await;

    let frames = frames_until_close(&mut ws).await;
    let errs: Vec<&Value> = frames.iter().filter(|f| f["type"] == "error").collect();
    assert_eq!(errs.len(), 1, "exactly one E-LOST error: {frames:?}");
    assert_eq!(
        errs[0]["message"],
        json!(elost()),
        "the locked E-LOST string"
    );
}

// Reconnect: a fresh local socket re-attaches with the SAME remote convo id
// → a fresh relay dials and re-attaches the remote with it.
#[tokio::test]
async fn reconnect_reattaches_the_same_remote_convo() {
    let _guard = env_guard();
    let env = ready_env().await;

    let mut first = ws_connect(env.local).await;
    ws_send(&mut first, &attach_frame(None, Some(L), None)).await;
    let convo = wait_handshake(&mut first).await;
    assert_eq!(convo, CONVO);
    drop(first);
    wait_remote_close(&env).await;

    // The SPA re-sends {convo: X, profile: L} — never dropping the profile.
    let mut second = ws_connect(env.local).await;
    ws_send(&mut second, &attach_frame(Some(CONVO), Some(L), None)).await;
    let convo2 = wait_handshake(&mut second).await;
    assert_eq!(convo2, CONVO, "the re-attach binds the same remote convo");

    let frames = env.ws_frames();
    let attaches: Vec<Value> = frames
        .iter()
        .filter_map(|f| serde_json::from_str::<Value>(f).ok())
        .filter(|v| v["type"] == "attach")
        .collect();
    assert_eq!(attaches.len(), 2, "two dials, two attaches: {frames:?}");
    assert_eq!(
        attaches[1]["convo"], CONVO,
        "the second attach names the same remote convo"
    );
    assert_eq!(attaches[1]["relayed"], true, "the re-attach is relayed");
    assert_eq!(
        attaches[1]["profile"], P,
        "the re-attach targets the remote profile"
    );
}
