#![allow(dead_code)] // shared harness: each test binary uses a slice of it

//! Device chain: genesis, append, CAS, revoke, fetch batching, restart
//! (design §9.2 `chain.rs`). RED on purpose.

mod common;

use cloud::chain::ChainOp;
use cloud::codec::{FromRelay, ToRelay};
use cloud::constants::relay_error;

use common::*;

#[tokio::test]
async fn genesis_on_empty_account_promotes_pending() {
    let t = TestRelay::spawn().await;
    let token = t.login("c1").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    // The pending socket is now Active: it can ChainAppend (Active only).
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
}

#[tokio::test]
async fn genesis_rejected_when_chain_exists() {
    let t = TestRelay::spawn().await;
    let token = t.login("c2").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut p = FakeDevice::new(2);
    p.connect(&t.ws_url(), &token).await;
    p.hello_pending(&t.origin()).await;
    expect_welcome(&p.recv().await, Some(0), true).await;
    let e = entry(0, [0; 32], ChainOp::Genesis, Some(p.card.clone()), None, &p);
    p.send(ToRelay::EnrollGenesis { entry: e.encode() }).await;
    expect_error(&p.recv().await, relay_error::CONFLICT).await;
}

#[tokio::test]
async fn genesis_card_must_match_hello_pending() {
    let t = TestRelay::spawn().await;
    let token = t.login("c3").await;
    let mut p = FakeDevice::new(1);
    p.connect(&t.ws_url(), &token).await;
    p.hello_pending(&t.origin()).await;
    expect_welcome(&p.recv().await, None, true).await;
    // Genesis entry carries a DIFFERENT card (not this socket's).
    let other = FakeDevice::new(9);
    let e = entry(
        0,
        [0; 32],
        ChainOp::Genesis,
        Some(other.card.clone()),
        None,
        &p,
    );
    p.send(ToRelay::EnrollGenesis { entry: e.encode() }).await;
    expect_error(&p.recv().await, relay_error::NOT_PERMITTED).await;
}

#[tokio::test]
async fn append_on_empty_account_conflict() {
    let t = TestRelay::spawn().await;
    let token = t.login("c4").await;
    let mut p = FakeDevice::new(1);
    p.connect(&t.ws_url(), &token).await;
    p.hello_pending(&t.origin()).await;
    expect_welcome(&p.recv().await, None, true).await;
    let e = entry(1, [0; 32], ChainOp::Add, Some(p.card.clone()), None, &p);
    p.send(ToRelay::ChainAppend { entry: e.encode() }).await;
    expect_error(&p.recv().await, relay_error::CONFLICT).await;
}

#[tokio::test]
async fn add_only_for_pending() {
    let t = TestRelay::spawn().await;
    let token = t.login("c5").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let g = fetch_chain(&mut a, 0).await;
    let head = cloud::chain::entry_hash(&g[0]);

    // No pending row for `stranger` → NOT_PERMITTED.
    let stranger = FakeDevice::new(3);
    let e = entry(1, head, ChainOp::Add, Some(stranger.card.clone()), None, &a);
    a.send(ToRelay::ChainAppend { entry: e.encode() }).await;
    expect_error(&a.recv().await, relay_error::NOT_PERMITTED).await;

    // With a pending row → ok; the row is gone and the socket is Active.
    let mut s = FakeDevice::new(3);
    s.connect(&t.ws_url(), &token).await;
    s.hello_pending(&t.origin()).await;
    expect_welcome(&s.recv().await, Some(0), true).await;
    let e = entry(1, head, ChainOp::Add, Some(s.card.clone()), None, &a);
    a.send(ToRelay::ChainAppend { entry: e.encode() }).await;
    expect_chain_changed(&a.recv().await, 1).await;
    expect_chain_changed(&s.recv().await, 1).await;
    // Active now: s can ChainAppend.
    let stranger2 = FakeDevice::new(4); // distinct from `s` (seed 3): re-adding s fails extend first (§4.4 step 3)
    let e2 = entry(
        2,
        cloud::chain::entry_hash(&e.encode()),
        ChainOp::Add,
        Some(stranger2.card.clone()),
        None,
        &s,
    );
    // (stranger still has no row: NOT_PERMITTED, but the socket role is proven
    // to be Active because ChainAppend is permitted from Active at all.)
    s.send(ToRelay::ChainAppend { entry: e2.encode() }).await;
    expect_error(&s.recv().await, relay_error::NOT_PERMITTED).await;
}

#[tokio::test]
async fn add_pending_of_other_account_not_permitted() {
    let t = TestRelay::spawn().await;
    let token_x = t.login("acct-cx").await;
    let token_y = t.login("acct-cy").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token_x, &mut a).await;
    let mut p = FakeDevice::new(2);
    p.connect(&t.ws_url(), &token_y).await; // pending under account Y
    p.hello_pending(&t.origin()).await;
    expect_welcome(&p.recv().await, None, true).await;
    let g = fetch_chain(&mut a, 0).await;
    let e = entry(
        1,
        cloud::chain::entry_hash(&g[0]),
        ChainOp::Add,
        Some(p.card.clone()),
        None,
        &a,
    );
    a.send(ToRelay::ChainAppend { entry: e.encode() }).await;
    expect_error(&a.recv().await, relay_error::NOT_PERMITTED).await;
}

#[tokio::test]
async fn add_after_pending_expired_not_permitted() {
    let t = TestRelay::spawn().await;
    let token = t.login("c6").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut p = FakeDevice::new(2);
    p.connect(&t.ws_url(), &token).await;
    p.hello_pending(&t.origin()).await;
    expect_welcome(&p.recv().await, Some(0), true).await;
    // Expire the pending row (and close its socket, design §4.7).
    t.sweep_at(now() + cloud::constants::TTL_LETTER + 1).await;
    p.expect_close(1008).await;
    let g = fetch_chain(&mut a, 0).await;
    let e = entry(
        1,
        cloud::chain::entry_hash(&g[0]),
        ChainOp::Add,
        Some(p.card.clone()),
        None,
        &a,
    );
    a.send(ToRelay::ChainAppend { entry: e.encode() }).await;
    expect_error(&a.recv().await, relay_error::NOT_PERMITTED).await;
}

async fn base_chain(t: &TestRelay, token: &str) -> (FakeDevice, [u8; 32]) {
    let mut a = FakeDevice::new(1);
    enroll_genesis(t, token, &mut a).await;
    let g = fetch_chain(&mut a, 0).await;
    (a, cloud::chain::entry_hash(&g[0]))
}

#[tokio::test]
async fn append_stale_seq_conflict() {
    let t = TestRelay::spawn().await;
    let token = t.login("c7").await;
    let (mut a, _head) = base_chain(&t, &token).await;
    // seq 0 is stale (head is 0, next must be 1).
    let p = FakeDevice::new(2);
    let e = entry(0, [0; 32], ChainOp::Add, Some(p.card.clone()), None, &a);
    a.send(ToRelay::ChainAppend { entry: e.encode() }).await;
    expect_error(&a.recv().await, relay_error::CONFLICT).await;
}

#[tokio::test]
async fn append_future_seq_conflict() {
    let t = TestRelay::spawn().await;
    let token = t.login("c8").await;
    let (mut a, head) = base_chain(&t, &token).await;
    // seq 2 is in the future (next must be 1).
    let p = FakeDevice::new(2);
    let e = entry(2, head, ChainOp::Add, Some(p.card.clone()), None, &a);
    a.send(ToRelay::ChainAppend { entry: e.encode() }).await;
    expect_error(&a.recv().await, relay_error::CONFLICT).await;
}

#[tokio::test]
async fn append_fork_rejected() {
    let t = TestRelay::spawn().await;
    let token = t.login("c9").await;
    let (mut a, _head) = base_chain(&t, &token).await;
    // Right seq (1), wrong `prev` → fork → INVALID_ENTRY (design §6.3).
    let p = FakeDevice::new(2);
    let e = entry(1, [0xab; 32], ChainOp::Add, Some(p.card.clone()), None, &a);
    a.send(ToRelay::ChainAppend { entry: e.encode() }).await;
    expect_error(&a.recv().await, relay_error::INVALID_ENTRY).await;
}

#[tokio::test]
async fn concurrent_append_exactly_one_wins() {
    let t = TestRelay::spawn().await;
    let token = t.login("c10").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut b = FakeDevice::new(2);
    let g = fetch_chain(&mut a, 0).await;
    let head0 = cloud::chain::entry_hash(&g[0]);
    enroll_add(&t, &token, 1, head0, &mut a, &mut b).await;

    // Two active sockets append seq 2 simultaneously. Each Add needs a live
    // pending row (design §4.4 step 3), so both targets connect pending first.
    let mut p1 = FakeDevice::new(3);
    let mut p2 = FakeDevice::new(4);
    for p in [&mut p1, &mut p2] {
        p.connect(&t.ws_url(), &token).await;
        p.hello_pending(&t.origin()).await;
        expect_welcome(&p.recv().await, Some(1), true).await;
    }
    let g2 = fetch_chain(&mut a, 0).await;
    let head1 = cloud::chain::entry_hash(g2.last().unwrap());
    let e1 = entry(2, head1, ChainOp::Add, Some(p1.card.clone()), None, &a);
    let e2 = entry(2, head1, ChainOp::Add, Some(p2.card.clone()), None, &b);
    let raw1 = e1.encode();
    let raw2 = e2.encode();
    tokio::join!(
        a.send(ToRelay::ChainAppend {
            entry: raw1.clone()
        }),
        b.send(ToRelay::ChainAppend {
            entry: raw2.clone()
        }),
    );
    // Design §4.4 step 5: the loser also gets the winner's ChainChanged{2}, and
    // its order against its CONFLICT is racy. Drain each socket up to a
    // ChainFetch sentinel and count (no sleeps).
    let (mut conflicts, mut changed) = (0, 0);
    for d in [&mut a, &mut b] {
        d.send(ToRelay::ChainFetch { from_seq: 0 }).await;
        loop {
            match d.recv().await {
                FromRelay::Chain { entries } => {
                    assert_eq!(entries.len(), 3, "one row at seq 2");
                    break;
                }
                FromRelay::ChainChanged { head_seq } => {
                    assert_eq!(head_seq, 2);
                    changed += 1;
                }
                FromRelay::Error { code } => {
                    assert_eq!(code, relay_error::CONFLICT, "loser gets CONFLICT");
                    conflicts += 1;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }
    assert_eq!(conflicts, 1, "exactly one append loses with CONFLICT");
    assert_eq!(
        changed, 2,
        "both sockets see ChainChanged{{2}} exactly once"
    );
}

#[tokio::test]
async fn revoke_closes_socket_deletes_letters_broadcasts() {
    let mut t = TestRelay::spawn().await;
    let token = t.login("c11").await;
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
    let mut p = FakeDevice::new(3);
    p.connect(&t.ws_url(), &token).await;
    p.hello_pending(&t.origin()).await;
    expect_welcome(&p.recv().await, Some(1), true).await;

    // A letter addressed to B (pending→active; active_id = B).
    let lid = post_to(&mut p, b.id, vec![1, 2, 3]).await;
    expect_letter(&b.recv().await, p.id, lid, &[1, 2, 3]);

    // A revokes B (seq 2).
    let all = fetch_chain(&mut a, 0).await;
    let head = cloud::chain::entry_hash(all.last().unwrap());
    let r = entry(2, head, ChainOp::Revoke, None, Some(b.id), &a);
    a.send(ToRelay::ChainAppend { entry: r.encode() }).await;

    b.expect_close(1008).await;
    // Every other socket (A and pending P) gets ChainChanged{head}.
    expect_chain_changed(&a.recv().await, 2).await;
    expect_chain_changed(&p.recv().await, 2).await;

    // B's re-Hello → BAD_HELLO (revoked).
    let mut b2 = FakeDevice::new(2);
    b2.connect(&t.ws_url(), &token).await;
    b2.hello(&t.origin()).await;
    expect_error(&b2.recv().await, relay_error::BAD_HELLO).await;
    b2.expect_close(1008).await;

    // DB check: letters with active_id = B are gone (design §6.4).
    t.shutdown_all().await;
    let conn = rusqlite::Connection::open(t.db_path()).expect("open db");
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM letters WHERE active_id = ?1",
            rusqlite::params![b.id],
            |row| row.get(0),
        )
        .expect("count letters");
    assert_eq!(n, 0, "letters with active_id = B deleted on revoke");
}

#[tokio::test]
async fn chain_fetch_batches_and_caught_up() {
    let t = TestRelay::spawn().await;
    let token = t.login("c12").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let g = fetch_chain(&mut a, 0).await;
    // 256 more entries ⇒ 257 total.
    let (_devs, _head) =
        add_devices(&t, &token, 1, cloud::chain::entry_hash(&g[0]), &mut a, 256).await;
    let batch = fetch_chain(&mut a, 0).await;
    assert_eq!(batch.len(), 256, "first batch is 256");
    let rest = fetch_chain(&mut a, 256).await;
    assert_eq!(rest.len(), 1, "second batch is the 257th entry");
    let caught = fetch_chain(&mut a, 257).await;
    assert!(caught.is_empty(), "caught up ⇒ empty batch");

    // 256-entry chain: 256 then empty.
    let token2 = t.login("c12b").await;
    let mut c = FakeDevice::new(50);
    enroll_genesis(&t, &token2, &mut c).await;
    let g2 = fetch_chain(&mut c, 0).await;
    let (_devs2, _head2) = add_devices(
        &t,
        &token2,
        1,
        cloud::chain::entry_hash(&g2[0]),
        &mut c,
        255,
    )
    .await;
    let batch = fetch_chain(&mut c, 0).await;
    assert_eq!(batch.len(), 256);
    let caught = fetch_chain(&mut c, 256).await;
    assert!(caught.is_empty());
}

#[tokio::test]
async fn chain_survives_restart() {
    let mut t = TestRelay::spawn().await;
    let token = t.login("c13").await;
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

    t.restart().await;
    let mut a2 = FakeDevice::new(1);
    connect_active(&t, &token, &mut a2, Some(1)).await;
    let all = fetch_chain(&mut a2, 0).await;
    assert_eq!(all.len(), 2, "chain survives restart");
}
