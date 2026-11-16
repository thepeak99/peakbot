//! P5 T5 RED tests — app-foreground `Suspend`/`Resume` (design §2 C3).
//! `set_foreground(false)` closes the relay socket after a 3 s debounce;
//! `set_foreground(true)` redials at once. Real time + real loopback
//! sockets: tokio's paused clock auto-advances while the runtime waits on
//! socket IO, so it cannot be used against the fake relay.
//!
//! RED today: `set_foreground` is `todo!()`.

mod cloud_common;

use cloud_common::fake_relay::FakeRelay;
use cloud_common::rig::{self, Dev, TIMEOUT, spawn_device};
use peakbot::cloud::CloudState;
use std::time::Duration;

/// Longer than the 3 s debounce.
const PAST_DEBOUNCE: Duration = Duration::from_millis(3600);

async fn wait_until(mut ok: impl FnMut() -> bool) -> bool {
    let end = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < end {
        if ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    ok()
}

/// One device, genesis done, Active, one live socket.
async fn active_one(fake: &FakeRelay) -> Dev {
    let a = spawn_device(1, "Phone", fake, None);
    a.login(fake).await;
    a.handle.genesis().await.expect("genesis");
    let _ = a.active().await;
    assert!(wait_until(|| fake.live_sockets() == 1).await, "socket up");
    a
}

#[tokio::test]
async fn suspend_closes_the_relay_socket_after_the_debounce() {
    let fake = FakeRelay::spawn().await;
    let a = active_one(&fake).await;
    a.handle.set_foreground(false);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(
        fake.live_sockets(),
        1,
        "within the 3 s debounce the socket stays"
    );
    assert!(
        wait_until(|| fake.live_sockets() == 0).await,
        "socket closed after the debounce"
    );
}

#[tokio::test]
async fn no_redial_while_suspended() {
    let fake = FakeRelay::spawn().await;
    let a = active_one(&fake).await;
    let before = fake.ws_connects();
    a.handle.set_foreground(false);
    assert!(wait_until(|| fake.live_sockets() == 0).await, "closed");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(fake.live_sockets(), 0, "still closed");
    assert_eq!(
        fake.ws_connects(),
        before,
        "no reconnect attempt while parked"
    );
}

#[tokio::test]
async fn suspend_does_not_wipe_identity_or_chain() {
    let fake = FakeRelay::spawn().await;
    let a = active_one(&fake).await;
    a.handle.set_foreground(false);
    assert!(wait_until(|| fake.live_sockets() == 0).await);
    let st = a.state();
    assert!(
        !matches!(
            st,
            CloudState::LoggedOut { .. } | CloudState::Disabled | CloudState::Trust(_)
        ),
        "suspend keeps enrollment/chain state, got {st:?}"
    );
    assert!(fake.token_valid(&token(&a)), "the session token is kept");
}

fn token(d: &Dev) -> String {
    use peakbot::cloud::SeedStore;
    d.store
        .load_token()
        .unwrap()
        .expect("token")
        .as_str()
        .to_string()
}

#[tokio::test]
async fn resume_redials_once_and_returns_to_active() {
    let fake = FakeRelay::spawn().await;
    let a = active_one(&fake).await;
    let before = fake.ws_connects();
    a.handle.set_foreground(false);
    assert!(wait_until(|| fake.live_sockets() == 0).await, "closed");
    a.handle.set_foreground(true);
    let st = rig::wait_state(a.handle.state(), |s| matches!(s, CloudState::Active { .. })).await;
    assert!(
        matches!(st, CloudState::Active { .. }),
        "back to Active: {st:?}"
    );
    assert!(
        wait_until(|| fake.live_sockets() == 1).await,
        "socket re-opened"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(fake.ws_connects(), before + 1, "exactly one redial");
}

#[tokio::test]
async fn foreground_flap_within_the_debounce_never_suspends() {
    let fake = FakeRelay::spawn().await;
    let a = active_one(&fake).await;
    let before = fake.ws_connects();
    a.handle.set_foreground(false);
    tokio::time::sleep(Duration::from_millis(800)).await;
    a.handle.set_foreground(true);
    tokio::time::sleep(PAST_DEBOUNCE).await;
    assert_eq!(fake.live_sockets(), 1, "the socket never closed");
    assert_eq!(fake.ws_connects(), before, "and was never redialled");
    assert!(matches!(a.state(), CloudState::Active { .. }));
}

#[tokio::test]
async fn resume_without_a_prior_suspend_is_a_noop() {
    let fake = FakeRelay::spawn().await;
    let a = active_one(&fake).await;
    let before = fake.ws_connects();
    a.handle.set_foreground(true);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(fake.ws_connects(), before, "no redial when not suspended");
    assert_eq!(fake.live_sockets(), 1);
}

#[tokio::test]
async fn suspend_while_logged_out_is_a_noop() {
    let fake = FakeRelay::spawn().await;
    let a = spawn_device(1, "Phone", &fake, None);
    assert!(matches!(a.state(), CloudState::LoggedOut { .. }));
    a.handle.set_foreground(false);
    tokio::time::sleep(PAST_DEBOUNCE).await;
    a.handle.set_foreground(true);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        matches!(a.state(), CloudState::LoggedOut { .. }),
        "still logged out: {:?}",
        a.state()
    );
    assert_eq!(
        fake.ws_connects(),
        0,
        "Resume must not dial without a token"
    );
    // Login still works afterwards (the actor was not parked).
    a.login(&fake).await;
    assert!(matches!(
        a.state(),
        CloudState::Pending { .. } | CloudState::Active { .. }
    ));
}

#[tokio::test]
async fn pending_enrollment_survives_suspend_resume_with_the_same_words_and_letter() {
    let fake = FakeRelay::spawn().await;
    let (a, b) = rig::a_active_b_pending(&fake).await;
    let CloudState::Pending {
        sas: Some(words), ..
    } = b.state()
    else {
        panic!("B pending with words: {:?}", b.state())
    };
    let s = rig::wait_state(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { requests, .. } if !requests.is_empty()),
    )
    .await;
    let CloudState::Active { requests, .. } = s else {
        panic!("A sees B's request: {s:?}")
    };
    let letter = requests[0].letter_id;

    b.handle.set_foreground(false);
    tokio::time::sleep(PAST_DEBOUNCE).await;
    b.handle.set_foreground(true);
    let st = rig::wait_state(b.handle.state(), |s| {
        matches!(s, CloudState::Pending { sas: Some(_), .. })
    })
    .await;
    let CloudState::Pending {
        sas: Some(after),
        rejected,
        ..
    } = st
    else {
        panic!("B still Pending with words after resume: {st:?}")
    };
    assert_eq!(after, words, "same 6 words — no new EnrollRequest");
    assert!(!rejected);

    let s = rig::wait_state(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { requests, .. } if !requests.is_empty()),
    )
    .await;
    let CloudState::Active { requests, .. } = s else {
        panic!("{s:?}")
    };
    assert_eq!(requests.len(), 1, "no duplicate request");
    assert_eq!(requests[0].letter_id, letter, "same letter");

    // The surviving enrollment is still approvable.
    a.handle.approve(letter).await.expect("approve");
    let st = b.active().await;
    assert!(
        matches!(st, CloudState::Active { .. }),
        "B enrolled: {st:?}"
    );
}
