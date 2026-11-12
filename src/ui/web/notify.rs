//! The turn-end feed: `GET /api/notify/stream` — a token-gated Server-Sent
//! Events stream with one `data:` JSON frame per finished agent turn (PR-B
//! design §5.5/§6.2). Merged before the token layer in `WebUi::router` iff
//! notifications are enabled; absent (→ the `/api/*` 404) when they are off.
//!
//! The handler's only state is the `TurnEndTx`: it can't attach, create or
//! keep alive a session (invariant I4), so the reaper ignores it.

use crate::notify::TurnEndTx;
use axum::Router;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use futures::stream::Stream;
use std::time::Duration;

/// Keepalive cadence: a `:` comment every 15 s keeps buffering proxies and
/// idle timeouts from killing a feed that is legitimately quiet.
const KEEPALIVE: Duration = Duration::from_secs(15);

/// The feed's route table (PR-B design §6.1).
pub(crate) fn router(turn_ends: TurnEndTx) -> Router {
    Router::new()
        .route("/api/notify/stream", get(stream))
        .with_state(turn_ends)
}

/// `GET /api/notify/stream` — the endless SSE body. The subscription
/// happens **before** the response is returned, so a client that has the
/// headers cannot miss an event published afterwards (design §5.5).
async fn stream(
    State(tx): State<TurnEndTx>,
) -> Sse<impl Stream<Item = Result<Event, serde_json::Error>>> {
    let rx = tx.subscribe();
    let body = futures::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    // Compact JSON has no raw newlines, so `Event::data`
                    // never needs to split the frame.
                    return Some((
                        serde_json::to_string(&ev).map(|json| Event::default().data(json)),
                        rx,
                    ));
                }
                // A lag means a wedged client; skip the dropped events and
                // keep streaming the rest (design §7, no lag frames).
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("turn-end feed: dropped {n} event(s) for a slow client");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(body).keep_alive(KeepAlive::new().interval(KEEPALIVE))
}

// ─────────────────────────────────────────────────────────────────────────────
// PR-B §9.5: the HTTP contract of the feed, tested through the REAL route
// table and token layer (`WebUi::router`) on a random loopback port.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use crate::notify::{TurnOutcome, TurnSource};
    use crate::session::test_support::test_deps;
    use crate::ui::WebUi;
    use crate::ui::web::registry::SessionRegistry;
    use crate::ui::web::settings::{SettingsBoot, TokenSource};
    use axum::http::StatusCode;
    use futures::StreamExt;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;
    use uuid::Uuid;

    /// The real route table + token layer, from deps that carry the turn-end
    /// feed (mirrors the default `general.notifications.enabled: true`).
    async fn spawn(
        deps: Arc<crate::session::SessionDeps>,
        token: &str,
    ) -> (SocketAddr, SessionRegistry) {
        let (addr, registry) = spawn_with(Some(deps), Some(token)).await;
        (addr, registry.expect("deps present"))
    }

    async fn spawn_with(
        deps: Option<Arc<crate::session::SessionDeps>>,
        token: Option<&str>,
    ) -> (SocketAddr, Option<SessionRegistry>) {
        let dir = tempfile::tempdir().expect("boot dir");
        let settings = SettingsBoot {
            config_dir: dir.path().to_path_buf(),
            boot_cwd: dir.path().to_path_buf(),
            migrate_error: None,
            token_source: TokenSource::Cli,
        };
        let ui = WebUi::new(
            "127.0.0.1:0".parse().expect("loopback"),
            deps.clone(),
            Vec::new(),
            "local".into(),
            token.map(str::to_string),
            false,
            Vec::new(),
            settings,
        );
        let registry = deps.map(SessionRegistry::new);
        let app = ui.router();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (addr, registry)
    }

    /// A client that follows no redirects and keeps no cookie store — the
    /// auth path is tested honestly, like the token tests in `web/mod.rs`.
    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    /// Open the feed with the token cookie.
    async fn open_stream(addr: SocketAddr) -> reqwest::Response {
        client()
            .get(format!("http://{addr}/api/notify/stream"))
            .header(reqwest::header::COOKIE, "peakbot_token=s3cret")
            .send()
            .await
            .expect("the request goes out")
    }

    /// Read an open SSE body until a `data:` frame naming `convo` arrives
    /// (≤5 s); returns the frame's JSON payload.
    async fn read_turn_end_frame<S, T>(mut stream: S, convo: Uuid) -> String
    where
        S: StreamExt<Item = Result<T, reqwest::Error>> + Unpin,
        T: AsRef<[u8]>,
    {
        let mut buf = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let chunk = tokio::time::timeout_at(deadline, stream.next())
                .await
                .expect("a turn-end frame arrives within 5s")
                .expect("the stream stays open")
                .expect("chunk reads");
            buf.push_str(&String::from_utf8_lossy(chunk.as_ref()));
            if let Some(line) = buf.lines().find(|l| l.starts_with("data: ")) {
                let payload = line["data: ".len()..].to_string();
                if payload.contains(&convo.to_string()) {
                    return payload;
                }
            }
        }
    }

    /// Mint a session through the registry and end one user turn on it.
    fn end_one_turn(registry: &SessionRegistry) -> Uuid {
        let r = registry.attach(None).expect("attach mints a session");
        let convo = r.session.conversation_id;
        r.session.state_manager.begin_turn(TurnSource::User);
        r.session.state_manager.end_turn(TurnOutcome::Ok);
        registry.detach(convo);
        convo
    }

    #[tokio::test]
    async fn stream_requires_the_token() {
        let (addr, _) = spawn(Arc::new(test_deps(None)), "s3cret").await;
        let c = client();
        let resp = c
            .get(format!("http://{addr}/api/notify/stream"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "no token");
        let resp = c
            .get(format!("http://{addr}/api/notify/stream?token=wrong"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "wrong token");
    }

    #[tokio::test]
    async fn stream_is_event_stream() {
        let (addr, _) = spawn(Arc::new(test_deps(None)), "s3cret").await;
        let resp = open_stream(addr).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .expect("content-type")
            .to_str()
            .unwrap();
        assert!(
            ct.starts_with("text/event-stream"),
            "the feed is SSE — got: {ct}"
        );
    }

    #[tokio::test]
    async fn stream_delivers_a_turn_end_within_one_second() {
        let (addr, registry) = spawn(Arc::new(test_deps(None)), "s3cret").await;
        // Open the stream first: headers received ⇒ subscribed (design §5.5),
        // so the turn ended after this point cannot be missed.
        let resp = open_stream(addr).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let convo = end_one_turn(&registry);

        let payload = read_turn_end_frame(resp.bytes_stream(), convo).await;
        let v: serde_json::Value = serde_json::from_str(&payload).expect("the frame is JSON");
        assert_eq!(v["type"], "turn_end");
        assert_eq!(v["convo"], convo.to_string(), "the session's conversation");
        assert_eq!(v["outcome"], "ok");
        assert_eq!(v["source"], "user");
    }

    #[tokio::test]
    async fn two_streams_both_receive() {
        let (addr, registry) = spawn(Arc::new(test_deps(None)), "s3cret").await;
        let r1 = open_stream(addr).await;
        let r2 = open_stream(addr).await;
        assert_eq!(r1.status(), StatusCode::OK);
        assert_eq!(r2.status(), StatusCode::OK);

        let convo = end_one_turn(&registry);

        let (p1, p2) = tokio::join!(
            read_turn_end_frame(r1.bytes_stream(), convo),
            read_turn_end_frame(r2.bytes_stream(), convo)
        );
        for (i, p) in [p1, p2].into_iter().enumerate() {
            let v: serde_json::Value = serde_json::from_str(&p).expect("every frame is JSON");
            assert_eq!(v["type"], "turn_end", "stream {i}");
            assert_eq!(v["convo"], convo.to_string(), "stream {i}");
        }
    }

    #[tokio::test]
    async fn open_stream_does_not_block_the_reaper() {
        let (addr, registry) = spawn(Arc::new(test_deps(None)), "s3cret").await;
        // Keep a stream open: it must count as nothing to the reaper.
        let resp = open_stream(addr).await;

        let r = registry.attach(None).expect("attach");
        let convo = r.session.conversation_id;
        registry.detach(convo);
        // Two zero-TTL ticks, like the reaper loop: arm, then reap.
        registry.reap(Duration::ZERO);
        registry.reap(Duration::ZERO);
        assert!(
            !registry.active_ids().contains(&convo),
            "the detached session must be reaped while only a feed stream is open"
        );

        // The stream itself is still streaming: a short chunk read times out
        // (a 404 body — the RED state — would end and return `None`).
        let mut stream = resp.bytes_stream();
        let chunk = tokio::time::timeout(Duration::from_millis(500), stream.next()).await;
        assert!(
            chunk.is_err(),
            "the open stream must not have ended; got: {chunk:?}"
        );
    }

    #[tokio::test]
    async fn stream_creates_no_session() {
        let (addr, registry) = spawn(Arc::new(test_deps(None)), "s3cret").await;
        let resp = open_stream(addr).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            registry.active_ids().is_empty(),
            "opening the feed must not mint a session"
        );
        drop(resp);
    }

    #[tokio::test]
    async fn disabled_returns_404() {
        let mut deps = test_deps(None);
        deps.turn_ends = None; // `general.notifications.enabled: false`
        let (addr, _) = spawn(Arc::new(deps), "s3cret").await;
        let resp = open_stream(addr).await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "no channel ⇒ no route ⇒ the /api catch-all 404s"
        );
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("error"),
            "the 404 is the JSON envelope: {body}"
        );
    }

    #[tokio::test]
    async fn needs_setup_returns_404() {
        let (addr, registry) = spawn_with(None, Some("s3cret")).await;
        let resp = open_stream(addr).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "no deps ⇒ no route");
        assert!(
            registry.as_ref().is_none_or(|r| r.active_ids().is_empty()),
            "needs_setup has no registry to mint from"
        );
    }
}
