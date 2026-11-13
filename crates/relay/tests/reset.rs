//! `reset-account` (design §9.2 `reset.rs`, §4.8). RED on purpose.

mod common;

use cloud::chain::ChainOp;
use cloud::codec::ToRelay;
use peakbot_relay::store::ResetError;

use common::*;

#[tokio::test]
async fn reset_account_refused_while_running() {
    let t = TestRelay::spawn().await;
    let _token = t.login("reset-1").await; // account exists
    let r = peakbot_relay::reset_account(t.db_path(), &t.idp.url, "reset-1");
    assert!(
        matches!(r, Err(ResetError::Locked)),
        "running relay ⇒ Locked, got {r:?}"
    );
}

#[tokio::test]
async fn reset_account_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("relay.db");
    let r = peakbot_relay::reset_account(&db, "http://issuer.example", "ghost-sub");
    assert!(
        matches!(r, Err(ResetError::NoSuchAccount)),
        "no such account, got {r:?}"
    );
}

#[tokio::test]
async fn reset_account_clears_chain_pending_letters_sessions() {
    let mut t = TestRelay::spawn().await;
    let sub = "reset-2";
    let token = t.login(sub).await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut b = FakeDevice::new(2);
    let g = fetch_chain(&mut a, 0).await;
    enroll_add(&t, &token, 1, cloud::chain::entry_hash(&g[0]), &mut a, &mut b).await;
    let mut p = FakeDevice::new(3);
    p.connect(&t.ws_url(), &token).await;
    p.hello_pending(&t.origin()).await;
    expect_welcome(&p.recv().await, Some(1), true).await;
    let sealed: Vec<u8> = vec![1, 2, 3];
    post_to(&mut a, p.id, sealed.clone()).await;
    match p.recv().await {
        cloud::codec::FromRelay::Letter { .. } => {}
        other => panic!("expected Letter, got {other:?}"),
    }
    // A second account that must stay untouched.
    let token_y = t.login("reset-other").await;
    let mut c = FakeDevice::new(10);
    enroll_genesis(&t, &token_y, &mut c).await;

    // The relay must be stopped for a reset.
    t.shutdown_all().await;
    let report = peakbot_relay::reset_account(t.db_path(), &t.idp.url, sub)
        .expect("reset ok");
    assert!(report.chain_entries >= 2, "chain deleted, got {report:?}");
    assert!(report.pending_devices >= 1, "pending deleted, got {report:?}");
    assert!(report.letters >= 1, "letters deleted, got {report:?}");
    assert!(report.sessions >= 1, "sessions deleted, got {report:?}");

    // Old token is dead.
    t.restart().await;
    let r = t.ws_status(Some(&token)).await;
    assert_eq!(r.status(), axum::http::StatusCode::UNAUTHORIZED, "old token → 401");

    // Re-login: the chain is empty and EnrollGenesis is allowed again.
    let token2 = t.login(sub).await;
    let mut a2 = FakeDevice::new(1);
    a2.connect(&t.ws_url(), &token2).await;
    a2.hello_pending(&t.origin()).await;
    expect_welcome(&a2.recv().await, None, true).await; // empty chain
    let e = entry(0, [0; 32], ChainOp::Genesis, Some(a2.card.clone()), None, &a2);
    a2.send(ToRelay::EnrollGenesis { entry: e.encode() }).await;
    expect_chain_changed(&a2.recv().await, 0).await;

    // The other account is untouched.
    let mut c2 = FakeDevice::new(10);
    connect_active(&t, &token_y, &mut c2, Some(0)).await;
    assert_eq!(fetch_chain(&mut c2, 0).await.len(), 1, "other account intact");
}
