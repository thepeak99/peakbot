//! Test for the `build_app` API seam (spec item 2).
//!
//! `build_app(cfg)` should return the relay Router + RelayHandle without
//! binding the public service itself, allowing the caller to add routes
//! (e.g. /probe) and serve it. Existing `serve` behavior must remain.
//!
//! Currently RED (compile-time): build_app does not exist yet.
//!
//! TODO (spec item 3 - CA contract): once the relay example exposes a CA
//! generation/fingerprint utility, add tests that:
//!   1. The CA fingerprint is stable across two loads from one temp dir
//!   2. The fingerprint format is `sha256:` + 64 lowercase hex chars
//!      computed over the CA DER
//!   3. The CA PEM corresponds to what's served by the example
//! These cannot be written now without the implementation helper existing;
//! adding them would produce only an unhelpful compile error.

mod common;
use common::*;

/// build_app returns a Router + handle; caller can mount extra routes and
/// serve on their own listener. This test adds a /probe route and verifies
/// both the probe and the relay's /login respond correctly.
#[tokio::test]
async fn build_app_allows_custom_routes() {
    let idp = FakeIdp::spawn(false).await;
    let tmp = tempfile::tempdir().unwrap();
    let secret_file = tmp.path().join("idp.secret");
    std::fs::write(&secret_file, format!("{}\n", idp.client_secret)).unwrap();
    let bind: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(bind).await.unwrap();
    let addr = listener.local_addr().unwrap();

    let cfg = peakbot_relay::config::Config {
        bind: addr,
        origin: peakbot_relay::config::Origin(format!("http://{addr}")),
        db: tmp.path().join("relay.db"),
        app_scheme: None,
        issuers: vec![peakbot_relay::config::Issuer {
            name: "fake".to_string(),
            url: idp.url.clone(),
            client_id: peakbot_relay::config::ClientId(idp.client_id.clone()),
            client_secret: peakbot_relay::config::ClientSecret::new(idp.client_secret.clone()),
        }],
    };

    // build_app(cfg) returns (Router, RelayHandle) — the caller owns serving.
    let (app, handle) = peakbot_relay::build_app(cfg).await.expect("build_app");

    // Mount a custom /probe route.
    let app = app.route(
        "/probe",
        axum::routing::get(|| async { axum::http::StatusCode::OK }),
    );

    // Serve on the listener we bound.
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    // Custom route works.
    let probe = http
        .get(format!("http://{addr}/probe"))
        .send()
        .await
        .unwrap();
    assert_eq!(probe.status(), axum::http::StatusCode::OK, "/probe works");

    // Relay route also works.
    let login = http
        .get(format!(
            "http://{addr}/login?code_challenge={}&redirect_uri=http://127.0.0.1:9/cb&state=s1",
            "test_challenge"
        ))
        .send()
        .await
        .unwrap();
    // Login will fail on PKCE verification (bad challenge), but the route must exist.
    assert!(
        login.status().is_client_error() || login.status().is_server_error(),
        "/login route must be reachable (got {})",
        login.status()
    );

    server.abort();
    handle.shutdown().await;
}

/// Existing `serve` behavior remains unchanged: build a config and call serve
/// directly; the relay must be accessible.
#[tokio::test]
async fn serve_still_works_without_build_app() {
    let t = TestRelay::spawn().await;
    let flow = t
        .begin_login(DEFAULT_REDIRECT)
        .await
        .expect("serve still works with direct call");
    assert!(!flow.nonce.is_empty());
}
