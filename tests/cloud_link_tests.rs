//! P3 PR-B — RED tests for the CloudLink actor (p3-design §D) driven
//! against the in-test FAKE RELAY (`cloud_common::fake_relay`).
//!
//! The actor is a `todo!()` skeleton; every test here compiles against the
//! locked seams (`spawn`, `CloudHandle::{state,login,logout,open,
//! genesis,approve,reject,revoke,profiles,device_id}`) and fails on the
//! first `todo!()`.
//!
//! Seeds (spec §15.1): A = 0x01×64, B = 0x02×64. Both devices join ONE
//! account on the fake relay.
//!
//! Test → spec map (§D / §G):
//!   fresh_link_starts_logged_out_never            D.1
//!   login_drives_the_pkce_flow_and_redeems        G.1
//!   pre_hello_chainfetch_precedes_hello           D.2 step 2
//!   genesis_pin_persisted_to_the_slug_dir         C / G.2
//!   genesis_makes_the_device_active               G.2
//!   a_different_genesis_pin_trusts_and_sends_no_hello  D.2 / I.2-6
//!   a_fork_trusts                                 D.3 / I.2-6
//!   chain_changed_triggers_a_resync               D.3
//!   reconnect_with_backoff_after_a_drop           D.4
//!   token_401_deletes_the_token_and_keeps_the_seed  D.2 / C
//!   open_fails_with_not_active_when_logged_out     D.6
//!   open_fails_with_unknown_device                D.6
//!   profiles_op_round_trips_between_two_links     D.5 (stub inbound hook)
//!   simultaneous_handshake_tie_breaks_to_one_session  G.7
//!   a_link_without_an_inbound_hook_refuses_hs1    A (phone/controller-only)

#[path = "cloud_common/mod.rs"]
mod cloud_common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cloud::frames::{Op, StreamMsg};
use futures::{SinkExt, StreamExt};
use peakbot::cloud::store::MemSeedStore;
use peakbot::cloud::{
    CloudConfig, CloudError, CloudState, DeviceInfo, InboundHook, Seed, SeedStore, SessionToken,
    spawn,
};
use url::Url;

use cloud_common::devices::{self, card_a, card_b, id_a, id_b};
use cloud_common::fake_relay::FakeRelay;

const TIMEOUT: Duration = Duration::from_secs(20);

/// Wait for the published state to satisfy `ok` (polling the watch), with
/// a deadline. Returns the last seen state.
async fn wait_state(
    mut rx: tokio::sync::watch::Receiver<CloudState>,
    ok: impl Fn(&CloudState) -> bool,
) -> CloudState {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let mut last = rx.borrow().clone();
    loop {
        if ok(&last) {
            return last;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep(remaining) => break,
            _ = rx.changed() => { last = rx.borrow().clone(); }
        }
    }
    last
}

async fn is_active(rx: &mut tokio::sync::watch::Receiver<CloudState>) -> CloudState {
    wait_state(rx.clone(), |s| matches!(s, CloudState::Active { .. })).await
}

/// Spawn a CloudLink for device `n` with its seed pre-stored, against
/// `fake`, with a fresh `state_dir`. `inbound` is the host hook (or `None`
/// for the phone / controller-only edition).
fn spawn_device(
    n: u8,
    name: &str,
    fake: &FakeRelay,
    state_dir: PathBuf,
    inbound: Option<InboundHook>,
) -> (peakbot::cloud::CloudHandle, Arc<MemSeedStore>) {
    let store = Arc::new(MemSeedStore::new());
    store.store_seed(&Seed::new([n; 64])).expect("seed");
    let origin = Url::parse(&fake.url()).expect("origin url");
    let cfg = CloudConfig {
        relay: peakbot::config::remote::Endpoint::LoopbackPlain { origin },
        state_dir,
        device_name: name.to_string(),
        platform: 1,
    };
    (spawn(cfg, store.clone(), inbound), store)
}
/// A client that does NOT follow the relay's 302 (the tests assert on it).
fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client")
}


/// Drive the PKCE login to completion without a browser: GET the auth URL
/// (302 → fake IdP), mint a code via `issue_code`, and hit the loopback
/// redirect. The `auth_url` is captured for assertions.
async fn drive_login(fake: &FakeRelay, auth_url: Url) {
    let resp = no_redirect_client()
        .get(auth_url.clone())
        .send()
        .await
        .expect("GET the auth url");
    assert_eq!(resp.status(), 302, "the auth url starts a 302 flow");
    let loc = resp
        .headers()
        .get("location")
        .expect("location")
        .to_str()
        .expect("str")
        .to_string();
    assert!(
        loc.contains("relay_state="),
        "302 → IdP with relay_state: {loc}"
    );
    let relay_state = loc
        .split("relay_state=")
        .nth(1)
        .expect("relay_state")
        .split('&')
        .next()
        .expect("rs");
    let (code, state) = fake.issue_code(relay_state);
    let cb = format!("{}?code={}&state={}", redirect_of(&auth_url), code, state);
    let _ = reqwest::get(&cb).await.expect("loopback /cb");
}

/// The `redirect_uri` query param of the auth URL.
fn redirect_of(auth_url: &Url) -> String {
    auth_url
        .query_pairs()
        .find(|(k, _)| k == "redirect_uri")
        .map(|(_, v)| v.into_owned())
        .expect("redirect_uri param")
}

fn token_of(store: &MemSeedStore) -> Option<SessionToken> {
    store.load_token().expect("load token")
}

/// Poll the store for a token (login runs in the background per G.1 step 2).
async fn wait_token(store: &MemSeedStore) -> SessionToken {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        if let Some(t) = token_of(store) {
            return t;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("no token stored within {TIMEOUT:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

// ── D.1 state machine ──────────────────────────────────────────────────────

#[tokio::test]
async fn fresh_link_starts_logged_out_never() {
    let fake = FakeRelay::spawn().await;
    let dir = tempfile::tempdir().expect("tmp");
    let (handle, _store) = spawn_device(1, "Laptop", &fake, dir.path().to_path_buf(), None);
    let mut rx = handle.state();
    let s = wait_state(rx.clone(), |s| matches!(s, CloudState::LoggedOut { .. })).await;
    assert_eq!(
        s,
        CloudState::LoggedOut {
            reason: peakbot::cloud::LogoutReason::Never
        }
    );
}

#[tokio::test]
async fn login_drives_the_pkce_flow_and_redeems() {
    let fake = FakeRelay::spawn().await;
    let dir = tempfile::tempdir().expect("tmp");
    let (handle, store) = spawn_device(1, "Laptop", &fake, dir.path().to_path_buf(), None);
    let mut rx = handle.state();

    // Capture the auth URL to pin the PKCE S256 shape (G.1 step 2).
    let (url_tx, mut url_rx) = tokio::sync::mpsc::unbounded_channel::<Url>();
    let h = handle.clone();
    let fk = fake.clone();
    h.login(move |url| {
        let _ = url_tx.send(url.clone());
        tokio::spawn(async move {
            drive_login(&fk, url).await;
        });
    })
    .await
    .expect("login completes");
    let auth_url = url_rx.recv().await.expect("the browser got a URL");
    let base = fake.origin();
    assert!(
        auth_url.as_str().starts_with(&format!("{base}/login")),
        "the auth url points at the relay /login: {auth_url}"
    );
    let q: Vec<(String, String)> = auth_url
        .query_pairs()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
    let challenge = q
        .iter()
        .find(|(k, _)| k == "code_challenge")
        .map(|(_, v)| v.clone())
        .expect("code_challenge");
    assert_eq!(challenge.len(), 43, "S256 challenge is 43 b64url chars");
    assert!(
        challenge
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'-' | b'_')),
        "b64url: {challenge}"
    );
    let redirect = q
        .iter()
        .find(|(k, _)| k == "redirect_uri")
        .map(|(_, v)| v.clone())
        .expect("redirect_uri");
    assert!(
        redirect.starts_with("http://127.0.0.1:") && redirect.ends_with("/cb"),
        "loopback /cb redirect: {redirect}"
    );
    let state = q
        .iter()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.clone())
        .expect("state");
    assert!(!state.is_empty(), "a random state is present");

    // The token endpoint asserted the PKCE verifier server-side (a mismatch
    // → invalid_grant → login Err). The token is stored once the background
    // listener finishes (G.1 step 2: login returns the URL immediately).
    let _tok = wait_token(&store).await;
    let s = is_active(&mut rx).await;
    // First device on an empty account: Pending (genesis not yet run).
    assert!(
        matches!(
            s,
            CloudState::Pending {
                empty_account: true,
                ..
            }
        ),
        "a fresh login on an empty account is Pending{{empty_account:true}}, got {s:?}"
    );
}

#[tokio::test]
async fn pre_hello_chainfetch_precedes_hello() {
    let fake = FakeRelay::spawn().await;
    let dir = tempfile::tempdir().expect("tmp");
    let (handle, _store) = spawn_device(1, "Laptop", &fake, dir.path().to_path_buf(), None);
    let h = handle.clone();
    let fk = fake.clone();
    h.login(move |url| {
        tokio::spawn(async move { drive_login(&fk, url).await });
    })
    .await
    .expect("login");
    // Give the actor a moment to run the connect sequence (D.2).
    let mut rx = handle.state();
    let _ = wait_state(rx.clone(), |s| {
        matches!(s, CloudState::Pending { .. } | CloudState::Active { .. })
    })
    .await;
    let msgs = fake.msgs();
    let fetch = msgs.iter().position(|m| m.starts_with("ChainFetch"));
    let hello = msgs.iter().position(|m| m.starts_with("Hello"));
    assert!(fetch.is_some(), "a ChainFetch was sent: {msgs:?}");
    assert!(hello.is_some(), "a Hello/HelloPending was sent: {msgs:?}");
    assert!(
        fetch.expect("f") < hello.expect("h"),
        "ChainFetch (D.2 step 2) precedes Hello: {msgs:?}"
    );
}

// ── G.2 genesis ────────────────────────────────────────────────────────────

#[tokio::test]
async fn genesis_pin_persisted_to_the_slug_dir() {
    let fake = FakeRelay::spawn().await;
    let dir = tempfile::tempdir().expect("tmp");
    let (handle, _store) = spawn_device(1, "Laptop", &fake, dir.path().to_path_buf(), None);
    let h = handle.clone();
    let fk = fake.clone();
    h.login(move |url| {
        tokio::spawn(async move { drive_login(&fk, url).await });
    })
    .await
    .expect("login");
    let mut rx = handle.state();
    let _ = wait_state(rx.clone(), |s| matches!(s, CloudState::Pending { .. })).await;

    let id = handle.genesis().await.expect("genesis on an empty account");
    assert_eq!(id, id_a(), "genesis returns our own DeviceId");

    let s = is_active(&mut rx).await;
    let CloudState::Active { devices, .. } = &s else {
        panic!("active after genesis, got {s:?}");
    };
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].id, id_a());

    // The genesis pin is written to <state_dir>/<origin-slug>/genesis and
    // equals the hash of entry 0 (the trust anchor, TOFU).
    let entries = fake.entries();
    assert_eq!(entries.len(), 1, "the chain holds exactly the genesis");
    let want = cloud::chain::entry_hash(&entries[0]);
    let slug = peakbot::cloud::config::origin_slug(&Url::parse(&fake.url()).expect("url"));
    let pin =
        std::fs::read(dir.path().join(&slug).join("genesis")).expect("the genesis pin file exists");
    assert_eq!(pin, want.to_vec(), "the pin is entry_hash(genesis)");
}

#[tokio::test]
async fn genesis_makes_the_device_active_with_one_device() {
    let fake = FakeRelay::spawn().await;
    let dir = tempfile::tempdir().expect("tmp");
    let (handle, _store) = spawn_device(1, "Laptop", &fake, dir.path().to_path_buf(), None);
    let h = handle.clone();
    let fk = fake.clone();
    h.login(move |url| {
        tokio::spawn(async move { drive_login(&fk, url).await });
    })
    .await
    .expect("login");
    let mut rx = handle.state();
    let _ = wait_state(rx.clone(), |s| matches!(s, CloudState::Pending { .. })).await;
    handle.genesis().await.expect("genesis");
    let s = is_active(&mut rx).await;
    let CloudState::Active { devices, requests } = &s else {
        panic!("{s:?}");
    };
    assert_eq!(devices.len(), 1);
    assert!(
        requests.is_empty(),
        "no enrollment requests on a solo device"
    );
    assert_eq!(devices[0].name, "Laptop", "the device name is published");
}

// ── D.2 / I.2 case 6: trust failures send NO Hello ─────────────────────────

#[tokio::test]
async fn a_different_genesis_pin_trusts_and_sends_no_hello() {
    let fake = FakeRelay::spawn().await;
    let dir = tempfile::tempdir().expect("tmp");
    // Seed a genesis pin that does NOT match the account's real genesis.
    let origin = Url::parse(&fake.url()).expect("url");
    let slug = peakbot::cloud::config::origin_slug(&origin);
    let slug_dir = dir.path().join(&slug);
    std::fs::create_dir_all(&slug_dir).expect("slug dir");
    std::fs::write(slug_dir.join("genesis"), [0xEE; 32]).expect("a foreign pin");

    let (handle, _store) = spawn_device(1, "Laptop", &fake, dir.path().to_path_buf(), None);
    let h = handle.clone();
    let fk = fake.clone();
    h.login(move |url| {
        tokio::spawn(async move { drive_login(&fk, url).await });
    })
    .await
    .expect("login");
    let mut rx = handle.state();
    let s = wait_state(rx.clone(), |s| {
        matches!(
            s,
            CloudState::Trust(peakbot::cloud::TrustError::GenesisMismatch)
        )
    })
    .await;
    assert_eq!(
        s,
        CloudState::Trust(peakbot::cloud::TrustError::GenesisMismatch),
        "a pin the chain does not verify is a terminal trust failure"
    );
    let msgs = fake.msgs();
    assert!(
        !msgs.iter().any(|m| m.starts_with("Hello")),
        "NO Hello is sent on a chain trust failure (D.2 step 2): {msgs:?}"
    );
}

#[tokio::test]
async fn a_fork_trusts() {
    let fake = FakeRelay::spawn().await;
    let dir = tempfile::tempdir().expect("tmp");
    let (handle, _store) = spawn_device(1, "Laptop", &fake, dir.path().to_path_buf(), None);
    let h = handle.clone();
    let fk = fake.clone();
    h.login(move |url| {
        tokio::spawn(async move { drive_login(&fk, url).await });
    })
    .await
    .expect("login");
    let mut rx = handle.state();
    let _ = wait_state(rx.clone(), |s| matches!(s, CloudState::Pending { .. })).await;
    handle.genesis().await.expect("genesis");
    let _ = is_active(&mut rx).await;

    // Fork: replace the account's single entry with a DIFFERENT valid
    // genesis (same account, different root) — the pin no longer verifies.
    let foreign = devices::genesis_a();
    // A genesis with a different card yields a different hash ⇒ mismatch/fork.
    let _ = foreign;
    fake.set_entries(vec![devices::entry(
        1,
        0,
        [0; 32],
        cloud::chain::ChainOp::Genesis,
        Some(&devices::card(1, "Laptop-renamed", 1)),
        None,
    )]);
    let s = wait_state(rx.clone(), |s| {
        matches!(
            s,
            CloudState::Trust(peakbot::cloud::TrustError::GenesisMismatch)
                | CloudState::Trust(peakbot::cloud::TrustError::Fork)
        )
    })
    .await;
    assert!(
        matches!(
            s,
            CloudState::Trust(peakbot::cloud::TrustError::GenesisMismatch)
                | CloudState::Trust(peakbot::cloud::TrustError::Fork)
        ),
        "a diverged root is a trust failure, got {s:?}"
    );
}

// ── D.3: ChainChanged ⇒ resync ─────────────────────────────────────────────

#[tokio::test]
async fn chain_changed_triggers_a_resync() {
    let fake = FakeRelay::spawn().await;
    let dir = tempfile::tempdir().expect("tmp");
    let (handle, _store) = spawn_device(1, "Laptop", &fake, dir.path().to_path_buf(), None);
    let h = handle.clone();
    let fk = fake.clone();
    h.login(move |url| {
        tokio::spawn(async move { drive_login(&fk, url).await });
    })
    .await
    .expect("login");
    let mut rx = handle.state();
    let _ = wait_state(rx.clone(), |s| matches!(s, CloudState::Pending { .. })).await;
    handle.genesis().await.expect("genesis");
    let _ = is_active(&mut rx).await;

    // A second device is Added (forged-but-valid, signed by A) → the relay
    // broadcasts ChainChanged → the actor must refetch and go to 2 devices.
    let e0 = fake.entries().first().expect("genesis").clone();
    let h0 = cloud::chain::entry_hash(&e0);
    fake.append_entry(devices::add_entry(1, 1, h0, &card_b()));

    let s = wait_state(
        rx.clone(),
        |s| matches!(s, CloudState::Active { devices, .. } if devices.len() == 2),
    )
    .await;
    let CloudState::Active { devices, .. } = &s else {
        panic!("two devices after the resync, got {s:?}");
    };
    let ids: Vec<_> = devices.iter().map(|d| d.id).collect();
    assert!(ids.contains(&id_a()) && ids.contains(&id_b()), "{ids:?}");
}

// ── D.4: reconnect with backoff ────────────────────────────────────────────

#[tokio::test]
async fn reconnect_with_backoff_after_a_drop() {
    let fake = FakeRelay::spawn().await;
    let dir = tempfile::tempdir().expect("tmp");
    let (handle, _store) = spawn_device(1, "Laptop", &fake, dir.path().to_path_buf(), None);
    let h = handle.clone();
    let fk = fake.clone();
    h.login(move |url| {
        tokio::spawn(async move { drive_login(&fk, url).await });
    })
    .await
    .expect("login");
    let mut rx = handle.state();
    let _ = wait_state(rx.clone(), |s| matches!(s, CloudState::Pending { .. })).await;
    handle.genesis().await.expect("genesis");
    let _ = is_active(&mut rx).await;

    fake.drop_all_sockets();
    // The socket drops → Connecting (backoff) → the relay is still up →
    // Active again. Trust/LoggedOut never auto-reconnect, so this must
    // land back on Active.
    let s = wait_state(rx.clone(), |s| matches!(s, CloudState::Active { .. })).await;
    assert!(
        matches!(s, CloudState::Active { .. }),
        "reconnects to Active after a drop, got {s:?}"
    );
}

#[tokio::test]
async fn token_401_deletes_the_token_and_keeps_the_seed() {
    let fake = FakeRelay::spawn().await;
    let dir = tempfile::tempdir().expect("tmp");
    let (handle, store) = spawn_device(1, "Laptop", &fake, dir.path().to_path_buf(), None);
    let h = handle.clone();
    let fk = fake.clone();
    h.login(move |url| {
        tokio::spawn(async move { drive_login(&fk, url).await });
    })
    .await
    .expect("login");
    let mut rx = handle.state();
    let _ = wait_state(rx.clone(), |s| matches!(s, CloudState::Pending { .. })).await;

    let tok = token_of(&store).expect("a token after login");
    fake.revoke_token(tok.as_str());
    fake.drop_all_sockets();

    let s = wait_state(rx.clone(), |s| matches!(s, CloudState::LoggedOut { .. })).await;
    assert_eq!(
        s,
        CloudState::LoggedOut {
            reason: peakbot::cloud::LogoutReason::Expired
        },
        "a 401 on /ws expires the token, got {s:?}"
    );
    assert!(token_of(&store).is_none(), "the token is deleted");
    assert!(
        store.load_seed().expect("seed").is_some(),
        "the seed survives (re-login keeps the same identity)"
    );
}

// ── D.6: open() contract ───────────────────────────────────────────────────

#[tokio::test]
async fn open_fails_with_not_active_when_logged_out() {
    let fake = FakeRelay::spawn().await;
    let dir = tempfile::tempdir().expect("tmp");
    let (handle, _store) = spawn_device(1, "Laptop", &fake, dir.path().to_path_buf(), None);
    let err = handle
        .open(id_b(), Op::Attach)
        .await
        .expect_err("not active");
    assert_eq!(
        err,
        CloudError::NotActive(peakbot::cloud::CloudStateKind::LoggedOut)
    );
}

#[tokio::test]
async fn open_fails_with_unknown_device() {
    let fake = FakeRelay::spawn().await;
    let dir = tempfile::tempdir().expect("tmp");
    let (handle, _store) = spawn_device(1, "Laptop", &fake, dir.path().to_path_buf(), None);
    let h = handle.clone();
    let fk = fake.clone();
    h.login(move |url| {
        tokio::spawn(async move { drive_login(&fk, url).await });
    })
    .await
    .expect("login");
    let mut rx = handle.state();
    let _ = wait_state(rx.clone(), |s| matches!(s, CloudState::Pending { .. })).await;
    handle.genesis().await.expect("genesis");
    let _ = is_active(&mut rx).await;

    // A device that is NOT in chain.active.
    let unknown = [0x77; 32];
    let err = handle.open(unknown, Op::Attach).await.expect_err("unknown");
    assert_eq!(err, CloudError::UnknownDevice, "{err:?}");
}

// ── D.5: a Profiles op round-trips between two links ───────────────────────

#[tokio::test]
async fn profiles_op_round_trips_between_two_links() {
    let fake = FakeRelay::spawn().await;
    let dir_a = tempfile::tempdir().expect("tmp a");
    let dir_b = tempfile::tempdir().expect("tmp b");

    // B's inbound hook: a stub that captures (device, op, stream) — the
    // real CloudServe is PR-C.
    let (cap_tx, mut cap_rx) = tokio::sync::mpsc::unbounded_channel::<(
        cloud::DeviceId,
        Op,
        peakbot::cloud::stream::CloudStream,
    )>();
    let inbound: InboundHook = Arc::new(move |dev, op, stream| {
        let _ = cap_tx.send((dev, op, stream));
    });

    let (handle_a, _sa) = spawn_device(1, "Laptop", &fake, dir_a.path().to_path_buf(), None);
    let h = handle_a.clone();
    let fk = fake.clone();
    h.login(move |url| {
        tokio::spawn(async move { drive_login(&fk, url).await });
    })
    .await
    .expect("login A");
    let mut rx_a = handle_a.state();
    let _ = wait_state(rx_a.clone(), |s| matches!(s, CloudState::Pending { .. })).await;
    handle_a.genesis().await.expect("genesis A");
    let _ = is_active(&mut rx_a).await;

    // Put B in the chain (a valid Add, signed by A) so A may open to B.
    let e0 = fake.entries().first().expect("genesis").clone();
    let h0 = cloud::chain::entry_hash(&e0);
    fake.append_entry(devices::add_entry(1, 1, h0, &card_b()));
    let _ = wait_state(
        rx_a.clone(),
        |s| matches!(s, CloudState::Active { devices, .. } if devices.len() == 2),
    )
    .await;

    // B logs in (now Active, since it is in chain.active) with the hook.
    let (handle_b, _sb) = spawn_device(
        2,
        "Desktop",
        &fake,
        dir_b.path().to_path_buf(),
        Some(inbound),
    );
    let h = handle_b.clone();
    let fk = fake.clone();
    h.login(move |url| {
        tokio::spawn(async move { drive_login(&fk, url).await });
    })
    .await
    .expect("login B");
    let mut rx_b = handle_b.state();
    let _ = is_active(&mut rx_b).await;

    // A opens Op::Profiles to B.
    let mut stream_a = handle_a.open(id_b(), Op::Profiles).await.expect("open A→B");
    stream_a
        .send(StreamMsg::Frame("hello-from-a".into()))
        .await
        .expect("send");

    // B's inbound hook receives (A, Profiles, stream).
    let (dev, op, mut stream_b) = tokio::time::timeout(TIMEOUT, cap_rx.recv())
        .await
        .expect("B's hook fired")
        .expect("hook payload");
    assert_eq!(dev, id_a(), "B sees A as the peer");
    assert_eq!(op, Op::Profiles, "the op is Profiles");

    let got = tokio::time::timeout(TIMEOUT, stream_b.next())
        .await
        .expect("B reads")
        .expect("a frame");
    assert_eq!(got, StreamMsg::Frame("hello-from-a".into()));

    // Round-trip: B answers, A reads it.
    stream_b
        .send(StreamMsg::Frame("pong-from-b".into()))
        .await
        .expect("B sends");
    let back = tokio::time::timeout(TIMEOUT, stream_a.next())
        .await
        .expect("A reads")
        .expect("a frame");
    assert_eq!(back, StreamMsg::Frame("pong-from-b".into()));
}

// ── G.7: simultaneous handshake tie-break ──────────────────────────────────

#[tokio::test]
async fn simultaneous_handshake_tie_breaks_to_one_session() {
    let fake = FakeRelay::spawn().await;
    let dir_a = tempfile::tempdir().expect("tmp a");
    let dir_b = tempfile::tempdir().expect("tmp b");

    // Two inbound hooks so both sides can receive an inbound open.
    let (cap_a, mut rx_a_hook) = tokio::sync::mpsc::unbounded_channel::<(
        cloud::DeviceId,
        Op,
        peakbot::cloud::stream::CloudStream,
    )>();
    let (cap_b, mut rx_b_hook) = tokio::sync::mpsc::unbounded_channel::<(
        cloud::DeviceId,
        Op,
        peakbot::cloud::stream::CloudStream,
    )>();
    let inbound_a: InboundHook = Arc::new({
        let tx = cap_a.clone();
        move |dev, op, stream| {
            let _ = tx.send((dev, op, stream));
        }
    });
    let inbound_b: InboundHook = Arc::new({
        let tx = cap_b.clone();
        move |dev, op, stream| {
            let _ = tx.send((dev, op, stream));
        }
    });

    let (handle_a, _) = spawn_device(
        1,
        "Laptop",
        &fake,
        dir_a.path().to_path_buf(),
        Some(inbound_a),
    );
    let h = handle_a.clone();
    let fk = fake.clone();
    h.login(move |url| {
        tokio::spawn(async move { drive_login(&fk, url).await });
    })
    .await
    .expect("login A");
    let mut rx_a = handle_a.state();
    let _ = wait_state(rx_a.clone(), |s| matches!(s, CloudState::Pending { .. })).await;
    handle_a.genesis().await.expect("genesis A");
    let _ = is_active(&mut rx_a).await;

    let e0 = fake.entries().first().expect("genesis").clone();
    let h0 = cloud::chain::entry_hash(&e0);
    fake.append_entry(devices::add_entry(1, 1, h0, &card_b()));
    let _ = wait_state(
        rx_a.clone(),
        |s| matches!(s, CloudState::Active { devices, .. } if devices.len() == 2),
    )
    .await;

    let (handle_b, _) = spawn_device(
        2,
        "Desktop",
        &fake,
        dir_b.path().to_path_buf(),
        Some(inbound_b),
    );
    let h = handle_b.clone();
    let fk = fake.clone();
    h.login(move |url| {
        tokio::spawn(async move { drive_login(&fk, url).await });
    })
    .await
    .expect("login B");
    let mut rx_b = handle_b.state();
    let _ = is_active(&mut rx_b).await;

    // Both initiate to the other at (nearly) the same time.
    let ha = handle_a.clone();
    let hb = handle_b.clone();
    let open_a = tokio::spawn(async move { ha.open(id_b(), Op::Attach).await });
    let open_b = tokio::spawn(async move { hb.open(id_a(), Op::Attach).await });
    let (res_a, res_b) = tokio::time::timeout(TIMEOUT, async { futures::join!(open_a, open_b) })
        .await
        .expect("both opens finish");
    let stream_a = res_a
        .expect("join A")
        .expect("A's open succeeds (tie-break)");
    let stream_b = res_b
        .expect("join B")
        .expect("B's open succeeds (tie-break)");

    // Exactly ONE established session: data flows both ways through the
    // same pair. (The lower-DeviceId initiator wins; the other responds.)
    let mut a = stream_a;
    let mut b = stream_b;
    a.send(StreamMsg::Frame("a→b".into()))
        .await
        .expect("a sends");
    b.send(StreamMsg::Frame("b→a".into()))
        .await
        .expect("b sends");
    let m1 = tokio::time::timeout(TIMEOUT, a.next())
        .await
        .expect("a reads")
        .expect("frame");
    let m2 = tokio::time::timeout(TIMEOUT, b.next())
        .await
        .expect("b reads")
        .expect("frame");
    assert_eq!(m1, StreamMsg::Frame("b→a".into()));
    assert_eq!(m2, StreamMsg::Frame("a→b".into()));

    // No spurious inbound hooks fired (the loser of the tie-break became a
    // responder, not a fresh inbound session).
    assert!(rx_a_hook.try_recv().is_err(), "A got no extra inbound hook");
    assert!(rx_b_hook.try_recv().is_err(), "B got no extra inbound hook");
}

// ── A: the phone / controller-only link refuses inbound Hs1 ────────────────

#[tokio::test]
async fn a_link_without_an_inbound_hook_refuses_hs1() {
    let fake = FakeRelay::spawn().await;
    let dir_a = tempfile::tempdir().expect("tmp a");
    let dir_b = tempfile::tempdir().expect("tmp b");

    let (handle_a, _) = spawn_device(1, "Laptop", &fake, dir_a.path().to_path_buf(), None);
    let h = handle_a.clone();
    let fk = fake.clone();
    h.login(move |url| {
        tokio::spawn(async move { drive_login(&fk, url).await });
    })
    .await
    .expect("login A");
    let mut rx_a = handle_a.state();
    let _ = wait_state(rx_a.clone(), |s| matches!(s, CloudState::Pending { .. })).await;
    handle_a.genesis().await.expect("genesis A");
    let _ = is_active(&mut rx_a).await;

    let e0 = fake.entries().first().expect("genesis").clone();
    let h0 = cloud::chain::entry_hash(&e0);
    fake.append_entry(devices::add_entry(1, 1, h0, &card_b()));
    let _ = wait_state(
        rx_a.clone(),
        |s| matches!(s, CloudState::Active { devices, .. } if devices.len() == 2),
    )
    .await;

    // B is the phone: `inbound = None` (controller-only, edition table §A).
    let (handle_b, _) = spawn_device(2, "Desktop", &fake, dir_b.path().to_path_buf(), None);
    let h = handle_b.clone();
    let fk = fake.clone();
    h.login(move |url| {
        tokio::spawn(async move { drive_login(&fk, url).await });
    })
    .await
    .expect("login B");
    let mut rx_b = handle_b.state();
    let _ = is_active(&mut rx_b).await;

    // A tries to open to B. B has no inbound hook, so it answers the Hs1
    // with Close{PROTOCOL} and A's open must fail (not hang).
    let res = tokio::time::timeout(TIMEOUT, handle_a.open(id_b(), Op::Attach)).await;
    match res {
        Ok(Ok(_)) => panic!("open to a controller-only device must not succeed"),
        Ok(Err(e)) => {
            // Offline / Reset / Trust are all acceptable refusals; the point
            // is it failed rather than opened.
            assert!(!matches!(e, CloudError::NotActive(_)), "{e:?}");
        }
        Err(_) => panic!("open to a controller-only device must not hang"),
    }
}

/// Keep the DeviceInfo import used (devices list shape).
#[allow(dead_code)]
fn _info(d: &DeviceInfo) -> &cloud::DeviceId {
    &d.id
}
