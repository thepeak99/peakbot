//! P3 PR-B — flow tests (p3-design §G), written against the locked
//! `peakbot::cloud` seams (§D.6, enroll.rs) BEFORE the behaviour exists.
//! Every test must compile and fail on `todo!()` or an assertion.
//!
//! ── TEST → SPEC map ──────────────────────────────────────────────────────
//!   login_url_uses_pkce_s256_and_callback_redeems      G.1 (relay §4.1)
//!   first_device_genesis_makes_it_active               G.2
//!   new_device_pending_and_request_reaches_approver    G.3
//!   sas_words_equal_on_both_sides                      G.3 / G.4 / I.2-7
//!   enroll_attempt_freezes_head_before_reveal          spec §5.5 / G.3-1
//!   approve_signs_add_and_new_device_becomes_active    G.4
//!   reject_sends_enrollreject_and_new_device_stays_pending  G.4-4
//!   revoking_a_peer_triggers_chainchanged_and_drops_sessions  G.5 / I.2-4
//!   logout_self_revokes_posts_logout_and_wipes         G.6 / I.2-9
//!   offline_logout_still_wipes_and_reports_not_revoked G.6-4

#[path = "cloud_common/mod.rs"]
mod cloud_common;

use cloud::frames::{Op, StreamMsg};
use futures::StreamExt;
use peakbot::cloud::enroll::{EnrollAttempt, EnrollError};
use peakbot::cloud::{
    CloudError, CloudState, FlowError, LogoutOutcome, LogoutReason, SeedStore, TrustError,
};
use url::Url;

use cloud_common::devices::{self, card_a, card_c, id_c};
use cloud_common::fake_relay::FakeRelay;
use cloud_common::rig::{self, TIMEOUT, spawn_device};

/// The two-device chain (A genesis, B added) as a verified chain — the
/// fixture for the §5.5 freeze tests (no network).
fn chain_ab() -> cloud::chain::VerifiedChain {
    let g = devices::genesis_a();
    let h = cloud::chain::entry_hash(&g);
    let entries = [g, devices::add_entry(1, 1, h, &devices::card_b())];
    cloud::chain::extend(None, &entries, None).expect("chain verifies")
}

/// b64url-no-padding charset check (S256 of 32 bytes is exactly 43 chars).
fn is_b64url_43(s: &str) -> bool {
    s.len() == 43
        && s.bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'-' | b'_'))
}

/// Wait until `ok` holds; the rig's `wait_state` returns the last state on
/// timeout, so callers assert on the result.
async fn state_where(
    rx: tokio::sync::watch::Receiver<CloudState>,
    ok: impl Fn(&CloudState) -> bool,
) -> CloudState {
    rig::wait_state(rx, ok).await
}

#[tokio::test]
async fn login_url_uses_pkce_s256_and_callback_redeems() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device(1, "Laptop", &fake, None);

    let (url_tx, url_rx) = tokio::sync::oneshot::channel::<Url>();
    let fk = fake.clone();
    dev.handle
        .login(move |url| {
            let _ = url_tx.send(url.clone());
            tokio::spawn(async move { rig::drive_login(&fk, url).await });
        })
        .await
        // The fake `/token` answers `invalid_grant` unless
        // b64url(sha256(code_verifier)) == code_challenge, so a successful
        // login is itself the assertion that S256 was used end to end.
        .expect("login: the fake token endpoint asserted the PKCE verifier");

    let url = url_rx.await.expect("the auth url was handed to the opener");
    let q: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let challenge = q
        .iter()
        .find(|(k, _)| k == "code_challenge")
        .map(|(_, v)| v.clone())
        .expect("auth url carries code_challenge");
    assert!(
        is_b64url_43(&challenge),
        "S256 challenge must be 43-char b64url without padding, got {challenge:?}"
    );
    let redirect = q
        .iter()
        .find(|(k, _)| k == "redirect_uri")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    assert!(
        redirect.contains("/cb"),
        "the loopback listener sits on /cb, got {redirect:?}"
    );
    let state = q
        .iter()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.len())
        .unwrap_or(0);
    assert!(
        (1..=128).contains(&state),
        "device state param must be 1..=128"
    );

    // The callback redeemed the code: a token is stored and the actor is
    // no longer LoggedOut.
    let _ = state_where(dev.handle.state(), |s| {
        matches!(s, CloudState::Pending { .. } | CloudState::Active { .. })
    })
    .await;
    assert!(
        dev.store.load_token().expect("store").is_some(),
        "the redeemed session token must be stored"
    );
}

#[tokio::test]
async fn first_device_genesis_makes_it_active() {
    let fake = FakeRelay::spawn().await;
    let a = spawn_device(1, "Laptop", &fake, None);
    a.login(&fake).await;

    match a.state() {
        CloudState::Pending { empty_account, .. } => {
            assert!(empty_account, "the first device sees an empty account");
        }
        other => panic!("expected Pending, got {other:?}"),
    }

    let id = a.handle.genesis().await.expect("genesis");
    let s = a.active().await;
    match s {
        CloudState::Active { devices, requests } => {
            assert!(requests.is_empty(), "no requests after genesis");
            assert_eq!(devices.len(), 1, "genesis pins exactly one device");
            assert_eq!(devices[0].id, id, "the response id is the pinned device");
            assert_eq!(devices[0].name, "Laptop");
        }
        other => panic!("expected Active, got {other:?}"),
    }
    // The account's chain is exactly our genesis entry.
    assert_eq!(fake.entries().len(), 1);
}

#[tokio::test]
async fn new_device_pending_and_request_reaches_approver() {
    let fake = FakeRelay::spawn().await;
    let (a, b) = rig::a_active_b_pending(&fake).await;

    // B: Pending with the SAS posted (head frozen, §5.5).
    match b.state() {
        CloudState::Pending {
            empty_account: false,
            sas: Some(_),
            rejected: false,
        } => {}
        other => panic!("B must be Pending with a posted request, got {other:?}"),
    }

    // A: the EnrollRequest letter arrived and was verified.
    let s = state_where(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { requests, .. } if !requests.is_empty()),
    )
    .await;
    match s {
        CloudState::Active { requests, .. } => {
            assert_eq!(requests.len(), 1, "exactly one pending request");
            assert_eq!(
                requests[0].device,
                b.handle.device_id().expect("B has a device id"),
                "the request names B"
            );
            assert_eq!(requests[0].name, "Desktop");
            assert_eq!(requests[0].sas.words().len(), 6);
            assert!(requests[0].expires_at > 0, "request carries a TTL");
        }
        other => panic!("A must hold the request, got {other:?}"),
    }
}

#[tokio::test]
async fn sas_words_equal_on_both_sides() {
    let fake = FakeRelay::spawn().await;
    let (a, b) = rig::a_active_b_pending(&fake).await;

    let CloudState::Pending { sas: Some(wb), .. } = b.state() else {
        panic!("B must have posted its SAS words");
    };
    let s = state_where(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { requests, .. } if !requests.is_empty()),
    )
    .await;
    let CloudState::Active { requests, .. } = s else {
        panic!("A must hold the request");
    };
    assert_eq!(
        requests[0].sas, wb,
        "the approver's recomputed words must equal the pending device's (I.2-7)"
    );
}

#[tokio::test]
async fn enroll_attempt_freezes_head_before_reveal() {
    // spec §5.5: the head is captured BEFORE anything is shown; a head that
    // moved after the card was produced is an ERROR, not a re-derivation.
    let c = chain_ab();
    let attempt = EnrollAttempt::freeze(&c, &card_a());
    assert_eq!(attempt.frozen(), c.head, "freeze captures chain.head");

    // Same head: reveal works.
    let words = attempt.reveal(&c.head).expect("head unchanged → reveal ok");
    assert_eq!(words.words().len(), 6);

    // A new Add moves the head: revealing against the NEW head must fail.
    let e = devices::add_entry(2, c.head.seq + 1, c.head.hash, &card_c());
    let c2 = cloud::chain::extend(Some(c.clone()), &[e], None).expect("extended chain verifies");
    assert_ne!(c2.head, c.head, "fixture: the head moved");
    assert_eq!(
        attempt.reveal(&c2.head),
        Err(EnrollError::HeadMoved),
        "a moved head must be refused, not silently re-derived"
    );
    // The frozen head is immutable and the original reveal still stands.
    assert_eq!(attempt.frozen(), c.head);
    assert!(attempt.reveal(&c.head).is_ok());
}

#[tokio::test]
async fn approve_signs_add_and_new_device_becomes_active() {
    let fake = FakeRelay::spawn().await;
    let (a, b) = rig::enrolled_pair(&fake).await;

    // The chain grew by exactly the Add for B (genesis + Add).
    assert_eq!(fake.entries().len(), 2, "genesis + one Add");

    let s = b.state();
    assert!(
        matches!(&s, CloudState::Active { devices, .. } if devices.len() == 2),
        "B is Active with both devices, got {s:?}"
    );
    let s = a.state();
    assert!(
        matches!(&s, CloudState::Active { requests, .. } if requests.is_empty()),
        "the approved request is gone from A, got {s:?}"
    );
    let _ = a.handle.device_id().expect("A keeps its id");
}

#[tokio::test]
async fn reject_sends_enrollreject_and_new_device_stays_pending() {
    let fake = FakeRelay::spawn().await;
    let (a, b) = rig::a_active_b_pending(&fake).await;

    let s = state_where(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { requests, .. } if !requests.is_empty()),
    )
    .await;
    let CloudState::Active { requests, .. } = s else {
        panic!("A must hold the request");
    };
    a.handle
        .reject(requests[0].letter_id)
        .await
        .expect("reject");

    // B sees the EnrollReject: stays Pending (NOT LoggedOut / Trust), SAS
    // cleared, no retry (G.3 step 4).
    let sb = state_where(b.handle.state(), |s| {
        matches!(s, CloudState::Pending { sas: None, .. })
    })
    .await;
    assert!(
        matches!(
            &sb,
            CloudState::Pending {
                sas: None,
                rejected: true,
                ..
            }
        ),
        "B must stay Pending with the SAS cleared and `rejected: true` published (P4 E-F7), got {sb:?}"
    );
    // A's request list is empty again.
    let sa = state_where(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { requests, .. } if requests.is_empty()),
    )
    .await;
    assert!(
        matches!(&sa, CloudState::Active { .. }),
        "A stays Active, got {sa:?}"
    );
}

#[tokio::test]
async fn revoking_a_peer_triggers_chainchanged_and_drops_sessions() {
    let fake = FakeRelay::spawn().await;

    // A (genesis) enrolls B, then C — three active devices so the revoke
    // is unambiguous (I.2-4 uses a pair; the rule is per G.5).
    let a = spawn_device(1, "Laptop", &fake, None);
    a.login(&fake).await;
    a.handle.genesis().await.expect("genesis");
    let _ = a.active().await;

    let b = spawn_device(2, "Desktop", &fake, None);
    b.login(&fake).await;
    // C must accept inbound sessions (design §A: no hook ⇒ `Close{PROTOCOL}`),
    // and its hook must HOLD the stream — a dropped one would send
    // `Reset(CANCELLED)` and fake the revoke assertion below.
    let (held_tx, _held_rx) = tokio::sync::mpsc::unbounded_channel();
    let hook: peakbot::cloud::InboundHook = std::sync::Arc::new(move |dev, op, stream| {
        let _ = held_tx.send((dev, op, stream));
    });
    let c = spawn_device(3, "Pixel 9", &fake, Some(hook));
    c.login(&fake).await;
    // Approve both requests (B's then C's, in arrival order).
    for name in ["Desktop", "Pixel 9"] {
        let s = state_where(
            a.handle.state(),
            |s| matches!(s, CloudState::Active { requests, .. } if !requests.is_empty()),
        )
        .await;
        let CloudState::Active { requests, .. } = s else {
            panic!("A must hold the request from {name}");
        };
        a.handle
            .approve(requests[0].letter_id)
            .await
            .expect("approve {name}");
    }
    let _ = b.active().await;
    let _ = c.active().await;
    let sa = state_where(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { devices, .. } if devices.len() == 3),
    )
    .await;
    assert!(
        matches!(&sa, CloudState::Active { devices, .. } if devices.len() == 3),
        "three active devices, got {sa:?}"
    );

    // A opens a live stream to C.
    let mut stream_a = a.handle.open(id_c(), Op::Attach).await.expect("open A→C");

    // C revokes away: A's ChainChanged must Reset the live session.
    a.handle.revoke(id_c()).await.expect("revoke C");
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let mut reset: Option<u16> = None;
    while reset.is_none() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "no Reset within {TIMEOUT:?}");
        match tokio::time::timeout(remaining, stream_a.next()).await {
            Ok(Some(StreamMsg::Reset(code))) => reset = Some(code),
            Ok(Some(_)) => {} // frames before the reset
            Ok(None) => panic!("the stream closed without a Reset frame"),
            Err(_) => panic!("no Reset within {TIMEOUT:?}"),
        }
    }

    // C sees itself revoked: terminal Trust(Revoked), no reconnect.
    let sc = state_where(c.handle.state(), |s| matches!(s, CloudState::Trust(_))).await;
    assert!(
        matches!(&sc, CloudState::Trust(TrustError::Revoked)),
        "C must land in Trust(Revoked), got {sc:?}"
    );
    // A stays Active with the two remaining devices.
    let sa = state_where(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { devices, .. } if devices.len() == 2),
    )
    .await;
    assert!(
        matches!(&sa, CloudState::Active { devices, .. } if devices.len() == 2),
        "A keeps the two remaining devices, got {sa:?}"
    );
}

#[tokio::test]
async fn logout_self_revokes_posts_logout_and_wipes() {
    let fake = FakeRelay::spawn().await;
    let (a, b) = rig::enrolled_pair(&fake).await;
    let token = b
        .store
        .load_token()
        .expect("store")
        .expect("B logged in")
        .as_str()
        .to_string();
    assert!(fake.token_valid(&token), "precondition: the token is live");

    let out = b.handle.logout().await.expect("logout");
    assert_eq!(
        out,
        LogoutOutcome { revoked: true },
        "B self-revoked (A remains, so the revoke runs)"
    );

    // Wipe: seed + token gone from the store (G.6 step 3).
    assert!(
        b.store.load_seed().expect("store").is_none(),
        "the seed is wiped"
    );
    assert!(
        b.store.load_token().expect("store").is_none(),
        "the token is wiped"
    );
    assert!(!fake.token_valid(&token), "the relay session is gone");

    // B: LoggedOut{SignedOut} — and it does not reconnect (no token).
    let sb = state_where(b.handle.state(), |s| {
        matches!(s, CloudState::LoggedOut { .. })
    })
    .await;
    assert!(
        matches!(
            &sb,
            CloudState::LoggedOut {
                reason: LogoutReason::SignedOut
            }
        ),
        "B must be LoggedOut{{SignedOut}}, got {sb:?}"
    );

    // A: B is no longer active.
    let sa = state_where(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { devices, .. } if devices.len() == 1),
    )
    .await;
    assert!(
        matches!(&sa, CloudState::Active { devices, .. } if devices.len() == 1),
        "A sees one remaining device, got {sa:?}"
    );
}

#[tokio::test]
async fn offline_logout_still_wipes_and_reports_not_revoked() {
    let fake = FakeRelay::spawn().await;
    let (_a, b) = rig::enrolled_pair(&fake).await;

    // The relay's /logout answers 500 (offline): the local steps 2–3 of
    // G.6 still run and the API can tell the user the revoke did not land.
    fake.fail_logout(true);
    let out = b
        .handle
        .logout()
        .await
        .expect("logout must not fail locally");
    assert_eq!(
        out,
        LogoutOutcome { revoked: false },
        "a failed self-revoke must report revoked:false"
    );
    assert!(
        b.store.load_seed().expect("store").is_none(),
        "the seed is wiped"
    );
    assert!(
        b.store.load_token().expect("store").is_none(),
        "the token is wiped"
    );
    let sb = state_where(b.handle.state(), |s| {
        matches!(s, CloudState::LoggedOut { .. })
    })
    .await;
    assert!(
        matches!(
            &sb,
            CloudState::LoggedOut {
                reason: LogoutReason::SignedOut
            }
        ),
        "B is SignedOut even offline, got {sb:?}"
    );
}

fn words_of(s: &CloudState) -> Vec<String> {
    match s {
        CloudState::Pending { sas: Some(w), .. } => {
            w.words().iter().map(|x| x.to_string()).collect()
        }
        other => panic!("expected Pending with a SAS, got {other:?}"),
    }
}

/// P4 E-F7: reject → `retry_enroll` redials with a fresh EnrollRequest:
/// `rejected` flips back to false, a NEW SAS appears, the approver sees a
/// new request, and approving it makes B Active.
#[tokio::test]
async fn rejected_device_retries_and_gets_fresh_sas() {
    let fake = FakeRelay::spawn().await;
    let (a, b) = rig::a_active_b_pending(&fake).await;
    let old_words = words_of(&b.state());

    let s = state_where(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { requests, .. } if !requests.is_empty()),
    )
    .await;
    let CloudState::Active { requests, .. } = s else {
        panic!("A must hold the request");
    };
    let old_letter = requests[0].letter_id;
    a.handle.reject(old_letter).await.expect("reject");
    let _ = state_where(b.handle.state(), |s| {
        matches!(s, CloudState::Pending { rejected: true, .. })
    })
    .await;

    b.handle
        .retry_enroll()
        .await
        .expect("retry_enroll after a rejection is accepted");

    let sb = state_where(b.handle.state(), |s| {
        matches!(
            s,
            CloudState::Pending {
                sas: Some(_),
                rejected: false,
                ..
            }
        )
    })
    .await;
    assert!(
        matches!(
            &sb,
            CloudState::Pending {
                sas: Some(_),
                rejected: false,
                ..
            }
        ),
        "after retry B is Pending{{rejected:false}} with a posted request, got {sb:?}"
    );
    assert_ne!(
        words_of(&sb),
        old_words,
        "a retry makes a NEW request, hence a new SAS"
    );

    // The approver sees the NEW letter and approving it activates B.
    let sa = state_where(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { requests, .. } if requests.iter().any(|r| r.letter_id != old_letter)),
    )
    .await;
    let CloudState::Active { requests, .. } = sa else {
        panic!("A must stay Active");
    };
    let fresh = requests
        .iter()
        .find(|r| r.letter_id != old_letter)
        .unwrap_or_else(|| panic!("A must hold a NEW request after the retry, got {requests:?}"));
    let CloudState::Pending {
        sas: Some(new_sas), ..
    } = &sb
    else {
        unreachable!()
    };
    assert_eq!(
        fresh.sas, *new_sas,
        "approver and retried device compute the same words"
    );
    a.handle.approve(fresh.letter_id).await.expect("approve");
    let sb = b.active().await;
    assert!(
        matches!(&sb, CloudState::Active { devices, .. } if devices.len() == 2),
        "B becomes Active after approval of the retried request, got {sb:?}"
    );
}

/// P4 E-F7: retrying while NOT rejected is refused with `NotRejected` and
/// changes nothing.
#[tokio::test]
async fn retry_when_not_rejected_is_not_rejected() {
    let fake = FakeRelay::spawn().await;
    let (_a, b) = rig::a_active_b_pending(&fake).await;
    let before = words_of(&b.state());

    let e = b
        .handle
        .retry_enroll()
        .await
        .expect_err("not rejected → refused");
    assert_eq!(e, CloudError::Flow(FlowError::NotRejected));
    assert_eq!(
        words_of(&b.state()),
        before,
        "a refused retry must not redial (SAS unchanged)"
    );
}
