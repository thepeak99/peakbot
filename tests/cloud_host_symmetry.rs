//! Pipe v3 symmetric-session regressions.
//!
//! Before pipe v3, only the session's *initiator* could send `Open`: a
//! responder-side `open()` was rejected locally with
//! `Reset(UNSUPPORTED)`. That makes the second direction of an established
//! peer pair unusable. These tests drive real CloudLink actors over the
//! in-test FakeRelay and assert that BOTH peers may open streams, that a
//! controller-only peer (`inbound = None`) refuses reverse opens *fast*,
//! and that the original direction survives every failure.
//!
//! Every network/request step is wrapped in a timeout so a hang is a
//! failure, not a stuck test run. The peers use canned inbound hooks
//! (Profiles responder + Attach capture) so the assertions are about the
//! actor's stream routing, not the web UI.

mod cloud_common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cloud::DeviceId;
use cloud::frames::{Op, StreamMsg};
use cloud_common::devices::{id_a, id_b};
use cloud_common::fake_relay::FakeRelay;
use cloud_common::rig::{Dev, TIMEOUT, spawn_device, wait_state};
use futures::{SinkExt, StreamExt};
use peakbot::cloud::login::CallbackSource;
use peakbot::cloud::session::ProfileInfo;
use peakbot::cloud::stream::CloudStream;
use peakbot::cloud::{CloudConfig, CloudError, CloudState, InboundHook, spawn_with_callback};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use url::Url;

/// A reverse open to a controller-only peer must fail with this code.
const REVERSE_REFUSAL_BOUND: Duration = Duration::from_secs(5);
/// A request to a shut-down peer must error within this bound (well under
/// the 30 s handshake timer, so a hang is a failure).
const OFFLINE_BOUND: Duration = Duration::from_secs(10);

// ── fixtures ───────────────────────────────────────────────────────────────

fn profiles_json(name: &str) -> String {
    // The actor parses the wire body as a BARE `Vec<ProfileInfo>` array
    // (see `do_profiles` / `profiles_body`), not an object wrapper.
    format!("[{{\"name\":\"{name}\",\"color\":\"#336699\"}}]")
}

/// A canned peer: answers `Profiles` with a fixed JSON body and hands every
/// inbound `Attach` stream to the test (or resets it when no capture channel
/// is supplied). No web UI, no real host core; this isolates session/stream
/// routing.
fn peer_hook(profiles: &str, attach_tx: Option<UnboundedSender<CloudStream>>) -> InboundHook {
    let body = Arc::new(profiles.as_bytes().to_vec());
    Arc::new(move |_peer, op, mut stream| match op {
        Op::Profiles => {
            let body = body.clone();
            tokio::spawn(async move {
                let _ = stream
                    .send(StreamMsg::Head {
                        status: 200,
                        content_type: "application/json".into(),
                        length: Some(body.len() as u64),
                    })
                    .await;
                let _ = stream.send(StreamMsg::Chunk(body.to_vec())).await;
                let _ = stream.send(StreamMsg::End).await;
                // Keep our side alive until the requester's End arrives, so
                // dropping the stream cannot race a Reset into a success.
                loop {
                    match stream.next_timed(TIMEOUT).await {
                        Ok(Some(StreamMsg::End | StreamMsg::Reset(_))) | Ok(None) | Err(_) => {
                            break;
                        }
                        Ok(Some(_)) => {}
                    }
                }
            });
        }
        Op::Attach => match &attach_tx {
            Some(tx) => {
                let _ = tx.send(stream);
            }
            None => {
                tokio::spawn(async move {
                    let _ = stream
                        .send(StreamMsg::Reset(cloud::constants::reset::UNSUPPORTED))
                        .await;
                });
            }
        },
        _ => {
            tokio::spawn(async move {
                let _ = stream
                    .send(StreamMsg::Reset(cloud::constants::reset::UNSUPPORTED))
                    .await;
            });
        }
    })
}

/// A and B on one fake relay, both Active, with caller-supplied inbound
/// hooks (A is the genesis device).
async fn pair_with_hooks(
    fake: &FakeRelay,
    hook_a: Option<InboundHook>,
    hook_b: Option<InboundHook>,
) -> (Dev, Dev) {
    let a = spawn_device(1, "Laptop", fake, hook_a);
    a.login(fake).await;
    a.handle.genesis().await.expect("genesis A");
    let _ = a.active().await;

    let b = spawn_device(2, "Desktop", fake, hook_b);
    b.login(fake).await;
    let s = wait_state(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { requests, .. } if !requests.is_empty()),
    )
    .await;
    let CloudState::Active { requests, .. } = s else {
        panic!("A must see B's enroll request, got {s:?}")
    };
    a.handle
        .approve(requests[0].letter_id)
        .await
        .expect("approve B");
    let _ = b.active().await;
    let _ = wait_state(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { devices, .. } if devices.len() == 2),
    )
    .await;
    (a, b)
}

fn config_for(name: &str, dir: &Path, fake: &FakeRelay) -> CloudConfig {
    CloudConfig {
        relay: peakbot::config::remote::Endpoint::LoopbackPlain {
            origin: Url::parse(&fake.url()).expect("origin"),
        },
        state_dir: dir.to_path_buf(),
        device_name: name.to_string(),
        platform: 1,
    }
}

// ── timeouts / stream helpers ──────────────────────────────────────────────

async fn profiles_result_within(
    dev: &Dev,
    peer: DeviceId,
    what: &str,
    bound: Duration,
) -> Result<Vec<ProfileInfo>, CloudError> {
    tokio::time::timeout(bound, dev.handle.profiles(peer))
        .await
        .unwrap_or_else(|_| panic!("{what}: profiles({peer:?}) hung > {bound:?}"))
}

async fn profiles_result(
    dev: &Dev,
    peer: DeviceId,
    what: &str,
) -> Result<Vec<ProfileInfo>, CloudError> {
    profiles_result_within(dev, peer, what, TIMEOUT).await
}

async fn profiles_ok(dev: &Dev, peer: DeviceId, expected: &str, what: &str) -> Vec<ProfileInfo> {
    let got = profiles_result(dev, peer, what)
        .await
        .unwrap_or_else(|e| panic!("{what}: profiles({peer:?}) failed: {e:?}"));
    assert!(
        got.iter().any(|p| p.name == expected),
        "{what}: expected profile {expected:?}, got {got:?}"
    );
    got
}

async fn open_attach(dev: &Dev, peer: DeviceId, what: &str) -> CloudStream {
    tokio::time::timeout(TIMEOUT, dev.handle.open(peer, Op::Attach))
        .await
        .unwrap_or_else(|_| panic!("{what}: open Attach hung > {TIMEOUT:?}"))
        .unwrap_or_else(|e| panic!("{what}: open Attach failed: {e:?}"))
}

async fn next_msg(stream: &mut CloudStream, what: &str) -> StreamMsg {
    match tokio::time::timeout(TIMEOUT, stream.next()).await {
        Ok(Some(msg)) => msg,
        Ok(None) => panic!("{what}: stream closed"),
        Err(_) => panic!("{what}: no message within {TIMEOUT:?}"),
    }
}

async fn send_msg(stream: &mut CloudStream, msg: StreamMsg, what: &str) {
    match tokio::time::timeout(TIMEOUT, stream.send(msg)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => panic!("{what}: send failed: {e:?}"),
        Err(_) => panic!("{what}: send hung > {TIMEOUT:?}"),
    }
}

async fn recv_attach(rx: &mut UnboundedReceiver<CloudStream>, what: &str) -> CloudStream {
    tokio::time::timeout(TIMEOUT, rx.recv())
        .await
        .unwrap_or_else(|_| panic!("{what}: inbound Attach hook did not fire"))
        .unwrap_or_else(|| panic!("{what}: inbound hook channel closed"))
}

/// Two-way frame exchange proving an Attach stream is still usable (a
/// Reset would make `next_msg` return `Reset`, failing the equality).
async fn attach_roundtrip(a: &mut CloudStream, b: &mut CloudStream, label: &str) {
    let a_msg = format!("{label}-a-to-b");
    send_msg(a, StreamMsg::Frame(a_msg.clone()), label).await;
    assert_eq!(
        next_msg(b, &format!("{label}: b receives")).await,
        StreamMsg::Frame(a_msg),
        "{label}: the original direction must survive"
    );
    let b_msg = format!("{label}-b-to-a");
    send_msg(b, StreamMsg::Frame(b_msg.clone()), label).await;
    assert_eq!(
        next_msg(a, &format!("{label}: a receives")).await,
        StreamMsg::Frame(b_msg),
        "{label}: the reverse direction must survive"
    );
}

async fn close_attach_pair(opener: &mut CloudStream, inbound: &mut CloudStream, label: &str) {
    send_msg(opener, StreamMsg::End, &format!("{label}: opener End")).await;
    assert_eq!(
        next_msg(inbound, &format!("{label}: inbound sees End")).await,
        StreamMsg::End
    );
    send_msg(inbound, StreamMsg::End, &format!("{label}: inbound End")).await;
    assert_eq!(
        next_msg(opener, &format!("{label}: opener sees End")).await,
        StreamMsg::End
    );
}

// ── relay readiness (condition waits, never arbitrary sleeps) ──────────────

async fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !ok() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: condition not met within {TIMEOUT:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_live_sockets(fake: &FakeRelay, n: usize) {
    wait_until(&format!("relay live sockets == {n}"), || {
        fake.live_sockets() == n
    })
    .await;
}

/// One retry after a reconnect only if the actor still reports
/// `NotActive(Connecting)`: that is readiness, not the role gate under
/// test. A `Reset` from the reverse-open path is never retried.
async fn profiles_ok_after_reconnect(
    dev: &Dev,
    peer: DeviceId,
    expected: &str,
    what: &str,
) -> Vec<ProfileInfo> {
    for attempt in 0..3 {
        match profiles_result(dev, peer, what).await {
            Ok(got) => {
                assert!(
                    got.iter().any(|p| p.name == expected),
                    "{what}: expected {expected:?}, got {got:?}"
                );
                return got;
            }
            Err(CloudError::NotActive(_)) if attempt < 2 => {
                let _ = wait_state(dev.handle.state(), |s| {
                    matches!(s, CloudState::Active { .. })
                })
                .await;
            }
            Err(e) => panic!("{what}: profiles({peer:?}) failed: {e:?}"),
        }
    }
    unreachable!("the loop returns or panics")
}

// ── 1. the role gate: A→B then B→A on the same established pair ────────────

#[tokio::test]
async fn a_then_b_then_a_profiles() {
    let fake = FakeRelay::spawn().await;
    let (a, b) = pair_with_hooks(
        &fake,
        Some(peer_hook(&profiles_json("from-a"), None)),
        Some(peer_hook(&profiles_json("from-b"), None)),
    )
    .await;

    // A is the handshake initiator: this direction works before and after.
    profiles_ok(&a, id_b(), "from-b", "A→B first direction").await;

    // A's session now exists with B as responder. B→A is the regression:
    // the responder-side open used to fail Reset(UNSUPPORTED).
    profiles_ok(&b, id_a(), "from-a", "B→A reverse direction").await;
}

// ── 2. reverse open while the outbound Attach is live ─────────────────────

#[tokio::test]
async fn reverse_profiles_while_outbound_attach_is_live() {
    let fake = FakeRelay::spawn().await;
    let (a_attach_tx, mut a_attach_rx) = unbounded_channel();
    let (a, b) = pair_with_hooks(
        &fake,
        Some(peer_hook(&profiles_json("from-a"), Some(a_attach_tx))),
        Some(peer_hook(&profiles_json("from-b"), None)),
    )
    .await;

    // B opens Attach to A and A holds the inbound stream. B is the
    // initiator; A is therefore the responder when it later calls
    // profiles(B).
    let mut b_holds = open_attach(&b, id_a(), "B→A attach").await;
    let mut a_holds = recv_attach(&mut a_attach_rx, "A inbound attach").await;
    attach_roundtrip(&mut b_holds, &mut a_holds, "before reverse profile").await;

    // The regression: A (responder) opens a second, independent stream.
    profiles_ok(&a, id_b(), "from-b", "reverse Profiles while Attach live").await;

    // The original Attach must be untouched by the reverse open.
    attach_roundtrip(&mut b_holds, &mut a_holds, "after reverse profile").await;
}

// ── 3. simultaneous opens in both directions ─────────────────────────────

#[tokio::test]
async fn simultaneous_bidirectional_profiles() {
    // Small, deterministic count: several streams per direction on one
    // established session.
    const PER_DIRECTION: usize = 3;

    let fake = FakeRelay::spawn().await;
    let (a, b) = pair_with_hooks(
        &fake,
        Some(peer_hook(&profiles_json("from-a"), None)),
        Some(peer_hook(&profiles_json("from-b"), None)),
    )
    .await;

    // Warm the pair so this test isolates concurrent OPENS on one live
    // session. (The cold simultaneous-handshake tie-break has its own
    // regression test in cloud_link_tests.)
    profiles_ok(&a, id_b(), "from-b", "warm before concurrent opens").await;

    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..PER_DIRECTION {
        let ha = a.handle.clone();
        tasks.spawn(async move { ("A→B", n, ha.profiles(id_b()).await) });
        let hb = b.handle.clone();
        tasks.spawn(async move { ("B→A", n, hb.profiles(id_a()).await) });
    }
    let results = tokio::time::timeout(TIMEOUT, async {
        let mut out = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            out.push(joined.expect("profiles task panicked"));
        }
        out
    })
    .await
    .expect("simultaneous bidirectional profiles must finish");

    assert_eq!(results.len(), PER_DIRECTION * 2);
    for (direction, n, result) in results {
        let got = result.unwrap_or_else(|e| panic!("{direction} #{n} failed: {e:?}"));
        let expected = if direction == "A→B" {
            "from-b"
        } else {
            "from-a"
        };
        assert!(
            got.iter().any(|p| p.name == expected),
            "{direction} #{n}: expected {expected:?}, got {got:?}"
        );
    }
}

// ── 4. controller-only peer: fast UNSUPPORTED, original stream survives ───

#[tokio::test]
async fn controller_only_peer_refuses_reverse_open_fast() {
    let fake = FakeRelay::spawn().await;
    let (a_attach_tx, mut a_attach_rx) = unbounded_channel();
    // A = host (serves inbound); B = controller-only phone (inbound = None).
    let (a, b) = pair_with_hooks(
        &fake,
        Some(peer_hook(&profiles_json("from-a"), Some(a_attach_tx))),
        None,
    )
    .await;

    // B, the controller, opens Attach to A. B is the initiator, so A is
    // the responder for the reverse request below.
    let mut b_holds = open_attach(&b, id_a(), "controller attach").await;
    let mut a_holds = recv_attach(&mut a_attach_rx, "host inbound attach").await;
    attach_roundtrip(&mut b_holds, &mut a_holds, "before reverse refusal").await;

    // A reverse open cannot be served by an inbound-less peer. It must be
    // refused promptly and specifically, not hang until the 30 s handshake
    // timer (or, worse, appear to succeed).
    let r = tokio::time::timeout(REVERSE_REFUSAL_BOUND, a.handle.profiles(id_b()))
        .await
        .unwrap_or_else(|_| {
            panic!("reverse open to a controller-only peer hung > {REVERSE_REFUSAL_BOUND:?}")
        });
    assert_eq!(
        r,
        Err(CloudError::Reset(cloud::constants::reset::UNSUPPORTED)),
        "a controller-only peer must refuse a reverse open with Reset(UNSUPPORTED)"
    );

    // The controller's original outbound Attach is untouched.
    attach_roundtrip(&mut b_holds, &mut a_holds, "after reverse refusal").await;
}

// ── 5. peer shutdown → error; respawn same identity → first try works ─────

#[tokio::test]
async fn peer_shutdown_errors_then_respawned_peer_answers_first_try() {
    let fake = FakeRelay::spawn().await;
    let (a, b) =
        pair_with_hooks(&fake, None, Some(peer_hook(&profiles_json("from-b"), None))).await;

    profiles_ok(&a, id_b(), "from-b", "warm A→B").await;

    b.handle.shutdown().await;
    wait_live_sockets(&fake, 1).await;

    let r = profiles_result_within(&a, id_b(), "peer is down", OFFLINE_BOUND).await;
    assert!(
        r.is_err(),
        "a request to a shut-down peer must error, got {r:?}"
    );

    // Respawn the SAME identity from the same store and state dir.
    let Dev { store, dir, .. } = b;
    let handle = spawn_with_callback(
        config_for("Desktop", dir.path(), &fake),
        store.clone(),
        Some(peer_hook(&profiles_json("from-b"), None)),
        CallbackSource::Loopback,
    );
    let b = Dev { handle, store, dir };
    let _ = wait_state(b.handle.state(), |s| matches!(s, CloudState::Active { .. })).await;
    wait_live_sockets(&fake, 2).await;

    // No "warm-up" request: the first request after respawn must succeed.
    profiles_ok(&a, id_b(), "from-b", "first request after respawn").await;
}

// ── 6. relay reconnect → both directions still open ─────────────────────

#[tokio::test]
async fn relay_reconnect_both_directions_work() {
    let fake = FakeRelay::spawn().await;
    let (a, b) = pair_with_hooks(
        &fake,
        Some(peer_hook(&profiles_json("from-a"), None)),
        Some(peer_hook(&profiles_json("from-b"), None)),
    )
    .await;

    profiles_ok(&a, id_b(), "from-b", "before relay reconnect").await;

    let connects_before = fake.ws_connects();
    fake.drop_all_sockets();
    wait_until("both links redialled the relay", || {
        fake.live_sockets() == 2 && fake.ws_connects() >= connects_before + 2
    })
    .await;
    let _ = wait_state(a.handle.state(), |s| matches!(s, CloudState::Active { .. })).await;
    let _ = wait_state(b.handle.state(), |s| matches!(s, CloudState::Active { .. })).await;

    profiles_ok_after_reconnect(&a, id_b(), "from-b", "A→B after reconnect").await;
    profiles_ok_after_reconnect(&b, id_a(), "from-a", "B→A after reconnect").await;
}

// ── 7. stream churn in both namespaces, then both directions still work ──

#[tokio::test]
async fn alternating_stream_churn_keeps_both_directions_usable() {
    const ROUNDS: usize = 4;

    let fake = FakeRelay::spawn().await;
    let (a_attach_tx, mut a_attach_rx) = unbounded_channel();
    let (b_attach_tx, mut b_attach_rx) = unbounded_channel();
    let (a, b) = pair_with_hooks(
        &fake,
        Some(peer_hook(&profiles_json("from-a"), Some(a_attach_tx))),
        Some(peer_hook(&profiles_json("from-b"), Some(b_attach_tx))),
    )
    .await;

    // Establish the pair with A as initiator (A→B Profiles).
    profiles_ok(&a, id_b(), "from-b", "warm before churn").await;

    for round in 0..ROUNDS {
        // Responder namespace: B opens; A's mux must accept 2^31+n.
        let mut opener = open_attach(&b, id_a(), &format!("churn {round} B→A")).await;
        let mut inbound = recv_attach(&mut a_attach_rx, &format!("churn {round} B→A hook")).await;
        close_attach_pair(&mut opener, &mut inbound, &format!("churn {round} B→A")).await;
        drop(opener);
        drop(inbound);

        // Initiator namespace: A opens on the same session.
        let mut opener = open_attach(&a, id_b(), &format!("churn {round} A→B")).await;
        let mut inbound = recv_attach(&mut b_attach_rx, &format!("churn {round} A→B hook")).await;
        close_attach_pair(&mut opener, &mut inbound, &format!("churn {round} A→B")).await;
        drop(opener);
        drop(inbound);
    }

    profiles_ok(&a, id_b(), "from-b", "A→B after churn").await;
    profiles_ok(&b, id_a(), "from-a", "B→A after churn").await;
}
