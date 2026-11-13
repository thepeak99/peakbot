#![allow(dead_code)] // shared harness: each test binary uses a slice of it

//! Enrollment letters: delivery, caps, expiry, acks, rate limits
//! (design §9.2 `mailbox.rs`). RED on purpose.

mod common;

use cloud::codec::ToRelay;
use cloud::constants::relay_error;

use common::*;

/// A: genesis (active). P: pending. Both sockets open.
async fn active_and_pending(t: &TestRelay, token: &str) -> (FakeDevice, FakeDevice) {
    let mut a = FakeDevice::new(1);
    enroll_genesis(t, token, &mut a).await;
    let mut p = FakeDevice::new(2);
    p.connect(&t.ws_url(), token).await;
    p.hello_pending(t.origin()).await;
    expect_welcome(&p.recv().await, Some(0), true).await;
    (a, p)
}

#[tokio::test]
async fn letter_pushed_live_and_replayed_until_ack() {
    let t = TestRelay::spawn().await;
    let token = t.login("m1").await;
    let (mut a, mut p) = active_and_pending(&t, &token).await;
    let sealed: Vec<u8> = vec![7; 64];
    let lid = post_to(&mut a, p.id, sealed.clone()).await;
    // Live push.
    expect_letter(&p.recv().await, a.id, lid, &sealed);
    // No ack: disconnect + reconnect ⇒ replayed.
    p.go_offline().await;
    let mut p2 = FakeDevice::new(2);
    p2.connect(&t.ws_url(), &token).await;
    p2.hello_pending(&t.origin()).await;
    expect_welcome(&p2.recv().await, Some(0), true).await;
    expect_letter(&p2.recv().await, a.id, lid, &sealed);
    // Ack ⇒ gone.
    p2.send(ToRelay::Ack { letter_id: lid }).await;
    p2.go_offline().await;
    let mut p3 = FakeDevice::new(2);
    p3.connect(&t.ws_url(), &token).await;
    p3.hello_pending(&t.origin()).await;
    expect_welcome(&p3.recv().await, Some(0), true).await;
    // Sentinel: no letter; the next frame is the ChainFetch answer.
    let _ = fetch_chain(&mut p3, 0).await;
}

#[tokio::test]
async fn letter_delivered_on_connect() {
    let t = TestRelay::spawn().await;
    let token = t.login("m2").await;
    let (mut a, mut p) = active_and_pending(&t, &token).await;
    let sealed: Vec<u8> = vec![9; 32];
    let lid = post_to(&mut a, p.id, sealed.clone()).await;
    expect_letter(&p.recv().await, a.id, lid, &sealed);
    // Recipient offline: the letter stays stored and is delivered on connect.
    p.go_offline().await;
    let mut p2 = FakeDevice::new(2);
    p2.connect(&t.ws_url(), &token).await;
    p2.hello_pending(&t.origin()).await;
    expect_welcome(&p2.recv().await, Some(0), true).await;
    expect_letter(&p2.recv().await, a.id, lid, &sealed);
}

#[tokio::test]
async fn mailbox_full_then_ack_frees_slot() {
    let t = TestRelay::spawn().await;
    let token = t.login("m3").await;
    let (mut a, mut p) = active_and_pending(&t, &token).await;
    let cap = cloud::constants::MAX_PENDING_LETTERS;
    let mut lids = Vec::new();
    for i in 0..cap {
        let lid = post_to(&mut a, p.id, vec![i as u8; 16]).await;
        expect_letter(&p.recv().await, a.id, lid, &[i as u8; 16]);
        lids.push(lid);
    }
    // 9th → MAILBOX_FULL.
    let lid = post_to(&mut a, p.id, vec![250; 16]).await;
    expect_error(&a.recv().await, relay_error::MAILBOX_FULL).await;
    // Ack frees a slot.
    p.send(ToRelay::Ack { letter_id: lids[0] }).await;
    let lid2 = post_to(&mut a, p.id, vec![251; 16]).await;
    expect_letter(&p.recv().await, a.id, lid2, &[251; 16]);
    let _ = lid;
}

#[tokio::test]
async fn letter_8192_ok_8193_too_large() {
    let t = TestRelay::spawn().await;
    let token = t.login("m4").await;
    let (mut a, mut p) = active_and_pending(&t, &token).await;
    let ok: Vec<u8> = vec![1; cloud::constants::MAX_LETTER];
    let lid = post_to(&mut a, p.id, ok.clone()).await;
    expect_letter(&p.recv().await, a.id, lid, &ok);
    let big: Vec<u8> = vec![2; cloud::constants::MAX_LETTER + 1];
    post_to(&mut a, p.id, big).await;
    expect_error(&a.recv().await, relay_error::TOO_LARGE).await;
}

#[tokio::test]
async fn letter_expiry_purges_and_closes_pending() {
    let t = TestRelay::spawn().await;
    let token = t.login("m5").await;
    let (mut a, mut p) = active_and_pending(&t, &token).await;
    let sealed: Vec<u8> = vec![3; 16];
    let lid = post_to(&mut a, p.id, sealed.clone()).await;
    expect_letter(&p.recv().await, a.id, lid, &sealed);
    // Expire the pending device (letters cascade away, design §4.7/§6.4).
    t.sweep_at(now() + cloud::constants::TTL_LETTER + 1).await;
    p.expect_close(1008).await;
    // Reconnect: fresh pending row, no replay.
    let mut p2 = FakeDevice::new(2);
    p2.connect(&t.ws_url(), &token).await;
    p2.hello_pending(&t.origin()).await;
    expect_welcome(&p2.recv().await, Some(0), true).await;
    let _ = fetch_chain(&mut p2, 0).await; // sentinel: no letter before it
}

#[tokio::test]
async fn pending_reconnect_keeps_letters_and_expiry() {
    // §4.3 P2-edit regression: a reconnect keeps the pending row, its letters
    // and its absolute expiry (no INSERT OR REPLACE).
    let t = TestRelay::spawn().await;
    let token = t.login("m6").await;
    let (mut a, mut p) = active_and_pending(&t, &token).await;
    let sealed: Vec<u8> = vec![4; 16];
    let lid = post_to(&mut a, p.id, sealed.clone()).await;
    expect_letter(&p.recv().await, a.id, lid, &sealed);
    p.go_offline().await;
    // Reconnect: the row (and letter) survive.
    let mut p2 = FakeDevice::new(2);
    p2.connect(&t.ws_url(), &token).await;
    p2.hello_pending(&t.origin()).await;
    expect_welcome(&p2.recv().await, Some(0), true).await;
    expect_letter(&p2.recv().await, a.id, lid, &sealed);
    // The original expiry still applies: one sweep past it closes the socket.
    t.sweep_at(now() + cloud::constants::TTL_LETTER + 1).await;
    p2.expect_close(1008).await;
}

#[tokio::test]
async fn post_rate_limited_31st() {
    let t = TestRelay::spawn().await;
    let token = t.login("m7").await;
    let (mut a, mut p) = active_and_pending(&t, &token).await;
    for i in 0..peakbot_relay::limits::POSTS_PER_MINUTE {
        let lid = post_to(&mut a, p.id, vec![i as u8; 8]).await;
        expect_letter(&p.recv().await, a.id, lid, &[i as u8; 8]);
        // Ack each letter: MAILBOX_FULL (8 per recipient, §6.4) would otherwise fire before the 31st-post rate limit.
        p.send(ToRelay::Ack { letter_id: lid }).await;
    }
    post_to(&mut a, p.id, vec![0xff; 8]).await;
    expect_error(&a.recv().await, relay_error::RATE_LIMITED).await;
}

#[tokio::test]
async fn active_to_active_post_not_permitted() {
    let t = TestRelay::spawn().await;
    let token = t.login("m8").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut b = FakeDevice::new(2);
    let g = fetch_chain(&mut a, 0).await;
    enroll_add(
        &t,
        &token,
        1,
        cloud::chain::entry_hash(&g[0]),
        &mut a,
        &mut b,
    )
    .await;
    post_to(&mut a, b.id, vec![1]).await;
    expect_error(&a.recv().await, relay_error::NOT_PERMITTED).await;
}

#[tokio::test]
async fn pending_to_pending_post_not_permitted() {
    let t = TestRelay::spawn().await;
    let token = t.login("m9").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut p1 = FakeDevice::new(2);
    p1.connect(&t.ws_url(), &token).await;
    p1.hello_pending(&t.origin()).await;
    expect_welcome(&p1.recv().await, Some(0), true).await;
    let mut p2 = FakeDevice::new(3);
    p2.connect(&t.ws_url(), &token).await;
    p2.hello_pending(&t.origin()).await;
    expect_welcome(&p2.recv().await, Some(0), true).await;
    post_to(&mut p1, p2.id, vec![1]).await;
    expect_error(&p1.recv().await, relay_error::NOT_PERMITTED).await;
}

#[tokio::test]
async fn post_cross_account_not_permitted() {
    let t = TestRelay::spawn().await;
    let token_x = t.login("acct-mx").await;
    let token_y = t.login("acct-my").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token_x, &mut a).await;
    let mut p = FakeDevice::new(2);
    p.connect(&t.ws_url(), &token_y).await;
    p.hello_pending(&t.origin()).await;
    expect_welcome(&p.recv().await, None, true).await;
    post_to(&mut a, p.id, vec![1]).await;
    expect_error(&a.recv().await, relay_error::NOT_PERMITTED).await;
}

#[tokio::test]
async fn ack_by_non_recipient_ignored() {
    let t = TestRelay::spawn().await;
    let token = t.login("m10").await;
    let (mut a, mut p) = active_and_pending(&t, &token).await;
    let sealed: Vec<u8> = vec![5; 16];
    let lid = post_to(&mut a, p.id, sealed.clone()).await;
    expect_letter(&p.recv().await, a.id, lid, &sealed);
    // A (the sender) acks: not the recipient ⇒ ignored, letter stays.
    a.send(ToRelay::Ack { letter_id: lid }).await;
    p.go_offline().await;
    let mut p2 = FakeDevice::new(2);
    p2.connect(&t.ws_url(), &token).await;
    p2.hello_pending(&t.origin()).await;
    expect_welcome(&p2.recv().await, Some(0), true).await;
    expect_letter(&p2.recv().await, a.id, lid, &sealed);
}

#[tokio::test]
async fn duplicate_letter_id_not_permitted() {
    let t = TestRelay::spawn().await;
    let token = t.login("m11").await;
    let (mut a, mut p) = active_and_pending(&t, &token).await;
    let lid = letter_id();
    a.send(ToRelay::Post {
        to: p.id,
        letter_id: lid,
        sealed: vec![1],
    })
    .await;
    expect_letter(&p.recv().await, a.id, lid, &[1]);
    a.send(ToRelay::Post {
        to: p.id,
        letter_id: lid,
        sealed: vec![2],
    })
    .await;
    expect_error(&a.recv().await, relay_error::NOT_PERMITTED).await;
}

#[tokio::test]
async fn expired_letter_not_delivered_before_purge() {
    // Store-level semantics (design §6.4, task 4): a letter whose pending row
    // expired is never delivered, even before the physical purge runs.
    let t = TestRelay::spawn().await;
    let token = t.login("m12").await;
    let (mut a, mut p) = active_and_pending(&t, &token).await;
    let sealed: Vec<u8> = vec![6; 16];
    let lid = post_to(&mut a, p.id, sealed.clone()).await;
    expect_letter(&p.recv().await, a.id, lid, &sealed);
    t.sweep_at(now() + cloud::constants::TTL_LETTER + 1).await;
    p.expect_close(1008).await;
    // Reconnect (new pending row): the old letter must not be replayed.
    let mut p2 = FakeDevice::new(2);
    p2.connect(&t.ws_url(), &token).await;
    p2.hello_pending(&t.origin()).await;
    expect_welcome(&p2.recv().await, Some(0), true).await;
    let _ = fetch_chain(&mut p2, 0).await; // sentinel: no letter
}
