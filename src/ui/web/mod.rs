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
//! are deferred to Phase 4 (remote access); for loopback the browser
//! doesn't care.

use crate::session::SessionDeps;
use crate::ui::Ui;
use crate::ui::ui_trait::builtin_commands;
use crate::ui::wire::{InboundMessage, ModelInfo, OutboundMessage, build_conversations_snapshot};
use crate::{StateManager, UiAction};
use anyhow::Result;
use axum::{
    Router,
    body::Body,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::{Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use futures::{SinkExt, StreamExt};
use rust_embed::RustEmbed;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc::{self, UnboundedSender};

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

/// Shared state handed to every WebSocket connection: the immutable
/// session deps (one session is built per connection) plus the one-shot
/// model-picker snapshot.
#[derive(Clone)]
struct WsState {
    deps: Arc<SessionDeps>,
    models: Arc<Vec<ModelInfo>>,
    active_alias: Arc<str>,
}

/// `peakbot --web` — serves the embedded SPA and a `/ws` endpoint on a
/// fixed loopback port. `run` blocks on the axum server until Ctrl+C
/// triggers graceful shutdown.
pub struct WebUi {
    addr: SocketAddr,
    ws_state: WsState,
}

impl WebUi {
    pub fn new(
        addr: SocketAddr,
        deps: Arc<SessionDeps>,
        models: Vec<ModelInfo>,
        active_alias: String,
    ) -> Self {
        Self {
            addr,
            ws_state: WsState {
                deps,
                models: Arc::new(models),
                active_alias: active_alias.into(),
            },
        }
    }
}

impl Ui for WebUi {
    async fn init(&mut self) -> Result<()> {
        // The axum server runs in `run`. Nothing to do here.
        Ok(())
    }

    async fn run(&mut self) -> Result<()> {
        let app: Router = Router::new()
            .route("/ws", get(ws_handler))
            .route("/commands", get(commands_handler))
            .with_state(self.ws_state.clone())
            .fallback(static_handler);

        let listener = tokio::net::TcpListener::bind(self.addr).await?;
        eprintln!("🌐 PeakBot web UI: http://{}  (Ctrl+C to quit)", self.addr);

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

/// Drive one browser connection: build a fresh session, then run the
/// `StdioUi` three-task shape (writer sink, state broadcast, inbound
/// reader) over WS frames. When the socket closes or `/exit` sets
/// `exit_requested`, the session is dropped — which tears down its
/// controller loop and kills its bg PTY children.
async fn handle_socket(socket: WebSocket, state: WsState) {
    // Build this connection's session. On failure, send one error frame
    // and drop the socket.
    let session = match crate::create_session(&state.deps) {
        Ok(s) => s,
        Err(e) => {
            let (mut sink, _) = socket.split();
            let msg = OutboundMessage::Error {
                message: format!("failed to start session: {e}"),
            };
            if let Ok(txt) = serde_json::to_string(&msg) {
                let _ = sink.send(Message::Text(txt.into())).await;
            }
            return;
        }
    };

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

    // `ready` precedes `models_available`.
    let _ = out_tx.send(OutboundMessage::Ready);
    if !state.models.is_empty() {
        let _ = out_tx.send(OutboundMessage::ModelsAvailable {
            active: state.active_alias.to_string(),
            models: state.models.as_ref().clone(),
        });
    }

    // Inbound reader: parse WS frames → UiAction, answering
    // `request_conversations` off-band (not a UiAction), matching stdio.
    let action_sender = session.action_sender.clone();
    let sm_for_reader = session.state_manager.clone();
    let reader_tx = out_tx.clone();
    let reader_task = tokio::spawn(async move {
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
            if !dispatch_inbound(trimmed, &action_sender, &reader_tx, &sm_for_reader) {
                break;
            }
        }
    });

    // State broadcast: push every AppState snapshot to the client until the
    // session requests exit or the writer drops.
    let mut state_rx = session.state_manager.subscribe();
    while let Some(app_state) = state_rx.recv().await {
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

    // Teardown: stop the reader, drain the writer, then drop the session
    // (unwinds its controller loop + kills its bg PTY children).
    reader_task.abort();
    drop(out_tx);
    let _ = writer_task.await;
    drop(session);
}

/// Parse one inbound line and dispatch it. Returns `false` when the loop
/// should stop (channel closed or `shutdown` received). Mirrors
/// `stdio::run_stdin_loop`'s match arms.
fn dispatch_inbound(
    line: &str,
    action_sender: &UnboundedSender<UiAction>,
    out_tx: &UnboundedSender<OutboundMessage>,
    state_manager: &StateManager,
) -> bool {
    match serde_json::from_str::<InboundMessage>(line) {
        Ok(InboundMessage::SendMessage { text }) => {
            action_sender.send(UiAction::SendMessage(text)).is_ok()
        }
        Ok(InboundMessage::Stop) => action_sender.send(UiAction::RequestStop).is_ok(),
        Ok(InboundMessage::SwitchModel { alias }) => {
            action_sender.send(UiAction::SwitchModel(alias)).is_ok()
        }
        Ok(InboundMessage::RequestConversations) => {
            let items = build_conversations_snapshot(state_manager);
            out_tx
                .send(OutboundMessage::ConversationsList { items })
                .is_ok()
        }
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
}
