#![allow(dead_code)] // shared harness: each test binary uses a slice of it

//! WebSocket authentication, Hello verification, frame caps and socket caps
//! (design §9.2 `ws_auth.rs`, plus the global `socket_cap_returns_503`).
//! RED on purpose: the relay is a `todo!()` skeleton.

mod common;

use cloud::chain::ChainOp;
use cloud::codec::{FromRelay, ToRelay};
use cloud::constants::relay_error;
use tokio_tungstenite::tungstenite::Message;

use common::*;

#[tokio::test]
async fn ws_no_token_401() {
    let t = TestRelay::spawn().await;
    let r = t.ws_status(None).await;
    assert_eq!(
        r.status(),
        axum::http::StatusCode::UNAUTHORIZED,
        "no token → 401"
    );
}

#[tokio::test]
async fn ws_bad_token_401() {
    let t = TestRelay::spawn().await;
    let bad = b64u(&rand_bytes(32));
    let r = t.ws_status(Some(&bad)).await;
    assert_eq!(
        r.status(),
        axum::http::StatusCode::UNAUTHORIZED,
        "bad token → 401"
    );
}

#[tokio::test]
async fn pre_hello_socket_can_only_chain_fetch() {
    let t = TestRelay::spawn().await;
    let token = t.login("w1").await;
    let mut d = FakeDevice::new(1);
    d.connect(&t.ws_url(), &token).await; // Authed, pre-Hello

    // Everything except ChainFetch is NOT_PERMITTED, and the socket stays open.
    d.send(ToRelay::Pipe {
        to: d.id,
        bytes: vec![1],
    })
    .await;
    expect_error(&d.recv().await, relay_error::NOT_PERMITTED).await;
    d.send(ToRelay::Post {
        to: d.id,
        letter_id: letter_id(),
        sealed: vec![1],
    })
    .await;
    expect_error(&d.recv().await, relay_error::NOT_PERMITTED).await;
    d.send(ToRelay::Ack {
        letter_id: letter_id(),
    })
    .await;
    expect_error(&d.recv().await, relay_error::NOT_PERMITTED).await;
    d.send(ToRelay::ChainAppend { entry: vec![0] }).await;
    expect_error(&d.recv().await, relay_error::NOT_PERMITTED).await;
    d.send(ToRelay::EnrollGenesis { entry: vec![0] }).await;
    expect_error(&d.recv().await, relay_error::NOT_PERMITTED).await;

    // The socket survived: ChainFetch answers with a Chain (empty here).
    d.send(ToRelay::ChainFetch { from_seq: 0 }).await;
    match d.recv().await {
        FromRelay::Chain { entries } => assert!(entries.is_empty(), "empty chain"),
        other => panic!("expected Chain, got {other:?}"),
    }
}

#[tokio::test]
async fn hello_window_timeout_1008() {
    // R7: log in with real time, then pause the clock.
    let t = TestRelay::spawn().await;
    let token = t.login("w2").await;
    let mut d = FakeDevice::new(1);
    d.connect(&t.ws_url(), &token).await;
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(
        cloud::constants::HELLO_WINDOW + 1,
    ))
    .await;
    // No Hello within the window ⇒ close 1008 (design §4.2).
    d.expect_close(1008).await;
}

#[tokio::test]
async fn hello_bad_sig() {
    let t = TestRelay::spawn().await;
    let token = t.login("w3").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut d = FakeDevice::new(2);
    d.connect(&t.ws_url(), &token).await;
    d.hello_bad_sig().await;
    expect_error(&d.recv().await, relay_error::BAD_HELLO).await;
    d.expect_close(1008).await;
}

#[tokio::test]
async fn hello_unknown_device() {
    let t = TestRelay::spawn().await;
    let token = t.login("w4").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut stranger = FakeDevice::new(99);
    stranger.connect(&t.ws_url(), &token).await;
    stranger.hello(&t.origin()).await;
    expect_error(&stranger.recv().await, relay_error::BAD_HELLO).await;
    stranger.expect_close(1008).await;
}

#[tokio::test]
async fn hello_revoked_device() {
    let t = TestRelay::spawn().await;
    let token = t.login("w5").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut b = FakeDevice::new(2);
    let graw_hash = {
        // head after genesis is seq 0; re-fetch to learn its hash
        a.send(ToRelay::ChainFetch { from_seq: 0 }).await;
        match a.recv().await {
            FromRelay::Chain { entries } => {
                assert_eq!(entries.len(), 1);
                cloud::chain::entry_hash(&entries[0])
            }
            other => panic!("expected Chain, got {other:?}"),
        }
    };
    enroll_add(&t, &token, 1, graw_hash, &mut a, &mut b).await;
    // A revokes B.
    let head = {
        a.send(ToRelay::ChainFetch { from_seq: 0 }).await;
        match a.recv().await {
            FromRelay::Chain { entries } => {
                assert_eq!(entries.len(), 2);
                cloud::chain::entry_hash(&entries[1])
            }
            other => panic!("expected Chain, got {other:?}"),
        }
    };
    let r = entry(2, head, ChainOp::Revoke, None, Some(b.id), &a);
    a.send(ToRelay::ChainAppend { entry: r.encode() }).await;
    expect_chain_changed(&a.recv().await, 2).await;
    // The revoked socket is closed 1008 (design §4.4 step 5).
    b.expect_close(1008).await;
    // B reconnects and tries to Hello: it is revoked ⇒ BAD_HELLO.
    let mut b2 = FakeDevice::new(2);
    b2.connect(&t.ws_url(), &token).await;
    b2.hello(&t.origin()).await;
    expect_error(&b2.recv().await, relay_error::BAD_HELLO).await;
    b2.expect_close(1008).await;
}

#[tokio::test]
async fn hello_signed_for_other_origin() {
    let t = TestRelay::spawn().await;
    let token = t.login("w6").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut d = FakeDevice::new(2);
    d.connect(&t.ws_url(), &token).await;
    d.hello_with_origin("http://evil.example").await;
    expect_error(&d.recv().await, relay_error::BAD_HELLO).await;
    d.expect_close(1008).await;
}

#[tokio::test]
async fn newest_socket_wins() {
    let t = TestRelay::spawn().await;
    let token = t.login("w7").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut s1 = FakeDevice::new(1);
    connect_active(&t, &token, &mut s1, Some(0)).await;
    let mut s2 = FakeDevice::new(1);
    connect_active(&t, &token, &mut s2, Some(0)).await;
    // The older socket is replaced and closed 1008.
    s1.expect_close(1008).await;
    // The newer socket is alive: a ChainFetch gets an answer.
    s2.send(ToRelay::ChainFetch { from_seq: 0 }).await;
    match s2.recv().await {
        FromRelay::Chain { .. } => {}
        other => panic!("expected Chain, got {other:?}"),
    }
}

#[tokio::test]
async fn socket_cap_429_and_permit_released() {
    let t = TestRelay::spawn().await;
    let token = t.login("w8").await;
    let cap = peakbot_relay::limits::MAX_SOCKETS_PER_ACCOUNT;
    let mut open: Vec<FakeDevice> = Vec::new();
    for i in 0..cap as u64 {
        let mut d = FakeDevice::new(1000 + i);
        d.connect(&t.ws_url(), &token).await; // pre-Hello sockets count
        open.push(d);
    }
    // At the cap: 429 before the upgrade (design §6.5).
    let r = t.ws_status(Some(&token)).await;
    assert_eq!(
        r.status(),
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        "cap → 429"
    );
    // Release one permit: the next upgrade is allowed again (RAII drop).
    open.pop().expect("a socket to close");
    for _ in 0..200 {
        let r = t.ws_status(Some(&token)).await;
        if r.status() != axum::http::StatusCode::TOO_MANY_REQUESTS {
            break;
        }
        tokio::task::yield_now().await;
    }
    let r = t.ws_status(Some(&token)).await;
    assert_ne!(
        r.status(),
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        "permit released after close"
    );
}

#[tokio::test]
async fn frame_at_cap_accepted() {
    let t = TestRelay::spawn().await;
    let token = t.login("w9").await;
    let mut d = FakeDevice::new(1);
    d.connect(&t.ws_url(), &token).await;
    // Exactly 1 + MAX_ENVELOPE_PIPE bytes: the WS layer must accept the frame
    // (not 1009). Content decoding is a separate protocol concern.
    let at_cap = vec![0u8; 1 + cloud::constants::MAX_ENVELOPE_PIPE];
    d.send_raw(Message::Binary(at_cap.into())).await;
    let code = d.close_code().await;
    assert_ne!(code, Some(1009), "frame at the cap is not oversize");
}

#[tokio::test]
async fn oversize_frame_closed_1009() {
    let t = TestRelay::spawn().await;
    let token = t.login("w10").await;
    let mut d = FakeDevice::new(1);
    d.connect(&t.ws_url(), &token).await;
    let oversize = vec![0u8; 1 + cloud::constants::MAX_ENVELOPE_PIPE + 1];
    d.send_raw(Message::Binary(oversize.into())).await;
    d.expect_close(1009).await;
}

#[tokio::test]
async fn text_frame_closed_1002() {
    let t = TestRelay::spawn().await;
    let token = t.login("w11").await;
    let mut d = FakeDevice::new(1);
    d.connect(&t.ws_url(), &token).await;
    d.send_raw(Message::Text("not a pbc1 envelope".into()))
        .await;
    d.expect_close(1002).await;
}

#[tokio::test]
async fn too_many_pending_rejected() {
    let t = TestRelay::spawn().await;
    let token = t.login("w12").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    // Four pending devices fit (MAX_PENDING_DEVICES = 4); the fifth is refused.
    let mut pendings: Vec<FakeDevice> = Vec::new();
    for i in 2..=5 {
        let mut p = FakeDevice::new(i);
        p.connect(&t.ws_url(), &token).await;
        p.hello_pending(&t.origin()).await;
        expect_welcome(&p.recv().await, Some(0), true).await;
        pendings.push(p);
    }
    let mut fifth = FakeDevice::new(6);
    fifth.connect(&t.ws_url(), &token).await;
    fifth.hello_pending(&t.origin()).await;
    expect_error(&fifth.recv().await, relay_error::TOO_MANY_PENDING).await;
    fifth.expect_close(1008).await;
}

#[tokio::test]
async fn socket_cap_returns_503() {
    // Global cap (added beyond the design, §8 R4): at MAX_SOCKETS live /ws
    // sockets the upgrade is refused with HTTP 503. The cap is global while
    // `MAX_SOCKETS_PER_ACCOUNT` is per account, so fill it with many accounts
    // at the per-account cap: 128 × 32 = 4096, then one more.
    let t = TestRelay::spawn().await;
    let per_acct = peakbot_relay::limits::MAX_SOCKETS_PER_ACCOUNT;
    let accounts = peakbot_relay::limits::MAX_SOCKETS / per_acct;
    let mut open: Vec<FakeDevice> = Vec::with_capacity(peakbot_relay::limits::MAX_SOCKETS);
    for a in 0..accounts as u64 {
        let token = t.login(&format!("global-cap-{a}")).await;
        for i in 0..per_acct as u64 {
            let mut d = FakeDevice::new(100_000 + a * 100 + i);
            d.connect(&t.ws_url(), &token).await;
            open.push(d);
        }
    }
    let overflow = t.login("global-cap-overflow").await;
    let r = t.ws_status(Some(&overflow)).await;
    assert_eq!(
        r.status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "global cap → 503"
    );
}
