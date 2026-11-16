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
