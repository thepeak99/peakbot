//! Remote-profiles T4 — the remote-facing server surface (design §5.1, §5.6,
//! §6.2, §9 T4). RED until the implementation lands.
//!
//! Written against the interface the design specifies, NOT an existing impl:
//!
//! * `OutboundMessage::Ready { v }` — `handle_socket` sends `v: PROTOCOL_VERSION`.
//! * `InboundMessage::Attach { convo, profile, agent, relayed }` handled by
//!   `handle_socket` for **local** targets: "ensure" the selection on a
//!   session with no turns; unknown profile/agent, `--profile` pin conflict
//!   and chaining (`relayed` + remote profile) answer one `error` frame and
//!   close.
//! * A new child module `super::conversations` exporting
//!   `pub(crate) fn router(state: WsState) -> axum::Router` which serves
//!   `GET /api/conversations` and `POST /api/conversations/{convo}/kill`.
//!   It is merged BEFORE the token layer in `WebUi::run`, so the tests wrap
//!   it in the same `require_token` layer.
//!
//! Harness: a real `SessionRegistry` over offline `test_support` deps (Ollama
//! provider, `InMemoryStorage`), a real axum `/ws` + `conversations::router`
//! on loopback ports, `tokio-tungstenite` as the WS client and `reqwest` for
//! HTTP. Sessions load a FRESH config from `XDG_CONFIG_HOME`, so every test
//! points it at a tempdir under `CONFIG_ENV_LOCK`.

use super::*;
use crate::config::CONFIG_ENV_LOCK;
use crate::config::resolve::{Config, ParseCtx, Source};
use crate::conversation::Conversation;
use crate::session::{SessionDeps, test_support};
use crate::storage::{ConversationStorage, InMemoryStorage};
use chrono::{TimeZone, Utc};
use futures::SinkExt;
use serde_json::{Value, json};
use std::ffi::OsString;
use tokio_tungstenite::tungstenite::Message as TMsg;

// ── fixtures ────────────────────────────────────────────────────────────

/// Two local profiles on one offline Ollama model. `dev` is first, so it is
/// the default; `alt` has two agents so "first agent" and "chosen agent" are
/// distinguishable.
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

/// `LOCAL_YAML` + a REMOTE profile (needs T3's `remote:` parse).
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

const TOKEN: &str = "t4-secret";

/// Points `XDG_CONFIG_HOME` at a tempdir holding `yaml` as the global config,
/// under the crate-wide env lock; restores on drop.
struct XdgHome {
    // Drop order: restore env, delete dir, release lock.
    prev: Option<OsString>,
    _dir: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl XdgHome {
    fn new(yaml: &str) -> Self {
        let lock = CONFIG_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().expect("XDG tempdir");
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let cfg_dir = crate::config::get_config_dir().expect("config dir under XDG_CONFIG_HOME");
        std::fs::create_dir_all(&cfg_dir).expect("config dir");
        std::fs::write(cfg_dir.join("config.yaml"), yaml).expect("write global config");
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

struct Harness {
    _home: XdgHome,
    registry: SessionRegistry,
    storage: Arc<InMemoryStorage>,
    state: WsState,
}

fn harness(yaml: &str, pin: Option<&str>) -> Harness {
    let home = XdgHome::new(yaml);
    let storage = Arc::new(InMemoryStorage::new());
    let mut deps = test_support::test_deps(Some(storage.clone() as Arc<dyn ConversationStorage>));
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
    deps.profile_pin = pin.map(str::to_string);
    let deps: Arc<SessionDeps> = Arc::new(deps);
    let registry = SessionRegistry::new(deps.clone());
    let state = WsState {
        registry: Some(registry.clone()),
        models: Arc::new(Vec::new()),
        active_alias: "local".into(),
        session_ttl: Duration::from_secs(3600),
        uploads: deps.uploads.clone(),
    };
    Harness {
        _home: home,
        registry,
        storage,
        state,
    }
}

async fn serve(app: Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    addr
}

async fn spawn_ws(h: &Harness) -> std::net::SocketAddr {
    serve(
        Router::new()
            .route("/ws", get(ws_handler))
            .with_state(h.state.clone()),
    )
    .await
}

/// The conversations router wrapped in the SAME token layer `WebUi::run`
/// applies to everything merged before it.
async fn spawn_api(h: &Harness) -> std::net::SocketAddr {
    let app = conversations::router(h.state.clone());
    let app = app.layer(from_fn_with_state(Arc::<str>::from(TOKEN), require_token));
    serve(app).await
}

// ── WS client helpers ───────────────────────────────────────────────────

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
        .expect("ws send");
}

/// Next JSON text frame within `secs`; `Err(true)` = closed, `Err(false)` = timeout.
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

fn sel(frame: &Value) -> Option<(String, String)> {
    if frame["type"] != "state" {
        return None;
    }
    let s = &frame["state"]["selection"];
    Some((
        s["profile"].as_str()?.to_string(),
        s["agent"].as_str()?.to_string(),
    ))
}

/// Read frames until a `state` carries `(profile, agent)`; panics with the
/// last selection seen if it never does.
async fn wait_selection(ws: &mut Ws, profile: &str, agent: &str) {
    let mut last = None;
    for _ in 0..200 {
        match next_frame(ws, 15).await {
            Ok(f) => {
                if f["type"] == "error" {
                    panic!("unexpected error frame while waiting for selection: {f}");
                }
                if let Some(s) = sel(&f) {
                    if s == (profile.to_string(), agent.to_string()) {
                        return;
                    }
                    last = Some(s);
                }
            }
            Err(closed) => panic!(
                "socket {} before selection ({profile},{agent}) arrived; last = {last:?}",
                if closed { "closed" } else { "timed out" }
            ),
        }
    }
    panic!("selection ({profile},{agent}) never arrived; last = {last:?}");
}

/// Read until an `error` frame; returns its message. Panics on timeout/close.
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

/// Attach frame helper.
fn attach(convo: Option<&str>, profile: Option<&str>, agent: Option<&str>, relayed: bool) -> Value {
    let mut v = json!({"type": "attach", "convo": convo});
    if let Some(p) = profile {
        v["profile"] = json!(p);
    }
    if let Some(a) = agent {
        v["agent"] = json!(a);
    }
    if relayed {
        v["relayed"] = json!(true);
    }
    v
}

// ═══ Part B1 — attach { profile, agent } on a LOCAL target (§5.1) ═══════

/// `ready` carries `v == PROTOCOL_VERSION` on the live handshake (frame order
/// stays attached → ready).
#[tokio::test]
async fn handshake_ready_frame_carries_protocol_version() {
    let h = harness(LOCAL_YAML, None);
    let addr = spawn_ws(&h).await;
    let mut ws = ws_connect(addr).await;
    ws_send(&mut ws, attach(None, None, None, false)).await;

    let first = next_frame(&mut ws, 15).await.expect("attached");
    assert_eq!(first["type"], "attached", "first frame: {first}");
    let second = next_frame(&mut ws, 15).await.expect("ready");
    assert_eq!(second["type"], "ready", "second frame: {second}");
    assert_eq!(
        second["v"],
        json!(crate::ui::wire::PROTOCOL_VERSION),
        "ready must carry v == PROTOCOL_VERSION (exact-match input for the relay)"
    );
}

/// A fresh conversation attached with `{profile, agent}` is bound to that
/// pair ("ensure" → `UiAction::Select`).
#[tokio::test]
async fn attach_new_convo_with_profile_and_agent_binds_selection() {
    let h = harness(LOCAL_YAML, None);
    let addr = spawn_ws(&h).await;
    let mut ws = ws_connect(addr).await;
    ws_send(&mut ws, attach(None, Some("alt"), Some("a2"), false)).await;
    wait_selection(&mut ws, "alt", "a2").await;
}

/// `profile` without `agent` selects that profile's FIRST agent.
#[tokio::test]
async fn attach_new_convo_with_profile_only_binds_first_agent() {
    let h = harness(LOCAL_YAML, None);
    let addr = spawn_ws(&h).await;
    let mut ws = ws_connect(addr).await;
    ws_send(&mut ws, attach(None, Some("alt"), None, false)).await;
    wait_selection(&mut ws, "alt", "a1").await;
}

/// Legacy attach (no new fields) still mints a session on the default
/// profile — nothing is selected away from `dev/team`.
#[tokio::test]
async fn legacy_attach_without_profile_keeps_the_default_selection() {
    let h = harness(LOCAL_YAML, None);
    let addr = spawn_ws(&h).await;
    let mut ws = ws_connect(addr).await;
    ws_send(&mut ws, json!({"type": "attach", "convo": null})).await;
    wait_selection(&mut ws, "dev", "team").await;
    assert_eq!(h.registry.active_ids().len(), 1, "one session minted");
}

/// "Ensure" is idempotent: reconnecting to the live convo with the same
/// profile and NO agent, or with the same agent, must not reset a chosen
/// agent (the chip choice survives a reconnect).
#[tokio::test]
async fn attach_ensure_does_not_reset_the_chosen_agent_on_reconnect() {
    let h = harness(LOCAL_YAML, None);
    let addr = spawn_ws(&h).await;

    let mut first = ws_connect(addr).await;
    ws_send(&mut first, attach(None, Some("alt"), Some("a2"), false)).await;
    let convo = loop {
        let f = next_frame(&mut first, 15).await.expect("attached frame");
        if f["type"] == "attached" {
            break f["convo"].as_str().expect("convo id").to_string();
        }
    };
    wait_selection(&mut first, "alt", "a2").await;

    // Reconnect #1: same profile, agent absent → no-op, NOT "first agent".
    let mut second = ws_connect(addr).await;
    ws_send(&mut second, attach(Some(&convo), Some("alt"), None, false)).await;
    let frames = collect_for(&mut second, 1500).await;
    let sels: Vec<_> = frames.iter().filter_map(sel).collect();
    assert!(!sels.is_empty(), "the reconnect must receive a state frame");
    for s in &sels {
        assert_eq!(
            s,
            &("alt".to_string(), "a2".to_string()),
            "profile-only reconnect reset the chosen agent"
        );
    }
    assert!(
        frames.iter().all(|f| f["type"] != "error"),
        "an ensure no-op must not error: {frames:?}"
    );

    // Reconnect #2: explicit same agent → also a no-op.
    let mut third = ws_connect(addr).await;
    ws_send(
        &mut third,
        attach(Some(&convo), Some("alt"), Some("a2"), false),
    )
    .await;
    let frames = collect_for(&mut third, 1500).await;
    assert!(
        frames
            .iter()
            .filter_map(sel)
            .all(|s| s == ("alt".into(), "a2".into())),
        "explicit same-agent reconnect changed the selection: {frames:?}"
    );
}

/// A conversation that already has turns ignores `attach.profile`/`agent`:
/// the persisted selection governs, and there is no error or lock message —
/// even for a profile that would otherwise be refused (unknown / pinned).
#[tokio::test]
async fn attach_on_a_convo_with_turns_ignores_the_profile() {
    let h = harness(LOCAL_YAML, None);
    let mut c = Conversation::new(
        "started".to_string(),
        "ollama".to_string(),
        "llama3".to_string(),
        "/tmp".to_string(),
    );
    c.profile = Some("dev".to_string());
    c.agent = Some("team".to_string());
    c.add_user_message("hi".to_string());
    c.add_assistant_message("hello".to_string());
    h.storage.save(&c).unwrap();
    let addr = spawn_ws(&h).await;

    for (profile, agent) in [("alt", Some("a2")), ("nope", None)] {
        let mut ws = ws_connect(addr).await;
        ws_send(
            &mut ws,
            attach(Some(&c.id.to_string()), Some(profile), agent, false),
        )
        .await;
        let frames = collect_for(&mut ws, 1500).await;
        assert!(
            frames.iter().all(|f| f["type"] != "error"),
            "attach profile '{profile}' on a started convo must not error or lock-message: {frames:?}"
        );
        let sels: Vec<_> = frames.iter().filter_map(sel).collect();
        assert!(!sels.is_empty(), "the attach must receive a state frame");
        assert!(
            !Value::Array(frames.clone()).to_string().contains("locked"),
            "no lock message may reach the chat"
        );
        for s in &sels {
            assert_eq!(
                s,
                &("dev".to_string(), "team".to_string()),
                "the persisted selection must govern"
            );
        }
    }
}

/// An unknown profile → one `error` frame with the locked E-UNKNOWN-PROFILE
/// string, then the socket closes.
#[tokio::test]
async fn attach_with_unknown_profile_errors_and_closes() {
    let h = harness(LOCAL_YAML, None);
    let addr = spawn_ws(&h).await;
    let mut ws = ws_connect(addr).await;
    ws_send(&mut ws, attach(None, Some("nope"), None, false)).await;
    let msg = wait_error(&mut ws).await;
    assert_eq!(
        msg, "Unknown profile 'nope'.",
        "locked E-UNKNOWN-PROFILE text"
    );
    assert_closes(&mut ws).await;
}

/// A known profile with an unknown agent is refused too (error naming the
/// agent) and never binds the bogus pair.
#[tokio::test]
async fn attach_with_unknown_agent_errors_and_closes() {
    let h = harness(LOCAL_YAML, None);
    let addr = spawn_ws(&h).await;
    let mut ws = ws_connect(addr).await;
    ws_send(&mut ws, attach(None, Some("alt"), Some("ghost"), false)).await;
    let msg = wait_error(&mut ws).await;
    assert!(
        msg.contains("ghost"),
        "the error must name the unknown agent, got: {msg:?}"
    );
    assert_closes(&mut ws).await;
}

/// `--profile X` pin + attach naming another profile → locked E-PINNED text,
/// then close.
#[tokio::test]
async fn attach_conflicting_with_profile_pin_errors_and_closes() {
    let h = harness(LOCAL_YAML, Some("dev"));
    let addr = spawn_ws(&h).await;
    let mut ws = ws_connect(addr).await;
    ws_send(&mut ws, attach(None, Some("alt"), None, false)).await;
    let msg = wait_error(&mut ws).await;
    assert_eq!(
        msg, "This server is pinned to profile 'dev' (--profile).",
        "locked E-PINNED text"
    );
    assert_closes(&mut ws).await;
}

/// Under a pin, attaching the pinned profile itself is fine (no error, stays
/// on the pinned selection).
#[tokio::test]
async fn attach_naming_the_pinned_profile_is_accepted() {
    let h = harness(LOCAL_YAML, Some("dev"));
    let addr = spawn_ws(&h).await;
    let mut ws = ws_connect(addr).await;
    ws_send(&mut ws, attach(None, Some("dev"), None, false)).await;
    wait_selection(&mut ws, "dev", "team").await;
}

/// `relayed: true` + a LOCAL profile is a normal attach (the marker only
/// forbids chaining).
#[tokio::test]
async fn relayed_attach_naming_a_local_profile_binds_normally() {
    let h = harness(LOCAL_YAML, None);
    let addr = spawn_ws(&h).await;
    let mut ws = ws_connect(addr).await;
    ws_send(&mut ws, attach(None, Some("alt"), Some("a2"), true)).await;
    wait_selection(&mut ws, "alt", "a2").await;
}

/// Chaining is blocked: a relayed attach that names a REMOTE profile on this
/// server → E-CHAIN error + close, and no session is minted (the registry is
/// never touched).
///
/// Depends on T3 (`remote:` profile kind) parsing in the fixture.
#[tokio::test]
async fn relayed_attach_naming_a_remote_profile_is_refused_as_chaining() {
    let h = harness(WITH_REMOTE_YAML, None);
    let addr = spawn_ws(&h).await;
    let mut ws = ws_connect(addr).await;
    ws_send(&mut ws, attach(None, Some("home-dev"), None, true)).await;
    let msg = wait_error(&mut ws).await;
    assert_eq!(
        msg,
        "profile 'home-dev' on this server is itself a remote profile — \
         chaining remote profiles isn't supported",
        "E-CHAIN text (design §5.9)"
    );
    assert_closes(&mut ws).await;
    assert!(
        h.registry.active_ids().is_empty(),
        "E-CHAIN must return before registry.attach — no session may exist"
    );
}

// ═══ Part B2 — GET /api/conversations (§6.2) ═══════════════════════════

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn cookie() -> String {
    format!("peakbot_token={TOKEN}")
}

/// Save a conversation row with a fixed `updated_at` (no wall clock).
fn seed(h: &Harness, id: Option<Uuid>, name: &str, profile: Option<&str>, day: u32) -> Uuid {
    let mut c = Conversation::new(
        name.to_string(),
        "ollama".to_string(),
        "llama3".to_string(),
        "/tmp".to_string(),
    );
    if let Some(id) = id {
        c.id = id;
    }
    c.profile = profile.map(str::to_string);
    c.updated_at = Utc.with_ymd_and_hms(2026, 1, day, 12, 0, 0).unwrap();
    h.storage.save(&c).unwrap();
    c.id
}

#[tokio::test]
async fn list_conversations_without_token_is_401() {
    let h = harness(LOCAL_YAML, None);
    let addr = spawn_api(&h).await;
    let resp = client()
        .get(format!("http://{addr}/api/conversations"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "GET /api/conversations is token-gated");
}

#[tokio::test]
async fn list_conversations_with_wrong_token_is_401() {
    let h = harness(LOCAL_YAML, None);
    let addr = spawn_api(&h).await;
    let resp = client()
        .get(format!("http://{addr}/api/conversations"))
        .header("cookie", "peakbot_token=wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

/// `200 {"items":[ConversationSummaryWire]}`: newest first, `profile`
/// carried (null for legacy rows), `active`/`running` false, and the call
/// creates NO session.
#[tokio::test]
async fn list_conversations_with_token_returns_local_rows_with_profile() {
    let h = harness(LOCAL_YAML, None);
    let older = seed(&h, None, "older", None, 1);
    let newer = seed(&h, None, "newer", Some("dev"), 2);
    let addr = spawn_api(&h).await;

    let resp = client()
        .get(format!("http://{addr}/api/conversations"))
        .header("cookie", cookie())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = serde_json::from_str(&resp.text().await.unwrap()).expect("JSON body");
    let items = body["items"].as_array().expect("`items` array");
    assert_eq!(items.len(), 2, "body: {body}");
    assert_eq!(items[0]["id"], newer.to_string(), "newest first");
    assert_eq!(items[1]["id"], older.to_string());
    for k in [
        "id",
        "name",
        "updated_at",
        "message_count",
        "model",
        "active",
        "running",
    ] {
        assert!(
            items[0].get(k).is_some(),
            "row is missing `{k}`: {}",
            items[0]
        );
    }
    assert_eq!(items[0]["profile"], "dev", "profile tag is on the row");
    assert!(items[1]["profile"].is_null(), "legacy row ⇒ profile null");
    assert_eq!(items[0]["active"], false);
    assert_eq!(items[0]["running"], false);
    assert!(
        body.get("error").is_none(),
        "a plain list carries no `error` key: {body}"
    );
    assert!(
        h.registry.active_ids().is_empty(),
        "GET /api/conversations must not create or bind a session"
    );
}

/// The registry's flags ride on the rows: `active` for a live session,
/// `running` when its turn is in flight.
#[tokio::test]
async fn list_conversations_reports_active_and_running_flags() {
    let h = harness(LOCAL_YAML, None);
    let reg = h.registry.attach(None).expect("mint a session");
    let live = reg.live_convo().expect("live convo");
    seed(&h, Some(live), "live", Some("dev"), 3);
    reg.session.state_manager.set_running(true);
    let addr = spawn_api(&h).await;

    let body: Value = serde_json::from_str(
        &client()
            .get(format!("http://{addr}/api/conversations"))
            .header("cookie", cookie())
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
    )
    .unwrap();
    let row = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == live.to_string())
        .unwrap_or_else(|| panic!("live convo missing from {body}"));
    assert_eq!(row["active"], true);
    assert_eq!(row["running"], true);
}

/// Empty storage ⇒ `200 {"items":[]}`, not an error.
#[tokio::test]
async fn list_conversations_is_empty_array_when_nothing_is_saved() {
    let h = harness(LOCAL_YAML, None);
    let addr = spawn_api(&h).await;
    let resp = client()
        .get(format!("http://{addr}/api/conversations?token={TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = serde_json::from_str(&resp.text().await.unwrap()).unwrap();
    assert_eq!(body["items"], json!([]));
}

// ═══ Part B3 — POST /api/conversations/{convo}/kill (§6.2) ═════════════

fn kill_url(addr: std::net::SocketAddr, convo: &str) -> String {
    format!("http://{addr}/api/conversations/{convo}/kill")
}

#[tokio::test]
async fn kill_without_token_is_401_and_kills_nothing() {
    let h = harness(LOCAL_YAML, None);
    let reg = h.registry.attach(None).unwrap();
    let live = reg.live_convo().unwrap();
    let addr = spawn_api(&h).await;
    let resp = client()
        .post(kill_url(addr, &live.to_string()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "kill is token-gated");
    assert!(
        h.registry.active_ids().contains(&live),
        "an unauthenticated kill must not touch the session"
    );
}

/// Idle live session → 200 `{"items":[…]}` with NO `error` key, the session
/// gone from the registry and its row inactive.
#[tokio::test]
async fn kill_idle_session_returns_200_list_without_error() {
    let h = harness(LOCAL_YAML, None);
    let reg = h.registry.attach(None).unwrap();
    let live = reg.live_convo().unwrap();
    seed(&h, Some(live), "victim", Some("dev"), 4);
    let addr = spawn_api(&h).await;

    let resp = client()
        .post(kill_url(addr, &live.to_string()))
        .header("cookie", cookie())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = serde_json::from_str(&resp.text().await.unwrap()).unwrap();
    assert!(
        body.get("error").is_none(),
        "granted kill ⇒ no error: {body}"
    );
    let row = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|r| r["id"] == live.to_string())
        .expect("killed conversation stays listed");
    assert_eq!(row["active"], false, "row reflects the kill");
    assert!(!h.registry.active_ids().contains(&live), "session removed");
}

/// Mid-turn kill is refused with the SAME `KILL_RUNNING_MSG` the WS frame
/// uses; status stays 200 and the session survives.
#[tokio::test]
async fn kill_running_session_is_refused_with_kill_running_msg() {
    let h = harness(LOCAL_YAML, None);
    let reg = h.registry.attach(None).unwrap();
    let live = reg.live_convo().unwrap();
    seed(&h, Some(live), "busy", Some("dev"), 5);
    reg.session.state_manager.set_running(true);
    let addr = spawn_api(&h).await;

    let resp = client()
        .post(kill_url(addr, &live.to_string()))
        .header("cookie", cookie())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = serde_json::from_str(&resp.text().await.unwrap()).unwrap();
    assert_eq!(body["error"], KILL_RUNNING_MSG, "body: {body}");
    assert!(body["items"].is_array(), "the list is still returned");
    assert!(
        h.registry.active_ids().contains(&live),
        "a refused kill must leave the session alive"
    );
}

/// A well-formed UUID with no live session is the registry's `NotLive`: same
/// as the WS frame, a plain 200 list with no error.
#[tokio::test]
async fn kill_unknown_but_valid_uuid_is_200_list_without_error() {
    let h = harness(LOCAL_YAML, None);
    seed(&h, None, "bystander", None, 6);
    let addr = spawn_api(&h).await;
    let resp = client()
        .post(kill_url(addr, &Uuid::new_v4().to_string()))
        .header("cookie", cookie())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = serde_json::from_str(&resp.text().await.unwrap()).unwrap();
    assert!(
        body.get("error").is_none(),
        "NotLive is not an error: {body}"
    );
    assert_eq!(body["items"].as_array().map(Vec::len), Some(1));
}

/// A malformed id → 400 (and nothing is killed).
#[tokio::test]
async fn kill_with_malformed_uuid_is_400() {
    let h = harness(LOCAL_YAML, None);
    let reg = h.registry.attach(None).unwrap();
    let live = reg.live_convo().unwrap();
    let addr = spawn_api(&h).await;
    for bad in ["not-a-uuid", "123", "zzzzzzzz-zzzz-zzzz-zzzz-zzzzzzzzzzzz"] {
        let resp = client()
            .post(kill_url(addr, bad))
            .header("cookie", cookie())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "`{bad}` must be a 400");
    }
    assert!(h.registry.active_ids().contains(&live));
}
