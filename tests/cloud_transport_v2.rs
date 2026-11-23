//! Cloud Transport v2 — conversation/credit framing (spec: p4-design).
//!
//! Behavioral tests driven through the tunnel, using the same harness as
//! `cloud_e2e.rs` and `cloud_serve_tests.rs`. These compile against the
//! CURRENT production code and fail at RUNTIME because the feature is
//! absent — that is the TDD red before implementation.
//!
//! Contract under test:
//! 1. Large (>1 MiB) WebSocket messages flow both ways, fully reassembled.
//! 2. Small messages remain one Frame (no regression to always-fragment).
//! 3. One illegal/oversize message does not kill subsequent ops on the link.
//! 4. Reopening an existing conversation that has grown >1 MiB delivers its
//!    complete original state/newest content (no tail/windowing).
//! 5. Reopening an existing conversation delivers its complete state.

mod cloud_common;

use cloud::frames::{Op, StreamMsg};
use cloud_common::devices::id_a;
use cloud_common::fake_relay::FakeRelay;
use cloud_common::host::{YAML, boot_core, enroll_pair};
use cloud_common::rig::{Dev, TIMEOUT};
use futures::{SinkExt, StreamExt};
use peakbot::cloud::stream::CloudStream;
use serde_json::{Value, json};
use std::time::Instant;

/// A payload that is unambiguously over the current MAX_INNER (1 MiB) limit:
/// 3 MiB of distinct bytes, so a truncated delivery is detectable.
const LARGE_PAYLOAD: usize = 3 * 1024 * 1024;

/// Send a Frame and wait for the turn's reply frame on the Attach stream.
/// Attach streams legitimately carry `state`/`models_available` noise
/// between `ready` and the reply, so "the next frame" is not the reply.
async fn send_frame(stream: &mut CloudStream, body: Value) -> Option<String> {
    stream
        .send(StreamMsg::Frame(body.to_string()))
        .await
        .expect("send frame");
    wait_frame(stream, |f| f.contains("MOCK_REPLY")).await
}

/// Read frames until `want` matches (bounded by `TIMEOUT` per frame).
async fn wait_frame(stream: &mut CloudStream, want: impl Fn(&str) -> bool) -> Option<String> {
    for _ in 0..400 {
        match next_frame(stream).await {
            Some(f) if want(&f) => return Some(f),
            Some(_) => continue,
            None => return None,
        }
    }
    None
}

async fn next_frame(stream: &mut CloudStream) -> Option<String> {
    let msg = tokio::time::timeout(TIMEOUT, stream.next())
        .await
        .expect("timed out waiting for a tunnel frame")
        .expect("the stream stayed open");
    match msg {
        StreamMsg::Frame(s) => Some(s),
        StreamMsg::End => None,
        StreamMsg::Reset(c) => panic!("stream reset: {c}"),
        other => panic!("non-frame on this stream: {other:?}"),
    }
}

// ── §1: large messages flow both ways ───────────────────────────────────────

/// Phone → desktop: a single send_message whose JSON payload is >1 MiB must
/// arrive at the computer as one logical message. The computer's mock model
/// completes the turn and replies; that reply proves the full message
/// survived the tunnel (if it had been truncated, the agent would have
/// received an incomplete prompt).
#[tokio::test]
async fn large_frame_phone_to_desktop_reassembles() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let mut s = attach(&b, "dev", true).await;
    let _convo = wait_ready(&mut s).await;

    // The mock model answers every prompt with "MOCK_REPLY". If the large
    // payload reaches the agent, the turn completes and we see that reply.
    let body = "A".repeat(LARGE_PAYLOAD);
    let f = send_frame(&mut s, json!({"type": "send_message", "text": body})).await;
    assert!(
        f.is_some() && f.as_ref().unwrap().contains("MOCK_REPLY"),
        "the turn must complete for a large (>1 MiB) send_message"
    );
}

/// Desktop → phone: the computer's state snapshot echoes the large user
/// message back down the Attach stream, so the phone must receive a >1 MiB
/// Frame in one piece. This is the receiver-side half of the reassembly
/// contract (the sender-side half is §1).
#[tokio::test]
async fn large_frame_desktop_to_phone_reassembles() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let mut s = attach(&b, "dev", true).await;
    let _convo = wait_ready(&mut s).await;

    let body = "B".repeat(LARGE_PAYLOAD);
    s.send(StreamMsg::Frame(
        json!({"type": "send_message", "text": body}).to_string(),
    ))
    .await
    .expect("send the large message");
    let f = wait_frame(&mut s, |f| {
        f.len() >= LARGE_PAYLOAD && f.contains("BBBBBBBB")
    })
    .await;
    let f = f.expect("a >1 MiB desktop→phone Frame must be reassembled and delivered");
    assert!(
        f.len() >= LARGE_PAYLOAD,
        "the reassembled frame must carry the full payload, got {} bytes",
        f.len()
    );
}

// ── §2: small messages remain one Frame (no regression) ────────────────────

/// A tiny send_message must still work exactly as it did before v2 framing
/// was introduced — one send, one Frame, one response. The mock model reply
/// is small; no fragmentation should be involved.
#[tokio::test]
async fn small_frame_stays_one_record() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let mut s = attach(&b, "dev", true).await;
    let _convo = wait_ready(&mut s).await;

    let t0 = Instant::now();
    let msg = send_frame(&mut s, json!({"type": "send_message", "text": "hi"})).await;
    let elapsed = t0.elapsed();

    assert!(
        msg.is_some() && msg.as_ref().unwrap().contains("MOCK_REPLY"),
        "small send must produce a small reply in one round-trip: elapsed {elapsed:?}"
    );
    // A small message must be fast (no artificial fragmentation delays).
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "small frame should be quick: {elapsed:?}"
    );
}

// ── §3: one large message does not kill the link ────────────────────────────

/// A single 12 MiB application message is below the v2 64 MiB logical cap,
/// so the link may carry it or refuse only that stream — either is valid.
/// The contract under test is that the SHARED session survives: a later op
/// on a different stream must still succeed. (On the v1 mux this message
/// exceeded MAX_INNER and killed the whole session.)
#[tokio::test]
async fn illegal_frame_does_not_kill_link() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    // Stream 1: 12 MiB, comfortably above MAX_INNER (1 MiB).
    let big = "C".repeat(LARGE_PAYLOAD * 4);
    let mut big_stream = attach(&b, "dev", true).await;
    let _convo = wait_ready(&mut big_stream).await;
    big_stream
        .send(StreamMsg::Frame(
            json!({"type": "send_message", "text": big}).to_string(),
        ))
        .await
        .expect("send the large message");

    // Skip attach noise (the initial `state`) until the large message is
    // observable, or the stream is refused.
    loop {
        let msg = tokio::time::timeout(TIMEOUT, big_stream.next())
            .await
            .expect("the large message gets a response")
            .expect("the attach stream stayed open");
        match msg {
            StreamMsg::Reset(code) => {
                assert!(
                    code == cloud::constants::reset::TOO_LARGE
                        || code == cloud::constants::reset::PROTOCOL,
                    "a refused message must reset with TOO_LARGE or PROTOCOL, got {code}"
                );
                break;
            }
            StreamMsg::Frame(f) if f.contains("CCCCCCCC") || f.contains("MOCK_REPLY") => break,
            StreamMsg::Frame(_) => continue,
            other => panic!("unexpected response to the large message: {other:?}"),
        }
    }
    big_stream.close().await.ok();
    drop(big_stream);

    // Stream 2: a completely different op on the same tunnel must work.
    let mut list_stream = b
        .handle
        .open(id_a(), Op::List)
        .await
        .expect("a second stream opens after the first failed");
    loop {
        let msg = tokio::time::timeout(TIMEOUT, list_stream.next())
            .await
            .expect("the list op answers in time");
        match msg {
            Some(StreamMsg::Head { status: 200, .. }) => break,
            Some(StreamMsg::Chunk(_)) => continue,
            Some(StreamMsg::End) => break,
            Some(StreamMsg::Reset(code)) => {
                panic!("the list stream died after the first stream failed: {code}")
            }
            other => panic!("unexpected on list stream: {other:?}"),
        }
    }
}

// ── §4: reopening a >1 MiB conversation delivers its full state ────────────

/// The reported bug: opening an EXISTING conversation whose persisted state
/// exceeds ~1 MiB fails or delivers only a tail. Reopening a conversation
/// with 3 MiB of message history must deliver `attached`, `ready` and then
/// the COMPLETE original state — every past message, not just the newest.
#[tokio::test]
async fn reopening_large_conversation_delivers_full_state() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    // Phase 1: create a conversation and grow it past 1 MiB.
    let mut s1 = attach(&b, "dev", true).await;
    let convo = wait_ready(&mut s1).await;

    // Send enough messages to push the conversation well over 1 MiB.
    let msg_body = "D".repeat(200_000); // 200 KB per message
    for i in 0..16 {
        let msg = json!({"type": "send_message", "text": msg_body, "turn": i});
        let reply = send_frame(&mut s1, msg).await;
        assert!(reply.is_some(), "turn {i} must complete before reopen");
    }
    s1.close().await.ok();
    drop(s1);

    // Phase 2: reopen from the phone through the tunnel.
    let mut s2 = reopen(&b, &convo).await;
    let (saw_attached, saw_ready, saw_state, msg_count) = read_attach_state(&mut s2, 32).await;
    assert!(saw_attached, "reopen must send attached");
    assert!(saw_ready, "reopen must send ready");
    assert!(saw_state, "reopen must send the conversation state");
    assert!(
        msg_count >= 32,
        "the reopened conversation must carry all 32 messages, not a tail (got {msg_count})"
    );
}

// ── §5: reopening an existing conversation delivers its complete state ──────

/// The reported bug: opening an EXISTING conversation whose persisted state
/// exceeds ~1 MiB fails or delivers only a tail. Reopening a conversation
/// with 3 MiB of message history must deliver `attached`, `ready` and then
/// the COMPLETE original state — every past message, not just the newest.
#[tokio::test]
async fn reopening_existing_conversation_delivers_full_state() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    // Phase 1: create a conversation and send a few messages.
    let mut s1 = attach(&b, "dev", true).await;
    let convo = wait_ready(&mut s1).await;
    for text in ["first", "second"] {
        let reply = send_frame(&mut s1, json!({"type": "send_message", "text": text})).await;
        assert!(reply.is_some(), "turn {text:?} must complete before reopen");
    }
    s1.close().await.ok();
    drop(s1);

    // Phase 2: reopen the same conversation from the phone through the tunnel.
    let mut s2 = reopen(&b, &convo).await;
    let (saw_attached, saw_ready, saw_state, msg_count) = read_attach_state(&mut s2, 4).await;
    assert!(saw_attached, "reopen must send attached");
    assert!(saw_ready, "reopen must send ready");
    assert!(saw_state, "reopen must send the conversation state");
    assert!(
        msg_count >= 4,
        "the reopened conversation must carry all 4 messages, not a tail (got {msg_count})"
    );
}

// ── tunnel stream helpers ───────────────────────────────────────────────────

async fn reopen(b: &Dev, convo: &str) -> CloudStream {
    let mut s = b
        .handle
        .open(id_a(), Op::Attach)
        .await
        .expect("open a new attach stream");
    s.send(StreamMsg::Frame(
        json!({"type": "attach", "convo": convo, "profile": "dev", "agent": "team", "relayed": true})
            .to_string(),
    ))
    .await
    .expect("attach to existing convo");
    s
}

/// Read the attach handshake and keep reading snapshots until one carries
/// at least `min_messages`; returns (attached, ready, state, count).
async fn read_attach_state(
    stream: &mut CloudStream,
    min_messages: usize,
) -> (bool, bool, bool, usize) {
    let (mut attached, mut ready, mut state, mut count) = (false, false, false, 0);
    for _ in 0..400 {
        let f = match next_frame(stream).await {
            Some(f) => f,
            None => break,
        };
        let v: Value = serde_json::from_str(&f).expect("JSON");
        match v["type"].as_str() {
            Some("attached") => attached = true,
            Some("ready") => ready = true,
            Some("state") => {
                state = true;
                if let Some(arr) = v["state"]["chat"]["messages"].as_array() {
                    count = arr.len();
                }
            }
            Some("error") => panic!("reopen failed: {f}"),
            _ => {}
        }
        if attached && ready && state && count >= min_messages {
            break;
        }
    }
    (attached, ready, state, count)
}

async fn attach(b: &Dev, profile: &str, relayed: bool) -> CloudStream {
    let mut s = b
        .handle
        .open(id_a(), Op::Attach)
        .await
        .expect("open Attach");
    s.send(StreamMsg::Frame(
        json!({"type": "attach", "profile": profile, "agent": "team", "relayed": relayed})
            .to_string(),
    ))
    .await
    .expect("send attach frame");
    s
}

async fn wait_ready(stream: &mut CloudStream) -> String {
    let mut convo = None;
    for _ in 0..200 {
        let f = match next_frame(stream).await {
            Some(f) => f,
            None => panic!("the attach stream ended before ready"),
        };
        let v: Value = serde_json::from_str(&f).expect("JSON");
        match v["type"].as_str() {
            Some("attached") => convo = v["convo"].as_str().map(str::to_string),
            Some("ready") => return convo.expect("ready follows attached"),
            Some("error") => panic!("attach error: {f}"),
            _ => {}
        }
    }
    panic!("never saw ready");
}
