#![allow(dead_code)] // shared harness: each test binary uses a slice of it

//! Pipe routing and isolation (design §9.2 `routing.rs`). RED on purpose.

mod common;

use cloud::codec::{FromRelay, ToRelay};
use cloud::constants::relay_error;

use common::*;

#[tokio::test]
async fn pipe_active_to_active() {
    let t = TestRelay::spawn().await;
    let token = t.login("r1").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut b = FakeDevice::new(2);
    let g = fetch_chain(&mut a, 0).await;
    let head = cloud::chain::entry_hash(&g[0]);
    enroll_add(&t, &token, 1, head, &mut a, &mut b).await;

    let payload: Vec<u8> = (0u8..=255).cycle().take(1000).collect();
    a.send(ToRelay::Pipe {
        to: b.id,
        bytes: payload.clone(),
    })
    .await;
    // Bytes identical, `from` = the sender's Hello id (design §4.5).
    expect_pipe(&b.recv().await, a.id, &payload);
}

#[tokio::test]
async fn pipe_order_preserved() {
    let t = TestRelay::spawn().await;
    let token = t.login("r2").await;
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

    for i in 0..100u8 {
        a.send(ToRelay::Pipe {
            to: b.id,
            bytes: vec![i],
        })
        .await;
    }
    for i in 0..100u8 {
        expect_pipe(&b.recv().await, a.id, &[i]);
    }
}

#[tokio::test]
async fn routing_isolation_between_accounts() {
    let t = TestRelay::spawn().await;
    let token_x = t.login("acct-x").await;
    let token_y = t.login("acct-y").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token_x, &mut a).await;
    let mut c = FakeDevice::new(11); // distinct id: one DeviceId on two accounts is newest-wins (§4.3)
    enroll_genesis(&t, &token_y, &mut c).await;

    // Pipe/Post to another account's device id → NOT_PERMITTED (design §4.5).
    a.send(ToRelay::Pipe {
        to: c.id,
        bytes: vec![1],
    })
    .await;
    expect_error(&a.recv().await, relay_error::NOT_PERMITTED).await;
    a.send(ToRelay::Post {
        to: c.id,
        letter_id: letter_id(),
        sealed: vec![1],
    })
    .await;
    expect_error(&a.recv().await, relay_error::NOT_PERMITTED).await;

    // C receives nothing (sentinel: a ChainFetch answer is the next frame).
    let _ = fetch_chain(&mut c, 0).await;
    assert_eq!(
        fetch_chain(&mut c, 0).await.len(),
        1,
        "C sees only its own chain"
    );

    // ChainChanged never crosses accounts: X appends an entry, C must not see it.
    let mut e = FakeDevice::new(2);
    let gx = fetch_chain(&mut a, 0).await;
    enroll_add(
        &t,
        &token_x,
        1,
        cloud::chain::entry_hash(&gx[0]),
        &mut a,
        &mut e,
    )
    .await;
    let _ = fetch_chain(&mut c, 0).await;
    assert_eq!(fetch_chain(&mut c, 0).await.len(), 1, "C's chain untouched");
}

#[tokio::test]
async fn pending_cannot_pipe() {
    let t = TestRelay::spawn().await;
    let token = t.login("r3").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut p = FakeDevice::new(2);
    p.connect(&t.ws_url(), &token).await;
    p.hello_pending(&t.origin()).await;
    expect_welcome(&p.recv().await, Some(0), true).await;
    p.send(ToRelay::Pipe {
        to: a.id,
        bytes: vec![1],
    })
    .await;
    expect_error(&p.recv().await, relay_error::NOT_PERMITTED).await;
}

#[tokio::test]
async fn pipe_to_pending_not_permitted() {
    let t = TestRelay::spawn().await;
    let token = t.login("r4").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut p = FakeDevice::new(2);
    p.connect(&t.ws_url(), &token).await;
    p.hello_pending(&t.origin()).await;
    expect_welcome(&p.recv().await, Some(0), true).await;
    a.send(ToRelay::Pipe {
        to: p.id,
        bytes: vec![1],
    })
    .await;
    expect_error(&a.recv().await, relay_error::NOT_PERMITTED).await;
}

#[tokio::test]
async fn pipe_to_self_not_permitted() {
    let t = TestRelay::spawn().await;
    let token = t.login("r5").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    a.send(ToRelay::Pipe {
        to: a.id,
        bytes: vec![1],
    })
    .await;
    expect_error(&a.recv().await, relay_error::NOT_PERMITTED).await;
}

#[tokio::test]
async fn pipe_unreachable_when_target_offline() {
    let t = TestRelay::spawn().await;
    let token = t.login("r6").await;
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
    b.go_offline().await; // B goes offline

    a.send(ToRelay::Pipe {
        to: b.id,
        bytes: vec![1],
    })
    .await;
    expect_unreachable(&a.recv().await, b.id).await;
    // Nothing was stored (no letters involved, design §4.5).
}

#[tokio::test]
async fn slow_consumer_closed() {
    let t = TestRelay::spawn().await;
    let token = t.login("r7").await;
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

    b.stall(); // B stops reading
    let frame: Vec<u8> = vec![0u8; 1_048_576]; // ~1 MiB payload
    // Live Pipes are silent (design §4.5): send the whole budget (≤ 200
    // frames), then drain A until a ChainFetch sentinel. B's full outbox must
    // have closed B (1013/EOF) and at least one Pipe must read Unreachable{B}.
    for _ in 0..200 {
        a.send(ToRelay::Pipe {
            to: b.id,
            bytes: frame.clone(),
        })
        .await;
    }
    a.send(ToRelay::ChainFetch { from_seq: 0 }).await;
    let mut unreachable_seen = false;
    loop {
        match a.recv().await {
            FromRelay::Chain { .. } => break,
            FromRelay::Unreachable { to } if to == b.id => unreachable_seen = true,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(
        unreachable_seen,
        "Unreachable for the stalled target within 200 iterations"
    );

    // B saw close 1013 (slow consumer) or EOF.
    let code = b.close_code().await;
    assert!(
        code == Some(1013) || code.is_none(),
        "B closed 1013 or EOF, got {code:?}"
    );

    // A's socket is unaffected.
    let _ = fetch_chain(&mut a, 0).await;
}
