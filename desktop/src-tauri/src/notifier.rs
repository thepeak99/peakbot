//! The turn-end notifier (PR-C design §4.4/§6.2): owns the SSE listener
//! task for the current `Ready` phase, shows native notifications and
//! handles their clicks. The `Notifier` struct, `run`, `feed_client`,
//! `show` and the plugin wiring are the implementation's (tasks 6/7); this
//! file currently holds the TDD stubs below plus the §9.3 tests.

use peakbot_desktop_core::notify::{StopReason, TurnEnd};
use tauri_plugin_notification::ActionPerformed;
use url::Url;
use uuid::Uuid;

/// How one connection attempt ended.
// TDD stub — PR-C implementation replaces this (Notifier::run matches on it).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    /// 404 / 401 / 403: stop until the next `Ready`.
    Stop(StopReason),
    /// We had a 200 stream and it ended (EOF or read error): reset the backoff.
    Closed,
    /// Never got a 200 stream (connect/TLS error, timeout, 5xx).
    Failed,
}

/// One feed connection: `GET` the URL, map the status through
/// `notify::verdict`, then decode SSE frames and forward every `TurnEnd`
/// to `on_event`. Never returns `Err`: the loop in `run` owns retries.
// TDD stub — PR-C implementation replaces this
#[allow(dead_code, unused_variables)]
pub(crate) async fn stream_feed(
    client: &reqwest::Client,
    url: Url,
    on_event: impl FnMut(TurnEnd),
) -> Attempt {
    todo!("PR-C")
}

/// The conversation a notification click refers to: `action_id == "tap"`
/// and a `convo` extra that parses as a `Uuid`; anything else is ignored.
// TDD stub — PR-C implementation replaces this
#[allow(dead_code, unused_variables)]
pub(crate) fn convo_from_action(p: &ActionPerformed) -> Option<Uuid> {
    todo!("PR-C")
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
