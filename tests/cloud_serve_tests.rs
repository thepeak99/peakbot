//! P3 PR-C RED tests — CloudServe (p3-design §E), driven through the
//! inbound hook: a host core with `WebUi::cloud_inbound()` wired into its
//! CloudLink, a controller device opening the six ops, and the frames the
//! tunnel carries. The host serves the SAME in-process routers the HTTP
//! routes use, so every op must produce the same results as `serve_op`
//! (A2, green in `src/ui/web/cloud_serve.rs`).
//!
//! RED today: `CloudServe::accept` is a `todo!()` skeleton — every case
//! fails on the panic (or on the missing ready frame), for the right
//! reason (feature absent).

mod cloud_common;

use cloud_common::host::{YAML, boot_core, enroll_pair, yaml_small_uploads};

use std::sync::Arc;

use cloud::frames::{GetTarget, Op, StreamMsg};
use futures::{SinkExt, StreamExt};
use peakbot::cloud::InboundHook;
use peakbot::cloud::stream::CloudStream;

use cloud_common::devices::id_a;
use cloud_common::fake_relay::FakeRelay;
use cloud_common::rig::{Dev, TIMEOUT};

// ── tunnel frame helpers ──────────────────────────────────────────────────

/// Next `Frame` text on an Attach stream (an Attach stream carries only
/// `Frame`s; `Head`/`Chunk` there would be a protocol bug).
async fn next_frame(stream: &mut CloudStream) -> Option<String> {
    loop {
        let msg = tokio::time::timeout(TIMEOUT, stream.next())
            .await
            .expect("timed out waiting for a tunnel frame")
            .expect("the attach stream stayed open");
        match msg {
            StreamMsg::Frame(s) => return Some(s),
            StreamMsg::End => return None,
            StreamMsg::Reset(c) => panic!("attach stream reset: {c}"),
            other => panic!("non-frame on an attach stream: {other:?}"),
        }
    }
}

/// Open `Op::Attach` A→(peer) and drive the browser attach protocol:
/// first frame out is the attach frame (with `relayed` as the test says —
/// the server must force it for a CloudPeer, §E.1).
async fn attach(b: &Dev, profile: &str, relayed: bool) -> CloudStream {
    let mut s = b
        .handle
        .open(id_a(), Op::Attach)
        .await
        .expect("open Attach");
    s.send(StreamMsg::Frame(format!(
        r#"{{"type":"attach","profile":"{profile}","agent":"team","relayed":{relayed}}}"#
    )))
    .await
    .expect("send attach frame");
    s
}

/// Attached + ready, and the convo id the socket bound to.
async fn wait_ready(stream: &mut CloudStream) -> String {
    let mut convo = None;
    for _ in 0..20 {
        let f = next_frame(stream)
            .await
            .expect("the attach stream must stay open");
        let v: serde_json::Value = serde_json::from_str(&f).expect("JSON frame");
        match v["type"].as_str() {
            Some("attached") => convo = v["convo"].as_str().map(str::to_string),
            Some("ready") => {
                return convo.expect("ready follows attached, which carries the convo id");
            }
            Some("error") => panic!("attach refused: {f}"),
            other => panic!("unexpected frame before ready: {other:?} {f}"),
        }
    }
    panic!("no ready frame on the attach stream")
}

/// One request/response op: read `Head`, then `Chunk`s until `End`.
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
            } => return read_body(stream, status, content_type, &mut body).await,
            StreamMsg::Chunk(c) => body.extend(c),
            StreamMsg::Reset(code) => panic!("op stream reset: {code}"),
            StreamMsg::End => panic!("End before the Head"),
            StreamMsg::Frame(f) => panic!("unexpected frame on an op stream: {f}"),
            StreamMsg::Open(_) => panic!("unexpected Open on an op stream"),
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
    loop {
        let msg = tokio::time::timeout(TIMEOUT, stream.next())
            .await
            .expect("op body in time")
            .expect("op stream stayed open");
        match msg {
            StreamMsg::Chunk(c) => body.extend(c),
            StreamMsg::End => return (status, content_type, body),
            StreamMsg::Reset(code) => panic!("op body reset: {code}"),
            other => panic!("unexpected message in an op body: {other:?}"),
        }
    }
}

/// Upload bytes in MAX_CHUNK-sized pieces; return the reply.
async fn upload(
    b: &Dev,
    convo: &str,
    name: &str,
    content_type: &str,
    bytes: Vec<u8>,
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
            },
        )
        .await
        .expect("open Upload");
    // A send may race a host-side Reset (§E.4: TOO_LARGE fires mid-upload):
    // stop sending then; the reply (or Reset) is read by the caller.
    for chunk in bytes.chunks(cloud::constants::MAX_CHUNK) {
        if s.send(StreamMsg::Chunk(chunk.to_vec())).await.is_err() {
            return s;
        }
    }
    let _ = s.send(StreamMsg::End).await;
    s
}

// ── §E.1/§E.2: attach through the hook ────────────────────────────────────

/// Attach → ready, and a typed message reaches the mock model: the reply
/// (`MOCK_REPLY`) streams back down the tunnel — frames flow both ways.
#[tokio::test]
async fn attach_over_inbound_hook_yields_ready_and_frames_flow() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let mut s = attach(&b, "dev", false).await;
    let convo = wait_ready(&mut s).await;
    assert!(!convo.is_empty(), "attached carries the convo id");

    s.send(StreamMsg::Frame(
        r#"{"type":"send_message","text":"hi"}"#.into(),
    ))
    .await
    .expect("send the user message");
    let mut saw_reply = false;
    for _ in 0..100 {
        let f = next_frame(&mut s)
            .await
            .expect("the session must stay alive");
        if f.contains("MOCK_REPLY") {
            saw_reply = true;
            break;
        }
    }
    assert!(
        saw_reply,
        "the mock model's reply must flow back through the tunnel"
    );
}

/// §E.1: the ONLY refused inbound message. `shutdown` from a cloud peer is
/// answered with the error frame, the session continues, and the next
/// message still works.
#[tokio::test]
async fn cloud_peer_shutdown_is_dropped_and_session_continues() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let mut s = attach(&b, "dev", false).await;
    wait_ready(&mut s).await;

    s.send(StreamMsg::Frame(r#"{"type":"shutdown"}"#.into()))
        .await
        .expect("send shutdown");
    // The server legitimately sends `models_available` right after `ready`;
    // skip that benign frame, then the refusal must be the very next one.
    let mut f = next_frame(&mut s)
        .await
        .expect("the session must survive the refused shutdown");
    if f.contains("\"models_available\"") {
        f = next_frame(&mut s)
            .await
            .expect("the session must survive the refused shutdown");
    }
    let v: serde_json::Value = serde_json::from_str(&f).expect("JSON frame");
    assert_eq!(v["type"], "error", "shutdown must be refused, got: {f}");
    assert!(
        f.contains("not allowed over Cloud Mode"),
        "the refusal names Cloud Mode, got: {f}"
    );

    // The session is alive: a normal turn still completes.
    s.send(StreamMsg::Frame(
        r#"{"type":"send_message","text":"still here?"}"#.into(),
    ))
    .await
    .expect("send after the refused shutdown");
    let mut saw_reply = false;
    for _ in 0..100 {
        let f = next_frame(&mut s)
            .await
            .expect("the session must stay alive");
        if f.contains("MOCK_REPLY") {
            saw_reply = true;
            break;
        }
    }
    assert!(
        saw_reply,
        "the session must still serve turns after the refused shutdown"
    );
}

/// §E.1: `relayed` is forced server-side for a CloudPeer — so a cloud peer
/// can NEVER attach through one of this host's remote profiles, whether it
/// claims `relayed:false` (client lie) or `relayed:true` (honest): both are
/// chaining and both are refused.
#[tokio::test]
async fn cloud_peer_cannot_chain_even_with_relayed_false() {
    for relayed in [false, true] {
        let fake = FakeRelay::spawn().await;
        let (core, hook) = boot_core(YAML).await;
        let _ = core;
        let (_a, b) = enroll_pair(&fake, hook).await;

        let mut s = attach(&b, "home-dev", relayed).await;
        // The refusal arrives as an error frame (or the stream ends) —
        // never a ready for the remote profile.
        let mut refused = false;
        for _ in 0..20 {
            let f = match next_frame(&mut s).await {
                Some(f) => f,
                None => {
                    refused = true;
                    break;
                }
            };
            let v: serde_json::Value = serde_json::from_str(&f).expect("JSON frame");
            if v["type"] == "ready" {
                panic!(
                    "chaining through the remote profile 'home-dev' (relayed:{relayed}) must be refused"
                );
            }
            if v["type"] == "error" {
                refused = true;
                break;
            }
        }
        assert!(
            refused,
            "the remote-profile attach (relayed:{relayed}) must be refused"
        );
    }
}

// ── §E.3: the Profiles op lists LOCAL profiles only ───────────────────────

/// `Profiles` → the local profile rows the web UI shows (`dev`, `alt`) —
/// NEVER the `remote:` profile (`home-dev`): exposing it would make this
/// host a chain hop, which E-CHAIN forbids.
#[tokio::test]
async fn profiles_op_lists_local_profiles_only() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let mut s = b
        .handle
        .open(id_a(), Op::Profiles)
        .await
        .expect("open Profiles");
    let (status, _ct, body) = op_reply(&mut s).await;
    assert_eq!(status, 200, "the profiles op is a success");
    let v: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
    let text = v.to_string();
    assert!(
        text.contains("dev"),
        "local profile 'dev' is listed: {text}"
    );
    assert!(
        text.contains("alt"),
        "local profile 'alt' is listed: {text}"
    );
    assert!(
        !text.contains("home-dev"),
        "the remote profile 'home-dev' must never be listed (E-CHAIN): {text}"
    );
}

// ── §E.2: List / Kill / Get / Upload through the hook ─────────────────────

/// `List` returns the same rows the local `/api/conversations` gives:
/// after a turn on `dev`, the list carries that conversation tagged `dev`.
#[tokio::test]
async fn list_op_returns_the_local_rows() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    // A real conversation: attach + one turn.
    let mut s = attach(&b, "dev", false).await;
    let convo = wait_ready(&mut s).await;
    s.send(StreamMsg::Frame(
        r#"{"type":"send_message","text":"hi"}"#.into(),
    ))
    .await
    .expect("send");
    for _ in 0..100 {
        let f = next_frame(&mut s).await.expect("session alive");
        if f.contains("MOCK_REPLY") {
            break;
        }
    }

    let mut l = b.handle.open(id_a(), Op::List).await.expect("open List");
    let (status, _ct, body) = op_reply(&mut l).await;
    assert_eq!(status, 200, "the list op is a success");
    let v: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
    let items = v["items"]
        .as_array()
        .expect("the list shape is {items:[…]}");
    let row = items
        .iter()
        .find(|i| i["id"] == convo)
        .unwrap_or_else(|| panic!("the new conversation {convo} must be in the list: {v}"));
    assert_eq!(
        row["profile"], "dev",
        "the row is tagged with its local profile"
    );
}

/// `Kill` stops the run, exactly like the local kill route: the list reply
/// (kill routes answer with the fresh list) shows the row no longer
/// `running`… and a kill of a remote row is refused as it is today.
#[tokio::test]
async fn kill_op_stops_the_run() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let mut s = attach(&b, "dev", false).await;
    let convo = wait_ready(&mut s).await;
    s.send(StreamMsg::Frame(
        r#"{"type":"send_message","text":"hi"}"#.into(),
    ))
    .await
    .expect("send");
    for _ in 0..100 {
        let f = next_frame(&mut s).await.expect("session alive");
        if f.contains("MOCK_REPLY") {
            break;
        }
    }

    let id = uuid::Uuid::parse_str(&convo).expect("convo uuid");
    let mut k = b
        .handle
        .open(id_a(), Op::Kill { id: *id.as_bytes() })
        .await
        .expect("open Kill");
    let (status, _ct, body) = op_reply(&mut k).await;
    assert_eq!(status, 200, "kill of a live local run succeeds");
    let v: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
    let items = v["items"]
        .as_array()
        .expect("kill answers with the fresh list");
    let row = items
        .iter()
        .find(|i| i["id"] == convo)
        .unwrap_or_else(|| panic!("the conversation {convo} must still be listed: {v}"));
    assert_eq!(row["running"], false, "the killed run is no longer running");
}

/// `Upload` then `Get(Upload)` round-trip the exact bytes — the same store
/// the HTTP routes use. An unknown image id is a 404 with no oracle.
#[tokio::test]
async fn upload_then_get_roundtrips_bytes_and_unknown_image_is_404() {
    let fake = FakeRelay::spawn().await;
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let mut s = attach(&b, "dev", false).await;
    let convo = wait_ready(&mut s).await;
    s.send(StreamMsg::Frame(
        r#"{"type":"send_message","text":"hi"}"#.into(),
    ))
    .await
    .expect("send");
    for _ in 0..100 {
        let f = next_frame(&mut s).await.expect("session alive");
        if f.contains("MOCK_REPLY") {
            break;
        }
    }

    let payload: Vec<u8> = (0..=255u8).cycle().take(300_000).collect();
    let mut u = upload(
        &b,
        &convo,
        "blob.bin",
        "application/octet-stream",
        payload.clone(),
    )
    .await;
    eprintln!("DBG upload sent");
    let (status, _ct, body) = op_reply(&mut u).await;
    eprintln!("DBG upload replied");
    assert_eq!(status, 200, "the upload succeeds");
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
    let (status, _ct, bytes) = op_reply(&mut g).await;
    assert_eq!(status, 200, "the upload GET succeeds");
    assert_eq!(bytes, payload, "the uploaded bytes come back bit-identical");

    // The no-existence-oracle 404: an image id that was never minted.
    let mut i = b
        .handle
        .open(
            id_a(),
            Op::Get(GetTarget::Image {
                id: "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899.png".into(),
            }),
        )
        .await
        .expect("open Get(Image)");
    let (status, _ct, _body) = op_reply(&mut i).await;
    assert_eq!(
        status, 404,
        "an unknown image id is a plain 404 (no oracle)"
    );
}

/// §E.4: CloudServe counts the upload bytes and `Reset(TOO_LARGE)`s as soon
/// as the total exceeds `general.uploads.max_file_mb` — it doesn't wait for
/// the router (with a 1 MB cap, 2 MB of chunks resets).
#[tokio::test]
async fn oversize_upload_resets_too_large_before_the_router() {
    let fake = FakeRelay::spawn().await;
    let yaml = yaml_small_uploads();
    let (core, hook) = boot_core(&yaml).await;
    let _ = core;
    let (_a, b) = enroll_pair(&fake, hook).await;

    let mut s = attach(&b, "dev", false).await;
    let convo = wait_ready(&mut s).await;
    s.send(StreamMsg::Frame(
        r#"{"type":"send_message","text":"hi"}"#.into(),
    ))
    .await
    .expect("send");
    for _ in 0..100 {
        let f = next_frame(&mut s).await.expect("session alive");
        if f.contains("MOCK_REPLY") {
            break;
        }
    }

    // 2 MB > the 1 MB cap: the reset must arrive before any Head.
    let big: Vec<u8> = vec![7u8; 2 * 1024 * 1024];
    let mut u = upload(&b, &convo, "big.bin", "application/octet-stream", big).await;
    let mut saw_head = false;
    loop {
        let msg = tokio::time::timeout(TIMEOUT, u.next())
            .await
            .expect("the answer arrives in time")
            .expect("the oversize upload stream stays open");
        match msg {
            StreamMsg::Reset(code) => {
                assert_eq!(
                    code,
                    cloud::constants::reset::TOO_LARGE,
                    "the oversize upload must reset with TOO_LARGE"
                );
                break;
            }
            StreamMsg::Head { .. } => saw_head = true,
            StreamMsg::Chunk(_) => {}
            other => panic!("unexpected message on the oversize upload: {other:?}"),
        }
    }
    assert!(
        !saw_head,
        "the size cap must fire BEFORE the router (no Head first)"
    );
}

// ── the hook is the InboundHook shape the actor calls ─────────────────────

/// `WebUi::cloud_inbound()` hands back the hook the boot wiring passes to
/// `cloud::spawn` (host edition: `Some`; the closure is `Fn + Send + Sync`).
#[tokio::test]
async fn cloud_inbound_hook_is_some_on_the_host_edition() {
    let (core, hook) = boot_core(YAML).await;
    let _ = core;
    let hook = hook.expect("the host edition builds the CloudServe inbound hook");
    let _shared: Arc<InboundHook> = Arc::new(hook);
}
