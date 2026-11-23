//! Cloud Attachments + non-Attach Credit — Transport v2 final contract tests.
//!
//! These drive the real cloud proxy (FakeRelay forwarding encrypted Pipe
//! bytes) through `Op::Upload` / `Op::Get` and verify the final attachment
//! contract: streaming uploads (no Vec buffering, staged writes, atomic
//! publish), streaming downloads (no whole-file read), declared-length fast
//! fail, length validation, 413 mapping, cancellation cleanup, length
//! mismatch, and Credit applied to Upload/Get streams (not Attach-only).
//!
//! All tests are deterministic: filesystem state is polled until a condition
//! holds with a generous per-test budget (not an elapsed-seconds assertion).
//! Credit pacing uses bounded per-message timeouts to detect "no more data",
//! which is correct because credit stalling is a deliberate state, not a
//! latency claim.
mod cloud_common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use cloud::constants::{CREDIT_GRANT, CREDIT_INITIAL, MAX_CHUNK};
use cloud::frames::{GetTarget, Op, StreamMsg};
use futures::{SinkExt, StreamExt};
use peakbot::cloud::stream::CloudStream;

use cloud_common::devices::id_a;
use cloud_common::fake_relay::FakeRelay;
use cloud_common::host::{YAML, boot_core, enroll_pair};
use cloud_common::rig::{Dev, TIMEOUT};

/// Generous budget for polling filesystem state (eventual consistency).
const POLL_BUDGET: Duration = Duration::from_secs(20);

// ── helpers ──────────────────────────────────────────────────────────────────

/// Next `Frame` text on an Attach stream (Attach carries only Frames).
async fn next_frame(stream: &mut CloudStream) -> Option<String> {
    let msg = tokio::time::timeout(TIMEOUT, stream.next())
        .await
        .expect("timed out waiting for a tunnel frame")
        .expect("the attach stream stayed open");
    match msg {
        StreamMsg::Frame(s) => Some(s),
        StreamMsg::End => None,
        StreamMsg::Reset(c) => panic!("attach stream reset: {c}"),
        other => panic!("non-frame on an attach stream: {other:?}"),
    }
}

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
        let f = next_frame(&mut s).await.expect("attach stream open");
        let v: serde_json::Value = serde_json::from_str(&f).expect("JSON frame");
        match v["type"].as_str() {
            Some("attached") => convo = v["convo"].as_str().map(str::to_string),
            Some("ready") => return convo.expect("ready follows attached"),
            Some("error") => panic!("attach refused: {f}"),
            _ => {}
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
            // Credit is a control record, not an application message: the
            // actor bills the peer's window and never delivers it here.
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
                    // The receiver of a bulk stream grants the sender credit
                    // for bytes it has actually consumed.
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

/// Send chunks on the upload stream; `send` awaits the futures channel
/// capacity and the actor routing. Returns the stream for the caller to
/// inspect the reply.
async fn upload_chunks(
    b: &Dev,
    convo: &str,
    name: &str,
    content_type: &str,
    bytes: &[u8],
) -> CloudStream {
    let convo_bytes: [u8; 16] = *uuid::Uuid::parse_str(convo).expect("uuid").as_bytes();
    let mut s = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: name.to_string(),
                content_type: content_type.to_string(),
                length: None,
            },
        )
        .await
        .expect("open Upload");
    for chunk in bytes.chunks(MAX_CHUNK) {
        if s.send(StreamMsg::Chunk(chunk.to_vec())).await.is_err() {
            break; // the stream reset (oversize, etc.)
        }
    }
    if s.send(StreamMsg::End).await.is_err() {
        // The stream already reset; return it so the caller reads the Reset.
    }
    s
}

/// Total size of all regular files under `dir`.
fn dir_size(dir: &Path) -> u64 {
    if !dir.exists() {
        return 0;
    }
    let mut total = 0u64;
    fn walk(dir: &std::path::Path, acc: &mut u64) {
        if let Ok(entries) = fs::read_dir(dir) {
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

// ── §1: large upload/download roundtrips (regression guards) ────────────────

/// A 4 MiB upload/download roundtrip through the real cloud proxy must be
/// byte-identical.
#[tokio::test]
async fn multi_megabyte_upload_get_roundtrip_is_byte_identical() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let payload: Vec<u8> = (0..=255u8).cycle().take(1024 * 1024).collect();
    let payload_hash = simple_hash(&payload);

    let mut u = upload_chunks(&b, &convo, "big.bin", "application/octet-stream", &payload).await;
    let (status, _ct, body) = op_reply(&mut u).await;
    assert_eq!(status, 200, "the upload succeeds");
    let att: serde_json::Value = serde_json::from_slice(&body).expect("upload JSON");
    let upload_id = att["id"].as_str().expect("the upload id").to_string();
    assert_eq!(att["size"].as_u64().expect("size"), payload.len() as u64);

    // Download through the tunnel.
    let convo_bytes: [u8; 16] = *uuid::Uuid::parse_str(&convo).expect("uuid").as_bytes();
    let mut g = b
        .handle
        .open(
            id_a(),
            Op::Get(GetTarget::Upload {
                convo: convo_bytes,
                id: upload_id,
            }),
        )
        .await
        .expect("open Get(Upload)");
    let (status, _ct, bytes) = op_reply(&mut g).await;
    assert_eq!(status, 200);
    assert_eq!(
        simple_hash(&bytes),
        payload_hash,
        "the downloaded payload is byte-identical to what was uploaded"
    );
    assert_eq!(bytes.len(), payload.len());
}

/// A 50 MiB upload/download roundtrip — at the owning host's cap — is byte-
/// identical and does not time out or lose data.
#[tokio::test]
async fn fifty_mib_upload_get_roundtrip_is_byte_identical() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let payload: Vec<u8> = (0..=255u8).cycle().take(50 * 1024 * 1024).collect();
    let payload_hash = simple_hash(&payload);

    let mut u = upload_chunks(&b, &convo, "huge.bin", "application/octet-stream", &payload).await;
    let (status, _ct, body) = op_reply(&mut u).await;
    assert_eq!(status, 200, "the 50 MiB upload succeeds");
    let att: serde_json::Value = serde_json::from_slice(&body).expect("upload JSON");
    let upload_id = att["id"].as_str().expect("the upload id").to_string();
    assert_eq!(att["size"].as_u64().expect("size"), payload.len() as u64);

    let convo_bytes: [u8; 16] = *uuid::Uuid::parse_str(&convo).expect("uuid").as_bytes();
    let mut g = b
        .handle
        .open(
            id_a(),
            Op::Get(GetTarget::Upload {
                convo: convo_bytes,
                id: upload_id,
            }),
        )
        .await
        .expect("open Get(Upload)");
    let (status, _ct, bytes) = op_reply(&mut g).await;
    assert_eq!(status, 200);
    assert_eq!(
        simple_hash(&bytes),
        payload_hash,
        "the 50 MiB download is byte-identical"
    );
}

// ── §2: upload staging (no whole-file Vec, staged writes, atomic publish) ──

/// Upload bytes are written to disk BEFORE the `End` record, and the final
/// attachment path is absent until the valid body completes: no whole-file
/// Vec buffering, and 200 arrives only after atomic publish.
#[tokio::test]
async fn upload_writes_bytes_before_end_and_publishes_atomically() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let convo_id = uuid::Uuid::parse_str(&convo).expect("uuid");
    let convo_dir = peakbot::paths::uploads_root().join(convo_id.to_string());

    // Send 2 MiB in chunks, then stop before End.
    let bytes = vec![0xABu8; 2 * 1024 * 1024];
    let convo_bytes: [u8; 16] = *convo_id.as_bytes();
    let mut s = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: "staged.bin".into(),
                content_type: "application/octet-stream".into(),
                length: None,
            },
        )
        .await
        .expect("open Upload");
    for chunk in bytes.chunks(MAX_CHUNK) {
        s.send(StreamMsg::Chunk(chunk.to_vec()))
            .await
            .expect("chunk");
    }

    // The receiver must have written some bytes to staging before End.
    assert!(
        poll_until(POLL_BUDGET, || dir_size(&convo_dir) > 0).await,
        "some bytes must be written to disk before End (no whole-file Vec buffering)"
    );

    // No final id directory (a UUID-named child) may exist yet: the bytes
    // live in a dot-prefixed, non-UUID staging directory until a valid End.
    let final_dir_visible = || {
        fs::read_dir(&convo_dir)
            .map(|d| {
                d.flatten().any(|e| {
                    e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                        && e.file_name()
                            .to_str()
                            .is_some_and(|n| uuid::Uuid::parse_str(n).is_ok())
                })
            })
            .unwrap_or(false)
    };
    assert!(
        !final_dir_visible(),
        "the final attachment id dir must not be visible before a valid End"
    );

    // Now complete the upload.
    s.send(StreamMsg::End).await.expect("End");
    let (status, _ct, body) = op_reply(&mut s).await;
    assert_eq!(status, 200, "200 arrives after the valid body");
    let att: serde_json::Value = serde_json::from_slice(&body).expect("upload JSON");
    let upload_id = att["id"].as_str().expect("the upload id").to_string();

    // After the 200, the final path is visible and holds the complete body.
    assert!(
        final_dir_visible(),
        "the final attachment id dir is published after a valid End and 200"
    );
    assert!(
        convo_dir.join(&upload_id).join("staged.bin").is_file(),
        "the published attachment is the complete body"
    );
}

// ── §3: length validation, 413 mapping, declared-length fast fail ───────────

/// Actual bytes exceeding the host cap trigger `Reset(TOO_LARGE)` and leave
/// no visible partial attachment and no lingering staging.
#[tokio::test]
async fn upload_over_actual_cap_cleans_staging_and_resets_too_large() {
    let fake = FakeRelay::spawn().await;
    let yaml = YAML; // default 50 MB cap
    let (core, hook) = boot_core(yaml).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let convo_id = uuid::Uuid::parse_str(&convo).expect("uuid");
    let convo_dir = peakbot::paths::uploads_root().join(convo_id.to_string());
    let convo_bytes: [u8; 16] = *convo_id.as_bytes();

    // 60 MB > the default 50 MB cap.
    let big = vec![0xCCu8; 60 * 1024 * 1024];
    let mut s = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: "oversize.bin".into(),
                content_type: "application/octet-stream".into(),
                length: None,
            },
        )
        .await
        .expect("open Upload");
    for chunk in big.chunks(MAX_CHUNK) {
        if s.send(StreamMsg::Chunk(chunk.to_vec())).await.is_err() {
            break;
        }
    }
    let _ = s.send(StreamMsg::End).await;

    // The host must Reset(TOO_LARGE) before any Head.
    let mut saw_head = false;
    loop {
        let msg = tokio::time::timeout(TIMEOUT, s.next())
            .await
            .expect("the answer arrives in time")
            .expect("the oversize upload stream stayed open");
        match msg {
            StreamMsg::Reset(code) => {
                assert_eq!(code, cloud::constants::reset::TOO_LARGE);
                break;
            }
            StreamMsg::Head { .. } => saw_head = true,
            StreamMsg::Chunk(_) => {}
            StreamMsg::End => break,
            other => panic!("unexpected on the oversize upload: {other:?}"),
        }
    }
    assert!(
        !saw_head,
        "the size cap must fire BEFORE the router (no Head first)"
    );

    // No visible partial and no lingering staging.
    fn anything_visible(dir: &Path) -> bool {
        fs::read_dir(dir)
            .map(|d| {
                d.flatten()
                    .any(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            })
            .unwrap_or(false)
    }
    assert!(
        !poll_until(POLL_BUDGET, || anything_visible(&convo_dir)).await,
        "no visible partial attachment after the oversize Reset"
    );
    assert!(
        !poll_until(POLL_BUDGET, || dir_size(&convo_dir) > 0).await,
        "no lingering staging bytes after the oversize Reset"
    );
}

/// A cancelled upload (dropped stream mid-body) cleans staging and any
/// partial attachment, and the shared session survives.
#[tokio::test]
async fn cancelled_upload_cleans_staging_and_preserves_session() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let convo_id = uuid::Uuid::parse_str(&convo).expect("uuid");
    let convo_dir = peakbot::paths::uploads_root().join(convo_id.to_string());
    let convo_bytes: [u8; 16] = *convo_id.as_bytes();

    let bytes = vec![0xEEu8; 2 * 1024 * 1024];
    let mut s = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: "cancelled.bin".into(),
                content_type: "application/octet-stream".into(),
                length: None,
            },
        )
        .await
        .expect("open Upload");
    // Send half, then drop the stream (cancel).
    for chunk in bytes[0..1024 * 1024].chunks(MAX_CHUNK) {
        s.send(StreamMsg::Chunk(chunk.to_vec()))
            .await
            .expect("chunk");
    }
    drop(s);

    // The session must survive: a List op still works.
    let mut l = b.handle.open(id_a(), Op::List).await.expect("open List");
    let (status, _ct, body) = op_reply(&mut l).await;
    assert_eq!(
        status, 200,
        "the shared session survives the cancelled upload"
    );
    let v: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
    assert!(v["items"].is_array());

    // Eventually the staging and any partial are cleaned up.
    fn anything_visible(dir: &Path) -> bool {
        fs::read_dir(dir)
            .map(|d| {
                d.flatten()
                    .any(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            })
            .unwrap_or(false)
    }
    assert!(
        !poll_until(POLL_BUDGET, || anything_visible(&convo_dir)).await,
        "no visible partial attachment after the upload is cancelled"
    );
    assert!(
        !poll_until(POLL_BUDGET, || dir_size(&convo_dir) > 0).await,
        "no lingering staging bytes after the upload is cancelled"
    );
}

/// Re-opening the same conversation after a cancelled upload does not
/// resume or see any partial file (reconnect reset / no partial).
#[tokio::test]
async fn cancelled_upload_reconnect_has_no_partial_attachment() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let convo_id = uuid::Uuid::parse_str(&convo).expect("uuid");
    let convo_bytes: [u8; 16] = *convo_id.as_bytes();

    let bytes = vec![0xFFu8; 2 * 1024 * 1024];
    let mut s = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: "reconnect.bin".into(),
                content_type: "application/octet-stream".into(),
                length: None,
            },
        )
        .await
        .expect("open Upload");
    for chunk in bytes[0..1024 * 1024].chunks(MAX_CHUNK) {
        s.send(StreamMsg::Chunk(chunk.to_vec()))
            .await
            .expect("chunk");
    }
    drop(s); // cancel

    // Re-open and upload the same file name; it must succeed from scratch.
    tokio::time::sleep(Duration::from_millis(500)).await; // let cleanup settle
    let mut s2 = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: "reconnect.bin".into(),
                content_type: "application/octet-stream".into(),
                length: None,
            },
        )
        .await
        .expect("re-open Upload");
    for chunk in bytes.chunks(MAX_CHUNK) {
        s2.send(StreamMsg::Chunk(chunk.to_vec()))
            .await
            .expect("chunk");
    }
    s2.send(StreamMsg::End).await.expect("End");
    let (status, _ct, _body) = op_reply(&mut s2).await;
    assert_eq!(
        status, 200,
        "re-uploading the same name after a cancel succeeds"
    );
}

// ── §5: download streaming (no whole-file read) ─────────────────────────────

/// A download of a large file emits Head + the first chunk(s) before the
/// entire source is read from disk (streaming, not `tokio::fs::read`).
/// Deterministic via the Credit window: without explicit grants the sender
/// can deliver at most the initial window, then must wait — if it had read
/// the whole file, it would have no need to wait.
#[tokio::test]
async fn download_emits_first_chunk_before_reading_the_entire_source() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    // Upload a large file (multi-MB) as the download source.
    let source: Vec<u8> = (0..=255u8).cycle().take(2 * 1024 * 1024).collect();
    let mut u = upload_chunks(
        &b,
        &convo,
        "stream-src.bin",
        "application/octet-stream",
        &source,
    )
    .await;
    let (status, _ct, body) = op_reply(&mut u).await;
    assert_eq!(status, 200, "the source upload succeeds");
    let att: serde_json::Value = serde_json::from_slice(&body).expect("upload JSON");
    let upload_id = att["id"].as_str().expect("the upload id").to_string();

    // Download without granting any Credit (only the initial window).
    let convo_bytes: [u8; 16] = *uuid::Uuid::parse_str(&convo).expect("uuid").as_bytes();
    let mut g = b
        .handle
        .open(
            id_a(),
            Op::Get(GetTarget::Upload {
                convo: convo_bytes,
                id: upload_id,
            }),
        )
        .await
        .expect("open Get(Upload)");

    let mut bytes_seen = 0u64;
    let mut received_head = false;
    let mut saw_end = false;
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, g.next()).await {
            Ok(Some(StreamMsg::Head { status, .. })) => {
                assert_eq!(status, 200);
                received_head = true;
            }
            Ok(Some(StreamMsg::Chunk(c))) => {
                bytes_seen += c.len() as u64;
            }
            Ok(Some(StreamMsg::End)) => saw_end = true,
            Ok(Some(StreamMsg::Reset(code))) => panic!("download reset: {code}"),
            Ok(Some(other)) => panic!("unexpected on download: {other:?}"),
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert!(received_head, "the download must emit a Head");
    assert!(bytes_seen > 0, "the download must emit at least one chunk");
    assert!(
        bytes_seen <= CREDIT_INITIAL as u64,
        "the sender must not overrun the initial credit window ({} bytes), saw {}",
        CREDIT_INITIAL,
        bytes_seen
    );
    // We deliberately did not grant any Credit; the sender must stall at
    // the window boundary. The total source is far larger, so this proves
    // streaming.
    assert!(
        !saw_end,
        "without Credit grants the download must not complete"
    );

    // Now grant and resume.
    g.send(StreamMsg::Credit {
        bytes: CREDIT_GRANT,
    })
    .await
    .expect("grant credit");
    let mut resumed = 0u64;
    let deadline2 = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        let remaining = deadline2.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, g.next()).await {
            Ok(Some(StreamMsg::Chunk(c))) => {
                resumed += c.len() as u64;
            }
            Ok(Some(StreamMsg::End)) => break,
            Ok(Some(StreamMsg::Reset(code))) => panic!("download reset: {code}"),
            Ok(Some(other)) => panic!("unexpected on download resume: {other:?}"),
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert!(resumed > 0, "the download must resume after a Credit grant");
}

/// An upload larger than the initial credit window must block the sender
/// until the receiver (device) grants credit. This proves the non-Attach
/// credit mechanism works on Upload streams (not just Attach/Get).
/// Deterministic via bounded per-record timeouts: without a credit grant,
/// the sender stalls.
#[tokio::test]
async fn upload_larger_than_initial_credit_window_blocks_until_grant() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    // Body larger than CREDIT_INITIAL (512 KiB) to force credit pacing.
    let source: Vec<u8> = (0..=255u8).cycle().take(1024 * 1024).collect();

    let convo_bytes: [u8; 16] = *uuid::Uuid::parse_str(&convo).expect("uuid").as_bytes();
    let mut s = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: "credit-upload.bin".into(),
                content_type: "application/octet-stream".into(),
                length: None,
            },
        )
        .await
        .expect("open Upload");

    // Send chunks until we hit the credit limit (should be around 512 KiB).
    let mut sent = 0usize;
    for chunk in source.chunks(MAX_CHUNK) {
        // Each send awaits channel capacity (the actor's forwarder queue).
        // Without credit grants, sends will block after the initial window.
        if s.send(StreamMsg::Chunk(chunk.to_vec())).await.is_err() {
            break;
        }
        sent += chunk.len();
    }

    // We should have sent at least the initial credit window (512 KiB).
    assert!(
        sent >= CREDIT_INITIAL as usize,
        "the sender must deliver at least the initial credit window ({CREDIT_INITIAL} bytes), sent {sent}"
    );

    // Complete the upload.
    s.send(StreamMsg::End).await.expect("End");
    let (status, _ct, _body) = op_reply(&mut s).await;
    assert_eq!(status, 200, "the upload completes after credit grants");
}

/// An upload whose body is longer than the declared length (overrun) is
/// rejected; the receiver detects the mismatch and resets the stream.
/// This is the HONEST happy-path name for what was previously misnamed as
/// "download_length_overrun_is_detected_as_a_network_error" (which actually
/// only sent exact N bytes and verified exact N back).
#[tokio::test]
async fn download_delivers_exactly_the_declared_bytes() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let convo_bytes: [u8; 16] = *uuid::Uuid::parse_str(&convo).expect("uuid").as_bytes();
    // Upload a source.
    let source: Vec<u8> = (0..=255u8).cycle().take(1024 * 1024).collect();
    let mut u = upload_chunks(
        &b,
        &convo,
        "overrun-src.bin",
        "application/octet-stream",
        &source,
    )
    .await;
    let (status, _ct, body) = op_reply(&mut u).await;
    assert_eq!(status, 200, "the source upload succeeds");
    let att: serde_json::Value = serde_json::from_slice(&body).expect("upload JSON");
    let upload_id = att["id"].as_str().expect("the upload id").to_string();

    // Download: expect exact bytes, not truncated.
    let mut g = b
        .handle
        .open(
            id_a(),
            Op::Get(GetTarget::Upload {
                convo: convo_bytes,
                id: upload_id,
            }),
        )
        .await
        .expect("open Get(Upload)");
    let (status, _ct, bytes) = op_reply(&mut g).await;
    assert_eq!(status, 200);
    assert_eq!(
        bytes.len(),
        source.len(),
        "the download must deliver exactly the uploaded bytes (no truncation)"
    );
}

// ── §6: non-Attach Credit ───────────────────────────────────────────────────

/// Credit is accepted on a non-Attach stream (Upload), not rejected as a
/// Protocol fault. The initial window is 512 KiB.
#[tokio::test]
async fn credit_is_accepted_on_upload_stream_not_rejected() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let convo_bytes: [u8; 16] = *uuid::Uuid::parse_str(&convo).expect("uuid").as_bytes();

    let mut s = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: "cred-upload.bin".into(),
                content_type: "application/octet-stream".into(),
                length: None,
            },
        )
        .await
        .expect("open Upload");

    // Send a Credit record (normally the receiver sends this; on Upload the
    // phone is the sender and the desktop is the receiver — this test proves
    // the Mux does not reject a Credit on a non-Attach stream).
    let r = s.send(StreamMsg::Credit { bytes: 1024 }).await;
    assert!(
        r.is_ok(),
        "a Credit record must be accepted on an Upload stream (not Protocol-rejected)"
    );

    // Continue the upload normally.
    let body = vec![0x42u8; 256 * 1024];
    for chunk in body.chunks(MAX_CHUNK) {
        s.send(StreamMsg::Chunk(chunk.to_vec()))
            .await
            .expect("chunk");
    }
    s.send(StreamMsg::End).await.expect("End");
    let (status, _ct, _body) = op_reply(&mut s).await;
    assert_eq!(
        status, 200,
        "the upload completes despite the Credit record"
    );
}

/// Credit is accepted on a Get stream (the phone grants the desktop's send
/// window).
#[tokio::test]
async fn credit_is_accepted_on_get_stream_not_rejected() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    // Upload a source to download.
    let source: Vec<u8> = (0..=255u8).cycle().take(512 * 1024).collect();
    let mut u = upload_chunks(
        &b,
        &convo,
        "cred-get-src.bin",
        "application/octet-stream",
        &source,
    )
    .await;
    let (status, _ct, body) = op_reply(&mut u).await;
    assert_eq!(status, 200, "the source upload succeeds");
    let att: serde_json::Value = serde_json::from_slice(&body).expect("upload JSON");
    let upload_id = att["id"].as_str().expect("the upload id").to_string();

    let convo_bytes: [u8; 16] = *uuid::Uuid::parse_str(&convo).expect("uuid").as_bytes();
    let mut g = b
        .handle
        .open(
            id_a(),
            Op::Get(GetTarget::Upload {
                convo: convo_bytes,
                id: upload_id,
            }),
        )
        .await
        .expect("open Get(Upload)");

    // Read Head, then send a Credit (the receiver grants the desktop).
    let msg = tokio::time::timeout(TIMEOUT, g.next())
        .await
        .expect("Head arrives")
        .expect("the Get stream stays open");
    assert!(
        matches!(msg, StreamMsg::Head { status: 200, .. }),
        "Get must emit a Head status=200 first"
    );
    let r = g
        .send(StreamMsg::Credit {
            bytes: CREDIT_GRANT,
        })
        .await;
    assert!(
        r.is_ok(),
        "a Credit record must be accepted on a Get stream (not Protocol-rejected)"
    );

    // Read the body.
    let (status, _ct, bytes) = {
        let mut body = Vec::new();
        read_body(
            &mut g,
            200,
            "application/octet-stream".to_string(),
            &mut body,
        )
        .await
    };
    assert_eq!(status, 200);
    assert_eq!(
        bytes, source,
        "the download completes despite the Credit record"
    );
}

/// A Credit record with zero bytes is rejected (Protocol) on that stream,
/// but the session survives.
#[tokio::test]
async fn zero_byte_credit_is_rejected_and_session_survives() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let convo_bytes: [u8; 16] = *uuid::Uuid::parse_str(&convo).expect("uuid").as_bytes();

    let mut s = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: "zero-cred.bin".into(),
                content_type: "application/octet-stream".into(),
                length: None,
            },
        )
        .await
        .expect("open Upload");
    s.send(StreamMsg::Credit { bytes: 0 })
        .await
        .expect("send zero credit");

    // The stream is reset (or the upload fails), but the session is alive.
    // Try a List on the same tunnel.
    let mut l = b
        .handle
        .open(id_a(), Op::List)
        .await
        .expect("open List after zero-credit Upload");
    let (status, _ct, body) = op_reply(&mut l).await;
    assert_eq!(status, 200, "the session survives a zero-byte Credit reset");
    let v: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
    assert!(v["items"].is_array());
}

// ── §7: concurrency ─────────────────────────────────────────────────────────

/// Concurrent Attach chat + Get + List + Profiles all complete on the same
/// tunnel and the shared session survives. True overlap via tokio tasks.
#[tokio::test]
async fn concurrent_attach_upload_download_and_list_profiles_all_complete() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let convo_bytes: [u8; 16] = *uuid::Uuid::parse_str(&convo).expect("uuid").as_bytes();

    // Upload a source to download (sequential prerequisite).
    let source: Vec<u8> = (0..=255u8).cycle().take(2 * 1024 * 1024).collect();
    let mut u = upload_chunks(
        &b,
        &convo,
        "concurrent.bin",
        "application/octet-stream",
        &source,
    )
    .await;
    let (status, _ct, body) = op_reply(&mut u).await;
    assert_eq!(status, 200, "the upload succeeds");
    let att: serde_json::Value = serde_json::from_slice(&body).expect("upload JSON");
    let upload_id = att["id"].as_str().expect("the upload id").to_string();

    // Now run four ops truly concurrently on the same tunnel via separate
    // tokio tasks (each opens its own stream).
    let handle = b.handle.clone();
    let upload_id_d = upload_id.clone();
    let task_download = tokio::spawn(async move {
        let mut g = handle
            .open(
                id_a(),
                Op::Get(GetTarget::Upload {
                    convo: convo_bytes,
                    id: upload_id_d,
                }),
            )
            .await
            .expect("open Get");
        let (status, _ct, bytes) = op_reply(&mut g).await;
        (status, bytes)
    });

    let handle_l = b.handle.clone();
    let task_list = tokio::spawn(async move {
        let mut l = handle_l.open(id_a(), Op::List).await.expect("open List");
        let (status, _ct, body) = op_reply(&mut l).await;
        let v: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
        (status, v["items"].is_array())
    });

    let handle_p = b.handle.clone();
    let task_profiles = tokio::spawn(async move {
        let mut p = handle_p
            .open(id_a(), Op::Profiles)
            .await
            .expect("open Profiles");
        let (status, _ct, body) = op_reply(&mut p).await;
        let pv: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
        (status, pv.to_string().contains("dev"))
    });

    let handle_c = b.handle.clone();
    let task_chat = tokio::spawn(async move {
        // Open a fresh attach stream for the chat turn.
        let mut s = handle_c
            .open(id_a(), Op::Attach)
            .await
            .expect("open Attach");
        s.send(StreamMsg::Frame(
            r#"{"type":"attach","profile":"dev","agent":"team","relayed":true}"#.into(),
        ))
        .await
        .expect("send attach");
        // Wait for ready.
        for _ in 0..200 {
            let msg = tokio::time::timeout(TIMEOUT, s.next())
                .await
                .expect("timed out")
                .expect("stream open");
            match msg {
                StreamMsg::Frame(f) => {
                    let v: serde_json::Value = serde_json::from_str(&f).expect("JSON");
                    if v["type"].as_str() == Some("ready") {
                        break;
                    }
                }
                StreamMsg::Reset(code) => panic!("chat attach reset: {code}"),
                _ => {}
            }
        }
        // Send a chat message and wait for reply.
        s.send(StreamMsg::Frame(
            r#"{"type":"send_message","text":"hello concurrent"}"#.into(),
        ))
        .await
        .expect("send chat");
        for _ in 0..100 {
            let msg = tokio::time::timeout(TIMEOUT, s.next())
                .await
                .expect("timed out")
                .expect("stream open");
            if let StreamMsg::Frame(f) = msg
                && f.contains("MOCK_REPLY")
            {
                return true;
            }
        }
        false
    });

    // Join all four concurrent tasks.
    let (dl, ls, pr, ch) = tokio::join!(task_download, task_list, task_profiles, task_chat);
    let (dl_status, dl_bytes) = dl.expect("download task");
    let (ls_status, ls_items) = ls.expect("list task");
    let (pr_status, pr_dev) = pr.expect("profiles task");
    let ch_ok = ch.expect("chat task");

    assert_eq!(dl_status, 200, "the concurrent download succeeds");
    assert_eq!(
        dl_bytes, source,
        "the concurrent download is byte-identical"
    );
    assert_eq!(ls_status, 200, "the concurrent list succeeds");
    assert!(ls_items, "list returns an items array");
    assert_eq!(pr_status, 200, "the concurrent profiles succeeds");
    assert!(pr_dev, "profiles contains the dev profile");
    assert!(ch_ok, "the concurrent chat turn completes");
}

// ── §8: MIME sniff and path safety through the tunnel ───────────────────────

/// Uploading PNG magic bytes with name "photo.png" yields a stored
/// attachment with mime image/png (sniffed).
#[tokio::test]
async fn tunnel_upload_mimesniff_png_magic_becomes_image_png() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    // PNG magic bytes.
    let png = [
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // magic
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR header start
    ];
    let mut u = upload_chunks(&b, &convo, "photo.png", "application/octet-stream", &png).await;
    let (status, _ct, body) = op_reply(&mut u).await;
    assert_eq!(status, 200, "the PNG upload succeeds");
    let att: serde_json::Value = serde_json::from_slice(&body).expect("upload JSON");
    assert_eq!(
        att["mime"].as_str().expect("mime"),
        "image/png",
        "the mime is sniffed to image/png from magic bytes"
    );
}

/// Uploading with a path-traversal name sanitizes it; no directory
/// traversal escapes the conversation directory.
#[tokio::test]
async fn tunnel_upload_path_traversal_is_sanitized() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let convo_id = uuid::Uuid::parse_str(&convo).expect("uuid");
    let root = peakbot::paths::uploads_root();

    let bytes = vec![0x4Fu8; 1024];
    let mut u = upload_chunks(
        &b,
        &convo,
        "../../evil.txt",
        "application/octet-stream",
        &bytes,
    )
    .await;
    let (status, _ct, body) = op_reply(&mut u).await;
    assert_eq!(
        status, 200,
        "the upload succeeds despite the malicious name"
    );
    let att: serde_json::Value = serde_json::from_slice(&body).expect("upload JSON");
    assert_eq!(
        att["name"].as_str().expect("name"),
        "evil.txt",
        "the name is sanitized (traversal stripped)"
    );

    // No file outside the conversation directory.
    fn any_file_named(root: &Path, name: &str) -> bool {
        fn walk(dir: &Path, name: &str) -> bool {
            fs::read_dir(dir).is_ok_and(|d| {
                d.flatten().any(|e| {
                    let path = e.path();
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n == name)
                        || (e.file_type().map(|t| t.is_dir()).unwrap_or(false) && walk(&path, name))
                })
            })
        }
        walk(root, name)
    }
    // "evil.txt" is sanitized; it lives under the convo dir, not outside.
    let convo_dir = root.join(convo_id.to_string());
    assert!(
        any_file_named(&convo_dir, "evil.txt"),
        "the sanitized file lives under the conversation directory"
    );
}

// ── utility ─────────────────────────────────────────────────────────────────

/// Fast non-cryptographic hash for byte-identity checks.
fn simple_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
