//! Turn-end events: one per finished agent turn, process-wide. Consumers
//! subscribe to the sender in `SessionDeps::turn_ends`; slow ones lag and
//! drop events (tokio broadcast semantics).

use serde::Serialize;
use tokio::sync::broadcast;
use uuid::Uuid;

/// Ample: turn ends are seconds apart; only a wedged client lags.
const CAPACITY: usize = 64;

/// How a turn ended. `process_message_internal` returns this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Ok,
    Error,
    Stopped,
}

/// What started the turn: a person, or a `[bg output]` synthetic turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnSource {
    User,
    Background,
}

/// One finished turn. Carries no message content (see `title`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename = "turn_end")]
pub struct TurnEnd {
    pub convo: Uuid,
    /// The conversation's display title, `None` until one exists. May be
    /// derived from the opening message; consumers that leave the device
    /// must drop it.
    pub title: Option<String>,
    pub outcome: TurnOutcome,
    pub source: TurnSource,
    /// Unix milliseconds.
    pub ts: i64,
}

pub type TurnEndTx = broadcast::Sender<TurnEnd>;

/// The one channel per process (built by `boot::session_deps`).
pub fn turn_end_channel() -> TurnEndTx {
    broadcast::channel(CAPACITY).0
}

// ─────────────────────────────────────────────────────────────────────────────
// PR-B §9.1 tests. Written against the §4.1 contract above. This file already
// holds the real types (they are pure contract: serde attrs + a channel
// constructor), so these tests are GREEN from the start — they pin the wire
// shape and channel semantics so the implementation PR cannot drift from them.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::broadcast::error::TryRecvError;

    fn sample(title: Option<&str>, outcome: TurnOutcome, source: TurnSource) -> TurnEnd {
        TurnEnd {
            convo: Uuid::new_v4(),
            title: title.map(str::to_string),
            outcome,
            source,
            ts: 1,
        }
    }

    #[test]
    fn turn_end_serializes_to_the_wire_shape() {
        let ev = sample(Some("T"), TurnOutcome::Ok, TurnSource::User);
        let value = serde_json::to_value(&ev).expect("TurnEnd serialises");
        assert_eq!(
            value,
            json!({
                "type": "turn_end",
                "convo": ev.convo.to_string(),
                "title": "T",
                "outcome": "ok",
                "source": "user",
                "ts": 1,
            }),
            "exact key set and values — the HTTP contract for every later consumer"
        );
    }

    #[test]
    fn untitled_turn_end_has_null_title() {
        let ev = sample(None, TurnOutcome::Ok, TurnSource::User);
        let value = serde_json::to_value(&ev).expect("TurnEnd serialises");
        let obj = value.as_object().expect("an object");
        assert!(obj.contains_key("title"), "title is present, not omitted");
        assert_eq!(obj["title"], serde_json::Value::Null);
    }

    #[test]
    fn outcomes_and_sources_use_snake_case() {
        let cases = [
            (TurnOutcome::Ok, "ok"),
            (TurnOutcome::Error, "error"),
            (TurnOutcome::Stopped, "stopped"),
        ];
        for (outcome, wire) in cases {
            assert_eq!(serde_json::to_value(outcome).unwrap(), json!(wire));
        }
        assert_eq!(
            serde_json::to_value(TurnSource::User).unwrap(),
            json!("user")
        );
        assert_eq!(
            serde_json::to_value(TurnSource::Background).unwrap(),
            json!("background")
        );
    }

    #[test]
    fn serialized_json_never_contains_a_raw_newline() {
        // The SSE handler frames `Event::default().data(json)`; a raw newline
        // would split the frame. Compact JSON escapes them — pin it for the
        // one free-text field.
        let ev = sample(
            Some("line one\nline two\r\nnaïve \"quoted\" 日本語 \u{1F600}"),
            TurnOutcome::Error,
            TurnSource::Background,
        );
        let json = serde_json::to_string(&ev).expect("serialises");
        assert!(!json.contains('\n') && !json.contains('\r'), "got: {json}");
        let back: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(
            back["title"], "line one\nline two\r\nnaïve \"quoted\" 日本語 \u{1F600}",
            "title survives escaping byte for byte"
        );
    }

    #[test]
    fn every_subscriber_receives_every_event() {
        let tx = turn_end_channel();
        let mut a = tx.subscribe();
        let mut b = tx.subscribe();
        let ev = sample(Some("T"), TurnOutcome::Stopped, TurnSource::User);
        tx.send(ev.clone()).expect("two receivers exist");
        assert_eq!(a.try_recv().expect("a gets it"), ev);
        assert_eq!(b.try_recv().expect("b gets it"), ev);
        assert!(matches!(a.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn send_without_subscribers_is_harmless() {
        let tx = turn_end_channel();
        let res = tx.send(sample(None, TurnOutcome::Ok, TurnSource::User));
        assert!(res.is_err(), "no receivers ⇒ Err, which publishers ignore");
        // …and the channel is still usable afterwards.
        let mut rx = tx.subscribe();
        tx.send(sample(None, TurnOutcome::Ok, TurnSource::User))
            .expect("now there is a receiver");
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn a_subscriber_only_sees_events_sent_after_it_subscribed() {
        let tx = turn_end_channel();
        let _keep = tx.subscribe();
        tx.send(sample(Some("early"), TurnOutcome::Ok, TurnSource::User))
            .unwrap();
        let mut late = tx.subscribe();
        assert!(
            matches!(late.try_recv(), Err(TryRecvError::Empty)),
            "no replay: the contract says missed events are gone"
        );
    }

    #[test]
    fn lagging_subscriber_reports_lag_and_recovers() {
        let tx = turn_end_channel();
        let mut idle = tx.subscribe();
        // Capacity is 64: the 65th send evicts the oldest for the idle receiver.
        for i in 0..65 {
            tx.send(sample(
                Some(&i.to_string()),
                TurnOutcome::Ok,
                TurnSource::User,
            ))
            .expect("a receiver exists");
        }
        assert!(
            matches!(idle.try_recv(), Err(TryRecvError::Lagged(_))),
            "the first read reports the lag"
        );
        let next = idle.try_recv().expect("then it recovers with an event");
        assert_eq!(next.title.as_deref(), Some("1"), "oldest surviving event");
    }
}
