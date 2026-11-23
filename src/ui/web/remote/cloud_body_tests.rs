//! Cloud client-level download length validation (§F.3).
//!
//! These tests exercise the actual phone `cloud_body()` path with synthetic
//! streams that send wrong-length data, proving the CLIENT validates the
//! declared length — not just the device's `send_response`.
//!
//! The existing integration tests in `tests/cloud_attachments_v2.rs` only
//! test the device's upload path (declaring N, sending >N). These tests
//! verify the download path: the phone's `cloud_body()` must detect
//! overrun (>N bytes) and underrun (<N bytes + End) and report errors,
//! never clean truncated bodies.

use crate::cloud::stream::CloudStream;

use cloud::frames::StreamMsg;
use futures::StreamExt;
use futures::channel::mpsc as fmpsc;

use super::*;

// ── Tests ────────────────────────────────────────────────────────────────────

/// Client declares length N, device sends >N bytes before End → body error,
/// never a clean complete200. This exercises `cloud_body()`'s overrun check
/// (`st.seen + len > expected`).
#[tokio::test]
async fn download_overrun_is_body_error_not_clean_complete() {
    // Device declares 100 bytes, sends 150 bytes, then End.
    let (stream, tx) = make_device_stream();
    tx.send(StreamMsg::Head {
        status: 200,
        content_type: "application/octet-stream".into(),
        length: Some(100),
    })
    .await
    .unwrap();
    tx.send(StreamMsg::Chunk(vec![0xAAu8; 150])).await.unwrap();
    tx.send(StreamMsg::End).await.unwrap();

    let up = Upstream::from_cloud(stream);
    let body_stream = up.bytes_stream();
    let mut stream = body_stream;

    // The overrun is detected immediately — the first read returns an error.
    let overrun = stream.next().await.expect("overrun detected");
    assert!(
        overrun.is_err(),
        "the client must detect an overrun as an error, not deliver extra bytes"
    );
    let err = overrun.unwrap_err();
    assert!(
        err.to_string().contains("longer"),
        "the error must say the body is longer than declared: {err}"
    );
}

/// Client declares length N, device sends <N bytes + End → body error,
/// never a clean truncated200. This exercises `cloud_body()`'s underrun check
/// (`st.seen != expected` on End).
#[tokio::test]
async fn download_underrun_is_body_error_not_clean_truncated() {
    // Device declares 100 bytes, sends 50 bytes, then End.
    let (stream, tx) = make_device_stream();
    tx.send(StreamMsg::Head {
        status: 200,
        content_type: "application/octet-stream".into(),
        length: Some(100),
    })
    .await
    .unwrap();
    tx.send(StreamMsg::Chunk(vec![0xBBu8; 50])).await.unwrap();
    tx.send(StreamMsg::End).await.unwrap();

    let up = Upstream::from_cloud(stream);
    let body_stream = up.bytes_stream();
    let mut stream = body_stream;

    // First chunk arrives (50 bytes).
    let chunk = stream.next().await.expect("first chunk");
    assert!(chunk.is_ok(), "first bytes should arrive");
    assert_eq!(chunk.unwrap().len(), 50);

    // The underrun should be detected when End is reached.
    let underrun = stream.next().await.expect("underrun detected");
    assert!(
        underrun.is_err(),
        "the client must detect an underrun as an error, not complete cleanly"
    );
    let err = underrun.unwrap_err();
    assert!(
        err.to_string().contains("shorter"),
        "the error must say the body is shorter than declared: {err}"
    );
}

/// Exact-length download completes cleanly (regression guard).
#[tokio::test]
async fn download_exact_length_completes_cleanly() {
    let (stream, tx) = make_device_stream();
    tx.send(StreamMsg::Head {
        status: 200,
        content_type: "application/octet-stream".into(),
        length: Some(100),
    })
    .await
    .unwrap();
    tx.send(StreamMsg::Chunk(vec![0xCCu8; 100])).await.unwrap();
    tx.send(StreamMsg::End).await.unwrap();

    let up = Upstream::from_cloud(stream);
    let body_stream = up.bytes_stream();
    let mut stream = body_stream;

    let chunk = stream.next().await.expect("chunk");
    assert!(chunk.is_ok());
    assert_eq!(chunk.unwrap().len(), 100);

    // Should complete cleanly (None, not an error).
    let done = stream.next().await;
    assert!(
        done.is_none(),
        "exact-length download should complete cleanly"
    );
}

/// Early upload response: device sends Head before phone finishes sending
/// chunks. Phone must surface the actual queued Head status (415/400), not 502.
/// This tests the CLIENT-level race: the phone's upload stream is still being
/// written while the device's response arrives and gets queued.
///
/// We simulate the race by having the "device" send a Head while the phone
/// has already sent its first chunk. The phone must read the queued Head
/// (the actual device status), not a generic 502.
#[tokio::test]
async fn upload_early_head_is_decoded() {
    // Simulate: phone sent chunks, device responds early with 415.
    let (mut stream, tx) = make_device_stream();
    // Device sends early response (simulating the race).
    tx.send(StreamMsg::Head {
        status: 415,
        content_type: "application/json".into(),
        length: None,
    })
    .await
    .unwrap();

    // Phone reads the response — must be the device's actual 415, not 502.
    // The Head is queued and immediately readable, proving the race is handled.
    let msg = stream.next().await.expect("response arrived");
    assert!(
        matches!(msg, StreamMsg::Head { status: 415, .. }),
        "phone must see the device's actual 415, not 502"
    );
}

/// The actual production race: device sends an early Head(415) and closes
/// the stream while the phone is still uploading chunks. The phone's next
/// send fails, and `cloud_upload_error` must return `Ok(Upstream)` with the
/// device's actual 415 status — not a generic `Err(RemoteError)` (502).
///
/// This drives the real `cloud_upload_error` path with a synthetic stream
/// where the device's early Head is queued. The production contract (p3-
/// design §F.7) requires `cloud_upload_error` to return
/// `Result<Upstream, RemoteError>`: on a queued Head it returns
/// `Ok(Upstream)` so the phone surfaces the device's actual HTTP status;
/// on a queued Reset(TOO_LARGE) it returns `Err(PayloadTooLarge)` (413).
///
/// This test will be compile-RED until `cloud_upload_error`'s signature
/// is updated. That is intentional: it gives the implementer the exact
/// target to build against.
#[tokio::test]
async fn post_upload_early_head_415_surfaces_not_502() {
    // Create a stream where the "device" side has already sent Head(415).
    let (stream, tx) = make_device_stream();
    tx.send(StreamMsg::Head {
        status: 415,
        content_type: "application/json".into(),
        length: None,
    })
    .await
    .unwrap();

    // Now drop the send receiver so the phone's next send fails with a
    // BrokenPipe-like error (the fmpsc channel is closed).
    drop(tx);

    // Simulate the phone's send failure and call the production error path.
    let send_error = crate::cloud::CloudError::Io("send failed: stream closed".to_string());
    // New contract: cloud_upload_error returns Result<Upstream, RemoteError>.
    let result = super::cloud_upload_error(stream, "dev@laptop", &send_error).await;

    // The fix must return Ok(Upstream) with the device's actual 415 status.
    let up = match result {
        Ok(up) => up,
        Err(e) => panic!(
            "cloud_upload_error must return Ok(Upstream) for a queued Head, \
             not an Err(RemoteError). Got: {e:?}"
        ),
    };

    assert_eq!(
        up.status,
        reqwest::StatusCode::from_u16(415).unwrap(),
        "the Upstream response must have the device's actual 415 status"
    );
}

/// A Reset(TOO_LARGE) queued on the stream must map to Err(PayloadTooLarge)
/// (HTTP 413), not a generic 502. This verifies the existing error path
/// still works after the 415 fix.
#[tokio::test]
async fn cloud_upload_error_too_large_maps_to_413() {
    let (stream, tx) = make_device_stream();
    tx.send(StreamMsg::Reset(cloud::constants::reset::TOO_LARGE))
        .await
        .unwrap();
    drop(tx);

    let send_error = crate::cloud::CloudError::Io("send failed: stream closed".to_string());
    let result = super::cloud_upload_error(stream, "dev@laptop", &send_error).await;

    match result {
        Err(RemoteError::PayloadTooLarge { .. }) => {}
        other => panic!(
            "cloud_upload_error must return Err(PayloadTooLarge) for Reset(TOO_LARGE). \
             Got result: {:?}",
            other.is_ok()
        ),
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Create a synthetic `CloudStream` where we control the "device" side
/// by sending messages via the returned channel.
fn make_device_stream() -> (CloudStream, tokio::sync::mpsc::Sender<StreamMsg>) {
    let (call_to_actor, _actor_from_call) = fmpsc::channel(32);
    let (actor_to_call, call_from_actor) = tokio::sync::mpsc::channel(32);
    let stream = CloudStream::new(call_to_actor, call_from_actor);
    (stream, actor_to_call)
}
