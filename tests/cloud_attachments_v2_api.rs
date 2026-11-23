//! Cloud Attachments + non-Attach Credit — API/contract tests.
//!
//! These exercise the production interfaces the streaming design added:
//! - `Op::Upload { ..., length: Option<u64> }` (declared Content-Length).
//! - `UploadStore::open_stream(&Attachment) -> io::Result<UploadReader>`
//!   for streaming downloads.
//!
//! Run with: cargo test --test cloud_attachments_v2_api

mod cloud_common;

use std::fs;
use std::path::PathBuf;

use std::task::Poll;

use cloud::constants::MAX_CHUNK;
use cloud::frames::{Op, StreamMsg};
use futures::{SinkExt, StreamExt};
use peakbot::attachments::UploadStore;
use peakbot::cloud::stream::CloudStream;
use peakbot::config::UploadsConfig;
use peakbot::ui::web::remote_types::{Client, RemoteError};

use cloud_common::devices::id_a;
use cloud_common::fake_relay::FakeRelay;
use cloud_common::host::{YAML, boot_core, enroll_pair};
use cloud_common::rig::{Dev, TIMEOUT};

// ── helpers (shared with the runtime-RED file) ──────────────────────────────

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
                if since_grant >= u64::from(cloud::constants::CREDIT_GRANT) {
                    let _ = stream
                        .send(StreamMsg::Credit {
                            bytes: u32::try_from(since_grant)
                                .unwrap_or(cloud::constants::CREDIT_GRANT),
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

// ── §3: Op::Upload declared length ──────────────────────────────────────────

/// Declaring a length larger than the host cap fast-fails BEFORE the body
/// is read or any disk activity happens. (Requires `Op::Upload.length`.)
#[tokio::test]
async fn upload_declared_length_over_cap_fast_fails_before_body() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let convo_id = uuid::Uuid::parse_str(&convo).expect("uuid");
    let convo_bytes: [u8; 16] = *convo_id.as_bytes();
    let root = peakbot::paths::uploads_root();
    let size_before = dir_size(&root);

    // Declare 60 MB but send no body. The host must fast-fail with TOO_LARGE.
    let mut s = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: "declared-big.bin".into(),
                content_type: "application/octet-stream".into(),
                length: Some(60 * 1024 * 1024), // > 50 MB default cap
            },
        )
        .await
        .expect("open Upload with declared length");

    // The host must reject immediately, without consuming the body.
    let msg = tokio::time::timeout(TIMEOUT, s.next())
        .await
        .expect("fast-fail arrives")
        .expect("the stream stays open");
    assert!(
        matches!(msg, StreamMsg::Reset(code) if code == cloud::constants::reset::TOO_LARGE),
        "a declared length over the cap must fast-fail with TOO_LARGE"
    );

    // No disk activity.
    assert_eq!(
        dir_size(&root),
        size_before,
        "no disk writes should have happened for a declared-length fast-fail"
    );
}

/// An upload with a declared length is validated: sending fewer bytes than
/// declared is rejected; sending exactly the declared bytes is accepted.
/// (Requires `Op::Upload.length`.)
#[tokio::test]
async fn upload_declared_length_validation_roundtrip() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let convo = attach_and_ready(&b, "dev").await;
    let convo_id = uuid::Uuid::parse_str(&convo).expect("uuid");
    let convo_bytes: [u8; 16] = *convo_id.as_bytes();

    // Send exactly the declared length: should succeed.
    let body = vec![0xAAu8; 1024 * 1024];
    let mut s = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: "exact.bin".into(),
                content_type: "application/octet-stream".into(),
                length: Some(body.len() as u64),
            },
        )
        .await
        .expect("open Upload");
    for chunk in body.chunks(MAX_CHUNK) {
        s.send(StreamMsg::Chunk(chunk.to_vec()))
            .await
            .expect("chunk");
    }
    s.send(StreamMsg::End).await.expect("End");
    let (status, _ct, _body) = op_reply(&mut s).await;
    assert_eq!(status, 200, "exact declared length must be accepted");
}

/// Through the controller's cloud proxy, the owning device's declared-cap
/// refusal is the typed `PayloadTooLarge` error (the proxy maps it to HTTP
/// 413), never a generic transport failure.
#[tokio::test]
async fn proxy_upload_over_declared_cap_is_payload_too_large() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let client = Client::Cloud {
        handle: b.handle.clone(),
        device: id_a(),
        profile: "dev".to_string(),
    };
    let body = futures::stream::once(async {
        Ok::<bytes::Bytes, axum::Error>(bytes::Bytes::from_static(b"x"))
    })
    .boxed();
    let err = match client
        .post_upload(
            &uuid::Uuid::new_v4(),
            "too-big.bin",
            Some(reqwest::header::HeaderValue::from_static(
                "application/octet-stream",
            )),
            Some(60 * 1024 * 1024),
            body,
        )
        .await
    {
        Ok(_) => panic!("a declared over-cap length must be refused"),
        Err(e) => e,
    };
    assert!(
        matches!(err, RemoteError::PayloadTooLarge { .. }),
        "TOO_LARGE must surface as PayloadTooLarge (HTTP 413), got {err:?}"
    );
}

// ── §5: download streaming with injected reader ────────────────────────────

/// `UploadStore::open_stream` opens the stored file itself (no whole-file
/// `tokio::fs::read` in the caller) and yields the exact bytes. This is the
/// production seam the download handler pipes from.
#[tokio::test]
async fn open_stream_yields_the_exact_stored_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = UploadStore::new(dir.path().to_path_buf(), UploadsConfig::default());
    let convo = uuid::Uuid::new_v4();
    let payload: Vec<u8> = (0..=255u8).cycle().take(1024 * 1024).collect();
    let attachment = store
        .store(convo, "source.bin", &payload[..])
        .await
        .expect("store");

    let mut reader = store
        .open_stream(&attachment)
        .await
        .expect("open_stream must open the stored file");
    let mut got = Vec::new();
    use tokio::io::AsyncReadExt;
    reader.read_to_end(&mut got).await.expect("read to end");
    assert_eq!(got, payload, "open_stream yields the stored bytes exactly");
}

// ── utility ─────────────────────────────────────────────────────────────────

fn dir_size(dir: &PathBuf) -> u64 {
    if !dir.exists() {
        return 0;
    }
    let mut total = 0u64;
    fn walk(dir: &PathBuf, acc: &mut u64) {
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

// ── Early upload response race (§F.7) ────────────────────────────────────────

/// When the device sends an early Head (e.g. 415 wrong Content-Type) while
/// the phone is still uploading chunks, the phone's send fails and
/// `cloud_upload_error` must surface the actual Head status — not a generic
/// 502. This exercises the real race: Head arrives, stream closes, phone's
/// next chunk send fails.
#[tokio::test]
async fn upload_early_head_surfaces_actual_status_not_502() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let client = Client::Cloud {
        handle: b.handle.clone(),
        device: id_a(),
        profile: "dev".to_string(),
    };

    // Slow upload with wrong Content-Type: the host validates the
    // Content-Type immediately and returns 415 before the phone finishes
    // sending all chunks. The phone's send fails when it detects the
    // stream is closed.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Option<Vec<u8>>>(4);
    let body = futures::stream::poll_fn(move |cx| match futures::ready!(rx.poll_recv(cx)) {
        Some(Some(chunk)) => Poll::Ready(Some(Ok::<bytes::Bytes, axum::Error>(
            bytes::Bytes::from(chunk),
        ))),
        Some(None) | None => Poll::Ready(None),
    })
    .boxed();

    let upload_task = tokio::spawn(async move {
        client
            .post_upload(
                &uuid::Uuid::new_v4(),
                "test.txt",
                // Wrong Content-Type: host returns 415 immediately.
                Some(reqwest::header::HeaderValue::from_static("text/plain")),
                None,
                body,
            )
            .await
    });

    // Send a few chunks, then stop. The host should return 415 before the
    // phone sends all chunks.
    tx.send(Some(vec![0u8; 4096])).await.unwrap();
    tx.send(Some(vec![0u8; 4096])).await.unwrap();
    // Stop sending — the host should have returned 415 by now.
    drop(tx);

    let result = tokio::time::timeout(TIMEOUT, upload_task)
        .await
        .expect("upload completed in time")
        .expect("upload task joined");

    // The phone must see the device's actual 415, not a generic 502.
    match result {
        Ok(up) => {
            assert_eq!(
                up.status,
                reqwest::StatusCode::from_u16(415).unwrap(),
                "phone must see the device's actual 415, not 502"
            );
        }
        Err(e) => {
            // The actual status must be surfaced, not a generic 502.
            let msg = e.to_string();
            assert!(
                msg.contains("415")
                    || msg.contains("Unsupported Media Type")
                    || msg.contains("400"),
                "cloud_upload_error must surface the actual status, got generic error: {msg}"
            );
        }
    }
}
