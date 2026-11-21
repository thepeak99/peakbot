//! P5 RED repro — controller→peer hang (emulator bug): an enrolled
//! controller-only device (no CloudServe, like the Local-edition phone) and
//! an enrolled host are both `active`, yet the controller's
//! `profiles(<host>)` and a `/ws` attach to `profile@host` hang.
//!
//! The host is a real web core (CloudServe wired); the controller is a bare
//! link with `inbound = None`. Every request is wrapped in a timeout so a
//! hang is a FAIL, not a stuck run. Each request is the FIRST one to the
//! peer (no warm session), which is what the app does on open.

#![cfg(feature = "host")]

mod cloud_common;

use std::path::PathBuf;
use std::time::Duration;

use cloud_common::devices::id_a;
use cloud_common::fake_relay::FakeRelay;
use cloud_common::host::{YAML, boot_core, enroll_pair};
use cloud_common::rig::{Dev, wait_state};
use peakbot::cloud::login::CallbackSource;
use peakbot::cloud::{CloudConfig, CloudState, spawn_with_callback};
use url::Url;

/// A request to an online peer answers well within this.
const ANSWER: Duration = Duration::from_secs(10);
/// The bound for an offline peer (task spec: ≤ 15 s).
const OFFLINE_BOUND: Duration = Duration::from_secs(15);
/// Longer than the 3 s `set_foreground(false)` debounce.
const PAST_DEBOUNCE: Duration = Duration::from_millis(3600);

async fn profiles_within(dev: &Dev, bound: Duration, what: &str) -> String {
    let r = tokio::time::timeout(bound, dev.handle.profiles(id_a()))
        .await
        .unwrap_or_else(|_| panic!("{what}: profiles(host) hung > {bound:?}"));
    format!("{r:?}")
}

async fn active(dev: &Dev) {
    let _ = wait_state(dev.handle.state(), |s| {
        matches!(s, CloudState::Active { devices, .. } if devices.iter().any(|d| d.id == id_a()))
    })
    .await;
}

/// (i) baseline: the controller's first request to the host is `profiles`.
#[tokio::test]
async fn controller_fetches_host_profiles_cold() {
    let fake = FakeRelay::spawn().await;
    let (_core, hook) = boot_core(YAML).await;
    let (_host, ctl) = enroll_pair(&fake, hook).await;

    let p = profiles_within(&ctl, ANSWER, "cold").await;
    assert!(
        p.starts_with("Ok(") && p.contains("dev"),
        "host profiles: {p}"
    );
    // The link must still serve a second request (it is not wedged).
    let p = profiles_within(&ctl, ANSWER, "second").await;
    assert!(p.starts_with("Ok("), "second request: {p}");
}

/// (ii) unfocus → past the debounce (Suspend) → focus (Resume) → request.
#[tokio::test]
async fn controller_fetches_host_profiles_after_suspend_resume() {
    let fake = FakeRelay::spawn().await;
    let (_core, hook) = boot_core(YAML).await;
    let (_host, ctl) = enroll_pair(&fake, hook).await;

    ctl.handle.set_foreground(false).await;
    tokio::time::sleep(PAST_DEBOUNCE).await;
    ctl.handle.set_foreground(true).await;
    active(&ctl).await;

    let p = profiles_within(&ctl, ANSWER, "after resume").await;
    assert!(
        p.starts_with("Ok(") && p.contains("dev"),
        "host profiles: {p}"
    );
}

/// (iii) app restart: a new link on the same store + state dir boots from
/// the stored token (b935eb5 `Stage::Connecting` path).
#[tokio::test]
async fn controller_fetches_host_profiles_after_restart_from_stored_token() {
    let fake = FakeRelay::spawn().await;
    let (_core, hook) = boot_core(YAML).await;
    let (_host, ctl) = enroll_pair(&fake, hook).await;

    ctl.handle.shutdown().await;
    let cfg = CloudConfig {
        relay: peakbot::config::remote::Endpoint::LoopbackPlain {
            origin: Url::parse(&fake.url()).expect("origin"),
        },
        state_dir: PathBuf::from(ctl.dir.path()),
        device_name: "Desktop".to_string(),
        platform: 1,
    };
    let handle = spawn_with_callback(cfg, ctl.store.clone(), None, CallbackSource::Loopback);
    let ctl = Dev {
        handle,
        store: ctl.store,
        dir: ctl.dir,
    };
    active(&ctl).await;

    let p = profiles_within(&ctl, ANSWER, "after restart").await;
    assert!(
        p.starts_with("Ok(") && p.contains("dev"),
        "host profiles: {p}"
    );
}

/// (iv) the host is offline: the request errors within the bound.
#[tokio::test]
async fn request_to_an_offline_peer_errors_within_the_bound() {
    let fake = FakeRelay::spawn().await;
    let (_core, hook) = boot_core(YAML).await;
    let (host, ctl) = enroll_pair(&fake, hook).await;
    host.handle.shutdown().await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let p = profiles_within(&ctl, OFFLINE_BOUND, "offline").await;
    assert!(p.starts_with("Err("), "an error, not a list: {p}");
}

/// The bound for "a clean answer, not a hang" (P5 task spec: ≤ 5 s).
const BOUND: Duration = Duration::from_secs(5);

/// Open an Attach stream to the host for an agent the host cannot build and
/// return what the stream yields: the host's error frame, then the stream
/// ending (End/Reset/None). A stream reset BEFORE any frame is the bug.
async fn failed_attach(ctl: &Dev, what: &str) -> Vec<String> {
    use cloud::frames::{Op, StreamMsg};
    use futures::{SinkExt, StreamExt};
    let run = async {
        let mut s = ctl
            .handle
            .open(id_a(), Op::Attach)
            .await
            .map_err(|e| format!("open Attach: {e}"))?;
        s.send(StreamMsg::Frame(
            r#"{"type":"attach","profile":"dev","agent":"no-such-agent","relayed":true}"#
                .to_string(),
        ))
        .await
        .map_err(|e| format!("send attach frame: {e}"))?;
        let mut seen = Vec::new();
        while let Some(m) = s.next().await {
            match m {
                StreamMsg::Frame(f) => seen.push(f),
                other => {
                    seen.push(format!("{other:?}"));
                    break;
                }
            }
        }
        Ok::<_, String>(seen)
    };
    tokio::time::timeout(BOUND, run)
        .await
        .unwrap_or_else(|_| panic!("{what}: attach hung > {BOUND:?}"))
        .unwrap_or_else(|e| panic!("{what}: {e}"))
}

/// (v) P5 emulator bug: a session the HOST fails to start must not poison
/// the peer session — the next requests still get answered.
#[tokio::test]
async fn failed_session_on_host_does_not_kill_the_peer_session() {
    let fake = FakeRelay::spawn().await;
    let (_core, hook) = boot_core(YAML).await;
    let (_host, ctl) = enroll_pair(&fake, hook).await;
    active(&ctl).await;

    let first = failed_attach(&ctl, "first attach").await;
    assert!(
        first.first().is_some_and(|f| f.contains("\"error\"")),
        "the host must answer a clean error frame: {first:?}"
    );

    let p = profiles_within(&ctl, BOUND, "after failed session").await;
    assert!(
        p.starts_with("Ok(") && p.contains("dev"),
        "profiles after a failed session: {p}"
    );

    let second = failed_attach(&ctl, "second attach").await;
    assert!(
        second.first().is_some_and(|f| f.contains("\"error\"")),
        "a second attempt must get a clean error again, not a reset: {second:?}"
    );
    let p = profiles_within(&ctl, BOUND, "after second failed session").await;
    assert!(p.starts_with("Ok("), "profiles after two failures: {p}");

    // The phone drops its side silently (app backgrounded → Suspend clears
    // its sessions with no `Close`), while the host keeps its Open one. The
    // phone's fresh Hs1 must re-establish, not bounce off the stale session
    // with `Close{PROTOCOL}` ("stream reset: 6" in ~8 ms on the emulator).
    ctl.handle.set_foreground(false).await;
    tokio::time::sleep(PAST_DEBOUNCE).await;
    ctl.handle.set_foreground(true).await;
    active(&ctl).await;
    let p = profiles_within(&ctl, BOUND, "after the phone dropped its session").await;
    assert!(
        p.starts_with("Ok(") && p.contains("dev"),
        "profiles after re-handshake: {p}"
    );
    let third = failed_attach(&ctl, "attach after re-handshake").await;
    assert!(
        third.first().is_some_and(|f| f.contains("\"error\"")),
        "clean error after re-handshake: {third:?}"
    );
}

/// (vi) a request aimed at the device's OWN id is refused fast, not hung.
#[tokio::test]
async fn request_to_own_device_id_is_refused_within_the_bound() {
    let fake = FakeRelay::spawn().await;
    let (_core, hook) = boot_core(YAML).await;
    let (_host, ctl) = enroll_pair(&fake, hook).await;
    active(&ctl).await;

    let me = ctl.handle.device_id().expect("own id");
    let r = tokio::time::timeout(BOUND, ctl.handle.profiles(me))
        .await
        .unwrap_or_else(|_| panic!("profiles(self) hung > {BOUND:?}"));
    let p = format!("{r:?}");
    assert!(p.starts_with("Err("), "self-target must be an error: {p}");
}

/// (vii) RED repro — host restarts with the same identity; the controller's
/// session table still has the old session as Open. The host's session table
/// is empty, so it drops the controller's request frames silently. The
/// controller's request hangs forever (no timeout on an established stream).
/// After the fix, the first request should complete (Ok or Err) within 5 s,
/// and a second request should succeed within 10 s.
#[tokio::test]
async fn controller_recovers_after_host_restart() {
    let fake = FakeRelay::spawn().await;
    let (_core, hook) = boot_core(YAML).await;
    let (host, ctl) = enroll_pair(&fake, hook.clone()).await;
    active(&ctl).await;

    // Warm the session: controller sends a request to the host.
    let p = profiles_within(&ctl, ANSWER, "warm").await;
    assert!(p.starts_with("Ok("), "warm request: {p}");

    // Host restarts (same identity, same store, same config).
    host.handle.shutdown().await;
    let host_cfg = CloudConfig {
        relay: peakbot::config::remote::Endpoint::LoopbackPlain {
            origin: Url::parse(&fake.url()).expect("origin"),
        },
        state_dir: PathBuf::from(host.dir.path()),
        device_name: "Laptop".to_string(),
        platform: 1,
    };
    let host_handle =
        spawn_with_callback(host_cfg, host.store.clone(), hook, CallbackSource::Loopback);
    let new_host = Dev {
        handle: host_handle,
        store: host.store,
        dir: host.dir,
    };
    active(&new_host).await;

    // The controller's FIRST profiles request must complete (Ok or Err)
    // within 5 s, not hang. The host has an empty session table; the
    // controller still has the old session as Open. The bug: the request
    // hangs.
    let p = profiles_within(&ctl, BOUND, "after host restart").await;
    // The host restarted and lost session state; the controller's old
    // session is invalid. The request should fail (not hang).
    assert!(
        p.starts_with("Ok(") || p.starts_with("Err("),
        "request after host restart must complete, not hang: {p}"
    );

    // A SECOND profiles request, issued after the first completes, must
    // succeed (fresh handshake) within 10 s.
    let p = profiles_within(&ctl, ANSWER, "second after host restart").await;
    assert!(p.starts_with("Ok("), "second request: {p}");
}
