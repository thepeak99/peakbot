//! Shared rig for the flow / API tests: spawn a CloudLink for device `n`
//! against a `FakeRelay`, drive the browser-less PKCE login, and wait on
//! the published state.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use peakbot::cloud::login::{AppRedirect, CallbackSource};
use peakbot::cloud::store::MemSeedStore;
use peakbot::cloud::{
    CloudConfig, CloudHandle, CloudState, InboundHook, Seed, SeedStore, spawn_with_callback,
};
use url::Url;

use super::fake_relay::FakeRelay;

pub const TIMEOUT: Duration = Duration::from_secs(20);

pub async fn wait_state(
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
            return last;
        }
        tokio::select! {
            _ = tokio::time::sleep(remaining) => return last,
            r = rx.changed() => { if r.is_err() { return last; } last = rx.borrow().clone(); }
        }
    }
}

/// One test device: its link, its in-memory store, its state dir.
pub struct Dev {
    pub handle: CloudHandle,
    pub store: Arc<MemSeedStore>,
    pub dir: tempfile::TempDir,
}

pub fn spawn_device(n: u8, name: &str, fake: &FakeRelay, inbound: Option<InboundHook>) -> Dev {
    spawn_device_with(n, name, fake, inbound, CallbackSource::Loopback)
}

/// P5: a mobile-style device — the OAuth callback arrives as a deep link.
pub fn spawn_device_deeplink(n: u8, name: &str, fake: &FakeRelay) -> Dev {
    spawn_device_with(
        n,
        name,
        fake,
        None,
        CallbackSource::DeepLink(AppRedirect::mobile()),
    )
}

pub fn spawn_device_with(
    n: u8,
    name: &str,
    fake: &FakeRelay,
    inbound: Option<InboundHook>,
    callback: CallbackSource,
) -> Dev {
    let dir = tempfile::tempdir().expect("tmp");
    let store = Arc::new(MemSeedStore::new());
    store.store_seed(&Seed::new([n; 64])).expect("seed");
    let cfg = CloudConfig {
        relay: peakbot::config::remote::Endpoint::LoopbackPlain {
            origin: Url::parse(&fake.url()).expect("origin"),
        },
        state_dir: PathBuf::from(dir.path()),
        device_name: name.to_string(),
        platform: 1,
    };
    let handle = spawn_with_callback(cfg, store.clone(), inbound, callback);
    Dev { handle, store, dir }
}

/// A client that does NOT follow the relay's 302 (the tests assert on it).
fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client")
}

/// GET the auth URL (302 → fake IdP), mint the code, hit the loopback `/cb`.
pub async fn drive_login(fake: &FakeRelay, auth_url: Url) {
    let resp = no_redirect_client()
        .get(auth_url.clone())
        .send()
        .await
        .expect("GET auth url");
    assert_eq!(resp.status(), 302, "the auth url starts a 302 flow");
    let loc = resp
        .headers()
        .get("location")
        .expect("location")
        .to_str()
        .expect("s")
        .to_string();
    let relay_state = loc
        .split("relay_state=")
        .nth(1)
        .expect("relay_state")
        .split('&')
        .next()
        .expect("rs");
    let (code, state) = fake.issue_code(relay_state);
    let redirect = auth_url
        .query_pairs()
        .find(|(k, _)| k == "redirect_uri")
        .map(|(_, v)| v.into_owned())
        .expect("redirect_uri");
    let _ = reqwest::get(format!("{redirect}?code={code}&state={state}"))
        .await
        .expect("/cb");
}

impl Dev {
    /// Log in through the fake IdP and wait for `Pending`/`Active`.
    pub async fn login(&self, fake: &FakeRelay) {
        let fk = fake.clone();
        self.handle
            .login(move |url| {
                tokio::spawn(async move { drive_login(&fk, url).await });
            })
            .await
            .expect("login");
        let _ = wait_state(self.handle.state(), |s| {
            matches!(s, CloudState::Pending { .. } | CloudState::Active { .. })
        })
        .await;
    }

    pub async fn active(&self) -> CloudState {
        wait_state(self.handle.state(), |s| {
            matches!(s, CloudState::Active { .. })
        })
        .await
    }

    pub fn state(&self) -> CloudState {
        self.handle.state().borrow().clone()
    }
}

/// A = genesis (Active); B = logged in, Pending with a posted request.
pub async fn a_active_b_pending(fake: &FakeRelay) -> (Dev, Dev) {
    let a = spawn_device(1, "Laptop", fake, None);
    a.login(fake).await;
    a.handle.genesis().await.expect("genesis");
    let _ = a.active().await;
    let b = spawn_device(2, "Desktop", fake, None);
    b.login(fake).await;
    let _ = wait_state(b.handle.state(), |s| {
        matches!(s, CloudState::Pending { sas: Some(_), .. })
    })
    .await;
    (a, b)
}

/// Both Active: B's request approved by A.
pub async fn enrolled_pair(fake: &FakeRelay) -> (Dev, Dev) {
    let (a, b) = a_active_b_pending(fake).await;
    let s = wait_state(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { requests, .. } if !requests.is_empty()),
    )
    .await;
    let CloudState::Active { requests, .. } = s else {
        panic!("A has a request: {s:?}")
    };
    a.handle
        .approve(requests[0].letter_id)
        .await
        .expect("approve");
    let _ = b.active().await;
    let _ = wait_state(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { devices, .. } if devices.len() == 2),
    )
    .await;
    (a, b)
}
