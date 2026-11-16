//! P5 T3 tests — an erroring `SeedStore` must never turn into "fresh
//! install": no genesis, no fresh keygen, no seed write; a visible error
//! state instead. Audit finding: `spawn` -> `load_identity` already maps a
//! `load_seed` Err to a terminal `Trust(ChainInvalid("identity: ..."))`, so
//! the error cases are expected GREEN today — they are regression guards
//! for the mobile keystore, where an Err used to be indistinguishable from
//! "no seed". Runs in both editions.

mod cloud_common;

use cloud_common::failing_store::FailingSeedStore;
use cloud_common::fake_relay::FakeRelay;
use cloud_common::rig;
use peakbot::cloud::store::Seed;
use peakbot::cloud::{CloudConfig, CloudHandle, CloudState, SeedStore, spawn};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;
use url::Url;

fn boot(fake: &FakeRelay, store: Arc<FailingSeedStore>) -> (CloudHandle, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tmp");
    let cfg = CloudConfig {
        relay: peakbot::config::remote::Endpoint::LoopbackPlain {
            origin: Url::parse(&fake.url()).expect("origin"),
        },
        state_dir: PathBuf::from(dir.path()),
        device_name: "Phone".into(),
        platform: 4,
    };
    (spawn(cfg, store, None), dir)
}

fn is_error_state(s: &CloudState) -> bool {
    matches!(s, CloudState::Trust(_))
}

#[tokio::test]
async fn seed_read_error_at_boot_is_an_error_state_and_writes_no_seed() {
    let fake = FakeRelay::spawn().await;
    let store = Arc::new(FailingSeedStore::new());
    store.fail_load_seed.store(true, SeqCst);
    let (h, _d) = boot(&fake, store.clone());
    let st = h.state().borrow().clone();
    assert!(is_error_state(&st), "visible error, not LoggedOut: {st:?}");
    assert_eq!(store.seeds_written(), 0, "no fresh keygen on a read error");
    assert!(h.device_id().is_none(), "no identity was invented");
}

#[tokio::test]
async fn genesis_with_an_erroring_store_fails_and_posts_nothing() {
    let fake = FakeRelay::spawn().await;
    let store = Arc::new(FailingSeedStore::new());
    store.fail_load_seed.store(true, SeqCst);
    let (h, _d) = boot(&fake, store.clone());
    assert!(h.genesis().await.is_err(), "genesis must not run");
    assert_eq!(store.seeds_written(), 0);
    assert!(
        fake.entries().is_empty(),
        "no genesis entry reached the relay"
    );
    assert_eq!(fake.ws_connects(), 0);
}

#[tokio::test]
async fn login_with_an_erroring_store_keeps_the_error_state_and_writes_no_seed() {
    let fake = FakeRelay::spawn().await;
    let store = Arc::new(FailingSeedStore::new());
    store.fail_load_seed.store(true, SeqCst);
    let (h, _d) = boot(&fake, store.clone());
    let fk = fake.clone();
    let _ = h
        .login(move |url| {
            tokio::spawn(async move { rig::drive_login(&fk, url).await });
        })
        .await; // Err or Ok are both acceptable; the invariants below are not.
    tokio::time::sleep(Duration::from_millis(700)).await;
    let st = h.state().borrow().clone();
    assert!(is_error_state(&st), "still an error state: {st:?}");
    assert_eq!(store.seeds_written(), 0, "login must not mint a seed");
    assert!(fake.entries().is_empty(), "no genesis");
    assert_eq!(fake.ws_connects(), 0, "never dialled the relay");
}

#[tokio::test]
async fn seed_write_error_on_a_fresh_install_is_an_error_state_with_no_actor() {
    let fake = FakeRelay::spawn().await;
    let store = Arc::new(FailingSeedStore::new());
    store.fail_store_seed.store(true, SeqCst);
    let (h, _d) = boot(&fake, store.clone());
    let st = h.state().borrow().clone();
    assert!(
        is_error_state(&st),
        "keystore write failure is visible: {st:?}"
    );
    assert!(h.genesis().await.is_err());
    assert!(fake.entries().is_empty());
    assert_eq!(fake.ws_connects(), 0);
}

// ── guards against over-correcting: the healthy paths still work ────────

#[tokio::test]
async fn genuine_fresh_install_generates_exactly_one_seed() {
    let fake = FakeRelay::spawn().await;
    let store = Arc::new(FailingSeedStore::new());
    let (h, _d) = boot(&fake, store.clone());
    assert_eq!(
        store.seeds_written(),
        1,
        "one keygen on a real fresh install"
    );
    assert!(store.inner.load_seed().unwrap().is_some());
    assert!(matches!(
        h.state().borrow().clone(),
        CloudState::LoggedOut { .. }
    ));
}

#[tokio::test]
async fn existing_seed_is_reused_and_never_rewritten() {
    let fake = FakeRelay::spawn().await;
    let store = Arc::new(FailingSeedStore::new());
    store.inner.store_seed(&Seed::new([9; 64])).unwrap();
    let (h, _d) = boot(&fake, store.clone());
    assert_eq!(
        store.seeds_written(),
        0,
        "the seed is read, not regenerated"
    );
    assert!(h.device_id().is_some());
    assert!(matches!(
        h.state().borrow().clone(),
        CloudState::LoggedOut { .. }
    ));
}
