//! `peakbot --web` — the Web `Ui` implementation.
//!
//! Phase 0 (this file) is the **static shell**: it embeds `web/dist/` and
//! serves it via axum with an SPA fallback (unknown routes → `index.html`).
//! No WebSocket route yet — Phase 1 adds `GET /ws` and the per-connection
//! session factory call.
//!
//! `WebUi::run` blocks on the axum server's graceful-shutdown future. Each
//! browser tab opens its own WebSocket; each WS connection builds its own
//! fresh session (see `crate::session`, Phase 1) so tabs are independent
//! agents, not windows onto one shared conversation. Phase 0 has no
//! sessions to build — it just serves the SPA bundle.
//!
//! ## Static handler — why hand-rolled
//!
//! `axum-embed` 0.1.0 is axum-0.7-only and unmaintained. We use the
//! first-party `axum` feature of `rust-embed` 8.x plus a small
//! hand-rolled `IntoResponse` for `EmbeddedFile` to get the right
//! `Content-Type` (mime_guess) and SPA fallback. ETag/304 + compression
//! are deferred to Phase 4 (remote access); for loopback Phase 0 the
//! browser doesn't care.

use crate::ui::Ui;
use anyhow::Result;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;
use std::net::SocketAddr;

/// Port the web UI listens on. Fixed for Phase 0 (`--port` flag is
/// Phase 4). See `webui.md` §3 decision 1.
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

/// `peakbot --web` — serves the embedded SPA on a fixed loopback port.
/// `run` blocks on the axum server until Ctrl+C triggers graceful shutdown.
pub struct WebUi {
    addr: SocketAddr,
}

impl WebUi {
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr }
    }
}

impl Ui for WebUi {
    async fn init(&mut self) -> Result<()> {
        // The axum server runs in `run`. Nothing to do here.
        Ok(())
    }

    async fn run(&mut self) -> Result<()> {
        let app: Router = Router::new().fallback(static_handler);

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
}
