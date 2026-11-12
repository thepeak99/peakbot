//! The turn-end feed (PR-B `GET /api/notify/stream`) as the desktop sees
//! it: SSE bytes → data payloads → `TurnEnd` → maybe a notification.
//! Pure; the Tauri glue (`src-tauri/src/notifier.rs`) does the I/O.

use std::time::Duration;

use serde::Deserialize;
use uuid::Uuid;

use crate::consts::NOTIFY_RETRY_MAX;

/// How the turn ended (PR-B §4.2 `outcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Error,
    Stopped,
}

/// What started the turn (PR-B §4.2 `source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    User,
    Background,
}

/// One finished turn. `ts` is not read (nothing uses it); unknown
/// fields are ignored, so the server may add fields freely.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TurnEnd {
    /// A parsed UUID, never a raw string: it is interpolated into JS on click.
    pub convo: Uuid,
    pub title: Option<String>,
    pub outcome: Outcome,
    pub source: Source,
}

/// The feed's frame types. Only `turn_end` exists; any other `type` fails
/// to deserialize and is skipped (PR-B §6.2: "ignore unknown type values").
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum FeedEvent {
    TurnEnd(TurnEnd),
}

/// Parse one SSE `data` payload. `None` = not a turn end we understand.
pub fn parse_event(data: &str) -> Option<TurnEnd> {
    match serde_json::from_str::<FeedEvent>(data).ok()? {
        FeedEvent::TurnEnd(ev) => Some(ev),
    }
}

/// Incremental SSE decoder: bytes in, complete `data` payloads out.
/// Handles LF and CRLF, `:` comments (keepalives), multi-line `data`
/// (joined with '\n'), `data:` with or without one leading space; other
/// fields (`event`, `id`, `retry`) are ignored. Lines are decoded as UTF-8
/// only once complete, so a chunk boundary inside a multi-byte char is fine.
/// A payload is emitted on the blank line that ends its event; an
/// unterminated event at EOF is dropped (SSE spec).
#[derive(Debug, Default)]
pub struct SseDecoder {
    /// Bytes after the last '\n'.
    pending: Vec<u8>,
    /// The current event's `data`, `None` until its first `data` line.
    data: Option<String>,
}
impl SseDecoder {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(chunk);
        let mut out = Vec::new();
        // Only complete lines: a chunk may end mid-line (even mid-UTF-8-char),
        // so the rest stays in `pending`.
        while let Some(nl) = self.pending.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=nl).collect();
            line.pop(); // the '\n'
            if line.last() == Some(&b'\r') {
                line.pop(); // CRLF
            }
            let line = std::str::from_utf8(&line).unwrap_or("");
            if line.is_empty() {
                // The blank line that ends an event (SSE spec).
                if let Some(data) = self.data.take() {
                    out.push(data);
                }
            } else if let Some(value) = line.strip_prefix("data:") {
                // At most one leading space; other fields are ignored.
                let value = value.strip_prefix(' ').unwrap_or(value);
                self.data = Some(match self.data.take() {
                    None => value.to_string(),
                    Some(mut prev) => {
                        prev.push('\n');
                        prev.push_str(value);
                        prev
                    }
                });
            }
        }
        out
    }
}

/// What a feed response's status means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 200: read the stream.
    Stream,
    /// Won't change until the server restarts (= the next `Ready`).
    Stop(StopReason),
    /// Anything else (5xx, …): back off and retry.
    Retry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// 404: notifications are off (or the server needs setup).
    Disabled,
    /// 401/403: the token was refused.
    Rejected,
}

/// 200→Stream, 401|403→Stop(Rejected), 404→Stop(Disabled), _→Retry.
pub fn verdict(status: u16) -> Verdict {
    match status {
        200 => Verdict::Stream,
        401 | 403 => Verdict::Stop(StopReason::Rejected),
        404 => Verdict::Stop(StopReason::Disabled),
        _ => Verdict::Retry,
    }
}

/// Delay before reconnect attempt after `failures` consecutive failed
/// attempts (0 = the previous stream was healthy and just ended):
/// 1 s · 2^failures, capped at NOTIFY_RETRY_MAX (1, 2, 4, 8, 16, 30, 30, …).
pub fn retry_delay(failures: u32) -> Duration {
    // The shift saturates at the cap: 2^5 = 32 s is already over 30 s.
    Duration::from_secs(1u64 << failures.min(5)).min(NOTIFY_RETRY_MAX)
}

/// The title used when the conversation has none yet.
pub const FALLBACK_TITLE: &str = "PeakBot";

/// A notification to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub title: String,
    pub body: &'static str,
}

/// The decision: `None` while the user is looking at the window
/// (visible AND focused); otherwise the content (§6.3 table).
pub fn toast(ev: &TurnEnd, user_is_looking: bool) -> Option<Toast> {
    if user_is_looking {
        return None;
    }
    let title = ev
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(FALLBACK_TITLE)
        .to_string();
    let body = match (ev.source, ev.outcome) {
        (Source::User, Outcome::Ok) => "Task complete — ready for your next message.",
        (Source::User, Outcome::Error) => "Task failed.",
        (Source::User, Outcome::Stopped) => "Task stopped.",
        (Source::Background, Outcome::Ok) => "Background task update.",
        (Source::Background, Outcome::Error) => "Background task update failed.",
        (Source::Background, Outcome::Stopped) => "Task stopped.",
    };
    Some(Toast { title, body })
}

/// The script a notification click runs in the page (C5 contract).
/// Safe to interpolate: a `Uuid` displays as `[0-9a-f-]{36}`.
/// `window.peakbotOpenConvo?.("<uuid>")`
pub fn open_convo_js(convo: Uuid) -> String {
    format!("window.peakbotOpenConvo?.(\"{convo}\")")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed conversation id for the wire-shape tests.
    const CONVO: &str = "6f1c1b9a-52ab-4c3d-9e21-8a3b0c1d2e3f";

    // n01 — two events, each terminated by its blank line.
    #[test]
    fn n01_decoder_emits_one_payload_per_event() {
        let mut d = SseDecoder::default();
        let out = d.feed(b"data: {\"a\":1}\n\ndata: x\n\n");
        assert_eq!(out, vec!["{\"a\":1}".to_string(), "x".to_string()]);
    }

    // n02 — keepalives are comments: no data lines, no payloads.
    #[test]
    fn n02_decoder_ignores_keepalive_comments() {
        let mut d = SseDecoder::default();
        let out = d.feed(b":\n\n: ping\n\n");
        assert!(out.is_empty(), "comments must not yield payloads: {out:?}");
    }

    // n03 — one frame split into three chunks, one boundary inside the
    // UTF-8 bytes of "é" (0xC3 0xA9): the decoder must reassemble it and
    // emit exactly one payload equal to the original JSON.
    #[test]
    fn n03_decoder_reassembles_across_chunks() {
        let payload = format!(
            "{{\"type\":\"turn_end\",\"convo\":\"{CONVO}\",\"title\":\"Café\",\"outcome\":\"ok\",\"source\":\"user\",\"ts\":1}}"
        );
        let frame = format!("data: {payload}\n\n");
        let bytes = frame.as_bytes();

        // Split between the two bytes of "é" (chunk 1 ends mid-character).
        let e_accent = bytes
            .iter()
            .position(|&b| b == 0xC3)
            .expect("the frame contains a multi-byte é");
        let (c1, rest) = bytes.split_at(e_accent + 1);
        let (c2, c3) = rest.split_at(8);

        let mut d = SseDecoder::default();
        let mut out: Vec<String> = Vec::new();
        out.extend(d.feed(c1));
        out.extend(d.feed(c2));
        out.extend(d.feed(c3));

        assert_eq!(
            out,
            vec![payload.clone()],
            "exactly one payload, equal to the original JSON"
        );
    }

    // n04 — CRLF line ends and a `data:` with no space after the colon.
    #[test]
    fn n04_decoder_accepts_crlf_and_no_space() {
        let mut d = SseDecoder::default();
        let out = d.feed(b"data:x\r\n\r\n");
        assert_eq!(out, vec!["x".to_string()]);
    }

    // n05 — multi-line `data` joins with '\n'; other fields are ignored.
    #[test]
    fn n05_decoder_joins_data_lines_and_ignores_other_fields() {
        let mut d = SseDecoder::default();
        let out = d.feed(b"event: e\nid: 1\nretry: 5\ndata: a\ndata: b\n\n");
        assert_eq!(out, vec!["a\nb".to_string()]);
    }

    // n06 — an event without its terminating blank line is dropped.
    #[test]
    fn n06_decoder_drops_unterminated_event() {
        let mut d = SseDecoder::default();
        let out = d.feed(b"data: x\n");
        assert!(out.is_empty(), "no blank line, no payload: {out:?}");
    }

    // n07 — the exact PR-B §4.2 wire shape (incl. the `ts` the desktop
    // ignores): `parse_event` must read it into a `TurnEnd`.
    #[test]
    fn n07_parse_event_reads_the_pr_b_wire_shape() {
        let json = format!(
            "{{\"type\":\"turn_end\",\"convo\":\"{CONVO}\",\"title\":\"Fix the reaper race\",\"outcome\":\"ok\",\"source\":\"user\",\"ts\":1700000000000}}"
        );
        let ev = parse_event(&json).expect("the wire shape parses");
        assert_eq!(
            ev,
            TurnEnd {
                convo: Uuid::parse_str(CONVO).expect("test uuid"),
                title: Some("Fix the reaper race".to_string()),
                outcome: Outcome::Ok,
                source: Source::User,
            }
        );
    }

    // n08 — null title, background source, stopped outcome.
    #[test]
    fn n08_parse_event_null_title() {
        let json = format!(
            "{{\"type\":\"turn_end\",\"convo\":\"{CONVO}\",\"title\":null,\"outcome\":\"stopped\",\"source\":\"background\",\"ts\":1}}"
        );
        let ev = parse_event(&json).expect("null title parses");
        assert_eq!(ev.title, None, "title: null ⇒ None");
        assert_eq!(ev.outcome, Outcome::Stopped);
        assert_eq!(ev.source, Source::Background);
    }

    // n09 — an unknown `type`, garbage, and an empty object are all skipped.
    #[test]
    fn n09_parse_event_skips_unknown_type_and_garbage() {
        let unknown_type = format!(
            "{{\"type\":\"other\",\"convo\":\"{CONVO}\",\"title\":null,\"outcome\":\"ok\",\"source\":\"user\",\"ts\":1}}"
        );
        assert_eq!(parse_event(&unknown_type), None, "unknown type");
        assert_eq!(parse_event("not json"), None, "garbage");
        assert_eq!(parse_event("{}"), None, "no type field");
    }

    // n10 — a non-UUID `convo` (JS-injection attempt, invariant N3) is
    // rejected: the id is a `Uuid` from parse to eval.
    #[test]
    fn n10_parse_event_rejects_non_uuid_convo() {
        let json = r#"{"type":"turn_end","convo":"x\");alert(1);//","title":null,"outcome":"ok","source":"user","ts":1}"#;
        assert_eq!(parse_event(json), None, "injection guard (N3)");
    }

    // n11 — the user is looking (visible AND focused): no notification.
    #[test]
    fn n11_toast_none_while_looking() {
        let ev = TurnEnd {
            convo: Uuid::parse_str(CONVO).expect("test uuid"),
            title: Some("Plan".to_string()),
            outcome: Outcome::Ok,
            source: Source::User,
        };
        assert_eq!(toast(&ev, true), None);
    }

    // n12 — the title falls back to FALLBACK_TITLE when absent or blank.
    #[test]
    fn n12_toast_title_falls_back() {
        fn ev(title: Option<&str>) -> TurnEnd {
            TurnEnd {
                convo: Uuid::parse_str(CONVO).expect("test uuid"),
                title: title.map(str::to_string),
                outcome: Outcome::Ok,
                source: Source::User,
            }
        }
        assert_eq!(
            toast(&ev(None), false)
                .expect("not looking ⇒ a toast")
                .title,
            FALLBACK_TITLE,
            "no title ⇒ the fallback"
        );
        assert_eq!(
            toast(&ev(Some("  ")), false)
                .expect("not looking ⇒ a toast")
                .title,
            FALLBACK_TITLE,
            "blank title ⇒ the fallback"
        );
        assert_eq!(
            toast(&ev(Some("Plan")), false)
                .expect("not looking ⇒ a toast")
                .title,
            "Plan",
            "a real title is kept"
        );
    }

    // n13 — the §6.3 content table, all six (source, outcome) pairs,
    // exactly (the wording the product speaks, incl. the em dash).
    #[test]
    fn n13_toast_body_table() {
        fn ev(source: Source, outcome: Outcome) -> TurnEnd {
            TurnEnd {
                convo: Uuid::parse_str(CONVO).expect("test uuid"),
                title: Some("Plan".to_string()),
                outcome,
                source,
            }
        }
        let expected = [
            (
                Source::User,
                Outcome::Ok,
                "Task complete — ready for your next message.",
            ),
            (Source::User, Outcome::Error, "Task failed."),
            (Source::User, Outcome::Stopped, "Task stopped."),
            (Source::Background, Outcome::Ok, "Background task update."),
            (
                Source::Background,
                Outcome::Error,
                "Background task update failed.",
            ),
            (Source::Background, Outcome::Stopped, "Task stopped."),
        ];
        for (source, outcome, body) in expected {
            let t = toast(&ev(source, outcome), false).expect("not looking ⇒ a toast");
            assert_eq!(t.body, body, "body for {source:?}/{outcome:?}");
        }
    }

    // n14 — the status→verdict map.
    #[test]
    fn n14_verdict_by_status() {
        assert_eq!(verdict(200), Verdict::Stream);
        assert_eq!(verdict(401), Verdict::Stop(StopReason::Rejected));
        assert_eq!(verdict(403), Verdict::Stop(StopReason::Rejected));
        assert_eq!(verdict(404), Verdict::Stop(StopReason::Disabled));
        assert_eq!(verdict(500), Verdict::Retry);
        assert_eq!(verdict(502), Verdict::Retry);
        assert_eq!(verdict(503), Verdict::Retry);
        assert_eq!(verdict(302), Verdict::Retry);
    }

    // n15 — 1 s · 2^failures, capped at 30 s.
    #[test]
    fn n15_retry_delay_doubles_and_caps() {
        let secs: Vec<u64> = (0..=7u32)
            .map(|f| retry_delay(f).as_secs())
            .chain(std::iter::once(retry_delay(u32::MAX).as_secs()))
            .collect();
        assert_eq!(
            secs,
            vec![1, 2, 4, 8, 16, 30, 30, 30, 30],
            "1 s doubling, capped at 30 s (u32::MAX saturates, no overflow)"
        );
    }

    // n16 — the exact click script (the C5 contract).
    #[test]
    fn n16_open_convo_js_exact() {
        let convo = Uuid::parse_str(CONVO).expect("test uuid");
        assert_eq!(
            open_convo_js(convo),
            format!("window.peakbotOpenConvo?.(\"{CONVO}\")")
        );
    }
}
