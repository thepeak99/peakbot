//! P3 PR-C RED tests — the controller side (p3-design §F), both editions:
//! `Client::Cloud` → `WsStream::Cloud` (relay.rs consumes it unchanged),
//! the `Upstream` cloud body, `Directory`/`RemoteName` (`profile@device`),
//! and the §F.7 `CloudError` → `RemoteError` mapping.
//!
//! RED today: the `Client::Cloud` / `WsStream::Cloud` / `Upstream::from_cloud`
//! / `RemoteName::parse` / `Directory::resolve` / `from_cloud` seams are
//! `todo!()` skeletons — every case fails on the panic (feature absent).

mod cloud_common;

use cloud::frames::{Op, StreamMsg};
use futures::{SinkExt, StreamExt};
use peakbot::cloud::stream::CloudStream;
use peakbot::cloud::{CloudError, CloudState, TrustError};
use peakbot::config::{Config, ParseCtx, Source};
use peakbot::ui::web::remote_types::{
    Client, Directory, GetTarget, RemoteError, RemoteName, Upstream, WsStream, from_cloud,
};
use tokio_tungstenite::tungstenite::Message as TMsg;

use cloud_common::devices::id_a;
use cloud_common::fake_relay::FakeRelay;
use cloud_common::rig::{self, Dev, TIMEOUT, spawn_device};

// ── one active pair over the fake relay ───────────────────────────────────

async fn login(dev: &Dev, fake: &FakeRelay) {
    let fk = fake.clone();
    dev.handle
        .login(move |url| {
            tokio::spawn(async move { rig::drive_login(&fk, url).await });
        })
        .await
        .expect("login");
    let _ = rig::wait_state(dev.handle.state(), |s| {
        matches!(s, CloudState::Pending { .. } | CloudState::Active { .. })
    })
    .await;
}

/// A (host) and B (controller), both Active on one fake relay.
async fn enroll_pair(fake: &FakeRelay) -> (Dev, Dev) {
    let a = spawn_device(1, "Laptop", fake, None);
    let b = spawn_device(2, "Desktop", fake, None);
    login(&a, fake).await;
    a.handle.genesis().await.expect("genesis");
    login(&b, fake).await;
    let st = rig::wait_state(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { requests, .. } if !requests.is_empty()),
    )
    .await;
    let letter = match st {
        CloudState::Active { requests, .. } => requests[0].letter_id,
        _ => panic!("A must see B's enroll request, got {st:?}"),
    };
    a.handle.approve(letter).await.expect("approve");
    let _ = rig::wait_state(b.handle.state(), |s| {
        matches!(s, CloudState::Active { devices, .. }
                if devices.iter().any(|d| d.id == id_a()))
    })
    .await;
    (a, b)
}

// ── §F.1/§F.2: Client::Cloud dials a WsStream::Cloud relay.rs can eat ────

/// `Client::Cloud{…}.dial()` → a `WsStream::Cloud` that the existing relay
/// code consumes UNCHANGED: the same `TMsg::Text` attach frame in, the same
/// `attached`/`ready` JSON frames out (the §F.2 `Frame` ↔ `Text` mapping).
#[tokio::test]
async fn client_cloud_dial_yields_a_ws_stream_the_relay_consumes() {
    let fake = FakeRelay::spawn().await;
    let (_a, b) = enroll_pair(&fake).await;

    let client = Client::Cloud {
        handle: b.handle.clone(),
        device: id_a(),
        profile: "dev".to_string(),
    };

    let mut ws = client.dial().await.expect("the cloud dial opens");
    assert!(
        matches!(ws, WsStream::Cloud(_)),
        "Client::Cloud dials a WsStream::Cloud"
    );

    // relay.rs consumes it unchanged: the plain attach protocol over TMsg.
    ws.send(TMsg::Text(
        r#"{"type":"attach","profile":"dev","agent":"team","relayed":true}"#.into(),
    ))
    .await
    .expect("relay.rs sends a plain Text frame");
    let mut ready = false;
    for _ in 0..20 {
        let msg = tokio::time::timeout(TIMEOUT, ws.next())
            .await
            .expect("frames arrive in time")
            .expect("the stream stays open")
            .expect("message ok");
        let TMsg::Text(t) = msg else {
            panic!("the attach protocol is text-only, got {msg:?}")
        };
        let v: serde_json::Value = serde_json::from_str(&t).expect("JSON frame");
        if v["type"] == "ready" {
            ready = true;
            break;
        }
    }
    assert!(ready, "the attach protocol completes over WsStream::Cloud");
}

// ── §F.3: the Upstream cloud body ─────────────────────────────────────────

/// `Client::Cloud.get(Upload)` → `Upstream` whose `bytes_stream()` yields
/// the uploaded bytes: Head first, then the Chunks, then end.
#[tokio::test]
async fn upstream_cloud_body_streams_the_uploaded_bytes() {
    let fake = FakeRelay::spawn().await;
    let (_a, b) = enroll_pair(&fake).await;

    let convo = attach_ready(&b).await;

    let payload: Vec<u8> = (0..=255u8).cycle().take(150_000).collect();
    let convo_bytes: [u8; 16] = *uuid::Uuid::parse_str(&convo).expect("uuid").as_bytes();
    let mut u = b
        .handle
        .open(
            id_a(),
            Op::Upload {
                convo: convo_bytes,
                name: "blob.bin".into(),
                content_type: "application/octet-stream".into(),
            },
        )
        .await
        .expect("open Upload");
    for chunk in payload.chunks(cloud::constants::MAX_CHUNK) {
        u.send(StreamMsg::Chunk(chunk.to_vec()))
            .await
            .expect("chunk");
    }
    u.send(StreamMsg::End).await.expect("End");
    let upload_id = read_upload_id(&mut u).await;

    let client = Client::Cloud {
        handle: b.handle.clone(),
        device: id_a(),
        profile: "dev".to_string(),
    };
    let up = client
        .get(GetTarget::Upload {
            convo: uuid::Uuid::parse_str(&convo).expect("uuid"),
            id: uuid::Uuid::parse_str(&upload_id).expect("upload id uuid"),
        })
        .await
        .expect("the cloud GET succeeds");
    let mut stream = up.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        bytes.extend(chunk.expect("the body streams clean"));
    }
    assert_eq!(
        bytes, payload,
        "the upstream body streams the uploaded bytes"
    );
}

/// Drive a fresh attach and return the conversation id (from `attached`).
async fn attach_ready(b: &Dev) -> String {
    let mut s = b
        .handle
        .open(id_a(), Op::Attach)
        .await
        .expect("open Attach");
    s.send(StreamMsg::Frame(
        r#"{"type":"attach","profile":"dev","agent":"team","relayed":true}"#.into(),
    ))
    .await
    .expect("attach frame");
    loop {
        let msg = tokio::time::timeout(TIMEOUT, s.next())
            .await
            .expect("in time")
            .expect("the stream stays open");
        if let StreamMsg::Frame(f) = msg {
            let v: serde_json::Value = serde_json::from_str(&f).expect("JSON");
            if v["type"] == "attached" {
                return v["convo"].as_str().expect("convo id").to_string();
            }
            if v["type"] == "ready" {
                panic!("ready before attached?");
            }
        }
    }
}

/// Read a request/response op body: `Head` → `Chunk`s → `End`; return the
/// upload id from the JSON body.
async fn read_upload_id(stream: &mut CloudStream) -> String {
    let mut body = Vec::new();
    loop {
        let msg = tokio::time::timeout(TIMEOUT, stream.next())
            .await
            .expect("in time")
            .expect("the stream stays open");
        match msg {
            StreamMsg::Head { status, .. } => {
                assert_eq!(status, 200, "the upload succeeded");
            }
            StreamMsg::Chunk(c) => body.extend(c),
            StreamMsg::End => break,
            other => panic!("unexpected: {other:?}"),
        }
    }
    let v: serde_json::Value = serde_json::from_slice(&body).expect("upload JSON");
    v["id"].as_str().expect("upload id").to_string()
}

/// A cloud body whose first message is NOT a `Head` is a protocol fault:
/// `bytes_stream()` errors (it never yields bytes, never hangs).
#[tokio::test]
async fn upstream_cloud_body_without_a_head_is_a_protocol_error() {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tx.send(StreamMsg::Chunk(vec![1, 2, 3]))
        .await
        .expect("seed a Chunk first");
    let up = Upstream::from_cloud(CloudStream::new(tx, rx));
    let mut stream = up.bytes_stream();
    let err = loop {
        match stream.next().await {
            Some(Ok(_)) => panic!("no bytes may flow before a Head"),
            Some(Err(e)) => break e,
            None => panic!("the stream ended without a Head — a protocol fault must be an error"),
        }
    };
    let _ = err;
}

// ── §F.5: RemoteName / Directory ──────────────────────────────────────────

/// Parse at the boundary: split on the LAST `@`; no `@` → `Profile`.
#[test]
fn remote_name_splits_on_the_last_at() {
    assert_eq!(RemoteName::parse("dev"), RemoteName::Profile("dev".into()));
    assert_eq!(
        RemoteName::parse("dev@laptop"),
        RemoteName::Cloud {
            profile: "dev".into(),
            device: "laptop".into()
        }
    );
    // Last `@` wins: the profile part is passed through as-is (the host
    // validates it against its local profiles; an unknown profile is the
    // host's normal attach error).
    assert_eq!(
        RemoteName::parse("a@b@c"),
        RemoteName::Cloud {
            profile: "a@b".into(),
            device: "c".into()
        }
    );
}

/// A `profile@device` can never collide with a config name: profile names
/// are `^[A-Za-z0-9_ .-]{1,64}$` — `@` is not among them, so the config
/// parser rejects `dev@x` as a profile name.
#[test]
fn an_at_sign_cannot_be_a_profile_name() {
    let yaml = "providers:\n  - name: p\n    type: mock\n    models:\n      - { name: m1, alias: a }\nprofiles:\n  dev@laptop:\n    agents:\n      a:\n        model: a\n";
    assert!(
        Config::parse(
            Source {
                path: std::path::Path::new("/global/config.yaml"),
                text: yaml,
            },
            None,
            &ParseCtx { known_skills: &[] },
        )
        .is_err(),
        "a profile name containing '@' must be a config parse error"
    );
}

/// `Directory::resolve` on a `Cloud` name: the state must be Active (the
/// verified chain is the single source of truth), the slug maps to a
/// DeviceId, and the result is a `Client::Cloud`. An unknown device is the
/// same generic 404 an unknown profile gets (`NotFound`).
#[tokio::test]
async fn directory_resolves_profile_at_device_via_the_verified_chain() {
    let fake = FakeRelay::spawn().await;
    let (_a, b) = enroll_pair(&fake).await;

    let dir = Directory::new().with_cloud(Some(b.handle.clone()));
    let client = dir
        .resolve(&RemoteName::parse("dev@laptop"))
        .await
        .expect("an active device resolves");
    match client {
        Client::Cloud {
            device, profile, ..
        } => {
            assert_eq!(device, id_a(), "the slug maps to the verified DeviceId");
            assert_eq!(profile, "dev", "the profile passes through as-is");
        }
        other => panic!("a cloud name resolves to a Cloud client, got {other:?}"),
    }

    let err = dir
        .resolve(&RemoteName::parse("dev@no-such-device"))
        .await
        .expect_err("an unknown device is a 404");
    assert!(
        matches!(err, RemoteError::NotFound { .. }),
        "unknown device → NotFound (the generic 404), got {err:?}"
    );
}

// ── §F.7: the CloudError → RemoteError mapping ────────────────────────────

/// The §F.7 table, one case per row: offline → `Down("device offline")`;
/// trust refusals → `Auth`/`Trust`; unknown device → `NotFound`; a state
/// without a chain → `Down("cloud not connected …")`.
#[test]
fn the_f7_error_mapping_table() {
    // offline (relay Unreachable / HANDSHAKE_TIMEOUT)
    let e = from_cloud(&CloudError::Offline, "dev@laptop");
    assert!(
        matches!(&e, RemoteError::Down { reason, .. } if reason == "device offline"),
        "Offline → Down('device offline'), got {e:?}"
    );

    // the peer closed the handshake with TRUST → Auth
    let e = from_cloud(&CloudError::Trust(TrustError::Revoked), "dev@laptop");
    assert!(
        matches!(&e, RemoteError::Auth { .. } | RemoteError::Trust(_)),
        "a TRUST refusal maps to Auth or Trust, got {e:?}"
    );

    // a chain fork / genesis mismatch → the new Trust variant
    let e = from_cloud(&CloudError::Trust(TrustError::Fork), "dev@laptop");
    assert!(
        matches!(&e, RemoteError::Trust(_)),
        "Fork → Trust(…), got {e:?}"
    );

    // not connected → Down
    let e = from_cloud(
        &CloudError::NotActive(peakbot::cloud::CloudStateKind::Disabled),
        "dev@laptop",
    );
    assert!(
        matches!(&e, RemoteError::Down { .. }),
        "NotActive(Disabled) → Down('cloud not connected …'), got {e:?}"
    );

    // unknown device → NotFound
    let name = "dev@nope";
    let e = from_cloud(&CloudError::UnknownDevice, name);
    assert!(
        matches!(&e, RemoteError::NotFound { name: n } if n == name),
        "UnknownDevice → NotFound{{name}}, got {e:?}"
    );
}
