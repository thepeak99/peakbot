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
//! [`crate::create_session`] from the shared [`crate::SessionDeps`].
//!
//! ## Concurrency per socket
//!
//! Each connection has four concurrent jobs, with strict ownership to keep
//! frames whole and the memory bound a property of the type:
//!
//! | part                | owns                                                                                              |
//! |---------------------|---------------------------------------------------------------------------------------------------|
//! | writer task         | the WS sink (`writer_loop`) — the *only* thing that writes frames                                  |
//! | reader task         | the WS stream — parses inbound frames into `UiAction`s / off-band replies                          |
//! | state forwarder     | subscribes to `StateManager`, pushes snapshots into the coalescing slot (`forward_state`)         |
//! | shared channel      | bounded FIFO of 32 for ordered control frames + 1-deep coalescing `watch` slot for `state` (`src/ui/outbound.rs`) |
//!
//! The writer has a 120 s `WRITE_TIMEOUT` (tear-down, never retry — a
//! timed-out `send` may have written a partial frame into the TLS stream)
//! and a 30 s keepalive `PING_INTERVAL` so the timeout can observe a dead
//! idle peer. The forwarder's `select!` arms on writer completion too —
//! that's the bit the inline version was missing, and is what keeps a
//! torn-down socket from pinning `attached > 0` against the idle-TTL reaper.
//!
//! On close the session **detaches** — it survives for other sockets and
//! for reconnect, and only expires via the reaper or a kill.
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
use crate::ui::AppState;
use crate::ui::Ui;
use crate::ui::outbound::{OutboundRx, OutboundTx, outbound_channel};
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
use bytes::Bytes;
use futures::{Sink, SinkExt, StreamExt};
use registry::SessionRegistry;
use rust_embed::RustEmbed;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

mod registry;
pub mod setup;
pub mod tls;

/// Port the web UI listens on. Fixed for now (`--port` flag is Phase 4).
/// See `webui.md` §3 decision 1.
pub const DEFAULT_WEB_ADDR: &str = "127.0.0.1:7823";

/// Hard constant. The writer reaper for any single frame: a stalled
/// `sink.send` past this is treated as a half-open peer (kernel/TLS write
/// buffers full, no FIN/RST) and the connection is torn down. Generous
/// because per-socket memory is already bounded by the coalescing slot
/// (`src/ui/outbound.rs`), so this is a reaper, not a memory limit — it
/// must comfortably exceed the time to push one worst-case snapshot
/// (~8 MiB, see #250) over a slow mobile link. See design §6 risk 1.
const WRITE_TIMEOUT: Duration = Duration::from_secs(120);

/// Keepalive cadence. The writer always has *something* to write so the
/// `WRITE_TIMEOUT` and the kernel's retransmission give-up can observe a
/// dead idle peer; we do not track pongs (browsers auto-pong; the reader
/// already ignores ping/pong, `:549-550`).
const PING_INTERVAL: Duration = Duration::from_secs(30);

fn windowed_from(ssh: bool, display: bool, wayland: bool, os: &str) -> bool {
    if ssh {
        return false;
    }
    matches!(os, "macos" | "windows") || display || wayland
}

/// True when this process is attached to a local graphical session.
pub fn windowed_session() -> bool {
    windowed_from(
        std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some(),
        std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty()),
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()),
        std::env::consts::OS,
    )
}

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
    /// Serve over HTTPS with PeakBot's built-in CA when true.
    tls: bool,
    /// Extra SAN names/IPs to add to the TLS leaf (from `--tls-name`), on top
    /// of the auto-discovered loopback, LAN IP, and `<hostname>.local`.
    extra_sans: Vec<String>,
    /// How often the reaper scans for expired sessions.
    reaper_tick: Duration,
    /// Redirect only the root document to the setup wizard on first run.
    needs_setup: bool,
}

impl WebUi {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        addr: SocketAddr,
        deps: Arc<SessionDeps>,
        models: Vec<ModelInfo>,
        active_alias: String,
        token: Option<String>,
        tls: bool,
        extra_sans: Vec<String>,
        needs_setup: bool,
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
            tls,
            extra_sans,
            reaper_tick: Duration::from_secs(web.reaper_tick_secs),
            needs_setup,
        }
    }

    /// The URL a browser should open. When a token is set it rides as a
    /// `?token=…` query so the first request establishes the auth cookie.
    fn entry_url(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        match &self.token {
            Some(t) => format!("{scheme}://{}/?token={}", self.addr, t),
            None => format!("{scheme}://{}/", self.addr),
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
        let local = self.addr.ip().is_loopback() && windowed_session();
        if local {
            // Detached because waiting on xdg-open can stall the runtime before
            // axum starts serving the already-bound port.
            let _ = open::that_detached(url);
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

        if self.needs_setup {
            app = app.route("/", get(setup_redirect));
        }

        // Mount the setup wizard (plan §A-Q2, §B, §D S3). The router is
        // merged BEFORE the token layer so every `/api/setup/*` route is
        // gated by the same `require_token` as `/ws` and `/commands`.
        // Constructed here (rather than in `WebUi::new`) so we read the
        // config path fresh; the resolution is `dirs` + `ProjectDirs`
        // and runs once at boot.
        let config_path = crate::config::get_config_file_path()
            .unwrap_or_else(|| std::path::PathBuf::from("config.yaml"));
        let setup_state = setup::SetupState {
            config_path,
            facts_base: setup::FactsBase::current(),
            needs_setup: self.needs_setup,
            // Track I: the seams now point at the real install / service
            // dispatchers. Tests still inject their own fakes via
            // `SetupState`; production never sees `default_for_tests()`.
            install: setup::InstallFn(setup::install_op_adapter),
            service: setup::ServiceFn(setup::service_op_adapter),
        };
        app = app.merge(setup::router(setup_state));

        // Gate every route behind the shared secret when one is configured.
        // Open by default (loopback); `main` guarantees a token exists before
        // a non-loopback bind ever reaches here.
        if let Some(token) = &self.token {
            app = app.layer(from_fn_with_state(token.clone(), require_token));
        }

        // The CA download is merged AFTER the token layer, so it stays
        // reachable without a token — a CA *public* cert is not a secret, and
        // the phone needs it before it can trust anything.
        if self.tls {
            app = app.merge(Router::new().route("/peakbot-ca.crt", get(ca_cert_handler)));
        }

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

        if self.tls {
            self.serve_tls(app).await
        } else {
            self.serve_plain(app).await
        }
    }

    async fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Format a human-readable error message for `AddrInUse` (plan §E.14 #10, task I7).
fn format_addr_in_use(addr: SocketAddr) -> String {
    format!(
        "{addr} is already in use — a PeakBot service may already be running.\n         Check with: peakbot service status\n         To use a different port, start with: peakbot --bind 127.0.0.1:<port>"
    )
}

impl WebUi {
    /// Serve plain HTTP with a Ctrl+C graceful shutdown.
    async fn serve_plain(&self, app: Router) -> Result<()> {
        let listener = match tokio::net::TcpListener::bind(self.addr).await {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                anyhow::bail!("{}", format_addr_in_use(self.addr));
            }
            Err(e) => return Err(e.into()),
        };
        let url = self.entry_url();
        eprintln!("🌐 Shifu web UI: {url}  (Ctrl+C to quit)");
        self.maybe_open_browser(&url);

        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        Ok(())
    }

    /// Serve HTTPS with PeakBot's built-in CA. Mints a leaf for this machine's
    /// SANs, prints the CA-install URL, and drains gracefully on Ctrl+C.
    async fn serve_tls(&self, app: Router) -> Result<()> {
        let tls_dir = tls::default_tls_dir()?;
        let sans = tls::local_sans(self.addr, &self.extra_sans);
        let server_config = tls::server_config(&tls_dir, &sans)?;

        let url = self.entry_url();
        eprintln!("🔒 Shifu web UI (HTTPS): {url}  (Ctrl+C to quit)");
        let host = tls::primary_lan_host(self.addr);
        eprintln!(
            "📲 Install the CA on your phone once: https://{host}:{}/peakbot-ca.crt",
            self.addr.port()
        );
        eprintln!(
            "   iOS: after installing, enable it under Settings → General → About → Certificate Trust Settings."
        );
        eprintln!("   CA stored at {}", tls_dir.display());
        self.maybe_open_browser(&url);

        let handle = axum_server::Handle::new();
        let shutdown = handle.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            shutdown.graceful_shutdown(Some(Duration::from_secs(3)));
        });

        let rustls_cfg =
            axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(server_config));
        match axum_server::bind_rustls(self.addr, rustls_cfg)
            .handle(handle)
            .serve(app.into_make_service())
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                anyhow::bail!("{}", format_addr_in_use(self.addr));
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// Serves the CA public certificate at `/peakbot-ca.crt` — tokenless on
/// purpose. The correct MIME + attachment disposition makes phones offer to
/// install it directly. A CA public cert carries no secret; only its private
/// key (never served, `0600` on disk) can sign.
async fn ca_cert_handler() -> Response {
    match tls::default_tls_dir().and_then(|dir| tls::ca_cert_pem(&dir)) {
        Ok(pem) => (
            [
                (header::CONTENT_TYPE, "application/x-x509-ca-cert"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"peakbot-ca.crt\"",
                ),
            ],
            pem,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("CA certificate unavailable: {e}"),
        )
            .into_response(),
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
pub async fn require_token(
    State(token): State<Arc<str>>,
    req: Request<Body>,
    next: Next,
) -> Response {
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

/// Sole owner of the WS sink. Generic over the sink so tests can inject a
/// stalled/recording/gated sink — a half-open peer is otherwise untestable.
/// Serialises every frame as JSON text, wraps with `WRITE_TIMEOUT` so a
/// stalled `send` tears the connection down (a timed-out send may have
/// written a partial frame into the TLS stream, so we never retry), and
/// interleaves `PING_INTERVAL` keepalives so the timeout can observe a
/// dead *idle* peer (see design §2.4).
pub(crate) async fn writer_loop<S>(mut sink: S, mut rx: OutboundRx)
where
    S: Sink<Message> + Unpin,
{
    let mut ping =
        tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    // If we're slow on a frame, do *not* back-to-back ping on recovery —
    // an unconditional 2-byte ping is cheaper than tracking the state.
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        let frame = tokio::select! {
            msg = rx.next() => match msg {
                Some(m) => match serde_json::to_string(&m) {
                    Ok(s) => Message::Text(s.into()),
                    Err(e) => {
                        tracing::error!("failed to serialise outbound message: {e:?}");
                        continue;
                    }
                },
                None => break, // all producers gone
            },
            _ = ping.tick() => Message::Ping(Bytes::new()),
        };
        // A timed-out `send` may have written a partial frame into the TLS
        // stream, so the sink is unusable afterwards: NEVER retry, always
        // tear down.
        match tokio::time::timeout(WRITE_TIMEOUT, sink.send(frame)).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => break, // socket closed
            Err(_) => {
                tracing::warn!("closing socket: write stalled >{WRITE_TIMEOUT:?} (half-open peer)");
                break;
            }
        }
    }
    let _ = sink.close().await;
}

/// Which task ended the forward loop. Encoded as a type so teardown cannot
/// re-await a `JoinHandle` that already completed (which panics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForwardExit {
    ReaderGone,
    WriterGone,
    StateEnded,
}

/// Pump `StateManager` snapshots into the connection's coalescing slot
/// until any leg of the connection dies. The `writer` arm is what turns
/// "the writer noticed the peer is dead" into "the socket detaches from
/// the registry" — without it a half-open socket would keep
/// `AttachState.attached > 0` forever and block the idle-TTL reaper.
pub(crate) async fn forward_state(
    state_rx: &mut mpsc::Receiver<AppState>,
    out: &OutboundTx,
    reader: &mut tokio::task::JoinHandle<()>,
    writer: &mut tokio::task::JoinHandle<()>,
) -> ForwardExit {
    loop {
        tokio::select! {
            maybe_state = state_rx.recv() => {
                let Some(app_state) = maybe_state else { return ForwardExit::StateEnded };
                let exit = app_state.exit_requested;
                if out.publish_state(Arc::new(app_state)).is_err() {
                    return ForwardExit::WriterGone;
                }
                if exit { return ForwardExit::StateEnded }
            }
            _ = &mut *reader => return ForwardExit::ReaderGone,
            _ = &mut *writer => return ForwardExit::WriterGone,
        }
    }
}

/// Drive one browser connection. The first frame must be `Attach`, which
/// binds the socket to a session in the registry (sharing an active one,
/// resuming a persisted one, or minting fresh). The socket then runs the
/// `StdioUi` three-task shape (writer sink, state broadcast, inbound reader)
/// over WS frames. On close it **detaches** — the session survives for other
/// sockets and for reconnect, and only expires via the reaper or a kill.
async fn handle_socket(socket: WebSocket, state: WsState) {
    let (ws_sink, mut ws_stream) = socket.split();

    // Bounded FIFO (32) for ordered frames + a 1-deep coalescing `watch`
    // slot for `state` snapshots. Per-socket memory is ~3 payload-sized
    // allocations regardless of peer behaviour — see `src/ui/outbound.rs`
    // for the two-class delivery contract.
    let (out_tx, out_rx) = outbound_channel();
    let mut writer_task = tokio::spawn(writer_loop(ws_sink, out_rx));

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

    // State broadcast: push every AppState snapshot to the client's
    // coalescing slot until any leg of the connection dies. The
    // `writer_task` arm is the missing one the inline version never had —
    // without it, a writer torn down by `WRITE_TIMEOUT` would never wake
    // this loop and the socket would leak `attached > 0` in the registry,
    // blocking the idle-TTL reaper.
    let mut state_rx = registered.session.state_manager.subscribe();
    let _exit = forward_state(&mut state_rx, &out_tx, &mut reader_task, &mut writer_task).await;

    // Teardown: stop the reader, drain the writer exactly once (the
    // forwarder already observed WriterGone if the writer died first;
    // awaiting again panics). The session stays alive in the registry for
    // other sockets / reconnect; it only ends via the reaper (idle TTL) or
    // an explicit kill.
    reader_task.abort();
    drop(out_tx);
    if !matches!(_exit, ForwardExit::WriterGone) {
        let _ = writer_task.await;
    }
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
    out_tx: &OutboundTx,
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
        Ok(InboundMessage::SelectPipeline { name }) => {
            action_sender.send(UiAction::SelectPipeline(name)).is_ok()
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

/// Redirect only the first document request to the setup SPA route. This sits
/// inside the existing token layer, so a token-bearing entry request receives
/// its auth cookie on this 303 before the browser follows `/setup`.
async fn setup_redirect() -> Response {
    (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, "/setup")],
        Body::empty(),
    )
        .into_response()
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

    #[test]
    fn format_addr_in_use_contains_expected_parts() {
        let addr: SocketAddr = "127.0.0.1:7823".parse().unwrap();
        let msg = format_addr_in_use(addr);
        assert!(msg.contains("127.0.0.1:7823"));
        assert!(msg.contains("already in use"));
        assert!(msg.contains("peakbot service status"));
        assert!(msg.contains("--bind"));
    }

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

    #[test]
    fn windowed_detection_matrix_is_pure() {
        assert!(!windowed_from(true, true, true, "linux"));
        assert!(windowed_from(false, false, true, "linux"));
        assert!(windowed_from(false, true, false, "linux"));
        assert!(!windowed_from(false, false, false, "linux"));
        assert!(windowed_from(false, false, false, "macos"));
        assert!(windowed_from(false, false, false, "windows"));
        assert!(!windowed_from(true, false, false, "macos"));
        assert!(!windowed_from(true, false, false, "windows"));
    }

    #[tokio::test]
    async fn root_serves_index_html() {
        let addr = spawn_app().await;
        let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
        let status = resp.status();
        let body = resp.text().await.unwrap();
        assert_eq!(status, 200, "body: {body}");
        let ct_index = body.find("Shifu").unwrap_or(usize::MAX);
        assert!(ct_index < 1024, "root body did not contain Shifu: {body}");
    }

    #[tokio::test]
    async fn first_run_root_redirects_to_setup() {
        let app: Router = Router::new()
            .route("/", get(setup_redirect))
            .fallback(static_handler);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let resp = client.get(format!("http://{addr}/")).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(resp.headers()[header::LOCATION], "/setup");
    }

    #[tokio::test]
    async fn unknown_route_falls_back_to_index() {
        let addr = spawn_app().await;
        let resp = reqwest::get(format!("http://{addr}/some/spa/route"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(body.contains("Shifu"), "SPA fallback body = {body:?}");
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

    // --- T7 / T9 / T13: WS outbound backpressure (design §5) ---
    //
    // These tests reference the planned `outbound` module API
    // (`OutboundTx`, `OutboundRx`, `outbound_channel`, `Disconnected`) and
    // the planned `writer_loop` / `forward_state` / `ForwardExit` items in
    // this module. None of them exist yet — that absence is the RED
    // baseline the design §5 documents. The bodies express the contract;
    // the implementer fills in the production code to make them pass.

    use crate::ui::app_state::{AppState, ChatMessage};
    use crate::ui::outbound::{OutboundRx, OutboundTx, outbound_channel};
    use crate::ui::wire::ModelInfo;
    use futures::Sink;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;
    use std::task::{Context, Poll};
    use tokio::sync::Semaphore;

    /// A `Sink<Message>` whose `poll_ready` and `poll_flush` return
    /// `Poll::Pending` forever. Models a half-open peer (kernel/TLS write
    /// buffers full, no FIN/RST) so a writer parked inside `send` is
    /// pinned there indefinitely — the production-incident condition.
    struct PendingSink;

    impl Sink<Message> for PendingSink {
        type Error = std::io::Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }

        fn start_send(self: Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    /// A `Sink<Message>` that records every frame it accepts and blocks
    /// each `poll_flush` until the test releases one permit on the
    /// semaphore. Lets the test gate exactly how many writes complete
    /// before the writer sees all producers gone.
    ///
    /// Implementation note: `poll_flush` stores its `Waker` in a shared
    /// slot so the test can wake the parked writer after `add_permits`.
    /// A `Semaphore`'s `try_acquire` does not register a waker on its
    /// own — only `acquire().await` does — so we need the explicit bridge
    /// to unblock a stalled flush.
    struct GatedSink {
        recorded: Arc<StdMutex<Vec<Message>>>,
        permits: Arc<Semaphore>,
        waker_slot: Arc<StdMutex<Option<std::task::Waker>>>,
    }

    impl Sink<Message> for GatedSink {
        type Error = std::io::Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            self.recorded
                .lock()
                .expect("gated-sink mutex poisoned")
                .push(item);
            Ok(())
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            // Store the waker so the test can wake us after adding permits.
            *self
                .waker_slot
                .lock()
                .expect("gated-sink waker mutex poisoned") = Some(cx.waker().clone());
            match self.permits.try_acquire() {
                Ok(_p) => Poll::Ready(Ok(())),
                Err(_) => Poll::Pending,
            }
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    /// T7 — incident regression. The design's `writer_loop` is generic
    /// over `S: Sink<Message> + Unpin`; we inject `PendingSink` to
    /// reproduce the half-open-peer condition without actually opening a
    /// socket. Live snapshot count must stay bounded and must not grow
    /// with the number of publishes — the invariance is the assertion.
    // The sentinel `usize::MAX` initialiser on `count_at_50`/`count_at_500`
    // is intentional (they're overwritten before being read) but trips the
    // unused-assignments lint; scope the allow to this test only.
    #[allow(unused_assignments)]
    #[tokio::test]
    async fn stalled_sink_keeps_exactly_one_snapshot_alive() {
        let (tx, rx): (OutboundTx, OutboundRx) = outbound_channel();
        let writer = tokio::spawn(writer_loop(PendingSink, rx));

        let mut weaks: Vec<std::sync::Weak<AppState>> = Vec::with_capacity(500);
        let mut count_at_50: usize = usize::MAX;
        let mut count_at_500: usize = usize::MAX;

        for i in 0..500 {
            let mut state = AppState::new();
            state.chat.messages = (0..(i + 1))
                .map(|n| ChatMessage::user(format!("m{n}")))
                .collect();
            let arc = Arc::new(state);
            weaks.push(Arc::downgrade(&arc));
            tx.publish_state(arc)
                .expect("producer must not be disconnected while tx is alive");
            // Give the writer task a chance to enter `sink.send(...).await`
            // and park on the first snapshot — without this yield the
            // writer never runs and the slot accumulates.
            tokio::task::yield_now().await;

            if i == 49 {
                count_at_50 = weaks.iter().filter(|w| w.upgrade().is_some()).count();
            }
        }

        count_at_500 = weaks.iter().filter(|w| w.upgrade().is_some()).count();

        // Drop the producer so the writer eventually exits; otherwise the
        // join handle leaks across the test boundary.
        drop(tx);
        let _ = writer.await;

        assert!(
            count_at_50 <= 2,
            "live snapshots after 50 publishes must be ≤ 2 (slot + at most one \
             in-flight); was {count_at_50}"
        );
        assert!(
            count_at_500 <= 2,
            "live snapshots after 500 publishes must be ≤ 2; was {count_at_500}"
        );
        // The invariance: a merely-slower leak would still pass the ≤2
        // bound at 50. The bug is that the count GROWS with publishes;
        // locking the equality is what catches the regression.
        assert_eq!(
            count_at_500, count_at_50,
            "live snapshot count must not grow between 50 and 500 publishes; \
             was {count_at_50} → {count_at_500}"
        );
    }

    /// T9 — locks the two-class delivery contract. Ordered control frames
    /// (`attached`/`ready`/`models_available`) must be delivered in send
    /// order, exactly once, never coalesced; `state` frames may be
    /// coalesced (newest wins) but the **last** one written must still be
    /// the newest published. A regression that drops ordered frames, or
    /// that reorders a state frame, would corrupt the SPA handshake.
    #[tokio::test]
    async fn slow_sink_coalesces_state_but_never_drops_ordered_frames() {
        let recorded: Arc<StdMutex<Vec<Message>>> = Arc::new(StdMutex::new(Vec::new()));
        let permits = Arc::new(Semaphore::new(0));
        let waker_slot: Arc<StdMutex<Option<std::task::Waker>>> = Arc::new(StdMutex::new(None));
        let sink = GatedSink {
            recorded: recorded.clone(),
            permits: permits.clone(),
            waker_slot: waker_slot.clone(),
        };
        let (tx, rx): (OutboundTx, OutboundRx) = outbound_channel();
        let writer = tokio::spawn(writer_loop(sink, rx));

        // Interleave 3 ordered frames with 50 state publishes — the
        // ordered trio is what the production code sends as the handshake.
        tx.send(OutboundMessage::Attached {
            convo: "c-1".to_string(),
        })
        .expect("ordered send must succeed before producer is dropped");
        for i in 0..50 {
            let mut state = AppState::new();
            state.chat.messages = (0..(i + 1))
                .map(|n| ChatMessage::user(format!("m{n}")))
                .collect();
            tx.publish_state(Arc::new(state))
                .expect("publish must succeed before producer is dropped");
        }
        tx.send(OutboundMessage::Ready)
            .expect("ordered send must succeed");
        tx.send(OutboundMessage::ModelsAvailable {
            active: "sonnet".to_string(),
            models: vec![ModelInfo {
                alias: "sonnet".to_string(),
                provider_name: "openrouter".to_string(),
                model_name: "anthropic/claude-sonnet-4.6".to_string(),
                context_size: 200_000,
            }],
        })
        .expect("ordered send must succeed");

        // Release enough permits to drain every queued frame (3 ordered
        // + ≤5 coalesced states = 8 max). Then wake the writer (it is
        // parked in `poll_flush` because permits started at 0).
        permits.add_permits(100);
        if let Some(w) = waker_slot
            .lock()
            .expect("gated-sink waker mutex poisoned")
            .take()
        {
            w.wake();
        }

        // Drop the producer so the writer sees "all producers gone" and
        // exits; then await the task so the recorded vec is final.
        drop(tx);
        let _ = writer.await;

        let frames = recorded.lock().expect("gated-sink mutex poisoned").clone();
        let mut ordered_kinds: Vec<&'static str> = Vec::new();
        let mut state_message_lens: Vec<usize> = Vec::new();
        for m in &frames {
            let text = match m {
                Message::Text(t) => t.clone(),
                other => panic!("unexpected non-text frame in GatedSink: {other:?}"),
            };
            match serde_json::from_str::<OutboundMessage>(&text)
                .expect("every recorded frame must deserialise as OutboundMessage")
            {
                OutboundMessage::Attached { .. } => ordered_kinds.push("attached"),
                OutboundMessage::Ready => ordered_kinds.push("ready"),
                OutboundMessage::ModelsAvailable { .. } => ordered_kinds.push("models_available"),
                OutboundMessage::State { state } => {
                    state_message_lens.push(state.chat.messages.len());
                }
                other => panic!("unexpected variant in GatedSink: {other:?}"),
            }
        }

        assert_eq!(
            ordered_kinds,
            vec!["attached", "ready", "models_available"],
            "ordered frames must be delivered in send order, each exactly once; got {ordered_kinds:?}"
        );
        assert!(
            state_message_lens.len() <= 5,
            "state frames must be coalesced to ≤ 5 (50 publishes → at most 5 writes); \
             got {} writes with message-lens {state_message_lens:?}",
            state_message_lens.len()
        );
        assert!(
            state_message_lens.contains(&50),
            "the last coalesced state frame must be the newest (snapshot #50, \
             chat.messages.len() == 50); got lens {state_message_lens:?}"
        );
    }

    /// T13 — the literal missing `select!` arm. The current inline
    /// forwarder at `handle_socket` selects on `state_rx` and
    /// `reader_task` but **not** on `writer_task`, so a writer that has
    /// already returned never wakes the forwarder. The fix extracts
    /// `forward_state` with a third arm; this test is its contract.
    ///
    /// RED against the current code in two stages: first the function
    /// does not exist (compile error), then — once extracted without the
    /// arm — the call hangs past the 1 s timeout. Either way, the
    /// assertion is the one that encodes the bug.
    #[tokio::test]
    async fn forwarder_exits_when_writer_dies() {
        // state_rx that never yields — controls out the StateEnded arm.
        let (_state_tx, mut state_rx) = mpsc::channel::<AppState>(1);

        // reader that never completes — controls out the ReaderGone arm.
        let mut reader: tokio::task::JoinHandle<()> =
            tokio::spawn(async { futures::future::pending::<()>().await });

        // writer that has already finished — the arm under test.
        let mut writer: tokio::task::JoinHandle<()> = tokio::spawn(async {});

        // The forwarder needs an `OutboundTx`; we never use it because the
        // writer arm must fire first, but the function signature requires
        // one. Disconnect does not matter to the assertion.
        let (out, _rx): (OutboundTx, OutboundRx) = outbound_channel();
        let _ = _rx;

        let exit = tokio::time::timeout(
            Duration::from_secs(1),
            forward_state(&mut state_rx, &out, &mut reader, &mut writer),
        )
        .await
        .expect("forwarder must exit promptly when the writer task has already returned");

        assert!(
            matches!(exit, ForwardExit::WriterGone),
            "forwarder must return WriterGone when the writer task completes first; got {exit:?}"
        );
    }

    /// T14 — mirror of T13: a reader that returns must surface as
    /// `ReaderGone`. Guards the existing prompt-close path the inline
    /// forwarder added deliberately (the comment block at `handle_socket`).
    #[tokio::test]
    async fn forwarder_exits_when_reader_ends() {
        let (_state_tx, mut state_rx) = mpsc::channel::<AppState>(1);

        // reader that has already finished — the arm under test.
        let mut reader: tokio::task::JoinHandle<()> = tokio::spawn(async {});

        // writer that never completes — controls out the WriterGone arm.
        let mut writer: tokio::task::JoinHandle<()> =
            tokio::spawn(async { futures::future::pending::<()>().await });

        let (out, _rx): (OutboundTx, OutboundRx) = outbound_channel();
        let _ = _rx;

        let exit = tokio::time::timeout(
            Duration::from_secs(1),
            forward_state(&mut state_rx, &out, &mut reader, &mut writer),
        )
        .await
        .expect("forwarder must exit promptly when the reader task has already returned");

        assert!(
            matches!(exit, ForwardExit::ReaderGone),
            "forwarder must return ReaderGone when the reader task completes first; got {exit:?}"
        );
    }

    /// T8 — a stalled `sink.send` cannot pin the writer forever: the
    /// `WRITE_TIMEOUT` reaper must close it (and the connection). The
    /// paused-time advance fires the timeout deterministically.
    ///
    /// Note: `tokio::time::advance` only yields *once* (via its internal
    /// `yield_now`). We use that one yield to drive the writer into
    /// `sink.send`, then a second advance fires the timeout that was
    /// started at the sink. With a single advance the timeout would be
    /// created *after* the time-jump and never fire.
    #[tokio::test(start_paused = true)]
    async fn stalled_sink_is_torn_down_after_write_timeout() {
        let (tx, rx) = outbound_channel();
        let writer = tokio::spawn(writer_loop(PendingSink, rx));

        tx.publish_state(Arc::new(AppState::new()))
            .expect("publish while rx is alive");

        // First advance — its internal yield drives the writer into
        // `sink.send`, where it creates the Sleep that backs
        // `WRITE_TIMEOUT`. The advance itself doesn't fire that Sleep
        // because the Sleep didn't exist when the clock was bumped.
        tokio::time::advance(Duration::from_secs(1)).await;

        // Now the writer is parked in `timeout(WRITE_TIMEOUT, sink.send(...))`.
        // Advance past WRITE_TIMEOUT so the Sleep fires.
        tokio::time::advance(WRITE_TIMEOUT).await;

        // Once the timeout reaper tears down, the writer task must complete
        // promptly (the timeout resolution itself causes the break/close).
        tokio::time::timeout(Duration::from_secs(1), writer)
            .await
            .expect("writer must exit within 1s after the timeout reaper fires")
            .expect("writer task must not panic");

        // And producers see `Disconnected` afterwards.
        assert!(
            tx.publish_state(Arc::new(AppState::new())).is_err(),
            "publish_state must return Err after the writer tore down"
        );
    }

    /// T10 — an idle socket must still generate traffic (pings) so the
    /// write timeout and the kernel's retransmission give-up can observe
    /// a half-open peer even when no app data flows.
    ///
    /// `MissedTickBehavior::Delay` reschedules each tick to `now + period`
    /// after the previous one — so one big advance doesn't burst-deliver
    /// three pings. We do many small advances and yield between them so
    /// the runtime has a chance to poll the writer and emit each ping in
    /// turn. RecordingSink returns `Ready` everywhere, so each ping writes
    /// immediately.
    #[tokio::test(start_paused = true)]
    async fn idle_socket_sends_keepalive_pings() {
        let recorded: Arc<StdMutex<Vec<Message>>> = Arc::new(StdMutex::new(Vec::new()));
        let close_count: Arc<StdMutex<usize>> = Arc::new(StdMutex::new(0));
        let sink = RecordingSink {
            recorded: recorded.clone(),
            close_count: close_count.clone(),
        };
        let (tx, rx) = outbound_channel();
        let writer = tokio::spawn(writer_loop(sink, rx));

        // Drive the writer through enough time for ≥ 3 pings under
        // MissedTickBehavior::Delay. Each small `advance` yields once,
        // which gives the runtime a chance to poll the writer and let it
        // emit the next ping.
        for _ in 0..(PING_INTERVAL.as_secs() as usize * 4) {
            tokio::time::advance(Duration::from_secs(1)).await;
            // The yields inside `advance` only drive *one* ready task per
            // call; an extra yield lets the writer re-enter `select!`
            // after emitting a ping.
            tokio::task::yield_now().await;
        }

        // Drop the producer so the writer exits cleanly, then await it.
        drop(tx);
        let _ = writer.await;

        let frames = recorded
            .lock()
            .expect("recording-sink mutex poisoned")
            .clone();
        let pings = frames
            .iter()
            .filter(|m| matches!(m, Message::Ping(_)))
            .count();
        let texts = frames
            .iter()
            .filter(|m| matches!(m, Message::Text(_)))
            .count();
        assert!(
            pings >= 3,
            "writer must emit ≥ 3 ping frames during idleness; got {pings}"
        );
        assert_eq!(texts, 0, "idle writer must not emit text frames");
    }

    /// T11 — healthy sink: the handshake trio is delivered in send order
    /// and the sink is closed exactly once on producer drop.
    #[tokio::test]
    async fn healthy_sink_writes_every_ordered_frame_then_closes() {
        let recorded: Arc<StdMutex<Vec<Message>>> = Arc::new(StdMutex::new(Vec::new()));
        let close_count: Arc<StdMutex<usize>> = Arc::new(StdMutex::new(0));
        let sink = RecordingSink {
            recorded: recorded.clone(),
            close_count: close_count.clone(),
        };
        let (tx, rx) = outbound_channel();
        let writer = tokio::spawn(writer_loop(sink, rx));

        tx.send(OutboundMessage::Attached {
            convo: "c-1".to_string(),
        })
        .unwrap();
        tx.send(OutboundMessage::Ready).unwrap();
        tx.send(OutboundMessage::ModelsAvailable {
            active: "sonnet".to_string(),
            models: vec![ModelInfo {
                alias: "sonnet".to_string(),
                provider_name: "openrouter".to_string(),
                model_name: "anthropic/claude-sonnet-4.6".to_string(),
                context_size: 200_000,
            }],
        })
        .unwrap();

        drop(tx);
        let _ = writer.await;

        let frames = recorded
            .lock()
            .expect("recording-sink mutex poisoned")
            .clone();
        let texts: Vec<String> = frames
            .iter()
            .filter_map(|m| match m {
                Message::Text(t) => Some(t.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts.len(),
            3,
            "exactly the three handshake frames; got {texts:?}"
        );
        let kinds: Vec<&'static str> = texts
            .iter()
            .map(|s| {
                serde_json::from_str::<OutboundMessage>(s)
                    .map(|m| match m {
                        OutboundMessage::Attached { .. } => "attached",
                        OutboundMessage::Ready => "ready",
                        OutboundMessage::ModelsAvailable { .. } => "models_available",
                        other => panic!("unexpected variant: {other:?}"),
                    })
                    .unwrap_or_else(|e| panic!("unparseable frame: {e}: {s}"))
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["attached", "ready", "models_available"],
            "handshake trio must be delivered in send order"
        );
        assert_eq!(
            *close_count.lock().expect("recording-sink mutex poisoned"),
            1,
            "sink.close() must be called exactly once on producer drop"
        );
    }

    /// A `Sink<Message>` that records every frame and counts `close()`
    /// invocations. Used by T10/T11 to assert delivery ordering and the
    /// one-and-only-one close on producer drop.
    struct RecordingSink {
        recorded: Arc<StdMutex<Vec<Message>>>,
        close_count: Arc<StdMutex<usize>>,
    }

    impl Sink<Message> for RecordingSink {
        type Error = std::io::Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            self.recorded
                .lock()
                .expect("recording-sink mutex poisoned")
                .push(item);
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            *self
                .close_count
                .lock()
                .expect("recording-sink mutex poisoned") += 1;
            Poll::Ready(Ok(()))
        }
    }
}
