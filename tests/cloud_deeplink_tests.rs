//! P5 T4 RED tests — `CallbackSource::DeepLink` (design §2 C1/C2, E-F3).
//! Fake relay + fake IdP (`cloud_common::fake_relay`); no browser, no
//! loopback socket: the test plays the OS and delivers the URL.
//!
//! RED today: `deliver_callback` is `todo!()`, `spawn_with_callback` ignores
//! `DeepLink`, and `login()` still cancels a pending login instead of
//! refusing with `LoginInFlight`.

mod cloud_common;

use cloud_common::fake_relay::FakeRelay;
use cloud_common::rig::{self, Dev, spawn_device, spawn_device_deeplink};
use peakbot::cloud::{CloudError, CloudState, FlowError, SeedStore};
use url::Url;

const REDIRECT: &str = "sh.shifu.mobile:/cb";

fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client")
}

/// Start `login()` and hand back the auth URL the "browser" would open.
async fn start_login(dev: &Dev) -> Result<Url, CloudError> {
    let (tx, rx) = std::sync::mpsc::channel();
    dev.handle
        .login(move |url| {
            tx.send(url).ok();
        })
        .await?;
    Ok(rx.recv().expect("open_browser was called"))
}

/// Play the relay + IdP: GET the auth URL, mint a code → `(code, state)`.
async fn idp_dance(fake: &FakeRelay, auth_url: &Url) -> (String, String) {
    let resp = no_redirect_client()
        .get(auth_url.clone())
        .send()
        .await
        .expect("GET auth url");
    assert_eq!(
        resp.status(),
        302,
        "relay accepts the redirect and starts the flow"
    );
    let loc = resp.headers()["location"].to_str().unwrap().to_string();
    let relay_state = loc
        .split("relay_state=")
        .nth(1)
        .expect("relay_state")
        .split('&')
        .next()
        .unwrap();
    fake.issue_code(relay_state)
}

fn link(code: &str, state: &str) -> String {
    format!("{REDIRECT}?code={code}&state={state}")
}

fn flow_err(r: Result<(), CloudError>) -> Option<FlowError> {
    match r {
        Err(CloudError::Flow(f)) => Some(f),
        _ => None,
    }
}

fn logged_out(dev: &Dev) -> bool {
    matches!(dev.state(), CloudState::LoggedOut { .. })
}

#[tokio::test]
async fn deep_link_login_requests_the_mobile_redirect_uri() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device_deeplink(1, "Phone", &fake);
    let url = start_login(&dev).await.expect("login");
    let redirect = url
        .query_pairs()
        .find(|(k, _)| k == "redirect_uri")
        .map(|(_, v)| v.into_owned());
    assert_eq!(
        redirect.as_deref(),
        Some(REDIRECT),
        "no loopback port on mobile"
    );
}

#[tokio::test]
async fn deep_link_completes_login_and_stores_token() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device_deeplink(1, "Phone", &fake);
    let url = start_login(&dev).await.expect("login");
    let (code, state) = idp_dance(&fake, &url).await;
    dev.handle
        .deliver_callback(&link(&code, &state))
        .await
        .expect("deliver_callback");
    assert!(dev.store.load_token().unwrap().is_some(), "token stored");
    let st = rig::wait_state(dev.handle.state(), |s| {
        matches!(s, CloudState::Pending { .. } | CloudState::Active { .. })
    })
    .await;
    assert!(
        matches!(st, CloudState::Pending { .. } | CloudState::Active { .. }),
        "the actor reconnects after TokenStored: {st:?}"
    );
    assert_eq!(dev.handle.last_login_error(), None);
}

#[tokio::test]
async fn wrong_state_is_rejected_then_the_right_state_still_completes() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device_deeplink(1, "Phone", &fake);
    let url = start_login(&dev).await.expect("login");
    let (code, state) = idp_dance(&fake, &url).await;
    let r = dev
        .handle
        .deliver_callback(&link(&code, "not-the-state"))
        .await;
    assert!(r.is_err(), "state mismatch is an error");
    assert!(
        dev.store.load_token().unwrap().is_none(),
        "no token on a stray link"
    );
    assert!(logged_out(&dev), "still logged out: {:?}", dev.state());
    assert!(dev.handle.last_login_error().is_some(), "error surfaced");
    // The stray link did not consume the pending login (slot KEPT).
    dev.handle
        .deliver_callback(&link(&code, &state))
        .await
        .expect("the genuine callback still completes");
    assert!(dev.store.load_token().unwrap().is_some());
}

#[tokio::test]
async fn deep_link_without_login_in_flight_is_no_login_pending_and_ignored() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device_deeplink(1, "Phone", &fake);
    let r = dev.handle.deliver_callback(&link("c", "s")).await;
    assert_eq!(flow_err(r), Some(FlowError::NoLoginPending));
    assert!(logged_out(&dev));
    assert!(dev.store.load_token().unwrap().is_none());
}

#[tokio::test]
async fn deep_link_on_a_loopback_core_is_no_login_pending() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device(1, "Laptop", &fake, None);
    let _url = start_login(&dev).await.expect("loopback login");
    let r = dev.handle.deliver_callback(&link("c", "s")).await;
    assert_eq!(flow_err(r), Some(FlowError::NoLoginPending));
}

#[tokio::test]
async fn deep_link_replay_after_success_is_no_login_pending() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device_deeplink(1, "Phone", &fake);
    let url = start_login(&dev).await.expect("login");
    let (code, state) = idp_dance(&fake, &url).await;
    dev.handle
        .deliver_callback(&link(&code, &state))
        .await
        .expect("first");
    let again = dev.handle.deliver_callback(&link(&code, &state)).await;
    assert_eq!(
        flow_err(again),
        Some(FlowError::NoLoginPending),
        "slot is Idle"
    );
}

#[tokio::test]
async fn garbage_foreign_and_wrong_path_links_are_errors_and_keep_the_pending_login() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device_deeplink(1, "Phone", &fake);
    let url = start_login(&dev).await.expect("login");
    let (code, state) = idp_dance(&fake, &url).await;
    for bad in [
        "",
        "not a url at all",
        "\u{1F980}://\u{0}",
        &format!("evil.app:/cb?code={code}&state={state}"),
        &format!("sh.shifu.mobile:/other?code={code}&state={state}"),
        &format!("{REDIRECT}?code={code}"),
    ] {
        assert!(
            dev.handle.deliver_callback(bad).await.is_err(),
            "{bad:?} must be an error, not a panic or a login"
        );
    }
    assert!(dev.store.load_token().unwrap().is_none());
    dev.handle
        .deliver_callback(&link(&code, &state))
        .await
        .expect("pending login survived the stray links");
}

#[tokio::test]
async fn idp_error_param_fails_the_login_and_frees_the_slot() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device_deeplink(1, "Phone", &fake);
    let url = start_login(&dev).await.expect("login");
    let (_code, state) = idp_dance(&fake, &url).await;
    let r = dev
        .handle
        .deliver_callback(&format!("{REDIRECT}?error=access_denied&state={state}"))
        .await;
    assert!(r.is_err());
    assert!(
        dev.handle
            .last_login_error()
            .is_some_and(|e| e.contains("access_denied")),
        "{:?}",
        dev.handle.last_login_error()
    );
    assert!(dev.store.load_token().unwrap().is_none());
    start_login(&dev)
        .await
        .expect("a fresh login is allowed after a failed one");
}

// ── E-F3: a second concurrent login is refused, never cancels the first ──

#[tokio::test]
async fn second_login_while_deep_link_login_pending_is_login_in_flight() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device_deeplink(1, "Phone", &fake);
    let url = start_login(&dev).await.expect("first login");
    let second = dev
        .handle
        .login(|_| panic!("must not open a browser"))
        .await;
    assert_eq!(flow_err(second), Some(FlowError::LoginInFlight));
    // …and the first login is intact.
    let (code, state) = idp_dance(&fake, &url).await;
    dev.handle
        .deliver_callback(&link(&code, &state))
        .await
        .expect("first login still completes");
}

#[tokio::test]
async fn second_login_while_loopback_login_pending_is_login_in_flight() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device(1, "Laptop", &fake, None);
    let _url = start_login(&dev).await.expect("first login");
    let second = dev
        .handle
        .login(|_| panic!("must not open a browser"))
        .await;
    assert_eq!(flow_err(second), Some(FlowError::LoginInFlight));
}
