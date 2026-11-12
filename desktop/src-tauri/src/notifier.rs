//! The turn-end notifier (PR-C design §4.4/§6.2): owns the SSE listener
//! task for the current `Ready` phase, shows native notifications and
//! handles their clicks. The only I/O in the feature; the decisions live
//! in `core::notify`.

use std::sync::Mutex;

use tauri::Manager;
use tauri_plugin_notification::{ActionPerformed, NotificationExt};
use url::Url;
use uuid::Uuid;

use peakbot_desktop_core::consts::{NOTIFY_CONNECT_TIMEOUT, NOTIFY_READ_TIMEOUT};
use peakbot_desktop_core::notify::{
    self, StopReason, TurnEnd, Verdict, open_convo_js, parse_event, retry_delay, toast,
};
use peakbot_desktop_core::paths::peakbot_ca_cert;
use peakbot_desktop_core::phase::Phase;
use peakbot_desktop_core::web_target::WebTarget;

/// How one connection attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    /// 404 / 401 / 403: stop until the next `Ready`.
    Stop(StopReason),
    /// We had a 200 stream and it ended (EOF or read error): reset the backoff.
    Closed,
    /// Never got a 200 stream (connect/TLS error, timeout, 5xx).
    Failed,
}

/// The listener for the current `Ready` phase. At most one task exists
/// (the Mutex holds it); `follow` always aborts the old one first.
pub struct Notifier {
    app: tauri::AppHandle,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl Notifier {
    /// Registers the notification click handler (must precede any `show`:
    /// the plugin reports clicks only to registered handlers).
    pub fn new(app: tauri::AppHandle) -> Self {
        // `on_action` never fails on desktop, but the signature returns a
        // Result (it matches the mobile one).
        if let Err(e) = app.notification().on_action({
            let app = app.clone();
            move |p: &ActionPerformed| {
                let Some(convo) = convo_from_action(p) else {
                    return;
                };
                // Focus first, then open the conversation in the page
                // (one-way eval; a non-SPA page no-ops on `?.`).
                crate::window::focus_main(&app);
                let Some(win) = app.get_webview_window(crate::window::MAIN_LABEL) else {
                    return;
                };
                if let Err(e) = win.eval(open_convo_js(convo)) {
                    log::warn!("notification click: the page eval failed: {e}");
                }
            }
        }) {
            log::error!("registering the notification click handler failed: {e}");
        }
        Self {
            app,
            task: Mutex::new(None),
        }
    }

    /// `Ready(t)` → (re)start the listener for `t`; any other phase → stop it.
    /// Non-blocking; safe while quitting (no main-thread call).
    pub fn follow(&self, phase: &Phase) {
        let mut task = self.task.lock().expect("notifier task lock");
        // Abort before spawning: the dropped connection must close before a
        // quit/restart stops the server, or every shutdown ends in a
        // SIGKILL (design §5.4).
        if let Some(t) = task.take() {
            t.abort()
        }
        if let Phase::Ready(target) = phase {
            let app = self.app.clone();
            *task = Some(tauri::async_runtime::spawn(run(app, target.clone())));
        }
    }
}

/// The listener task: connect, stream, reconnect with backoff; stop on a
/// definitive 404/401/403 (which won't change until the next `Ready`).
async fn run(app: tauri::AppHandle, target: WebTarget) {
    let client = match feed_client(&target) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("notifications unavailable: {e}");
            return;
        }
    };
    let url = target.notify_url();
    let mut failures = 0u32;
    loop {
        match stream_feed(&client, url.clone(), |ev| show(&app, &ev)).await {
            Attempt::Stop(StopReason::Disabled) => {
                log::info!("notifications are off on this PeakBot");
                return;
            }
            Attempt::Stop(StopReason::Rejected) => {
                log::warn!("the notification feed refused the token");
                return;
            }
            Attempt::Closed => failures = 0,
            Attempt::Failed => failures = failures.saturating_add(1),
        }
        tokio::time::sleep(retry_delay(failures)).await;
    }
}

/// The feed's HTTP client: direct (never via a proxy), with the locked
/// timeouts; a TLS target pins PeakBot's own CA (the file the webview
/// already trusts).
fn feed_client(target: &WebTarget) -> Result<reqwest::Client, String> {
    // `rustls-no-provider`: `build()` panics without a provider installed
    // (idempotent; the updater installs the same one).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(NOTIFY_CONNECT_TIMEOUT)
        .read_timeout(NOTIFY_READ_TIMEOUT);
    if target.tls {
        let Some(pem_path) = peakbot_ca_cert() else {
            return Err("no CA certificate path".into());
        };
        let pem =
            std::fs::read(&pem_path).map_err(|e| format!("reading the CA certificate: {e}"))?;
        let cert = reqwest::Certificate::from_pem(&pem)
            .map_err(|e| format!("the CA certificate does not parse: {e}"))?;
        builder = builder.tls_certs_only([cert]);
    }
    builder
        .build()
        .map_err(|e| format!("building the feed client: {e}"))
}

/// One feed connection: `GET` the URL, map the status through
/// `notify::verdict`, then decode SSE frames and forward every `TurnEnd`
/// to `on_event`. Never returns `Err`: the loop in `run` owns retries.
pub(crate) async fn stream_feed(
    client: &reqwest::Client,
    url: Url,
    mut on_event: impl FnMut(TurnEnd),
) -> Attempt {
    // N4: reqwest errors are logged with `.without_url()` — the URL
    // carries the token and must never reach a log.
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            log::warn!("notification feed unreachable: {}", e.without_url());
            return Attempt::Failed;
        }
    };
    match notify::verdict(resp.status().as_u16()) {
        Verdict::Stop(r) => return Attempt::Stop(r),
        Verdict::Retry => {
            log::warn!("the notification feed answered {}", resp.status());
            return Attempt::Failed;
        }
        Verdict::Stream => {}
    }
    let mut dec = notify::SseDecoder::default();
    let mut resp = resp;
    loop {
        match resp.chunk().await {
            Ok(Some(bytes)) => {
                for data in dec.feed(&bytes) {
                    match parse_event(&data) {
                        Some(ev) => on_event(ev),
                        None => log::debug!("skipped a feed frame"),
                    }
                }
            }
            // The server ended the stream (e.g. it is shutting down): a
            // healthy end, reconnect after 1 s — not a failure.
            Ok(None) => return Attempt::Closed,
            // Incl. the read timeout (a dead stream).
            Err(e) => {
                log::warn!("the notification feed stream ended: {}", e.without_url());
                return Attempt::Closed;
            }
        }
    }
}

/// Show a notification for `ev`, unless the user is looking at the window
/// (read at event time: `is_visible` AND `is_focused`, no cached flag).
fn show(app: &tauri::AppHandle, ev: &TurnEnd) {
    let looking = app
        .get_webview_window(crate::window::MAIN_LABEL)
        .is_some_and(|win| win.is_visible().unwrap_or(false) && win.is_focused().unwrap_or(false));
    let Some(t) = toast(ev, looking) else {
        return;
    };
    if let Err(e) = app
        .notification()
        .builder()
        .title(t.title)
        .body(t.body)
        .extra("convo", ev.convo.to_string())
        .show()
    {
        log::warn!("showing the notification failed: {e}");
    }
}

/// The conversation a notification click refers to: `action_id == "tap"`
/// and a `convo` extra that parses as a `Uuid`; anything else is ignored.
fn convo_from_action(p: &ActionPerformed) -> Option<Uuid> {
    // The plugin maps every platform click to the action id "tap".
    if p.action_id() != "tap" {
        return None;
    }
    // `extra` round-trips as JSON: our string comes back as a JSON string.
    let extra = p.notification()?.extra();
    let convo = extra.get("convo")?.as_str()?;
    Uuid::parse_str(convo).ok()
}

// ─────────────────────────────────────────────────────────────────────────────
// PR-C design §9.3: the feed client against a canned raw HTTP/1.1 server
// (a std TcpListener in a std thread — no Tauri app, no tokio-macros).
// Driven by `tauri::async_runtime::block_on`. TLS is not unit-tested
// (design §9.3); it is covered manually.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    /// The two valid frames the streaming tests send (PR-B §4.2 wire shape).
    const FRAME_A: &str = r#"{"type":"turn_end","convo":"6f1c1b9a-52ab-4c3d-9e21-8a3b0c1d2e3f","title":"Fix the reaper race","outcome":"ok","source":"user","ts":1}"#;
    const FRAME_B: &str = r#"{"type":"turn_end","convo":"7a2d2c0b-63bc-4d4e-af32-9b4c1d2e3f40","title":"Second","outcome":"error","source":"background","ts":2}"#;
    const CONVO_A: &str = "6f1c1b9a-52ab-4c3d-9e21-8a3b0c1d2e3f";
    const CONVO_B: &str = "7a2d2c0b-63bc-4d4e-af32-9b4c1d2e3f40";

    /// A body chunk, optionally preceded by a pause (splits a frame across
    /// two writes).
    type Chunk = (Vec<u8>, Option<Duration>);

    /// One canned HTTP/1.1 response, connection-close framed.
    struct Canned {
        status: &'static str,
        headers: &'static str,
        body: Vec<Chunk>,
        /// Keep the socket open this long after the body (the stall test).
        hold: Option<Duration>,
    }

    fn sse(canned_body: Vec<Chunk>, hold: Option<Duration>) -> Canned {
        Canned {
            status: "200 OK",
            headers: "content-type: text/event-stream\r\n",
            body: canned_body,
            hold,
        }
    }

    fn plain(status: &'static str) -> Canned {
        Canned {
            status,
            headers: "",
            body: Vec::new(),
            hold: None,
        }
    }

    /// Bind a loopback listener and serve ONE canned response in a thread:
    /// read the request head (recording the request line), write the
    /// response, then drop the socket (which ends the body). Returns the
    /// base URL and the recorded request line.
    fn canned_server(canned: Canned) -> (String, Arc<Mutex<Option<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback listener");
        let addr = listener.local_addr().expect("local addr");
        let request_line = Arc::new(Mutex::new(None));
        let recorded = request_line.clone();
        thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("the client connects");
            // Read the request head one byte at a time (a short GET).
            let mut head = Vec::new();
            let mut one = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if sock.read_exact(&mut one).is_err() {
                    break;
                }
                head.push(one[0]);
            }
            if let Some(end) = head.iter().position(|&b| b == b'\r') {
                *recorded.lock().expect("request-line lock") =
                    Some(String::from_utf8_lossy(&head[..end]).into_owned());
            }
            let _ = sock.write_all(format!("HTTP/1.1 {} \r\n", canned.status).as_bytes());
            let _ = sock.write_all(canned.headers.as_bytes());
            // The blank line that ends the header section (HTTP/1.1):
            // without it no client can tell where the body starts.
            let _ = sock.write_all(b"\r\n");
            for (chunk, pause) in &canned.body {
                if let Some(p) = pause {
                    thread::sleep(*p);
                }
                let _ = sock.write_all(chunk);
                let _ = sock.flush();
            }
            if let Some(hold) = canned.hold {
                thread::sleep(hold);
            }
            // Socket dropped: connection close ends the body.
        });
        (format!("http://{addr}"), request_line)
    }

    /// A URL whose port is closed right after the probe bind (refused).
    fn refused_url() -> Url {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);
        Url::parse(&format!("http://127.0.0.1:{port}/api/notify/stream")).expect("url parses")
    }

    fn client(read_timeout: Duration) -> reqwest::Client {
        // `rustls-no-provider`: a provider must be installed before a
        // Client is built (idempotent; the real feed_client does the same).
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::builder()
            .no_proxy()
            .read_timeout(read_timeout)
            .build()
            .expect("client builds")
    }

    /// The feed URL the tests open (the token is percent-encoded, as
    /// `WebTarget::notify_url` builds it).
    fn feed_url(base: &str) -> Url {
        Url::parse(&format!("{base}/api/notify/stream?token=tok%2Ben%261")).expect("url")
    }

    /// Run `stream_feed` against the canned server and collect the events.
    fn run_feed(canned: Canned) -> (Attempt, Vec<TurnEnd>, Arc<Mutex<Option<String>>>) {
        let (base, request_line) = canned_server(canned);
        let client = client(Duration::from_secs(5));
        let events: Arc<Mutex<Vec<TurnEnd>>> = Arc::new(Mutex::new(Vec::new()));
        let evs = events.clone();
        let attempt =
            tauri::async_runtime::block_on(stream_feed(&client, feed_url(&base), |ev: TurnEnd| {
                evs.lock().expect("events lock").push(ev)
            }));
        (
            attempt,
            events.lock().expect("events lock").clone(),
            request_line,
        )
    }

    // feed_streams_events_in_order_then_closed — 200 + a keepalive comment
    // + two turn-end frames (the 2nd split across two writes), then the
    // server closes: both events arrive in order, the attempt is Closed,
    // and the request line carried the token query.
    #[test]
    fn feed_streams_events_in_order_then_closed() {
        let frame_b = format!("data: {FRAME_B}\n\n");
        let b = frame_b.as_bytes();
        let mid = b.len() / 2;
        let (head, tail) = b.split_at(mid);
        let (attempt, events, request_line) = run_feed(sse(
            vec![
                (b":\n\n".to_vec(), None),
                (format!("data: {FRAME_A}\n\n").into_bytes(), None),
                (head.to_vec(), Some(Duration::from_millis(50))),
                (tail.to_vec(), None),
            ],
            None,
        ));
        assert_eq!(
            attempt,
            Attempt::Closed,
            "a 200 stream that the server ends is Closed"
        );
        assert_eq!(events.len(), 2, "both frames arrive, in order");
        assert_eq!(events[0].convo, Uuid::parse_str(CONVO_A).expect("uuid"));
        assert_eq!(events[1].convo, Uuid::parse_str(CONVO_B).expect("uuid"));
        assert_eq!(
            request_line.lock().expect("lock").as_deref(),
            Some("GET /api/notify/stream?token=tok%2Ben%261 HTTP/1.1"),
            "the feed request carries the token query"
        );
    }

    // feed_skips_bad_frames — a garbage payload and an unknown `type` are
    // skipped; the one valid frame arrives.
    #[test]
    fn feed_skips_bad_frames() {
        let (attempt, events, _request_line) = run_feed(sse(
            vec![
                (b"data: nope\n\n".to_vec(), None),
                (b"data: {\"type\":\"other\"}\n\n".to_vec(), None),
                (format!("data: {FRAME_A}\n\n").into_bytes(), None),
            ],
            None,
        ));
        assert_eq!(attempt, Attempt::Closed);
        assert_eq!(events.len(), 1, "only the valid frame");
        assert_eq!(events[0].convo, Uuid::parse_str(CONVO_A).expect("uuid"));
    }

    // feed_404_is_disabled — 404 (notifications off / no feed) stops the
    // listener; no events can have been delivered.
    #[test]
    fn feed_404_is_disabled() {
        let (attempt, events, _) = run_feed(plain("404 Not Found"));
        assert_eq!(attempt, Attempt::Stop(StopReason::Disabled));
        assert!(events.is_empty(), "on_event is never called on a 404");
    }

    // feed_401_is_rejected — a refused token stops the listener.
    #[test]
    fn feed_401_is_rejected() {
        let (attempt, events, _) = run_feed(plain("401 Unauthorized"));
        assert_eq!(attempt, Attempt::Stop(StopReason::Rejected));
        assert!(events.is_empty());
    }

    // feed_503_is_failed — anything else is a failure: back off and retry.
    #[test]
    fn feed_503_is_failed() {
        let (attempt, events, _) = run_feed(plain("503 Service Unavailable"));
        assert_eq!(attempt, Attempt::Failed);
        assert!(events.is_empty());
    }

    // feed_refused_is_failed — a closed port never got a 200 stream.
    #[test]
    fn feed_refused_is_failed() {
        let client = client(Duration::from_secs(5));
        let attempt =
            tauri::async_runtime::block_on(stream_feed(&client, refused_url(), |_: TurnEnd| {}));
        assert_eq!(attempt, Attempt::Failed);
    }

    // feed_stall_hits_read_timeout — headers, then silence: the client's
    // 300 ms read timeout ends the attempt as Closed (a dead stream
    // reconnects after 1 s; it is not a failure).
    #[test]
    fn feed_stall_hits_read_timeout() {
        let (base, _request_line) = canned_server(sse(Vec::new(), Some(Duration::from_secs(3))));
        let client = client(Duration::from_millis(300));
        let started = Instant::now();
        let attempt =
            tauri::async_runtime::block_on(stream_feed(&client, feed_url(&base), |_: TurnEnd| {}));
        let elapsed = started.elapsed();
        assert_eq!(
            attempt,
            Attempt::Closed,
            "a read timeout ends the stream (reconnect, no backoff)"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "the 300 ms read timeout must end it quickly, took {elapsed:?}"
        );
    }

    /// The plugin's click payload (2.8.1 `models.rs`): camelCase
    /// `{actionId, inputValue?, notification?}`; `ActiveNotification`
    /// requires an `id` and carries `extra` as a JSON map.
    fn action(value: serde_json::Value) -> ActionPerformed {
        serde_json::from_value(value).expect("the plugin's click payload shape")
    }

    // click_payload_parses_convo — a tap whose extra names the conversation
    // resolves to that `Uuid`.
    #[test]
    fn click_payload_parses_convo() {
        let p = action(serde_json::json!({
            "actionId": "tap",
            "notification": { "id": 1, "extra": { "convo": CONVO_A } }
        }));
        assert_eq!(
            convo_from_action(&p),
            Some(Uuid::parse_str(CONVO_A).expect("uuid"))
        );
    }

    // click_ignores_other_actions_and_bad_ids — a non-tap action, a tap
    // without an extra, and a tap whose convo is not a UUID are all
    // ignored (N3: nothing but a canonical UUID ever reaches the eval).
    #[test]
    fn click_ignores_other_actions_and_bad_ids() {
        let snooze = action(serde_json::json!({
            "actionId": "snooze",
            "notification": { "id": 1, "extra": { "convo": CONVO_A } }
        }));
        let no_extra = action(serde_json::json!({
            "actionId": "tap",
            "notification": { "id": 1 }
        }));
        let bad_id = action(serde_json::json!({
            "actionId": "tap",
            "notification": { "id": 1, "extra": { "convo": "x" } }
        }));
        assert_eq!(convo_from_action(&snooze), None, "not a tap");
        assert_eq!(convo_from_action(&no_extra), None, "no convo extra");
        assert_eq!(convo_from_action(&bad_id), None, "convo is not a UUID");
    }
}
