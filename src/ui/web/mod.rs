//! `peakbot --web` — the Web `Ui` implementation.
//!
//! Serves the embedded `web/dist/` SPA via axum (with an SPA fallback:
//! unknown routes → `index.html`) and a `GET /ws` WebSocket endpoint that
//! drives a **fresh, independent agent session per connection** (Option C,
//! `webui.md` §10). A browser tab = its own transcript, todo, stats, and
//! context; closing the tab tears its session down.
//!
//! `WebUi::run` blocks on the axum server's graceful-shutdown future
//! (Ctrl+C). Each WebSocket connection builds a session via
//! [`crate::create_session`] from the shared [`crate::SessionDeps`]; the
//! connection handler is `StdioUi`'s three-task shape (writer sink, state
//! broadcast, inbound reader) over WS frames instead of stdio lines,
//! reusing the same [`crate::ui::wire`] protocol.
//!
//! ## Static handler — why hand-rolled
//!
//! `axum-embed` 0.1.0 is axum-0.7-only and unmaintained. We use the
//! first-party `axum` feature of `rust-embed` 8.x plus a small
//! hand-rolled `IntoResponse` for `EmbeddedFile` to get the right
//! `Content-Type` (mime_guess) and SPA fallback. ETag/304 + compression
//! are intentionally omitted: the single-operator remote case (Phase 4)
//! loads the bundle once over a fast link, so the hand-rolled negotiation
//! they'd require isn't worth the complexity.
//!
//! ## Remote access (Phase 4)
//!
//! `--bind` may listen beyond loopback; then a shared secret (`--token` /
//! `PEAKBOT_WEB_TOKEN`) is mandatory (enforced in `main`). When a token is
//! set, [`require_token`] gates *every* route: the browser presents it once
//! as `?token=…`, the middleware sets a `peakbot_token` cookie, and all
//! later asset / `/commands` / `/ws` requests authenticate via that cookie —
//! so the frontend needs no token-threading code.

use crate::session::SessionDeps;
use crate::ui::Ui;
use crate::ui::ui_trait::builtin_commands;
use crate::ui::wire::{
    InboundMessage, ModelInfo, OutboundMessage, build_conversations_snapshot, build_dir_listing,
};
use crate::{StateManager, UiAction};
use anyhow::Result;
use axum::{
    Router,
    body::Body,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::{Request, StatusCode, header},
    middleware::{Next, from_fn_with_state},
    response::{IntoResponse, Response},
    routing::get,
};
use futures::{SinkExt, StreamExt};
use registry::SessionRegistry;
use rust_embed::RustEmbed;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedSender};
use uuid::Uuid;

mod registry;

/// Port the web UI listens on. Fixed for now (`--port` flag is Phase 4).
/// See `webui.md` §3 decision 1.
pub const DEFAULT_WEB_ADDR: &str = "127.0.0.1:7823";

/// Self-explaining page served when `web/dist/index.html` is missing from
/// the embedded bundle (i.e. the binary was built without `make web`).
/// Compiled in so the runtime never serves a blank page; CI/release
/// builds run the Node stage first, so this never ships.
const STUB_INDEX_HTML: &str = include_str!("../../../build/stub_index.html");

/// Embedded web UI bundle. `release` builds inline the bytes; `cargo run`
/// (debug) reads from disk so UI tweaks don't need a Rust rebuild.
#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct Assets;

/// Shared state handed to every WebSocket connection: the session registry
/// (sticky sessions keyed by conversation id) plus the one-shot model-picker
/// snapshot. `deps` lives inside the registry.
#[derive(Clone)]
struct WsState {
    registry: SessionRegistry,
    models: Arc<Vec<ModelInfo>>,
    active_alias: Arc<str>,
    /// Idle-session TTL for the reaper (from `config.web`).
    session_ttl: Duration,
}

/// `peakbot --web` — serves the embedded SPA and a `/ws` endpoint. `run`
/// blocks on the axum server until Ctrl+C triggers graceful shutdown. When
/// `token` is set, every route is gated by [`require_token`].
pub struct WebUi {
    addr: SocketAddr,
    ws_state: WsState,
    /// Shared secret guarding every route. `None` = open (loopback default).
    token: Option<Arc<str>>,
    /// How often the reaper scans for expired sessions.
    reaper_tick: Duration,
}

impl WebUi {
    pub fn new(
        addr: SocketAddr,
        deps: Arc<SessionDeps>,
        models: Vec<ModelInfo>,
        active_alias: String,
        token: Option<String>,
    ) -> Self {
        let web = deps.config.web.clone();
        Self {
            addr,
            ws_state: WsState {
                registry: SessionRegistry::new(deps),
                models: Arc::new(models),
                active_alias: active_alias.into(),
                session_ttl: Duration::from_secs(web.session_ttl_secs),
            },
            token: token.map(Into::into),
            reaper_tick: Duration::from_secs(web.reaper_tick_secs),
        }
    }

    /// The URL a browser should open. When a token is set it rides as a
    /// `?token=…` query so the first request establishes the auth cookie.
    fn entry_url(&self) -> String {
        match &self.token {
            Some(t) => format!("http://{}/?token={}", self.addr, t),
            None => format!("http://{}/", self.addr),
        }
    }

    /// Auto-open the browser only for a local, interactive session: a
    /// loopback bind with no SSH context. Remote / SSH sessions just get the
    /// printed URL (opening would target the wrong machine — same heuristic
    /// as the MCP OAuth flow). `PEAKBOT_NO_OPEN` suppresses it entirely — set
    /// by `make dev`, where Vite (:5173) owns the browser, not the backend.
    fn maybe_open_browser(&self, url: &str) {
        if std::env::var_os("PEAKBOT_NO_OPEN").is_some() {
            return;
        }
        let local = self.addr.ip().is_loopback() && std::env::var_os("SSH_CONNECTION").is_none();
        if local {
            let _ = open::that(url);
        }
    }
}

impl Ui for WebUi {
    async fn init(&mut self) -> Result<()> {
        // The axum server runs in `run`. Nothing to do here.
        Ok(())
    }

    async fn run(&mut self) -> Result<()> {
        let mut app: Router = Router::new()
            .route("/ws", get(ws_handler))
            .route("/commands", get(commands_handler))
            .with_state(self.ws_state.clone())
            .fallback(static_handler);

        // Gate every route behind the shared secret when one is configured.
        // Open by default (loopback); `main` guarantees a token exists before
        // a non-loopback bind ever reaches here.
        if let Some(token) = &self.token {
            app = app.layer(from_fn_with_state(token.clone(), require_token));
        }

        let listener = tokio::net::TcpListener::bind(self.addr).await?;
        let url = self.entry_url();
        eprintln!("🌐 PeakBot web UI: {url}  (Ctrl+C to quit)");
        self.maybe_open_browser(&url);

        // Reaper: periodically expire sessions idle (no sockets) past the TTL.
        // A detached task tied to the process lifetime — the registry it holds
        // is the same one every connection shares.
        let reaper_registry = self.ws_state.registry.clone();
        let ttl = self.ws_state.session_ttl;
        let tick = self.reaper_tick;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tick);
            loop {
                interval.tick().await;
                reaper_registry.reap(ttl);
            }
        });

        // The View owns its own shutdown signal — Ctrl+C ends the graceful
        // drain. No injected channel: the signal concern lives here, not
        // in `main`.
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Name of the cookie the browser carries after the first token-bearing
/// request, so subsequent asset / `/commands` / `/ws` requests authenticate
/// without the token in the URL.
const TOKEN_COOKIE: &str = "peakbot_token";

/// Gate every route behind the shared secret. Installed only when a token is
/// configured. Accepts the token from the `?token=…` query (browser first
/// load) or the `peakbot_token` cookie (every request after). A query-borne
/// match sets the cookie so the token leaves the URL after one hop.
async fn require_token(State(token): State<Arc<str>>, req: Request<Body>, next: Next) -> Response {
    let via_query = token_from_query(req.uri().query()).is_some_and(|t| ct_eq(t, &token));
    let ok = via_query || token_from_cookie(&req).is_some_and(|t| ct_eq(t, &token));

    if !ok {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }

    let mut resp = next.run(req).await;
    if via_query {
        // `HttpOnly` keeps JS from reading it; `SameSite=Strict` blocks
        // cross-site carriage. No `Secure` — the server is plain HTTP.
        let cookie = format!("{TOKEN_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict");
        if let Ok(v) = header::HeaderValue::from_str(&cookie) {
            resp.headers_mut().insert(header::SET_COOKIE, v);
        }
    }
    resp
}

/// Extract the `token` value from a URL query string, if present.
fn token_from_query(query: Option<&str>) -> Option<&str> {
    query?.split('&').find_map(|kv| kv.strip_prefix("token="))
}

/// Extract the `peakbot_token` value from the request's `Cookie` header.
fn token_from_cookie(req: &Request<Body>) -> Option<&str> {
    req.headers()
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|c| c.trim().strip_prefix(&format!("{TOKEN_COOKIE}=")))
}

/// Constant-time byte comparison. Length is allowed to leak (a token's length
/// is not the secret); the byte contents are compared in constant time so a
/// network timing side-channel can't recover the secret one byte at a time.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// `GET /ws` — upgrade to a WebSocket, then run one independent session
/// for the connection's lifetime.
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<WsState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// `GET /commands` — the slash-command list as JSON, the single source of
/// truth [`builtin_commands`]. Read-only; the frontend fetches it once to
/// populate the composer's slash palette. Not a WS frame, so `stdio` (which
/// doesn't render a palette) is untouched. Hand-rolled (like `serve_asset`)
/// to avoid pulling axum's `json` feature into all four platform builds.
async fn commands_handler() -> Response {
    match serde_json::to_vec(&builtin_commands()) {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            Body::from(bytes),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialise commands: {e}"),
        )
            .into_response(),
    }
}

/// Drive one browser connection. The first frame must be `Attach`, which
/// binds the socket to a session in the registry (sharing an active one,
/// resuming a persisted one, or minting fresh). The socket then runs the
/// `StdioUi` three-task shape (writer sink, state broadcast, inbound reader)
/// over WS frames. On close it **detaches** — the session survives for other
/// sockets and for reconnect, and only expires via the reaper or a kill.
async fn handle_socket(socket: WebSocket, state: WsState) {
    let (mut ws_sink, mut ws_stream) = socket.split();

    // Sole owner of the WS sink — every outbound frame funnels through
    // this task so frames stay whole (mirrors StdioUi's writer task).
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<OutboundMessage>();
    let writer_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let txt = match serde_json::to_string(&msg) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("failed to serialise outbound message: {e:?}");
                    continue;
                }
            };
            if ws_sink.send(Message::Text(txt.into())).await.is_err() {
                break; // socket closed
            }
        }
        let _ = ws_sink.close().await;
    });

    // Read the handshake frame. The client always sends `Attach` first; a
    // non-attach first frame attaches fresh and is then processed normally so
    // nothing is silently dropped. A socket that closes before any frame just
    // unwinds (writer drops on scope exit).
    let (want, leftover) = match read_first_frame(&mut ws_stream).await {
        Some(text) => match serde_json::from_str::<InboundMessage>(&text) {
            Ok(InboundMessage::Attach { convo }) => {
                (convo.and_then(|s| Uuid::parse_str(&s).ok()), None)
            }
            _ => (None, Some(text)),
        },
        None => return,
    };

    // Bind to a session. On failure, send one error frame and drop the socket.
    let registered = match state.registry.attach(want) {
        Ok(r) => r,
        Err(e) => {
            let _ = out_tx.send(OutboundMessage::Error {
                message: format!("failed to start session: {e}"),
            });
            drop(out_tx);
            let _ = writer_task.await;
            return;
        }
    };
    // Stable detach handle (the session's creation id, which never changes
    // even when `/model` moves the session to a new conversation).
    let session_key = registered.session.conversation_id;
    // The conversation the socket actually bound to — the live id (equals the
    // creation id on a fresh attach; the current id on a reused session).
    let convo_id = registered
        .live_convo()
        .unwrap_or(registered.session.conversation_id);

    // Report the real id (may differ from `want` on a resume→fresh fallback),
    // then the usual handshake: `attached` → `ready` → `models_available`.
    let _ = out_tx.send(OutboundMessage::Attached {
        convo: convo_id.to_string(),
    });
    let _ = out_tx.send(OutboundMessage::Ready);
    if !state.models.is_empty() {
        let _ = out_tx.send(OutboundMessage::ModelsAvailable {
            active: state.active_alias.to_string(),
            models: state.models.as_ref().clone(),
        });
    }

    let action_sender = registered.session.action_sender.clone();
    let sm_for_reader = registered.session.state_manager.clone();
    let registry_for_reader = state.registry.clone();
    let reader_tx = out_tx.clone();

    // Process a non-attach handshake frame before the reader loop starts.
    if let Some(frame) = leftover {
        dispatch_inbound(
            &frame,
            &action_sender,
            &reader_tx,
            &sm_for_reader,
            &registry_for_reader,
        );
    }

    // Inbound reader: parse WS frames → UiAction / off-band replies.
    let mut reader_task = tokio::spawn(async move {
        while let Some(frame) = ws_stream.next().await {
            let text = match frame {
                Ok(Message::Text(t)) => t.to_string(),
                Ok(Message::Close(_)) | Err(_) => break,
                // Ignore binary/ping/pong — the protocol is text-only.
                Ok(_) => continue,
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !dispatch_inbound(
                trimmed,
                &action_sender,
                &reader_tx,
                &sm_for_reader,
                &registry_for_reader,
            ) {
                break;
            }
        }
    });

    // State broadcast: push every AppState snapshot to the client until the
    // session requests exit, the writer drops, or the client disconnects.
    // Selecting on `reader_task` is what makes a client-initiated close
    // *prompt*: without it this loop parks on `state_rx.recv()` and only
    // notices the gone socket on the next failed `out_tx.send`, so an idle
    // session's close handshake would hang until a TCP timeout (~10s) — which
    // stalls every convo-switch reconnect and tab-close teardown. When the
    // reader ends (Close frame or stream end) we break at once, dropping
    // `out_tx` so the writer closes the sink and the handshake completes in ms.
    let mut state_rx = registered.session.state_manager.subscribe();
    loop {
        tokio::select! {
            maybe_state = state_rx.recv() => {
                let Some(app_state) = maybe_state else { break };
                let exit = app_state.exit_requested;
                if out_tx
                    .send(OutboundMessage::State {
                        state: Box::new(app_state),
                    })
                    .is_err()
                {
                    break; // writer/socket gone
                }
                if exit {
                    break;
                }
            }
            _ = &mut reader_task => break, // client disconnected
        }
    }

    // Teardown: stop the reader, drain the writer, then detach. The session
    // stays alive in the registry for other sockets / reconnect; it only ends
    // via the reaper (idle TTL) or an explicit kill.
    reader_task.abort();
    drop(out_tx);
    let _ = writer_task.await;
    state.registry.detach(session_key);
}

/// Await the first text frame of a connection, skipping non-text control
/// frames. Returns `None` if the socket closes first.
async fn read_first_frame(
    ws_stream: &mut futures::stream::SplitStream<WebSocket>,
) -> Option<String> {
    while let Some(frame) = ws_stream.next().await {
        match frame {
            Ok(Message::Text(t)) => return Some(t.to_string()),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => continue,
        }
    }
    None
}

/// Parse one inbound line and dispatch it. Returns `false` when the loop
/// should stop (channel closed or `shutdown` received). Mirrors
/// `stdio::run_stdin_loop`'s match arms, plus the sticky-session frames.
fn dispatch_inbound(
    line: &str,
    action_sender: &UnboundedSender<UiAction>,
    out_tx: &UnboundedSender<OutboundMessage>,
    state_manager: &StateManager,
    registry: &SessionRegistry,
) -> bool {
    match serde_json::from_str::<InboundMessage>(line) {
        Ok(InboundMessage::SendMessage { text }) => {
            action_sender.send(UiAction::SendMessage(text)).is_ok()
        }
        Ok(InboundMessage::Stop) => action_sender.send(UiAction::RequestStop).is_ok(),
        Ok(InboundMessage::SwitchModel { alias }) => {
            action_sender.send(UiAction::SwitchModel(alias)).is_ok()
        }
        Ok(InboundMessage::SwitchCwd { path }) => {
            action_sender.send(UiAction::ChangeCwd(path)).is_ok()
        }
        Ok(InboundMessage::ListDir { path }) => out_tx.send(build_dir_listing(&path)).is_ok(),
        Ok(InboundMessage::RequestConversations) => {
            let items = build_conversations_snapshot(state_manager, &registry.active_ids());
            out_tx
                .send(OutboundMessage::ConversationsList { items })
                .is_ok()
        }
        Ok(InboundMessage::KillSession { convo }) => {
            if let Ok(id) = Uuid::parse_str(&convo) {
                registry.kill(id);
            }
            true
        }
        // Re-attach mid-stream is a client bug; ignore rather than reset.
        Ok(InboundMessage::Attach { .. }) => true,
        Ok(InboundMessage::Shutdown) => {
            let _ = action_sender.send(UiAction::SendMessage("/exit".to_string()));
            false
        }
        Err(e) => out_tx
            .send(OutboundMessage::Error {
                message: format!("invalid inbound JSON: {e}"),
            })
            .is_ok(),
    }
}

/// Serve a single file from the embedded bundle. Unknown routes fall back
/// to `index.html` (SPA client-side routing → 200, not 404), except paths
/// containing `..` which are refused. When the bundle has no `index.html`
/// (built without `make web`), the compiled-in stub is served instead so
/// the browser shows actionable text rather than a blank page.
async fn static_handler(req: Request<Body>) -> Response {
    // Read the path from the URI directly — a `Path<String>` extractor
    // would 500 on `/` (zero segments) and force an `Option` wrapper.
    let path = req.uri().path().trim_start_matches('/');

    if let Some(resp) = serve_asset(path) {
        return resp;
    }

    // Refuse traversal attempts; everything else is an SPA route.
    if path.contains("..") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    serve_asset("index.html").unwrap_or_else(serve_stub)
}

/// Look up `path` in the embedded bundle, returning a 200 response with the
/// right `Content-Type`, or `None` if the bundle has no such file.
fn serve_asset(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    Some(
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime.to_string())],
            Body::from(file.data.into_owned()),
        )
            .into_response(),
    )
}

/// Serve the compiled-in "web UI not built" stub (bundle had no `index.html`).
fn serve_stub() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8".to_string())],
        Body::from(STUB_INDEX_HTML),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawn the real axum router on a random loopback port, then
    /// roundtrip through `reqwest`. More honest than a tower-oneshot
    /// stub — exercises actual TCP, actual content-type headers, etc.
    async fn spawn_app() -> std::net::SocketAddr {
        let app: Router = Router::new().fallback(static_handler);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        addr
    }

    #[tokio::test]
    async fn root_serves_index_html() {
        let addr = spawn_app().await;
        let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
        let status = resp.status();
        let body = resp.text().await.unwrap();
        assert_eq!(status, 200, "body: {body}");
        let ct_index = body.find("PeakBot").unwrap_or(usize::MAX);
        assert!(ct_index < 1024, "root body did not contain PeakBot: {body}");
    }

    #[tokio::test]
    async fn unknown_route_falls_back_to_index() {
        let addr = spawn_app().await;
        let resp = reqwest::get(format!("http://{addr}/some/spa/route"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(body.contains("PeakBot"), "SPA fallback body = {body:?}");
    }

    #[tokio::test]
    async fn dotdot_path_returns_404() {
        // Test the handler directly with a constructed Request — reqwest
        // normalises `..` segments client-side, so this scenario can
        // only be hit by a hand-crafted or malicious request.
        let req = axum::http::Request::builder()
            .uri("/../etc/passwd")
            .body(Body::empty())
            .unwrap();
        let resp = static_handler(req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn commands_route_returns_builtin_commands() {
        let app: Router = Router::new().route("/commands", get(commands_handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let resp = reqwest::get(format!("http://{addr}/commands"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()[reqwest::header::CONTENT_TYPE],
            "application/json"
        );
        let cmds: Vec<crate::ui::ui_trait::SlashCommand> = resp.json().await.unwrap();
        // Same list, same order as the source of truth.
        assert_eq!(cmds.len(), builtin_commands().len());
        assert_eq!(cmds[0].name, "help");
    }

    // --- Phase 4: token auth ---

    /// Spawn `/commands` behind the token layer on a random loopback port.
    async fn spawn_gated(token: &str) -> std::net::SocketAddr {
        let secret: Arc<str> = token.into();
        let app: Router = Router::new()
            .route("/commands", get(commands_handler))
            .layer(from_fn_with_state(secret, require_token));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        addr
    }

    /// A client that doesn't follow redirects, so each request carries
    /// exactly what we set — the auth path is tested honestly. reqwest keeps
    /// no cookie store by default, so Set-Cookie is never auto-replayed.
    fn bare_client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn gated_rejects_missing_token() {
        let addr = spawn_gated("s3cret").await;
        let resp = bare_client()
            .get(format!("http://{addr}/commands"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn gated_rejects_wrong_token() {
        let addr = spawn_gated("s3cret").await;
        let resp = bare_client()
            .get(format!("http://{addr}/commands?token=nope"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn gated_accepts_query_and_sets_cookie() {
        let addr = spawn_gated("s3cret").await;
        let resp = bare_client()
            .get(format!("http://{addr}/commands?token=s3cret"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let cookie = resp.headers()[reqwest::header::SET_COOKIE]
            .to_str()
            .unwrap();
        assert!(cookie.contains("peakbot_token=s3cret"), "cookie: {cookie}");
        assert!(cookie.contains("HttpOnly"), "cookie: {cookie}");
    }

    #[tokio::test]
    async fn gated_accepts_cookie_without_resetting_it() {
        let addr = spawn_gated("s3cret").await;
        let resp = bare_client()
            .get(format!("http://{addr}/commands"))
            .header(reqwest::header::COOKIE, "peakbot_token=s3cret")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // Cookie-authed requests don't re-issue Set-Cookie.
        assert!(!resp.headers().contains_key(reqwest::header::SET_COOKIE));
    }

    #[test]
    fn ct_eq_matches_only_identical_strings() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd")); // length mismatch
        assert!(!ct_eq("", "x"));
    }

    #[test]
    fn token_extractors_pick_the_right_field() {
        assert_eq!(token_from_query(Some("token=xyz")), Some("xyz"));
        assert_eq!(token_from_query(Some("a=1&token=xyz&b=2")), Some("xyz"));
        assert_eq!(token_from_query(Some("nope=1")), None);
        assert_eq!(token_from_query(None), None);

        let req = |cookie: &str| {
            Request::builder()
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap()
        };
        assert_eq!(token_from_cookie(&req("peakbot_token=xyz")), Some("xyz"));
        assert_eq!(
            token_from_cookie(&req("other=1; peakbot_token=xyz")),
            Some("xyz")
        );
        assert_eq!(token_from_cookie(&req("other=1")), None);
    }
}
