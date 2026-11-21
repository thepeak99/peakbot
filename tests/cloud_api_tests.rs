//! P3 PR-B — `/api/cloud/*` web API tests (p3-design §H), written against
//! the locked `peakbot::ui::web::cloud_api` seam BEFORE the behaviour
//! exists. Every test must compile and fail on `todo!()` or an assertion.
//!
//! Same harness style as `tests/system_api_tests.rs`: the real router on a
//! random loopback port, the production `require_token` layer when a token
//! is set, reqwest as the client.
//!
//! ── TEST → SPEC map (§H.1–H.3) ──────────────────────────────────────────
//!   state_route_renders_logged_out_dto           H.1 state
//!   login_route_returns_auth_url_second_is_409   H.1 login
//!   genesis_route_returns_device_id_then_409     H.1 genesis
//!   devices_route_lists_devices_and_409          H.1 devices
//!   approve_route_returns_device_id              H.1 approve
//!   approve_unknown_request_is_404               H.1 approve
//!   approve_chain_busy_is_409                    H.1 approve / G.4-3
//!   reject_route_returns_empty_object            H.1 reject
//!   reject_unknown_request_is_404                H.1 reject
//!   revoke_route_revokes_a_peer                  H.1 revoke / G.5
//!   revoke_self_is_400_use_logout                H.1 revoke / G.5
//!   revoke_unknown_device_is_404                 H.1 revoke
//!   revoke_bad_device_id_is_400                  H.1 extractor
//!   logout_route_returns_revoked_and_wipes       H.1 logout / G.6
//!   events_sse_emits_full_state_on_change        H.2
//!   request_without_token_gets_401               token layer (H intro)
//!   settings_filter_blocks_mutating_routes       H.3
//!   cloud_disabled_returns_404_json              H intro (no general.cloud)
//!   profiles_route_not_active_is_409             H.1 profiles

#[path = "cloud_common/mod.rs"]
mod cloud_common;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::middleware::from_fn_with_state;
use cloud::frames::Op;
use futures::{Stream, StreamExt};
use peakbot::cloud::{CloudState, SeedStore};
use peakbot::ui::web::cloud_api::{CloudApiState, router};
use peakbot::ui::web::require_token;
use serde_json::Value;
use url::Url;

use cloud_common::devices as dev;
use cloud_common::fake_relay::FakeRelay;
use cloud_common::rig::{self, TIMEOUT, spawn_device};

const WEB_TOKEN: &str = "cloud-test-token";

/// Mount `cloud_api::router` behind the production token layer (when set)
/// on a random loopback port.
async fn spawn_app(state: CloudApiState, token: Option<&str>) -> SocketAddr {
    let mut app: Router = router(state);
    if let Some(t) = token {
        let secret: Arc<str> = t.into();
        app = app.layer(from_fn_with_state(secret, require_token));
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    addr
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn api(addr: SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

/// The `{"error": <code>, "message": …}` envelope's code.
fn err_code(body: &Value) -> &str {
    body["error"]
        .as_str()
        .unwrap_or_else(|| panic!("expected the JSON error envelope, got {body}"))
}

/// POST /api/cloud/login → parse `auth_url` (the listener keeps running).
async fn post_login(c: &reqwest::Client, addr: SocketAddr) -> Url {
    let resp = c
        .post(api(addr, "/api/cloud/login"))
        .send()
        .await
        .expect("login request");
    assert_eq!(resp.status(), 200, "POST /api/cloud/login must be 200");
    let body: Value = resp.json().await.expect("login body is JSON");
    Url::parse(body["auth_url"].as_str().expect("auth_url")).expect("auth url")
}

/// Read the next `data:` payload of an SSE stream within the timeout.
async fn next_sse_data(
    stream: &mut (impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin),
) -> Value {
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "no SSE data within {TIMEOUT:?}; got: {buf}"
        );
        let chunk = tokio::time::timeout(remaining, stream.next())
            .await
            .expect("SSE deadline")
            .expect("the SSE stream ended early")
            .expect("SSE read");
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find('\n') {
            let line = buf[..pos].trim_end_matches('\r').to_string();
            buf.drain(..=pos);
            if let Some(data) = line.strip_prefix("data:") {
                return serde_json::from_str(data.trim()).expect("SSE data is JSON");
            }
        }
    }
}

#[tokio::test]
async fn state_route_renders_logged_out_dto() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device(1, "Laptop", &fake, None);
    let addr = spawn_app(
        CloudApiState {
            handle: Some(dev.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;

    let resp = client()
        .get(api(addr, "/api/cloud/state"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["state"], "logged_out", "{body}");
    assert_eq!(body["reason"], "never", "{body}");
}

#[tokio::test]
async fn login_route_returns_auth_url_second_is_409_login_in_flight() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device(1, "Laptop", &fake, None);
    let addr = spawn_app(
        CloudApiState {
            handle: Some(dev.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c = client();

    let url = post_login(&c, addr).await;
    assert!(
        url.as_str().contains("/login?"),
        "the auth url starts the relay's /login flow, got {url}"
    );
    // A second login while the first listener runs → 409 login_in_flight.
    // RED (regression): the API's duplicate precheck masked the handle's
    // `LoginInFlight` as `already_logged_in`, so the pane showed a red
    // "Already signed in." mid-flow. Removing the precheck lets the inner
    // `CloudHandle::login` refusal (link.rs) surface truthfully here.
    let resp = c.post(api(addr, "/api/cloud/login")).send().await.unwrap();
    assert_eq!(resp.status(), 409, "one listener at a time (G.1 step 5)");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(err_code(&body), "login_in_flight", "{body}");

    // Let the first flow finish.
    rig::drive_login(&fake, url).await;
    let _ = rig::wait_state(dev.handle.state(), |s| {
        matches!(s, CloudState::Pending { .. } | CloudState::Active { .. })
    })
    .await;
}

#[tokio::test]
async fn genesis_route_returns_device_id_then_409() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device(1, "Laptop", &fake, None);
    let addr = spawn_app(
        CloudApiState {
            handle: Some(dev.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c = client();

    rig::drive_login(&fake, post_login(&c, addr).await).await;
    let _ = rig::wait_state(dev.handle.state(), |s| {
        matches!(s, CloudState::Pending { .. })
    })
    .await;

    let resp = c
        .post(api(addr, "/api/cloud/genesis"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let id = body["device_id"].as_str().expect("device_id hex");
    assert_eq!(id.len(), 64, "a DeviceId is 64 lowercase hex: {id:?}");
    assert!(
        id.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );

    let _ = rig::wait_state(dev.handle.state(), |s| {
        matches!(s, CloudState::Active { .. })
    })
    .await;
    // No longer Pending{empty}: the route refuses.
    let resp = c
        .post(api(addr, "/api/cloud/genesis"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(err_code(&body), "not_pending_empty", "{body}");
}

#[tokio::test]
async fn devices_route_lists_devices_and_409() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device(1, "Laptop", &fake, None);
    let addr = spawn_app(
        CloudApiState {
            handle: Some(dev.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c = client();

    // Not active yet: 409 not_active.
    let resp = c.get(api(addr, "/api/cloud/devices")).send().await.unwrap();
    assert_eq!(resp.status(), 409);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(err_code(&body), "not_active", "{body}");

    // Genesis, then the list.
    rig::drive_login(&fake, post_login(&c, addr).await).await;
    let _ = rig::wait_state(dev.handle.state(), |s| {
        matches!(s, CloudState::Pending { .. })
    })
    .await;
    c.post(api(addr, "/api/cloud/genesis"))
        .send()
        .await
        .unwrap();
    let s = rig::wait_state(dev.handle.state(), |s| {
        matches!(s, CloudState::Active { .. })
    })
    .await;
    let CloudState::Active { devices, .. } = s else {
        panic!("must be Active")
    };

    let resp = c.get(api(addr, "/api/cloud/devices")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["self"],
        dev::hex(&devices[0].id),
        "self is this device's hex id"
    );
    let list = body["devices"].as_array().expect("devices[]");
    assert_eq!(list.len(), 1);
    for key in ["id", "name", "platform", "slug", "self"] {
        assert!(
            list[0].get(key).is_some(),
            "DeviceDto missing {key}: {body}"
        );
    }
    assert_eq!(list[0]["name"], "Laptop");
    assert_eq!(list[0]["self"], true);
    // `online` is `null` until a pipe answers (D.1) — it may be present as
    // null OR absent (serde skip); never a fabricated true.
    match list[0].get("online") {
        None | Some(Value::Null) => {}
        Some(v) => panic!("online must be null/absent before any pipe: {v}"),
    }
}

#[tokio::test]
async fn approve_route_returns_device_id() {
    let fake = FakeRelay::spawn().await;
    let (a, b) = rig::a_active_b_pending(&fake).await;
    let addr = spawn_app(
        CloudApiState {
            handle: Some(a.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c = client();

    let s = rig::wait_state(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { requests, .. } if !requests.is_empty()),
    )
    .await;
    let CloudState::Active { requests, .. } = s else {
        panic!("A must hold the request")
    };
    let letter_id = dev::hex(&requests[0].letter_id);

    let resp = c
        .post(api(addr, "/api/cloud/enroll/approve"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "letter_id": letter_id }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["device_id"],
        dev::hex(&b.handle.device_id().expect("B id")),
        "approve returns the new device's id"
    );
    let _ = b.active().await;
}

#[tokio::test]
async fn approve_unknown_request_is_404() {
    let fake = FakeRelay::spawn().await;
    let (a, _b) = rig::a_active_b_pending(&fake).await;
    let addr = spawn_app(
        CloudApiState {
            handle: Some(a.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c = client();

    let resp = c
        .post(api(addr, "/api/cloud/enroll/approve"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "letter_id": "00000000000000000000000000000000" }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "unknown (or expired) request → 404");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(err_code(&body), "unknown_request", "{body}");
}

#[tokio::test]
async fn approve_chain_busy_is_409() {
    let fake = FakeRelay::spawn().await;
    let (a, _b) = rig::a_active_b_pending(&fake).await;
    let addr = spawn_app(
        CloudApiState {
            handle: Some(a.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c = client();

    let s = rig::wait_state(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { requests, .. } if !requests.is_empty()),
    )
    .await;
    let CloudState::Active { requests, .. } = s else {
        panic!("A must hold the request")
    };
    let letter_id = dev::hex(&requests[0].letter_id);

    // The relay answers CONFLICT twice (head moved twice) → 409 chain_busy
    // (G.4 step 3: rebuild once, then give up).
    fake.conflict_next_appends(2);
    let resp = c
        .post(api(addr, "/api/cloud/enroll/approve"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "letter_id": letter_id }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409, "second conflict → 409 chain_busy");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(err_code(&body), "chain_busy", "{body}");
}

#[tokio::test]
async fn reject_route_returns_empty_object() {
    let fake = FakeRelay::spawn().await;
    let (a, b) = rig::a_active_b_pending(&fake).await;
    let addr = spawn_app(
        CloudApiState {
            handle: Some(a.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c = client();

    let s = rig::wait_state(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { requests, .. } if !requests.is_empty()),
    )
    .await;
    let CloudState::Active { requests, .. } = s else {
        panic!("A must hold the request")
    };
    let letter_id = dev::hex(&requests[0].letter_id);

    let resp = c
        .post(api(addr, "/api/cloud/enroll/reject"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "letter_id": letter_id }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body,
        Value::Object(Default::default()),
        "reject answers an empty object"
    );
    // The request is gone and B sees the rejection.
    let _ = rig::wait_state(b.handle.state(), |s| {
        matches!(s, CloudState::Pending { sas: None, .. })
    })
    .await;
}

#[tokio::test]
async fn reject_unknown_request_is_404() {
    let fake = FakeRelay::spawn().await;
    let (a, _b) = rig::a_active_b_pending(&fake).await;
    let addr = spawn_app(
        CloudApiState {
            handle: Some(a.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c = client();

    let resp = c
        .post(api(addr, "/api/cloud/enroll/reject"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "letter_id": "00000000000000000000000000000000" }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(err_code(&body), "unknown_request", "{body}");
}

/// A: genesis, B + C enrolled → three active devices (revoke is
/// unambiguous; see the flow test note on `last_device`).
async fn three_active(
    fake: &FakeRelay,
) -> (
    cloud_common::rig::Dev,
    cloud_common::rig::Dev,
    cloud_common::rig::Dev,
) {
    let a = spawn_device(1, "Laptop", fake, None);
    a.login(fake).await;
    a.handle.genesis().await.expect("genesis");
    let _ = a.active().await;
    let b = spawn_device(2, "Desktop", fake, None);
    b.login(fake).await;
    let c = spawn_device(3, "Pixel 9", fake, None);
    c.login(fake).await;
    for _ in 0..2 {
        let s = rig::wait_state(
            a.handle.state(),
            |s| matches!(s, CloudState::Active { requests, .. } if !requests.is_empty()),
        )
        .await;
        let CloudState::Active { requests, .. } = s else {
            panic!("A must hold a request")
        };
        a.handle
            .approve(requests[0].letter_id)
            .await
            .expect("approve");
    }
    let _ = b.active().await;
    let _ = c.active().await;
    let _ = rig::wait_state(
        a.handle.state(),
        |s| matches!(s, CloudState::Active { devices, .. } if devices.len() == 3),
    )
    .await;
    (a, b, c)
}

#[tokio::test]
async fn revoke_route_revokes_a_peer() {
    let fake = FakeRelay::spawn().await;
    let (a, _b, c) = three_active(&fake).await;
    let addr = spawn_app(
        CloudApiState {
            handle: Some(a.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c_client = client();

    let resp = c_client
        .post(api(
            addr,
            &format!(
                "/api/cloud/devices/{}/revoke",
                cloud_common::devices::hex(&cloud_common::devices::id_c())
            ),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = c_client
        .get(api(addr, "/api/cloud/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let _ = body; // state is asserted via the actor below (H.1 revoke answers {})
    let s = rig::wait_state(c.handle.state(), |s| matches!(s, CloudState::Trust(_))).await;
    assert!(
        matches!(s, CloudState::Trust(peakbot::cloud::TrustError::Revoked)),
        "C must be Trust(Revoked), got {s:?}"
    );
}

#[tokio::test]
async fn revoke_self_is_400_use_logout() {
    let fake = FakeRelay::spawn().await;
    let (a, _b, _c) = three_active(&fake).await;
    let addr = spawn_app(
        CloudApiState {
            handle: Some(a.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c = client();

    let self_id = dev::hex(&a.handle.device_id().expect("A id"));
    let resp = c
        .post(api(addr, &format!("/api/cloud/devices/{self_id}/revoke")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "self-revoke is logout-only (G.5)");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(err_code(&body), "use_logout", "{body}");
}

#[tokio::test]
async fn revoke_unknown_device_is_404() {
    let fake = FakeRelay::spawn().await;
    let (a, _b, _c) = three_active(&fake).await;
    let addr = spawn_app(
        CloudApiState {
            handle: Some(a.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c = client();

    let unknown = "77".repeat(32);
    let resp = c
        .post(api(addr, &format!("/api/cloud/devices/{unknown}/revoke")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(err_code(&body), "unknown_device", "{body}");
}

#[tokio::test]
async fn revoke_bad_device_id_is_400() {
    let fake = FakeRelay::spawn().await;
    let (a, _b, _c) = three_active(&fake).await;
    let addr = spawn_app(
        CloudApiState {
            handle: Some(a.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c = client();

    for bad in ["nothex".to_string(), "ZZ".repeat(32), "77".repeat(31)] {
        let resp = c
            .post(api(addr, &format!("/api/cloud/devices/{bad}/revoke")))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            400,
            "bad DeviceId → 400 bad_device_id: {bad}"
        );
        let body: Value = resp.json().await.unwrap();
        assert_eq!(err_code(&body), "bad_device_id", "{body}");
    }
}

#[tokio::test]
async fn logout_route_returns_revoked_and_wipes() {
    let fake = FakeRelay::spawn().await;
    let (_a, b) = rig::enrolled_pair(&fake).await;
    let token = b
        .store
        .load_token()
        .expect("store")
        .expect("B token")
        .as_str()
        .to_string();
    let addr = spawn_app(
        CloudApiState {
            handle: Some(b.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c = client();

    let resp = c.post(api(addr, "/api/cloud/logout")).send().await.unwrap();
    assert_eq!(resp.status(), 200, "logout always succeeds locally (G.6)");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["revoked"], true,
        "A remains, so the self-revoke ran: {body}"
    );

    assert!(b.store.load_seed().expect("store").is_none(), "seed wiped");
    assert!(
        b.store.load_token().expect("store").is_none(),
        "token wiped"
    );
    assert!(!fake.token_valid(&token), "relay session gone");
    let s = rig::wait_state(b.handle.state(), |s| {
        matches!(s, CloudState::LoggedOut { .. })
    })
    .await;
    assert!(
        matches!(
            s,
            CloudState::LoggedOut {
                reason: peakbot::cloud::LogoutReason::SignedOut
            }
        ),
        "B is SignedOut, got {s:?}"
    );
}

#[tokio::test]
async fn events_sse_emits_full_state_on_change() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device(1, "Laptop", &fake, None);
    let addr = spawn_app(
        CloudApiState {
            handle: Some(dev.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c = client();

    let resp = c.get(api(addr, "/api/cloud/events")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .expect("content-type")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        ct.contains("text/event-stream"),
        "SSE content type, got {ct:?}"
    );
    let mut stream = resp.bytes_stream();

    // On connect: the CURRENT state, as a full DTO (H.2).
    let first = next_sse_data(&mut stream).await;
    assert_eq!(first["state"], "logged_out", "connect event: {first}");
    assert_eq!(first["reason"], "never");

    // Change 1: login → pending.
    tokio::spawn({
        let fake = fake.clone();
        let url = post_login(&client(), addr).await;
        async move { rig::drive_login(&fake, url).await }
    });
    let second = next_sse_state_skipping(&mut stream, "connecting").await;
    assert_eq!(second["state"], "pending", "login event: {second}");
    assert_eq!(second["empty_account"], true);

    // Change 2: genesis → active.
    c.post(api(addr, "/api/cloud/genesis"))
        .send()
        .await
        .unwrap();
    let third = next_sse_state_skipping(&mut stream, "connecting").await;
    assert_eq!(third["state"], "active", "genesis event: {third}");
    assert_eq!(third["devices"].as_array().expect("devices[]").len(), 1);
}

/// The next SSE state that is not the transient `skip` state: H.2 sends one
/// full event per `watch` change, and the actor passes through
/// `connecting` (login redial, post-genesis re-Hello) on the way.
async fn next_sse_state_skipping(
    stream: &mut (impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin),
    skip: &str,
) -> Value {
    loop {
        let v = next_sse_data(stream).await;
        if v["state"] != skip {
            return v;
        }
    }
}

#[tokio::test]
async fn request_without_token_gets_401() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device(1, "Laptop", &fake, None);
    let addr = spawn_app(
        CloudApiState {
            handle: Some(dev.handle.clone()),
            settings_enabled: true,
        },
        Some(WEB_TOKEN),
    )
    .await;
    let c = client();

    // Every cloud route is inside the token layer.
    for (verb, path) in [
        ("get", "/api/cloud/state"),
        ("get", "/api/cloud/devices"),
        ("post", "/api/cloud/login"),
        ("post", "/api/cloud/logout"),
    ] {
        let url = api(addr, path);
        let req = match verb {
            "get" => c.get(url),
            _ => c.post(url),
        };
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status(), 401, "{verb} {path} must 401 without token");
    }

    // The token in the query admits (and sets the cookie).
    let resp = c
        .get(format!(
            "{}?token={WEB_TOKEN}",
            api(addr, "/api/cloud/state")
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "the web token admits");
}

#[tokio::test]
async fn settings_filter_blocks_mutating_routes() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device(1, "Laptop", &fake, None);
    let addr = spawn_app(
        CloudApiState {
            handle: Some(dev.handle.clone()),
            settings_enabled: false, // settings tab filtered off (H.3)
        },
        None,
    )
    .await;
    let c = client();

    // The six mutating routes → 403 settings_disabled.
    let revoke_path = format!("/api/cloud/devices/{}/revoke", "77".repeat(32));
    let cases: Vec<(&str, &str)> = vec![
        ("/api/cloud/login", "{}"),
        ("/api/cloud/genesis", "{}"),
        (
            "/api/cloud/enroll/approve",
            r#"{"letter_id":"00000000000000000000000000000000"}"#,
        ),
        (
            "/api/cloud/enroll/reject",
            r#"{"letter_id":"00000000000000000000000000000000"}"#,
        ),
        (revoke_path.as_str(), ""),
        ("/api/cloud/logout", "{}"),
    ];
    for (path, body) in cases {
        let resp = c
            .post(api(addr, path))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            403,
            "POST {path} must 403 under the settings filter"
        );
        let v: Value = resp.json().await.unwrap();
        assert_eq!(err_code(&v), "settings_disabled", "{v}");
    }

    // Readable routes stay readable (409 not_active is fine — NOT 403).
    let resp = c.get(api(addr, "/api/cloud/state")).send().await.unwrap();
    assert_eq!(resp.status(), 200, "state stays readable");
    let resp = c.get(api(addr, "/api/cloud/devices")).send().await.unwrap();
    assert_ne!(resp.status(), 403, "devices stays readable");
}

#[tokio::test]
async fn cloud_disabled_returns_404_json() {
    // No `general.cloud` ⇒ `handle: None`: every route 404s
    // {"error":"cloud_disabled"} but the router stays mounted (H intro).
    let addr = spawn_app(
        CloudApiState {
            handle: None,
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c = client();

    for (verb, path) in [
        ("get", "/api/cloud/state"),
        ("get", "/api/cloud/devices"),
        ("get", "/api/cloud/events"),
        ("post", "/api/cloud/login"),
        ("post", "/api/cloud/genesis"),
        ("post", "/api/cloud/logout"),
    ] {
        let url = api(addr, path);
        let req = match verb {
            "get" => c.get(url),
            _ => c.post(url),
        };
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status(), 404, "{verb} {path} must 404 when disabled");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(err_code(&body), "cloud_disabled", "{body}");
    }
}

#[tokio::test]
async fn profiles_route_not_active_is_409() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device(1, "Laptop", &fake, None);
    let addr = spawn_app(
        CloudApiState {
            handle: Some(dev.handle.clone()),
            settings_enabled: true,
        },
        None,
    )
    .await;
    let c = client();

    let resp = c
        .get(api(
            addr,
            &format!(
                "/api/cloud/devices/{}/profiles",
                cloud_common::devices::hex(&cloud_common::devices::id_b())
            ),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409, "not Active → 409 not_active");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(err_code(&body), "not_active", "{body}");
    let _ = Op::Profiles; // the happy path (profiles list) needs PR-C's CloudServe
}

// ── P4 E-F7: `rejected` in the Pending DTO + POST /api/cloud/enroll/retry ──

async fn app_for(dev: &cloud_common::rig::Dev, settings_enabled: bool) -> SocketAddr {
    spawn_app(
        CloudApiState {
            handle: Some(dev.handle.clone()),
            settings_enabled,
        },
        None,
    )
    .await
}

async fn get_state(c: &reqwest::Client, addr: SocketAddr) -> Value {
    let resp = c.get(api(addr, "/api/cloud/state")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

/// Reject B's request through A's handle and wait until B shows it.
async fn reject_b(a: &cloud_common::rig::Dev, b: &cloud_common::rig::Dev) {
    let s = rig::wait_state(
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
    let _ = rig::wait_state(b.handle.state(), |s| {
        matches!(s, CloudState::Pending { sas: None, .. })
    })
    .await;
}

#[tokio::test]
async fn state_json_for_pending_includes_rejected_false() {
    let fake = FakeRelay::spawn().await;
    let (_a, b) = rig::a_active_b_pending(&fake).await;
    let addr = app_for(&b, true).await;

    let body = get_state(&client(), addr).await;
    assert_eq!(body["state"], "pending", "{body}");
    assert_eq!(body["rejected"], false, "rejected is always sent: {body}");
    assert_eq!(body["sas"].as_array().map(Vec::len), Some(6), "{body}");
}

#[tokio::test]
async fn state_json_for_rejected_pending_has_rejected_true_and_null_sas() {
    let fake = FakeRelay::spawn().await;
    let (a, b) = rig::a_active_b_pending(&fake).await;
    reject_b(&a, &b).await;
    let addr = app_for(&b, true).await;

    let body = get_state(&client(), addr).await;
    assert_eq!(body["state"], "pending", "{body}");
    assert_eq!(body["rejected"], true, "{body}");
    assert!(body["sas"].is_null(), "rejected ⇒ sas is null: {body}");
}

#[tokio::test]
async fn retry_route_after_rejection_returns_empty_object_and_redials() {
    let fake = FakeRelay::spawn().await;
    let (a, b) = rig::a_active_b_pending(&fake).await;
    reject_b(&a, &b).await;
    let addr = app_for(&b, true).await;
    let c = client();

    let resp = c
        .post(api(addr, "/api/cloud/enroll/retry"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v, Value::Object(Default::default()), "retry answers {{}}");

    let _ = rig::wait_state(b.handle.state(), |s| {
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
    let body = get_state(&c, addr).await;
    assert_eq!(body["rejected"], false, "{body}");
    assert_eq!(body["sas"].as_array().map(Vec::len), Some(6), "{body}");
}

#[tokio::test]
async fn retry_route_409_not_rejected_when_not_rejected() {
    let fake = FakeRelay::spawn().await;
    let (_a, b) = rig::a_active_b_pending(&fake).await;
    let addr = app_for(&b, true).await;

    let resp = client()
        .post(api(addr, "/api/cloud/enroll/retry"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(err_code(&v), "not_rejected", "{v}");
}

#[tokio::test]
async fn retry_route_403_when_settings_disabled() {
    let fake = FakeRelay::spawn().await;
    let (_a, b) = rig::a_active_b_pending(&fake).await;
    let addr = app_for(&b, false).await;

    let resp = client()
        .post(api(addr, "/api/cloud/enroll/retry"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(err_code(&v), "settings_disabled", "{v}");
}

#[tokio::test]
async fn retry_route_without_token_gets_401() {
    let fake = FakeRelay::spawn().await;
    let dev = spawn_device(1, "Laptop", &fake, None);
    let addr = spawn_app(
        CloudApiState {
            handle: Some(dev.handle.clone()),
            settings_enabled: true,
        },
        Some(WEB_TOKEN),
    )
    .await;

    let resp = client()
        .post(api(addr, "/api/cloud/enroll/retry"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}
