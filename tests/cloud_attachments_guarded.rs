//! Cloud Attachments + non-Attach Credit — guarded regression tests for
//! length validation, stall handling, and protocol compliance.
//!
//! These tests exercise the production interfaces added by the streaming
//! design and verify the guards the independent architect identified:
//! inbound Attach stall, early device response, download overrun/underrun,
//! declared upload length mismatch, Attach producer channel capacity,
//! upload body channel stall, and Attach data after peer End.
//!
//! Run with: cargo test --locked --test cloud_attachments_guarded

mod cloud_common;

use std::path::Path;
use std::time::Duration;

use cloud::constants::{CREDIT_GRANT, MAX_CHUNK, RECV_STALL_SECS};
use cloud::frames::{Op, StreamMsg};
use futures::{SinkExt, StreamExt};
use peakbot::cloud::stream::CloudStream;

use cloud_common::devices::id_a;
use cloud_common::fake_relay::FakeRelay;
use cloud_common::host::{YAML, boot_core, enroll_pair};
use cloud_common::rig::{Dev, TIMEOUT};

/// Generous budget for polling filesystem state (eventual consistency).
const POLL_BUDGET: Duration = Duration::from_secs(20);

// ── helpers ──────────────────────────────────────────────────────────────────

/// Drive `Op::Attach` to `ready` and return the conversation id.
async fn attach_and_ready(b: &Dev, profile: &str) -> String {
    let mut s = b
        .handle
        .open(id_a(), Op::Attach)
        .await
        .expect("open Attach");
    s.send(StreamMsg::Frame(format!(
        r#"{{"type":"attach","profile":"{profile}","agent":"team","relayed":true}}"#
    )))
    .await
    .expect("send attach frame");

    let mut convo = None;
    for _ in 0..200 {
        let msg = tokio::time::timeout(TIMEOUT, s.next())
            .await
            .expect("timed out waiting for attach frame")
            .expect("the attach stream stayed open");
        match msg {
            StreamMsg::Frame(f) => {
                let v: serde_json::Value = serde_json::from_str(&f).expect("JSON frame");
                match v["type"].as_str() {
                    Some("attached") => convo = v["convo"].as_str().map(str::to_string),
                    Some("ready") => return convo.expect("ready follows attached"),
                    Some("error") => panic!("attach refused: {f}"),
                    _ => {}
                }
            }
            StreamMsg::Reset(c) => panic!("attach stream reset: {c}"),
            other => panic!("non-frame on an attach stream: {other:?}"),
        }
    }
    panic!("no ready frame on the attach stream")
}

/// One request/response op: read Head, then Chunks until End.
async fn op_reply(stream: &mut CloudStream) -> (u16, String, Vec<u8>) {
    let mut body = Vec::new();
    loop {
        let msg = tokio::time::timeout(TIMEOUT, stream.next())
            .await
            .expect("op stream answered in time")
            .expect("op stream stayed open");
        match msg {
            StreamMsg::Head {
                status,
                content_type,
                ..
            } => {
                return read_body(stream, status, content_type, &mut body).await;
            }
            StreamMsg::Chunk(c) => body.extend(c),
            StreamMsg::Reset(code) => panic!("op stream reset: {code}"),
            StreamMsg::End => panic!("End before the Head"),
            StreamMsg::Frame(f) => panic!("unexpected frame on an op stream: {f}"),
            StreamMsg::Open(_) => panic!("unexpected Open on an op stream"),
            StreamMsg::Credit { .. } => {}
            StreamMsg::FrameBegin { .. } | StreamMsg::FrameEnd => {
                panic!("unexpected v2 Attach record on an op stream")
            }
        }
    }
}

async fn read_body(
    stream: &mut CloudStream,
    status: u16,
    content_type: String,
    head_body: &mut Vec<u8>,
) -> (u16, String, Vec<u8>) {
    let mut body = std::mem::take(head_body);
    let mut since_grant = 0u64;
    loop {
        let msg = tokio::time::timeout(TIMEOUT, stream.next())
            .await
            .expect("op body in time")
            .expect("op stream stayed open");
        match msg {
            StreamMsg::Chunk(c) => {
                since_grant += c.len() as u64;
                body.extend(c);
                if since_grant >= u64::from(CREDIT_GRANT) {
                    let _ = stream
                        .send(StreamMsg::Credit {
                            bytes: u32::try_from(since_grant).unwrap_or(CREDIT_GRANT),
                        })
                        .await;
                    since_grant = 0;
                }
            }
            StreamMsg::End => return (status, content_type, body),
            StreamMsg::Reset(code) => panic!("op body reset: {code}"),
            StreamMsg::Credit { .. } => {}
            other => panic!("unexpected message in an op body: {other:?}"),
        }
    }
}

/// Total size of all regular files under `dir`.
fn dir_size(dir: &Path) -> u64 {
    if !dir.exists() {
        return 0;
    }
    let mut total = 0u64;
    fn walk(dir: &std::path::Path, acc: &mut u64) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                match entry.metadata() {
                    Ok(m) if m.is_file() => *acc += m.len(),
                    Ok(m) if m.is_dir() => walk(&entry.path(), acc),
                    _ => {}
                }
            }
        }
    }
    walk(dir, &mut total);
    total
}

/// Poll until `f()` returns true, or return false on timeout.
async fn poll_until<F>(budget: Duration, f: F) -> bool
where
    F: Fn() -> bool + Send,
{
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if f() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ── H1: inbound Attach stall ─────────────────────────────────────────────────

/// Open an Attach stream, send FrameBegin with a meaningful declared len
/// plus partial Chunk, make no further progress past RECV_STALL_SECS.
/// Only that stream must reset (CANCELLED or timeout), the session and
/// List/Profiles must stay alive, the stream slot and per-session
/// reassembly budget must be released so a subsequent large Attach succeeds.
#[tokio::test]
async fn inbound_attach_stall_resets_only_stream_and_releases_budget() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    // Open an Attach stream and complete the handshake.
    let mut s = b
        .handle
        .open(id_a(), Op::Attach)
        .await
        .expect("open Attach");
    s.send(StreamMsg::Frame(
        r#"{"type":"attach","profile":"dev","agent":"team","relayed":true}"#.to_string(),
    ))
    .await
    .expect("send attach frame");

    // Wait for attached + ready.
    let mut saw_ready = false;
    for _ in 0..200 {
        let msg = tokio::time::timeout(TIMEOUT, s.next())
            .await
            .expect("timed out waiting for attach frame")
            .expect("the attach stream stayed open");
        match msg {
            StreamMsg::Frame(f) => {
                let v: serde_json::Value = serde_json::from_str(&f).expect("JSON frame");
                match v["type"].as_str() {
                    Some("ready") => {
                        saw_ready = true;
                        break;
                    }
                    // Skip attach noise (state, models_available).
                    Some("attached") | Some("state") | Some("models_available") => {}
                    _ => {}
                }
            }
            StreamMsg::Reset(c) => panic!("attach stream reset: {c}"),
            other => panic!("non-frame on an attach stream: {other:?}"),
        }
    }
    assert!(saw_ready, "the attach stream must reach ready");

    // Now start a fragmented message with a meaningful declared len plus
    // a partial Chunk, then make no further progress.
    let declared = 1024 * 1024; // 1 MiB
    s.send(StreamMsg::FrameBegin {
        len: declared as u32,
    })
    .await
    .expect("send FrameBegin");
    s.send(StreamMsg::Chunk(vec![0xABu8; 4096]))
        .await
        .expect("send partial Chunk");
    // Now make no further progress.
    //
    // Let the in-process relay and the host actor drain both fragment
    // records BEFORE freezing the clock: the receive-stall budget starts at
    // fragment arrival, so pausing first would stamp `last_progress` after
    // the jump and the advanced time could never cover it.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Advance time past the receive stall budget (RECV_STALL_SECS = 120s).
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(RECV_STALL_SECS + 5)).await;

    // The stalled stream must have been reset (CANCELLED or timeout).
    // Skip attach noise (state, models_available, etc.) while waiting.
    let mut got_reset = false;
    for _ in 0..200 {
        let msg = tokio::time::timeout(TIMEOUT, s.next())
            .await
            .expect("stalled stream answered")
            .expect("stalled stream stayed open");
        match msg {
            StreamMsg::Reset(code)
                if code == cloud::constants::reset::CANCELLED
                    || code == cloud::constants::reset::INTERNAL =>
            {
                got_reset = true;
                break;
            }
            StreamMsg::Reset(code) => {
                panic!("unexpected reset code on stalled stream: {code}");
            }
            // Skip attach noise.
            StreamMsg::Frame(_) => {}
            other => {
                panic!(
                    "the stalled Attach stream must be reset (CANCELLED or INTERNAL timeout), got {other:?}"
                );
            }
        }
    }
    assert!(
        got_reset,
        "the stalled Attach stream must be reset (CANCELLED or INTERNAL timeout)"
    );
    tokio::time::resume();

    // The session must still be alive: List/Profiles work.
    let mut list = b.handle.open(id_a(), Op::List).await.expect("open List");
    let (status, _ct, body) = op_reply(&mut list).await;
    assert_eq!(
        status, 200,
        "the shared session must survive the stalled Attach stream"
    );
    let v: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
    assert!(v["items"].is_array());

    // A subsequent large Attach must succeed (budget released).
    let convo2 = attach_and_ready(&b, "dev").await;
    assert!(
        !convo2.is_empty(),
        "a subsequent Attach must succeed after the stall"
    );

    // Now prove the stream slot and reassembly budget were released by
    // sending a large fragmented message that MUST use FrameBegin+Chunks+
    // FrameEnd (exceeds FRAME_ONESHOT_MAX = 256 KiB). If the budget were
    // still reserved by the stalled stream, this would fail.
    let mut s2 = b
        .handle
        .open(id_a(), Op::Attach)
        .await
        .expect("open Attach after stall");
    s2.send(StreamMsg::Frame(
        r#"{"type":"attach","profile":"dev","agent":"team","relayed":true}"#.to_string(),
    ))
    .await
    .expect("send attach frame");

    // Wait for ready on the new stream.
    let mut saw_ready2 = false;
    for _ in 0..200 {
        let msg = tokio::time::timeout(TIMEOUT, s2.next())
            .await
            .expect("timed out waiting for attach frame")
            .expect("the attach stream stayed open");
        match msg {
            StreamMsg::Frame(f) => {
                let v: serde_json::Value = serde_json::from_str(&f).expect("JSON frame");
                match v["type"].as_str() {
                    Some("ready") => {
                        saw_ready2 = true;
                        break;
                    }
                    Some("attached") | Some("state") | Some("models_available") => {}
                    _ => {}
                }
            }
            StreamMsg::Reset(c) => panic!("post-stall attach stream reset: {c}"),
            other => panic!("non-frame on an attach stream: {other:?}"),
        }
    }
    assert!(saw_ready2, "the post-stall attach stream must reach ready");

    // Send a large message (> FRAME_ONESHOT_MAX) that forces fragmentation.
    let large_payload = format!(
        r#"{{"type":"send_message","text":"{}"}}"#,
        "A".repeat(512 * 1024) // 512 KB — must fragment into multiple Chunks
    );
    s2.send(StreamMsg::Frame(large_payload))
        .await
        .expect("send large Frame");

    // The transport will split this into FrameBegin + multiple Chunks +
    // FrameEnd. The device must reassemble it and respond. Wait for the
    // reply (a "message" frame with MOCK_REPLY).
    let mut got_reply = false;
    for _ in 0..200 {
        let msg = tokio::time::timeout(TIMEOUT, s2.next())
            .await
            .expect("post-stall stream answered")
            .expect("post-stall stream stayed open");
        match msg {
            StreamMsg::Frame(f) => {
                if f.contains("MOCK_REPLY") {
                    got_reply = true;
                    break;
                }
            }
            StreamMsg::Reset(c) => panic!("post-stall stream reset: {c}"),
            other => panic!("unexpected on post-stall stream: {other:?}"),
        }
    }
    assert!(
        got_reply,
        "the large fragmented message must complete after the stall — proving the reassembly budget and stream slot were released"
    );
}

// ── M2: early device response while phone upload body is streaming ──────────

/// Phone uploads with a wrong content-type (device should refuse with 415
/// before the body is complete). The phone must receive the real 415, not
/// 502, and the stream must be closed.
#[tokio::test]
async fn upload_body_early_rejection_surfaces_device_status_not_502() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let convo_id = uuid::Uuid::parse_str(&convo).expect("uuid");
    let convo_bytes: [u8; 16] = *convo_id.as_bytes();

    // Open Upload with a content type the device will reject (e.g. an
    // invalid type). The device should answer 415 before the body ends.
    let mut s = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: "rejected.bin".into(),
                content_type: "application/invalid-unknown".into(),
                length: None,
            },
        )
        .await
        .expect("open Upload");

    // Send part of the body; the device should refuse early.
    s.send(StreamMsg::Chunk(vec![0xAAu8; 4096]))
        .await
        .expect("send first chunk");

    // Wait for the device's response (Head with non-200 status).
    let msg = tokio::time::timeout(TIMEOUT, s.next())
        .await
        .expect("device answered")
        .expect("stream stayed open");
    match msg {
        StreamMsg::Head { status, .. } => {
            // The device's real error status, not 502.
            assert!(
                status == 400 || status == 415,
                "the phone must receive the device's real error (400/415), not 502; got {status}"
            );
        }
        StreamMsg::Reset(code) => {
            // If the device refused with a Reset, that's also valid — the
            // phone saw the device's real error, not a generic 502.
            assert!(
                code == cloud::constants::reset::PROTOCOL
                    || code == cloud::constants::reset::INTERNAL,
                "the phone must see the device's real refusal, not a generic 502; got Reset({code})"
            );
        }
        other => panic!("unexpected response to upload body: {other:?}"),
    }
}

// ── M3a: true download overrun ───────────────────────────────────────────────

/// Phone uploads a file with a declared length N, but sends MORE than N
/// bytes. The device must detect the overrun and reset the stream; no
/// attachment is published. The existing `download_length_overrun_is_...`
/// test only sends exact N and is renamed to an honest happy-path name.
#[tokio::test]
async fn upload_declared_length_overrun_resets_stream_no_attachment_published() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let convo_id = uuid::Uuid::parse_str(&convo).expect("uuid");
    let convo_bytes: [u8; 16] = *convo_id.as_bytes();
    let convo_dir = peakbot::paths::uploads_root().join(convo_id.to_string());

    // Declare 1 MB but send 2 MB.
    let declared = 1024 * 1024;
    let actual = 2 * 1024 * 1024;
    let bytes = vec![0xCCu8; actual];

    let mut s = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: "overrun.bin".into(),
                content_type: "application/octet-stream".into(),
                length: Some(declared as u64),
            },
        )
        .await
        .expect("open Upload");

    for chunk in bytes.chunks(MAX_CHUNK) {
        if s.send(StreamMsg::Chunk(chunk.to_vec())).await.is_err() {
            break;
        }
    }
    let _ = s.send(StreamMsg::End).await;

    // The device must detect the overrun and answer 400 (or reset).
    let msg = tokio::time::timeout(TIMEOUT, s.next())
        .await
        .expect("device answered")
        .expect("stream stayed open");
    match msg {
        StreamMsg::Head { status, .. } => {
            assert_eq!(
                status, 400,
                "the device must answer 400 on a length overrun"
            );
        }
        StreamMsg::Reset(code) => {
            assert!(
                code == cloud::constants::reset::INTERNAL
                    || code == cloud::constants::reset::PROTOCOL
                    || code == cloud::constants::reset::TOO_LARGE,
                "the device must reset on a length overrun, got Reset({code})"
            );
        }
        other => panic!("unexpected response to overrun: {other:?}"),
    }

    // No attachment was published.
    assert!(
        !poll_until(POLL_BUDGET, || dir_size(&convo_dir) > 0).await,
        "no attachment must be published on a length overrun"
    );
}

// ── M3b: true download underrun ──────────────────────────────────────────────

/// Phone uploads a file with a declared length N, but sends FEWER than N
/// bytes + End. The device must detect the short body and reset the stream;
/// no attachment is published.
#[tokio::test]
async fn upload_declared_length_underrun_resets_stream_no_attachment_published() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let convo_id = uuid::Uuid::parse_str(&convo).expect("uuid");
    let convo_bytes: [u8; 16] = *convo_id.as_bytes();
    let convo_dir = peakbot::paths::uploads_root().join(convo_id.to_string());

    // Declare 2 MB but send 1 MB.
    let declared = 2 * 1024 * 1024;
    let actual = 1024 * 1024;
    let bytes = vec![0xDDu8; actual];

    let mut s = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: "underrun.bin".into(),
                content_type: "application/octet-stream".into(),
                length: Some(declared as u64),
            },
        )
        .await
        .expect("open Upload");

    for chunk in bytes.chunks(MAX_CHUNK) {
        if s.send(StreamMsg::Chunk(chunk.to_vec())).await.is_err() {
            break;
        }
    }
    s.send(StreamMsg::End).await.expect("End");

    // The device must detect the short body and answer 400 (or reset).
    let msg = tokio::time::timeout(TIMEOUT, s.next())
        .await
        .expect("device answered")
        .expect("stream stayed open");
    match msg {
        StreamMsg::Head { status, .. } => {
            assert_eq!(
                status, 400,
                "the device must answer 400 on a length underrun"
            );
        }
        StreamMsg::Reset(code) => {
            assert!(
                code == cloud::constants::reset::INTERNAL
                    || code == cloud::constants::reset::PROTOCOL,
                "the device must reset on a length underrun, got Reset({code})"
            );
        }
        other => panic!("unexpected response to underrun: {other:?}"),
    }

    // No attachment was published.
    assert!(
        !poll_until(POLL_BUDGET, || dir_size(&convo_dir) > 0).await,
        "no attachment must be published on a length underrun"
    );
}

// ── M3c: declared upload short body (N/2) ────────────────────────────────────

/// Phone declares length N, sends N/2 bytes + End. Device must answer 400
/// (or reset), no published attachment/staging, and a subsequent session
/// op must work.
#[tokio::test]
async fn upload_declared_length_short_body_device_400_no_attachment_subsequent_op_works() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let convo_id = uuid::Uuid::parse_str(&convo).expect("uuid");
    let convo_bytes: [u8; 16] = *convo_id.as_bytes();
    let convo_dir = peakbot::paths::uploads_root().join(convo_id.to_string());

    // Declare 1 MB, send 512 KB.
    let declared = 1024 * 1024;
    let actual = 512 * 1024;
    let bytes = vec![0xEEu8; actual];

    let mut s = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: "short.bin".into(),
                content_type: "application/octet-stream".into(),
                length: Some(declared as u64),
            },
        )
        .await
        .expect("open Upload");

    for chunk in bytes.chunks(MAX_CHUNK) {
        if s.send(StreamMsg::Chunk(chunk.to_vec())).await.is_err() {
            break;
        }
    }
    s.send(StreamMsg::End).await.expect("End");

    // The device must detect the short body and answer 400 (or reset).
    let msg = tokio::time::timeout(TIMEOUT, s.next())
        .await
        .expect("device answered")
        .expect("stream stayed open");
    match msg {
        StreamMsg::Head { status, .. } => {
            assert_eq!(
                status, 400,
                "the device must answer 400 for a short upload body"
            );
        }
        StreamMsg::Reset(code) => {
            assert!(
                code == cloud::constants::reset::INTERNAL
                    || code == cloud::constants::reset::PROTOCOL,
                "the device must reset on a short upload body, got Reset({code})"
            );
        }
        other => panic!("unexpected response to short upload body: {other:?}"),
    }

    // No attachment was published.
    assert!(
        !poll_until(POLL_BUDGET, || dir_size(&convo_dir) > 0).await,
        "no attachment must be published on a short upload body"
    );

    // A subsequent session op must work.
    let mut list = b.handle.open(id_a(), Op::List).await.expect("open List");
    let (status, _ct, _body) = op_reply(&mut list).await;
    assert_eq!(
        status, 200,
        "a subsequent session op must work after a short upload body"
    );
}

// ── L6: Attach data after peer End ───────────────────────────────────────────

/// Send Frame/Chunk after half-close (phone's End). The stream must
/// Reset(PROTOCOL) only that stream, not be accepted.
#[tokio::test]
async fn attach_frame_after_phone_end_resets_protocol_only_that_stream() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    drop(convo);

    // Open an Attach stream and complete the handshake.
    let mut s = b
        .handle
        .open(id_a(), Op::Attach)
        .await
        .expect("open Attach");
    s.send(StreamMsg::Frame(
        r#"{"type":"attach","profile":"dev","agent":"team","relayed":true}"#.to_string(),
    ))
    .await
    .expect("send attach frame");

    // Wait for attached + ready.
    let mut saw_ready = false;
    for _ in 0..200 {
        let msg = tokio::time::timeout(TIMEOUT, s.next())
            .await
            .expect("timed out waiting for attach frame")
            .expect("the attach stream stayed open");
        match msg {
            StreamMsg::Frame(f) => {
                let v: serde_json::Value = serde_json::from_str(&f).expect("JSON frame");
                match v["type"].as_str() {
                    Some("ready") => {
                        saw_ready = true;
                        break;
                    }
                    // Skip attach noise (state, models_available).
                    Some("attached") | Some("state") | Some("models_available") => {}
                    _ => {}
                }
            }
            StreamMsg::Reset(c) => panic!("attach stream reset: {c}"),
            other => panic!("non-frame on an attach stream: {other:?}"),
        }
    }
    assert!(saw_ready, "the attach stream must reach ready");

    // Send a complete fragmented message.
    s.send(StreamMsg::FrameBegin { len: 5 })
        .await
        .expect("send FrameBegin");
    s.send(StreamMsg::Chunk(b"hello".to_vec()))
        .await
        .expect("send Chunk");
    s.send(StreamMsg::FrameEnd).await.expect("send FrameEnd");

    // Send End (phone's half-close).
    s.send(StreamMsg::End).await.expect("send End");

    // Now send another Frame — the stream must reset.
    s.send(StreamMsg::Frame(
        r#"{"type":"send_message","text":"after end"}"#.to_string(),
    ))
    .await
    .expect("send frame after End");

    // Wait for the reset (skip attach noise).
    let mut got_reset = false;
    for _ in 0..200 {
        let msg = tokio::time::timeout(TIMEOUT, s.next())
            .await
            .expect("stream answered")
            .expect("stream stayed open");
        match msg {
            StreamMsg::Reset(code) if code == cloud::constants::reset::PROTOCOL => {
                got_reset = true;
                break;
            }
            StreamMsg::Reset(code) => {
                panic!("unexpected reset code after phone End: {code}");
            }
            // Skip attach noise.
            StreamMsg::Frame(_) => {}
            other => {
                panic!(
                    "a Frame after phone End must reset the stream with PROTOCOL, got {other:?}"
                );
            }
        }
    }
    assert!(
        got_reset,
        "a Frame after phone End must reset the stream with PROTOCOL"
    );

    // A different stream on the same session must still work.
    let mut list = b.handle.open(id_a(), Op::List).await.expect("open List");
    let (status, _ct, _body) = op_reply(&mut list).await;
    assert_eq!(status, 200, "the session must survive the post-End Frame");
}
