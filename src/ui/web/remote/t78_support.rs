//! Shared fixture for the remote-profiles T7 (merged list + kill routing)
//! and T8 (`/r/` proxy) tests. Test-only.
//!
//! * [`FakeRemote`] — a plain-HTTP LOOPBACK remote (RP3 allows `http://127.0.0.1`
//!   without a `ca`, so no TLS minting is needed). It records every request
//!   (via `test_support::Recorder`) and every request BODY, gates on the
//!   `peakbot_token` cookie like a real PeakBot, serves a scripted
//!   `GET /api/conversations` / `POST /api/conversations/{id}/kill`, canned
//!   responses for any other `METHOD /path`, and a minimal `/ws` (attached +
//!   `ready{v}` then drain) so a RELAY socket can be attached to it.
//! * [`LocalEnv`] — the REAL local server pieces behind the SAME
//!   `require_token` layer `WebUi::run` applies: `/ws` (`ws_handler`) and
//!   `GET /api/conversations` + `POST …/kill` (`conversations::router`), plus
//!   an optional extra router (T8 passes `proxy::router()`), over a global
//!   `config.yaml` under `XDG_CONFIG_HOME` that lists the remote profiles.
//! * thin WS-client helpers for the browser's socket.

#![allow(dead_code)] // shared by t7_tests / t8_tests: each uses a subset
#![allow(clippy::await_holding_lock)] // XdgHome holds CONFIG_ENV_LOCK across awaits

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::extract::ws::{Message as AxMsg, WebSocketUpgrade};
use axum::http::{Request, StatusCode};
use axum::middleware::{self, Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message as TMsg;
use uuid::Uuid;

use super::WsStream;
use super::test_support::{self, Captured, Recorder, XdgHome};
use crate::config::resolve::{Config, ParseCtx, Source};
use crate::conversation::Conversation;
use crate::session::{SessionDeps, test_support as deps_ts};
use crate::storage::{ConversationStorage, InMemoryStorage};
use crate::ui::web::registry::SessionRegistry;
use crate::ui::web::{WsState, conversations, require_token, ws_handler};
use crate::ui::wire::PROTOCOL_VERSION;

/// The LOCAL web token — must never reach a remote.
pub const T_LOCAL: &str = "LOCAL-TOKEN-T78-5e1f";
/// A remote's token — travels only as `Cookie: peakbot_token=<it>`.
pub const T_REMOTE: &str = "remote-token-t78-9a2b";
/// The conversation id the fake remote's `/ws` reports in `attached`.
pub const RELAY_CONVO: &str = "77777777-7777-7777-7777-777777777777";

/// A deterministic UUID: `uid(7)` = `00000007-0000-4000-8000-000000000000`.
pub fn uid(n: u32) -> String {
    format!("{n:08x}-0000-4000-8000-000000000000")
}

/// A wire conversation row exactly as a remote's `GET /api/conversations`
/// serialises it (`ConversationSummaryWire`).
pub fn row(id: &str, name: &str, updated_at: &str, profile: Option<&str>) -> Value {
    json!({
        "id": id, "name": name, "updated_at": updated_at,
        "message_count": 3, "model": "m", "active": false, "running": false,
        "profile": profile,
    })
}

/// [`row`] with explicit live flags (the remote registry's `active` / `running`).
pub fn row_live(
    id: &str,
    name: &str,
    updated_at: &str,
    profile: Option<&str>,
    active: bool,
    running: bool,
) -> Value {
    let mut r = row(id, name, updated_at, profile);
    r["active"] = json!(active);
    r["running"] = json!(running);
    r
}

/// A refused address: a port that was free a moment ago and is now closed.
pub fn dead_url() -> String {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("port probe");
    let port = probe.local_addr().expect("port").port();
    drop(probe);
    format!("http://127.0.0.1:{port}")
}

// ── The fake remote ─────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub enum ListMode {
    /// `200 {"items": rows}`.
    Rows,
    /// Accept the request and never answer.
    Hang,
    /// Answer this status with a text body.
    Status(u16),
    /// `200` with a body that is not JSON.
    Garbage,
}

/// A canned HTTP response for `METHOD /path`.
#[derive(Clone)]
pub struct Canned {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// `(n, gate)`: send `body[..n]`, then wait for `gate`, then the rest —
    /// proves a proxy streams instead of buffering the whole body.
    pub gate: Option<(usize, Arc<Notify>)>,
}

impl Canned {
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
            gate: None,
        }
    }
    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.to_string(), v.to_string()));
        self
    }
    pub fn gated(mut self, first_len: usize, gate: Arc<Notify>) -> Self {
        self.gate = Some((first_len, gate));
        self
    }
    fn into_response(self) -> Response {
        let mut b = Response::builder().status(self.status);
        for (k, v) in &self.headers {
            b = b.header(k.as_str(), v.as_str());
        }
        let body = match self.gate {
            None => Body::from(self.body),
            Some((n, gate)) => {
                let first = self.body[..n].to_vec();
                let rest = self.body[n..].to_vec();
                let stream = futures::stream::unfold(
                    (0u8, first, rest, gate),
                    |(step, first, rest, gate)| async move {
                        match step {
                            0 => Some((
                                Ok::<Bytes, std::convert::Infallible>(Bytes::from(first.clone())),
                                (1, first, rest, gate),
                            )),
                            1 => {
                                gate.notified().await;
                                Some((Ok(Bytes::from(rest.clone())), (2, first, rest, gate)))
                            }
                            _ => None,
                        }
                    },
                );
                Body::from_stream(stream)
            }
        };
        b.body(body).expect("canned response")
    }
}

struct FakeState {
    token: String,
    rows: Mutex<Vec<Value>>,
    list: Mutex<ListMode>,
    kill_error: Mutex<Option<String>>,
    kills: Mutex<Vec<String>>,
    canned: Mutex<HashMap<String, Canned>>,
    /// `("METHOD uri", body bytes)` of every request.
    bodies: Mutex<Vec<(String, Vec<u8>)>>,
    ws_frames: Mutex<Vec<String>>,
    /// Fired (stored permit) when the first body chunk of `POST /api/uploads`
    /// arrives — before the rest of the body is read.
    upload_first: Notify,
}

pub struct FakeRemote {
    /// `http://127.0.0.1:<port>`.
    pub url: String,
    pub token: String,
    pub rec: Recorder,
    st: Arc<FakeState>,
}

fn json_response(status: u16, v: &Value) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(v.to_string()))
        .expect("json response")
}

async fn gate(State(st): State<Arc<FakeState>>, req: Request<Body>, next: Next) -> Response {
    if test_support::cookie_token(&req).as_deref() != Some(st.token.as_str()) {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    next.run(req).await
}

async fn handler(State(st): State<Arc<FakeState>>, req: Request<Body>) -> Response {
    let method = req.method().to_string();
    let uri = req.uri().to_string();
    let path = req.uri().path().to_string();
    let bytes = if method == "POST" && path == "/api/uploads" {
        // Frame by frame, so a test can see the first chunk arrive while the
        // sender is still mid-body (proves the proxy streams).
        let mut stream = req.into_body().into_data_stream();
        let mut buf = Vec::new();
        let mut first = true;
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            if first && !chunk.is_empty() {
                first = false;
                st.upload_first.notify_one();
            }
            buf.extend_from_slice(&chunk);
        }
        Bytes::from(buf)
    } else {
        match axum::body::to_bytes(req.into_body(), usize::MAX).await {
            Ok(b) => b,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        }
    };
    st.bodies
        .lock()
        .expect("bodies lock")
        .push((format!("{method} {uri}"), bytes.to_vec()));

    if method == "GET" && path == "/api/conversations" {
        let mode = *st.list.lock().expect("list lock");
        return match mode {
            ListMode::Rows => {
                let rows = st.rows.lock().expect("rows lock").clone();
                json_response(200, &json!({ "items": rows }))
            }
            ListMode::Hang => {
                std::future::pending::<()>().await;
                unreachable!()
            }
            ListMode::Status(code) => (
                StatusCode::from_u16(code).expect("status"),
                "fake remote: list failed",
            )
                .into_response(),
            ListMode::Garbage => (StatusCode::OK, "this is not json").into_response(),
        };
    }
    if method == "POST" && path.starts_with("/api/conversations/") && path.ends_with("/kill") {
        let id = path
            .trim_start_matches("/api/conversations/")
            .trim_end_matches("/kill")
            .to_string();
        st.kills.lock().expect("kills lock").push(id.clone());
        let err = st.kill_error.lock().expect("kill_error lock").clone();
        let mut rows = st.rows.lock().expect("rows lock");
        if err.is_none() {
            for r in rows.iter_mut() {
                if r["id"] == id {
                    r["active"] = json!(false);
                    r["running"] = json!(false);
                }
            }
        }
        let mut body = json!({ "items": rows.clone() });
        if let Some(e) = err {
            body["error"] = json!(e);
        }
        return json_response(200, &body);
    }

    let canned = st
        .canned
        .lock()
        .expect("canned lock")
        .get(&format!("{method} {path}"))
        .cloned();
    match canned {
        Some(c) => c.into_response(),
        None => (StatusCode::NOT_FOUND, "fake remote: nothing here").into_response(),
    }
}

async fn ws_route(ws: WebSocketUpgrade, State(st): State<Arc<FakeState>>) -> Response {
    ws.on_upgrade(move |mut socket| async move {
        let Some(Ok(AxMsg::Text(first))) = socket.recv().await else {
            return;
        };
        st.ws_frames
            .lock()
            .expect("ws lock")
            .push(first.to_string());
        let _ = socket
            .send(AxMsg::Text(
                format!(r#"{{"type":"attached","convo":"{RELAY_CONVO}"}}"#).into(),
            ))
            .await;
        let _ = socket
            .send(AxMsg::Text(
                format!(r#"{{"type":"ready","v":{PROTOCOL_VERSION}}}"#).into(),
            ))
            .await;
        while let Some(Ok(m)) = socket.recv().await {
            if let AxMsg::Text(t) = m {
                st.ws_frames.lock().expect("ws lock").push(t.to_string());
            }
        }
    })
}

impl FakeRemote {
    pub async fn start(token: &str) -> Self {
        let st = Arc::new(FakeState {
            token: token.to_string(),
            rows: Mutex::new(Vec::new()),
            list: Mutex::new(ListMode::Rows),
            kill_error: Mutex::new(None),
            kills: Mutex::new(Vec::new()),
            canned: Mutex::new(HashMap::new()),
            bodies: Mutex::new(Vec::new()),
            ws_frames: Mutex::new(Vec::new()),
            upload_first: Notify::new(),
        });
        let app = Router::new()
            .route("/ws", get(ws_route))
            .fallback(handler)
            .layer(middleware::from_fn_with_state(st.clone(), gate))
            .with_state(st.clone());
        let rec = Recorder::new();
        let url = test_support::start_plain_app(app, rec.clone()).await;
        Self {
            url,
            token: token.to_string(),
            rec,
            st,
        }
    }

    pub fn set_rows(&self, rows: Vec<Value>) {
        *self.st.rows.lock().expect("rows lock") = rows;
    }
    pub fn set_list_mode(&self, mode: ListMode) {
        *self.st.list.lock().expect("list lock") = mode;
    }
    /// Make `POST …/kill` answer `{"items":…,"error":msg}` (a refusal).
    pub fn refuse_kill_with(&self, msg: &str) {
        *self.st.kill_error.lock().expect("kill_error lock") = Some(msg.to_string());
    }
    /// The ids `POST /api/conversations/{id}/kill` was called with.
    pub fn kills(&self) -> Vec<String> {
        self.st.kills.lock().expect("kills lock").clone()
    }
    /// Serve `canned` for `METHOD path` (path without query).
    pub fn can(&self, method: &str, path: &str, canned: Canned) {
        self.st
            .canned
            .lock()
            .expect("canned lock")
            .insert(format!("{method} {path}"), canned);
    }
    /// `("METHOD uri", body)` of every request the remote received.
    pub fn bodies(&self) -> Vec<(String, Vec<u8>)> {
        self.st.bodies.lock().expect("bodies lock").clone()
    }
    /// True if the first body chunk of an upload reached the remote within `secs`.
    pub async fn first_upload_chunk_seen(&self, secs: u64) -> bool {
        tokio::time::timeout(Duration::from_secs(secs), self.st.upload_first.notified())
            .await
            .is_ok()
    }
    pub fn ws_frames(&self) -> Vec<String> {
        self.st.ws_frames.lock().expect("ws lock").clone()
    }
    /// Requests whose path (query stripped) is `path` and method `method`.
    pub fn hits_for(&self, method: &str, path: &str) -> Vec<Captured> {
        self.rec
            .hits()
            .into_iter()
            .filter(|h| h.method == method && h.uri.split('?').next() == Some(path))
            .collect()
    }
    pub fn host_port(&self) -> String {
        self.url.trim_start_matches("http://").to_string()
    }
}

/// The `cookie` header of a captured request.
pub fn hit_cookie(hit: &Captured) -> Option<String> {
    hit.headers
        .iter()
        .find(|(n, _)| n == "cookie")
        .map(|(_, v)| v.clone())
}

// ── The local server ────────────────────────────────────────────────────

const BASE_YAML: &str = r#"
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

/// `(local name L, url, token, target profile P)` per remote profile.
pub type RemoteSpec<'a> = (&'a str, &'a str, &'a str, &'a str);

pub fn yaml(remotes: &[RemoteSpec<'_>]) -> String {
    let mut y = BASE_YAML.to_string();
    for (name, url, token, target) in remotes {
        y.push_str(&format!(
            "  \"{name}\":\n    remote:\n      url: {url}\n      token: {token}\n      profile: {target}\n"
        ));
    }
    y
}

pub struct LocalEnv {
    _home: XdgHome,
    pub registry: SessionRegistry,
    pub storage: Arc<InMemoryStorage>,
    /// The local server (`/ws`, `/api/conversations*`, + `extra`), token-gated.
    pub addr: SocketAddr,
}

impl LocalEnv {
    pub async fn start(remotes: &[RemoteSpec<'_>], extra: Option<Router>) -> Self {
        let yaml = yaml(remotes);
        let home = XdgHome::new(&yaml);
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
        let mut app = Router::new()
            .route("/ws", get(ws_handler))
            .with_state(state.clone())
            .merge(conversations::router(state));
        if let Some(extra) = extra {
            app = app.merge(extra);
        }
        // Like the real server's SPA catch-all: an unrouted path answers 200
        // "SPA-INDEX". So "404 for everything else under /r/" can only come
        // from the proxy router's OWN routes (the `/api/*`-style catch-all),
        // never from the absence of a route.
        app = app.fallback(|| async { (StatusCode::OK, "SPA-INDEX") });
        // The SAME token layer `WebUi::run` applies to everything merged before it.
        let app = app.layer(from_fn_with_state(Arc::<str>::from(T_LOCAL), require_token));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local listener");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        Self {
            _home: home,
            registry,
            storage,
            addr,
        }
    }

    pub fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The global `config.yaml` the `Directory` re-reads on every call (§5.11).
    pub fn config_path(&self) -> PathBuf {
        crate::config::get_config_dir()
            .expect("config dir under XDG_CONFIG_HOME")
            .join("config.yaml")
    }

    /// Save a local conversation row with a fixed RFC 3339 `updated_at`.
    pub fn seed(&self, id: Option<Uuid>, name: &str, profile: Option<&str>, iso: &str) -> Uuid {
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
        c.updated_at = chrono::DateTime::parse_from_rfc3339(iso)
            .expect("fixture timestamp")
            .with_timezone(&Utc);
        self.storage.save(&c).expect("seed save");
        c.id
    }
}

// ── The browser's socket ────────────────────────────────────────────────

pub type Ws = WsStream;

pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("http client")
}

pub fn local_cookie() -> String {
    format!("peakbot_token={T_LOCAL}")
}

pub async fn ws_connect(addr: SocketAddr) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={T_LOCAL}"))
        .await
        .expect("local ws upgrade");
    ws
}

pub async fn ws_send(ws: &mut Ws, v: &Value) {
    ws.send(TMsg::Text(v.to_string().into()))
        .await
        .expect("ws send");
}

/// Next JSON text frame within `secs`; `Err(true)` = closed, `Err(false)` = timeout.
pub async fn next_frame(ws: &mut Ws, secs: u64) -> Result<Value, bool> {
    loop {
        match tokio::time::timeout(Duration::from_secs(secs), ws.next()).await {
            Err(_) => return Err(false),
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(TMsg::Close(_)))) => return Err(true),
            Ok(Some(Ok(TMsg::Text(t)))) => {
                return Ok(serde_json::from_str(&t).expect("server frames are JSON"));
            }
            Ok(Some(Ok(_))) => continue,
        }
    }
}

/// Read frames until one of `ty` arrives (10 s per frame). Panics on close/timeout.
pub async fn wait_type(ws: &mut Ws, ty: &str) -> Value {
    for _ in 0..400 {
        match next_frame(ws, 10).await {
            Ok(f) if f["type"] == ty => return f,
            Ok(_) => continue,
            Err(closed) => panic!(
                "socket {} before a `{ty}` frame",
                if closed { "closed" } else { "timed out" }
            ),
        }
    }
    panic!("no `{ty}` frame");
}

/// A LOCAL socket: attach (fresh convo, no profile) and wait for `ready`.
pub async fn connect_local(env: &LocalEnv) -> Ws {
    let mut ws = ws_connect(env.addr).await;
    ws_send(&mut ws, &json!({ "type": "attach", "convo": null })).await;
    wait_type(&mut ws, "ready").await;
    ws
}

/// A RELAY socket: attach naming remote profile `l`, wait for `ready`.
pub async fn connect_relay(env: &LocalEnv, l: &str) -> Ws {
    let mut ws = ws_connect(env.addr).await;
    ws_send(
        &mut ws,
        &json!({ "type": "attach", "convo": null, "profile": l }),
    )
    .await;
    wait_type(&mut ws, "ready").await;
    ws
}

/// `request_conversations` → the `conversations_list` frame.
pub async fn request_list(ws: &mut Ws) -> Value {
    ws_send(ws, &json!({ "type": "request_conversations" })).await;
    wait_type(ws, "conversations_list").await
}

/// `kill_session{convo, profile?}` → the `conversations_list` frame.
pub async fn kill(ws: &mut Ws, convo: &str, profile: Option<&str>) -> Value {
    let mut f = json!({ "type": "kill_session", "convo": convo });
    if let Some(p) = profile {
        f["profile"] = json!(p);
    }
    ws_send(ws, &f).await;
    wait_type(ws, "conversations_list").await
}

/// The row with `id` in a `conversations_list` frame / HTTP body.
pub fn find<'a>(list: &'a Value, id: &str) -> Option<&'a Value> {
    list["items"].as_array()?.iter().find(|r| r["id"] == id)
}

/// The ids of `list` that are in `interest`, in list order (ignores rows the
/// harness itself creates, e.g. the socket's own fresh conversation).
pub fn order(list: &Value, interest: &[String]) -> Vec<String> {
    list["items"]
        .as_array()
        .expect("items array")
        .iter()
        .filter_map(|r| r["id"].as_str())
        .filter(|id| interest.iter().any(|i| i == id))
        .map(str::to_string)
        .collect()
}

// ── Raw HTTP/1.1 (reqwest normalises `..` and has no streaming body) ────

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A raw response: status code + everything after the header block.
pub struct Raw {
    pub status: u16,
    pub head: String,
    pub body: Vec<u8>,
}

impl Raw {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn parse_raw(buf: &[u8]) -> Raw {
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("no header block in {:?}", String::from_utf8_lossy(buf)));
    let head = String::from_utf8_lossy(&buf[..split]).into_owned();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no status line in {head:?}"));
    Raw {
        status,
        head,
        body: buf[split + 4..].to_vec(),
    }
}

/// `METHOD target` sent byte-for-byte (no URL normalisation), local cookie
/// attached, `Connection: close`.
pub async fn raw_request(addr: SocketAddr, method: &str, target: &str) -> Raw {
    let mut s = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let req = format!(
        "{method} {target} HTTP/1.1\r\nHost: {addr}\r\nCookie: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        local_cookie()
    );
    s.write_all(req.as_bytes()).await.expect("write");
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut buf))
        .await
        .expect("raw response within 10 s")
        .expect("read");
    parse_raw(&buf)
}

/// Read from `s` until `buf` contains `needle` (bounded); returns false on timeout/EOF.
pub async fn read_until_contains(
    s: &mut tokio::net::TcpStream,
    buf: &mut Vec<u8>,
    needle: &[u8],
    secs: u64,
) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut chunk = [0u8; 8192];
    while !buf.windows(needle.len()).any(|w| w == needle) {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return false;
        }
        match tokio::time::timeout(left, s.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            _ => return false,
        }
    }
    true
}

/// Open a connection, send the head of a `Content-Length: total` POST and
/// the first `first` bytes of `body`; the caller finishes with
/// [`finish_post`].
pub async fn start_post(
    addr: SocketAddr,
    target: &str,
    content_type: &str,
    body: &[u8],
    first: usize,
) -> tokio::net::TcpStream {
    let mut s = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let head = format!(
        "POST {target} HTTP/1.1\r\nHost: {addr}\r\nCookie: {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        local_cookie(),
        body.len()
    );
    s.write_all(head.as_bytes()).await.expect("write head");
    s.write_all(&body[..first]).await.expect("write first part");
    s.flush().await.expect("flush");
    s
}

/// Send the rest of the body and read the response to the end.
pub async fn finish_post(mut s: tokio::net::TcpStream, rest: &[u8]) -> Raw {
    s.write_all(rest).await.expect("write rest");
    s.flush().await.expect("flush");
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), s.read_to_end(&mut buf))
        .await
        .expect("upload response within 20 s")
        .expect("read");
    parse_raw(&buf)
}
