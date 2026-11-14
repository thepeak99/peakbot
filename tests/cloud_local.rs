//! P3 PR-C RED tests — the Local edition (design case 10, locked): the
//! controller path (`Client::Cloud`, `WsStream::Cloud`) COMPILES and is
//! live in the Local build, and the Local core has NO CloudServe: its
//! CloudLink is spawned with `inbound = None`, so a paired peer's inbound
//! attach is refused with `Reset(PROTOCOL)` — never served.
//!
//! A host core and a Local core can't share one test binary
//! (`feature = "host"` is per build), so this file gates itself and runs
//! against the fake relay (the peer is a full `CloudLink` device — the
//! relay harness's `FakeDevice` needs no session-layer Hs1 for the
//! refusal path, and keeps this file independent of the relay crate).

#![cfg(not(feature = "host"))]

mod cloud_common;

use cloud::frames::Op;
use cloud_common::devices::id_a;
use cloud_common::fake_relay::FakeRelay;
use cloud_common::rig::{self, Dev, TIMEOUT, spawn_device};
use futures::{SinkExt, StreamExt};
use peakbot::cloud::{CloudError, CloudState};
use peakbot::ui::web::remote_types::{Client, WsStream};

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

/// A (a Local-style core: `spawn(…, inbound = None)`) and B (controller),
/// both Active on one fake relay.
async fn enroll_pair(fake: &FakeRelay) -> (Dev, Dev) {
    let a = spawn_device(1, "Laptop", fake, None); // ← no CloudServe, B8
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

/// The controller path is compiled AND live in the Local edition:
/// `Client::Cloud{…}.dial()` exists here and dials a `WsStream::Cloud`.
/// RED today: the `Client::Cloud` dial arm is a `todo!()` skeleton.
#[tokio::test]
async fn local_edition_compiles_the_controller_path() {
    let fake = FakeRelay::spawn().await;
    let (_a, b) = enroll_pair(&fake).await;

    let client = Client::Cloud {
        handle: b.handle.clone(),
        device: id_a(),
        profile: "dev".to_string(),
    };
    let ws = client
        .dial()
        .await
        .expect("the Local core dials the tunnel");
    assert!(
        matches!(ws, WsStream::Cloud(_)),
        "the Local edition dials a WsStream::Cloud"
    );
}

/// The Local edition has no CloudServe: a paired peer's inbound attach is
/// refused with `Reset(PROTOCOL)` — the stream is never served.
#[tokio::test]
async fn local_core_refuses_an_inbound_attach_with_protocol() {
    let fake = FakeRelay::spawn().await;
    let (_a, b) = enroll_pair(&fake).await;

    const PROTOCOL: u16 = cloud::constants::reset::PROTOCOL;
    let res = b.handle.open(id_a(), Op::Attach).await;
    match res {
        // The open already observed the refusal.
        Err(CloudError::Reset(code)) => {
            assert_eq!(code, PROTOCOL, "the refusal is Reset(PROTOCOL)")
        }
        // The stream comes back and must die with Reset(PROTOCOL) —
        // never a served frame.
        Ok(mut s) => loop {
            let msg = tokio::time::timeout(TIMEOUT, s.next())
                .await
                .expect("the refusal arrives in time")
                .expect("the stream stays open until the reset");
            match msg {
                cloud::frames::StreamMsg::Reset(code) => {
                    assert_eq!(code, PROTOCOL, "the refusal is Reset(PROTOCOL)")
                }
                cloud::frames::StreamMsg::Frame(f) => {
                    panic!("the Local core must never serve an inbound attach, got frame: {f}")
                }
                other => panic!("expected the PROTOCOL reset, got {other:?}"),
            }
        },
        Err(e) => panic!("expected a PROTOCOL reset, got {e:?}"),
    }
}
