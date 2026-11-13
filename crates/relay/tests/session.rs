#![allow(dead_code)] // shared harness: each test binary uses a slice of it

//! Session revocation tests (design §9.2 `session.rs`, §6.1). RED on purpose:
//! the relay is a `todo!()` skeleton.

mod common;

use cloud::chain::ChainOp;
use cloud::codec::ToRelay;
use cloud::constants::relay_error;

use common::*;

#[tokio::test]
async fn logout_revokes_token() {
    let t = TestRelay::spawn().await;
    let token = t.login("s1").await;
    // The token authorizes /ws before the logout.
    let mut d = FakeDevice::new(1);
    d.connect(&t.ws_url(), &token).await;
    let r = t.logout(Some(&token)).await;
    assert_eq!(
        r.status(),
        axum::http::StatusCode::NO_CONTENT,
        "logout → 204"
    );
    // Afterwards the token is dead: /ws → 401.
    let r = t.ws_status(Some(&token)).await;
    assert_eq!(
        r.status(),
        axum::http::StatusCode::UNAUTHORIZED,
        "/ws → 401"
    );
}

#[tokio::test]
async fn logout_closes_live_sockets() {
    let t = TestRelay::spawn().await;
    let token = t.login("s2").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    t.logout(Some(&token)).await;
    a.expect_close(1008).await;
}

#[tokio::test]
async fn logout_blocks_pre_hello_escalation() {
    let t = TestRelay::spawn().await;
    let token = t.login("s3").await;
    let mut d = FakeDevice::new(1);
    d.connect(&t.ws_url(), &token).await; // upgraded, pre-Hello
    t.logout(Some(&token)).await;
    d.hello(&t.origin()).await;
    expect_error(&d.recv().await, relay_error::UNAUTHORIZED).await;
    d.expect_close(1008).await;
}

#[tokio::test]
async fn logout_unknown_token_204() {
    let t = TestRelay::spawn().await;
    // A well-formed (43-char b64url) but unknown token: idempotent 204, no oracle.
    let unknown = b64u(&rand_bytes(32));
    let r = t.logout(Some(&unknown)).await;
    assert_eq!(
        r.status(),
        axum::http::StatusCode::NO_CONTENT,
        "unknown token → 204"
    );
}

#[tokio::test]
async fn logout_without_bearer_401() {
    let t = TestRelay::spawn().await;
    let r = t.logout(None).await;
    assert_eq!(
        r.status(),
        axum::http::StatusCode::UNAUTHORIZED,
        "no bearer → 401"
    );
}

#[tokio::test]
async fn session_idle_expiry() {
    let t = TestRelay::spawn().await;
    let token = t.login("s4").await;
    // 14 d idle (sliding) + 1 s ⇒ the sweeper deletes the session (design §4.7).
    t.sweep_at(now() + peakbot_relay::limits::SESSION_TTL + 1)
        .await;
    let r = t.ws_status(Some(&token)).await;
    assert_eq!(
        r.status(),
        axum::http::StatusCode::UNAUTHORIZED,
        "expired → 401"
    );
}

#[tokio::test]
async fn append_last_device_self_revoke_rejected() {
    let t = TestRelay::spawn().await;
    let token = t.login("s5").await;
    let mut a = FakeDevice::new(1);
    a.connect(&t.ws_url(), &token).await;
    a.hello_pending(&t.origin()).await;
    expect_welcome(&a.recv().await, None, true).await;
    let g = entry(0, [0; 32], ChainOp::Genesis, Some(a.card.clone()), None, &a);
    let graw = g.encode();
    a.send(ToRelay::EnrollGenesis {
        entry: graw.clone(),
    })
    .await;
    expect_chain_changed(&a.recv().await, 0).await;

    // A is the only active device: spec C6 (`LastDevice`) rejects the self-
    // revoke ⇒ INVALID_ENTRY, nothing changes (design §6.1 point 3).
    let r = entry(
        1,
        cloud::chain::entry_hash(&graw),
        ChainOp::Revoke,
        None,
        Some(a.id),
        &a,
    );
    a.send(ToRelay::ChainAppend { entry: r.encode() }).await;
    expect_error(&a.recv().await, relay_error::INVALID_ENTRY).await;

    // The chain is unchanged (still one entry at seq 0)…
    a.send(ToRelay::ChainFetch { from_seq: 0 }).await;
    match a.recv().await {
        cloud::codec::FromRelay::Chain { entries } => {
            assert_eq!(entries.len(), 1, "chain unchanged")
        }
        other => panic!("expected Chain, got {other:?}"),
    }
    // …and the session is still live.
    let r = t.ws_status(Some(&token)).await;
    assert_ne!(
        r.status(),
        axum::http::StatusCode::UNAUTHORIZED,
        "session unchanged"
    );
}
