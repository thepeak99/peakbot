//! Remote-profiles T5 — `remote::Directory` + `remote::Client` (design §3,
//! §5.8, §5.12, §7, §9-T5). RED until the implementation lands.
//!
//! Written against the interface the design specifies, NOT an existing impl:
//!
//! * `super::Client::new(&RemoteProfile, ClientOpts) -> Result<Client, RemoteError>`
//!   performs the CA bootstrap ONCE per Client (§5.8): for `PinnedCa` it
//!   GETs `https://H/peakbot-ca.crt` with a no-verify config and NO
//!   credentials, then hard-fails on a fingerprint mismatch (E-TLS-PIN) —
//!   nothing else is ever sent. `System` builds the same trust policy
//!   `http.rs` uses. `LoopbackPlain` skips TLS.
//! * `Client::dial() -> Result<WsStream, RemoteError>` — TCP → TLS → WS
//!   upgrade of `/ws` carrying `Cookie: peakbot_token=<remote token>`.
//! * `Client::list() -> Result<Vec<ConversationSummaryWire>, RemoteError>` —
//!   `GET /api/conversations`, the T4 shape `{"items":[…]}`.
//! * `Client::kill(&Uuid) -> Result<Option<String>, RemoteError>` —
//!   `POST /api/conversations/{id}/kill`; `Some(msg)` is the remote's
//!   `error` field (§6.2), `None` on a clean kill.
//! * `super::Directory::new()` / `Directory::client(name) ->
//!   Result<Arc<Client>, RemoteError>` — fresh global-config parse on every
//!   call, `Client` cached per (profile, definition); a changed definition
//!   (token/url/ca) rebuilds it.
//! * `RemoteError`'s `Display` is the §5.12 locked string (L = profile name,
//!   H = `host:port` without slash).
//! * `ClientOpts { ca_fetch_timeout, dial_timeout }` — the doc's 10 s CA /
//!   20 s dial bounds as the `Default`, injectable so tests run in seconds.
//!
//! The E-VERSION exact-match check is deliberately NOT covered here: §9
//! assigns the `ready{v}` read to T6(f) (the relay). T5's `dial()` is
//! TCP → TLS → WS upgrade only.
//!
//! Harness: in-process axum remotes — TLS via `axum-server` with a leaf
//! signed by a fresh rcgen test CA (the same minting as `ui::web::tls`),
//! plain via `axum::serve`. A recording middleware captures every request
//! line and header the client sends, so the §7 token rules are asserted on
//! the wire.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::header;
use axum::http::{HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum_server::tls_rustls::RustlsConfig;
use futures::SinkExt;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, date_time_ymd,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message as TMsg;
use uuid::Uuid;

use super::*;
use crate::config::CONFIG_ENV_LOCK;
use crate::config::file::RemoteDef;
use crate::config::remote::parse_remote;

// ---------------------------------------------------------------------------
// Contract constants (locked by the design).
// ---------------------------------------------------------------------------

/// The cookie name the remote token travels in (§7: `Cookie:
/// peakbot_token=<remote>`; accepted by `require_token` on the remote).
const COOKIE_NAME: &str = "peakbot_token";

/// The remote token the fake remote expects — cookie-safe, no `=` or `;`.
const REMOTE_TOKEN: &str = "remote-token-abc123";

/// A local web token that must NEVER reach the remote (§7 token isolation).
const LOCAL_TOKEN: &str = "LOCAL-TOKEN-MUST-NOT-LEAK-9f3a";

/// Profile name L and target profile as used in every locked message.
const PROFILE: &str = "dev";

fn short_opts() -> ClientOpts {
    ClientOpts {
        ca_fetch_timeout: Duration::from_secs(2),
        dial_timeout: Duration::from_secs(2),
    }
}

fn profile(url: &str, ca: &str, token: &str) -> RemoteProfile {
    let def = RemoteDef {
        url: url.to_string(),
        token: token.to_string(),
        ca: Some(ca.to_string()),
        profile: PROFILE.to_string(),
        agent: None,
    };
    expect_profile(parse_remote(PROFILE, &def, None))
}

/// `parse_remote`'s error type is intentionally not `Debug` (the file layer
/// keeps tokens out of `Debug` prints), so no `.expect()` — format it by hand.
fn expect_profile(
    result: Result<RemoteProfile, Vec<crate::config::remote::RemoteFieldError>>,
) -> RemoteProfile {
    result.unwrap_or_else(|errors| {
        let detail = errors
            .iter()
            .map(|e| format!("{}: {}", e.field, e.message))
            .collect::<Vec<_>>()
            .join("; ");
        panic!("valid remote def, got: {detail}")
    })
}

// ---------------------------------------------------------------------------
// Test CA / leaf minting (mirrors `ui::web::tls`, in-test only).
// ---------------------------------------------------------------------------

struct TestCa {
    /// `sha256:<64 lower hex>` of the CA DER — what a `ca:` pin compares.
    fingerprint: String,
    ca_pem: String,
    issuer: Issuer<'static, KeyPair>,
}

fn ca_params() -> CertificateParams {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA certificate params");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.not_before = date_time_ymd(2020, 1, 1);
    params.not_after = date_time_ymd(2035, 1, 1);
    params
        .distinguished_name
        .push(DnType::CommonName, "PeakBot T5 Test CA");
    params
}

fn mint_ca() -> TestCa {
    let key = KeyPair::generate().expect("CA key pair");
    let ca_pem = ca_params().self_signed(&key).expect("self-signed CA").pem();
    let der = CertificateDer::from_pem_slice(ca_pem.as_bytes()).expect("CA DER");
    let hex: String = Sha256::digest(der.as_ref())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let issuer = Issuer::new(ca_params(), key);
    TestCa {
        fingerprint: format!("sha256:{hex}"),
        ca_pem,
        issuer,
    }
}

/// A rustls server config presenting a leaf for `sans`, signed by `ca`.
fn leaf_server_config(ca: &TestCa, sans: &[&str]) -> rustls::ServerConfig {
    let key = KeyPair::generate().expect("leaf key pair");
    let mut params = CertificateParams::new(sans.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .expect("leaf certificate params");
    params.not_before = date_time_ymd(2025, 1, 1);
    params.not_after = date_time_ymd(2035, 1, 1);
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let cert = params.signed_by(&key, &ca.issuer).expect("sign leaf");
    let chain = vec![CertificateDer::from_pem_slice(cert.pem().as_bytes()).expect("leaf DER")];
    let key_der =
        PrivateKeyDer::from_pem_slice(key.serialize_pem().as_bytes()).expect("leaf key DER");
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("supported protocol versions")
        .with_no_client_auth()
        .with_single_cert(chain, key_der)
        .expect("usable leaf")
}

// ---------------------------------------------------------------------------
// Recording middleware + the fake remote.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Captured {
    method: String,
    /// The full request URI (path + query) as the remote saw it.
    uri: String,
    headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct Recorder {
    hits: Arc<Mutex<Vec<Captured>>>,
}

impl Recorder {
    fn new() -> Self {
        Self {
            hits: Arc::new(Mutex::new(Vec::new())),
        }
    }
    fn hits(&self) -> Vec<Captured> {
        self.hits.lock().expect("recorder lock").clone()
    }
    fn count(&self) -> usize {
        self.hits().len()
    }
}

async fn record(State(rec): State<Recorder>, req: Request<Body>, next: Next) -> Response {
    let mut headers = Vec::new();
    for (name, value) in req.headers() {
        headers.push((
            name.to_string(),
            value.to_str().unwrap_or("<binary>").to_string(),
        ));
    }
    rec.hits.lock().expect("recorder lock").push(Captured {
        method: req.method().to_string(),
        uri: req.uri().to_string(),
        headers,
    });
    next.run(req).await
}

fn cookie_token(req: &Request<Body>) -> Option<String> {
    req.headers()
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|c| {
            c.trim()
                .strip_prefix(&format!("{COOKIE_NAME}="))
                .map(str::to_string)
        })
}

/// The fake remote: the T4 token-gated surface (token layer merged BEFORE
/// the routes, exactly like `WebUi::run`) plus tokenless `/peakbot-ca.crt`.
#[derive(Clone)]
struct FakeRemote {
    token: String,
    ca_pem: String,
    items: serde_json::Value,
    kill: serde_json::Value,
}

impl FakeRemote {
    fn new(token: &str, ca: &TestCa) -> Self {
        Self {
            token: token.to_string(),
            ca_pem: ca.ca_pem.clone(),
            items: serde_json::json!({ "items": expected_items() }),
            kill: serde_json::json!({ "items": [] }),
        }
    }
}

fn expected_items() -> serde_json::Value {
    serde_json::json!([
        {
            "id": "11111111-1111-1111-1111-111111111111",
            "name": "one",
            "updated_at": "2026-10-30T12:00:00Z",
            "message_count": 3,
            "model": "llama3",
            "active": true,
            "running": false,
            "profile": "dev"
        },
        {
            "id": "22222222-2222-2222-2222-222222222222",
            "name": "two",
            "updated_at": "2026-10-31T08:30:00Z",
            "message_count": 1,
            "model": "gpt",
            "active": false,
            "running": false,
            "profile": null
        }
    ])
}

async fn require_token(State(fr): State<FakeRemote>, req: Request<Body>, next: Next) -> Response {
    if cookie_token(&req).as_deref() != Some(fr.token.as_str()) {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    next.run(req).await
}

async fn ca_route(State(fr): State<FakeRemote>) -> (StatusCode, String) {
    (StatusCode::OK, fr.ca_pem.clone())
}

async fn list_route(State(fr): State<FakeRemote>) -> Response {
    json_response(&fr.items)
}

async fn kill_route(State(fr): State<FakeRemote>, req: Request<Body>) -> Response {
    // `/api/conversations/{id}/kill` splits to `["", "api", "conversations", id, "kill"]`.
    let id = req.uri().path().split('/').nth(3).unwrap_or("").to_string();
    if Uuid::parse_str(&id).is_err() {
        return (StatusCode::BAD_REQUEST, "bad conversation id").into_response();
    }
    json_response(&fr.kill)
}

async fn ws_route(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(drain_ws)
}

/// T4's `items_response` shape as a hand-rolled JSON body (axum's `json`
/// feature is off in this crate).
fn json_response(value: &serde_json::Value) -> Response {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        serde_json::to_string(value).expect("serialize"),
    )
        .into_response()
}

/// Keep the upgraded socket open and swallow frames until the client closes.
async fn drain_ws(mut socket: WebSocket) {
    while let Some(Ok(msg)) = socket.recv().await {
        if matches!(msg, Message::Close(_)) {
            break;
        }
    }
}

fn fake_remote_app(fr: FakeRemote, rec: Recorder) -> Router {
    let authed = Router::new()
        .route("/api/conversations", get(list_route))
        .route("/api/conversations/{id}/kill", post(kill_route))
        .route("/ws", get(ws_route))
        .layer(middleware::from_fn_with_state(fr.clone(), require_token));
    Router::new()
        .route("/peakbot-ca.crt", get(ca_route))
        .merge(authed)
        .with_state(fr)
        .layer(middleware::from_fn_with_state(rec, record))
}

async fn start_tls_server(ca: &TestCa, sans: &[&str], fr: FakeRemote, rec: Recorder) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("TLS port probe");
    // tokio refuses to register a blocking socket (tokio#7172).
    listener
        .set_nonblocking(true)
        .expect("non-blocking TLS listener");
    let addr = listener.local_addr().expect("TLS addr");
    let config = Arc::new(leaf_server_config(ca, sans));
    tokio::spawn(async move {
        let server = axum_server::from_tcp_rustls(listener, RustlsConfig::from_config(config))
            .expect("TLS server");
        server
            .serve(fake_remote_app(fr, rec).into_make_service())
            .await
            .ok();
    });
    format!("https://{addr}")
}

async fn start_plain_server(fr: FakeRemote, rec: Recorder) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("plain listener");
    let addr = listener.local_addr().expect("plain addr");
    tokio::spawn(async move {
        axum::serve(listener, fake_remote_app(fr, rec)).await.ok();
    });
    format!("http://{addr}")
}

fn hit_cookie(hit: &Captured) -> Option<&str> {
    hit.headers
        .iter()
        .find(|(name, _)| name == "cookie")
        .map(|(_, value)| value.as_str())
}

fn last_cookie_on(rec: &Recorder, path: &str) -> Option<String> {
    rec.hits()
        .iter()
        .rev()
        .find(|h| h.uri == path)
        .and_then(|h| hit_cookie(h).map(str::to_string))
}

// ---------------------------------------------------------------------------
// Pinned CA — the trust matrix.
// ---------------------------------------------------------------------------

// §5.8 / §9-T5: pin == the server CA's fingerprint ⇒ both the HTTP call and
// the WS dial succeed against the TLS remote.
#[tokio::test]
async fn pinned_ca_matching_allows_http_list_and_ws_dial() {
    let ca = mint_ca();
    let rec = Recorder::new();
    let url = start_tls_server(
        &ca,
        &["127.0.0.1"],
        FakeRemote::new(REMOTE_TOKEN, &ca),
        rec.clone(),
    )
    .await;
    let p = profile(&url, &ca.fingerprint, REMOTE_TOKEN);

    let client = Client::new(&p, short_opts())
        .await
        .expect("pinned CA matches — build");
    client.list().await.expect("list over pinned TLS");
    let mut ws = client.dial().await.expect("WS dial over pinned TLS");
    ws.send(TMsg::Text("hello".into()))
        .await
        .expect("the upgraded socket carries frames");
    drop(ws);

    assert!(
        rec.hits().iter().any(|h| h.uri == "/ws"),
        "the remote saw the /ws upgrade"
    );
}

// §5.8 / §5.12: a different CA is a HARD E-TLS-PIN naming both fingerprints;
// after the (no-verify) CA fetch nothing else is sent — no API calls, no WS,
// no plaintext fallback (the remote is TLS-only and saw exactly one request).
#[tokio::test]
async fn pinned_ca_mismatch_is_etls_pin_with_both_fingerprints_and_sends_nothing_more() {
    let server_ca = mint_ca();
    let other_ca = mint_ca(); // the pin comes from a DIFFERENT CA
    let rec = Recorder::new();
    let url = start_tls_server(
        &server_ca,
        &["127.0.0.1"],
        FakeRemote::new(REMOTE_TOKEN, &server_ca),
        rec.clone(),
    )
    .await;
    let p = profile(&url, &other_ca.fingerprint, REMOTE_TOKEN);

    let err = Client::new(&p, short_opts())
        .await
        .expect_err("a pin mismatch must be a hard error");

    assert_eq!(
        err.to_string(),
        format!(
            "Remote profile '{PROFILE}': the server's CA is {}, but the config pins {}. \
             Refusing to connect. If the server's CA was rotated, update `ca` in Settings → Profiles.",
            server_ca.fingerprint, other_ca.fingerprint
        ),
        "E-TLS-PIN names both fingerprints"
    );

    // Nothing was sent: exactly the one tokenless CA fetch, and nothing after.
    let hits = rec.hits();
    assert_eq!(
        hits.len(),
        1,
        "only the CA fetch reached the remote: {hits:?}"
    );
    assert_eq!(hits[0].method, "GET");
    assert_eq!(hits[0].uri, "/peakbot-ca.crt");
}

// §7: `ca: system` is an explicit opt-in to the platform roots — a self-signed
// test CA is NOT in them, so the handshake is a hard E-TLS. No fallback to
// plaintext (the TLS-only remote saw zero HTTP requests).
#[tokio::test]
async fn ca_system_against_self_signed_ca_is_etls_with_no_fallback() {
    let ca = mint_ca();
    let rec = Recorder::new();
    let url = start_tls_server(
        &ca,
        &["127.0.0.1"],
        FakeRemote::new(REMOTE_TOKEN, &ca),
        rec.clone(),
    )
    .await;
    let p = profile(&url, "system", REMOTE_TOKEN);

    let client = Client::new(&p, short_opts())
        .await
        .expect("no bootstrap for system roots");
    let err = client
        .list()
        .await
        .expect_err("system roots don't trust the test CA");

    let msg = err.to_string();
    assert!(
        msg.starts_with(&format!(
            "Remote profile '{PROFILE}': TLS handshake failed: "
        )),
        "E-TLS names the profile: {msg}"
    );
    assert!(
        msg.ends_with('.'),
        "locked string ends with a period: {msg}"
    );
    assert_eq!(
        rec.count(),
        0,
        "no plaintext fallback: nothing reached the remote"
    );
}

// §5.12: the pinned CA matches (bootstrap passes) but the leaf has no SAN for
// the dialed host ⇒ E-TLS-NAME naming the host and `--tls-name`.
#[tokio::test]
async fn leaf_without_the_dialed_host_is_etls_name_naming_the_host() {
    let ca = mint_ca();
    let rec = Recorder::new();
    let url = start_tls_server(
        &ca,
        &["example.com"],
        FakeRemote::new(REMOTE_TOKEN, &ca),
        rec.clone(),
    )
    .await;
    let (host, _port) = url
        .strip_prefix("https://")
        .and_then(|h| h.split_once(':'))
        .expect("host:port");
    let p = profile(&url, &ca.fingerprint, REMOTE_TOKEN);

    let client = Client::new(&p, short_opts())
        .await
        .expect("pin matches — bootstrap passes");
    let err = client
        .list()
        .await
        .expect_err("leaf has no SAN for the dialed host");

    assert_eq!(
        err.to_string(),
        format!(
            "Remote profile '{PROFILE}': the certificate isn't valid for '{host}'. \
             On the server, add it with --tls-name {host}."
        ),
        "E-TLS-NAME names the host and --tls-name"
    );
    assert_eq!(
        rec.count(),
        1,
        "only the CA fetch reached the remote (the handshake failed before HTTP)"
    );
}

// ---------------------------------------------------------------------------
// §7 token rules — captured on the wire.
// ---------------------------------------------------------------------------

// §7: the remote token travels as `Cookie: peakbot_token=<remote>` on every
// authenticated request (list, kill, the /ws upgrade).
#[tokio::test]
async fn remote_token_travels_as_peakbot_token_cookie_on_every_request() {
    let ca = mint_ca();
    let rec = Recorder::new();
    let url = start_tls_server(
        &ca,
        &["127.0.0.1"],
        FakeRemote::new(REMOTE_TOKEN, &ca),
        rec.clone(),
    )
    .await;
    let p = profile(&url, &ca.fingerprint, REMOTE_TOKEN);
    let client = Client::new(&p, short_opts()).await.expect("build");

    client.list().await.expect("list");
    let id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
    client.kill(&id).await.expect("kill");
    drop(client.dial().await.expect("dial"));

    let want = format!("{COOKIE_NAME}={REMOTE_TOKEN}");
    let paths: Vec<String> = vec![
        "/api/conversations".to_string(),
        format!("/api/conversations/{id}/kill"),
        "/ws".to_string(),
    ];
    for path in paths {
        let hits = rec.hits();
        let hit = hits
            .iter()
            .find(|h| h.uri == path)
            .unwrap_or_else(|| panic!("the remote saw {path}"));
        assert_eq!(
            hit_cookie(hit),
            Some(want.as_str()),
            "{path} carries the remote token as the {COOKIE_NAME} cookie"
        );
    }
}

// §7: the token never appears in a URL — not in the path, not in the query,
// so it can't reach the remote's access logs or a `Debug` of a URL.
#[tokio::test]
async fn remote_token_never_appears_in_any_url() {
    let ca = mint_ca();
    let rec = Recorder::new();
    let url = start_tls_server(
        &ca,
        &["127.0.0.1"],
        FakeRemote::new(REMOTE_TOKEN, &ca),
        rec.clone(),
    )
    .await;
    let p = profile(&url, &ca.fingerprint, REMOTE_TOKEN);
    let client = Client::new(&p, short_opts()).await.expect("build");

    client.list().await.expect("list");
    let id = Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap();
    client.kill(&id).await.expect("kill");
    drop(client.dial().await.expect("dial"));

    for hit in rec.hits() {
        assert!(
            !hit.uri.contains("token="),
            "no `token=` in any URL: {}",
            hit.uri
        );
        assert!(
            !hit.uri.contains(REMOTE_TOKEN),
            "the remote token is not in any URL: {}",
            hit.uri
        );
    }
}

// §7 token isolation: a local web token in the env (`PEAKBOT_WEB_TOKEN`)
// never appears in any header, URL or body of an outbound request; the remote
// token appears ONLY in `Cookie` headers.
#[tokio::test]
// The env lock must span every request; the current-thread runtime runs
// nothing else that takes it, so holding it across awaits can't deadlock.
#[allow(clippy::await_holding_lock)]
async fn local_web_token_never_reaches_the_remote() {
    let _lock = CONFIG_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var_os("PEAKBOT_WEB_TOKEN");
    unsafe { std::env::set_var("PEAKBOT_WEB_TOKEN", LOCAL_TOKEN) };

    let ca = mint_ca();
    let rec = Recorder::new();
    let url = start_tls_server(
        &ca,
        &["127.0.0.1"],
        FakeRemote::new(REMOTE_TOKEN, &ca),
        rec.clone(),
    )
    .await;
    let p = profile(&url, &ca.fingerprint, REMOTE_TOKEN);
    let client = Client::new(&p, short_opts()).await.expect("build");

    client.list().await.expect("list");
    let id = Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap();
    client.kill(&id).await.expect("kill");
    drop(client.dial().await.expect("dial"));

    // Restore before asserting, so a failure message can't leak the setup.
    match prev {
        Some(v) => unsafe { std::env::set_var("PEAKBOT_WEB_TOKEN", v) },
        None => unsafe { std::env::remove_var("PEAKBOT_WEB_TOKEN") },
    }

    let hits = rec.hits();
    assert!(!hits.is_empty(), "the client sent requests");
    for hit in &hits {
        assert!(
            !hit.uri.contains(LOCAL_TOKEN),
            "the local token is not in any URL: {}",
            hit.uri
        );
        for (name, value) in &hit.headers {
            assert!(
                !value.contains(LOCAL_TOKEN),
                "the local token is not in any header ({name}): {value}"
            );
            if value.contains(REMOTE_TOKEN) {
                assert_eq!(
                    name, "cookie",
                    "the remote token appears only in the {COOKIE_NAME} cookie, not {name}"
                );
            }
        }
    }
}

// §7: the CA bootstrap request carries NO credentials (it is the tokenless
// `/peakbot-ca.crt` route).
#[tokio::test]
async fn ca_bootstrap_fetch_carries_no_cookie() {
    let ca = mint_ca();
    let rec = Recorder::new();
    let url = start_tls_server(
        &ca,
        &["127.0.0.1"],
        FakeRemote::new(REMOTE_TOKEN, &ca),
        rec.clone(),
    )
    .await;
    let p = profile(&url, &ca.fingerprint, REMOTE_TOKEN);

    Client::new(&p, short_opts()).await.expect("bootstrap");

    let hits = rec.hits();
    let hit = hits
        .iter()
        .find(|h| h.uri == "/peakbot-ca.crt")
        .expect("the CA fetch reached the remote");
    assert!(
        hit_cookie(hit).is_none(),
        "the CA bootstrap carries no Cookie header: {:?}",
        hit.headers
    );
}

// ---------------------------------------------------------------------------
// LoopbackPlain — the only plaintext.
// ---------------------------------------------------------------------------

// §7 (RP3): plaintext is expressible only for loopback — plain http/ws to
// 127.0.0.1 works with no TLS at all.
#[tokio::test]
async fn loopback_plain_uses_plain_http_and_ws() {
    let ca = mint_ca(); // unused by the server, present for the CA route
    let rec = Recorder::new();
    let url = start_plain_server(FakeRemote::new(REMOTE_TOKEN, &ca), rec.clone()).await;

    let def = RemoteDef {
        url: url.clone(),
        token: REMOTE_TOKEN.to_string(),
        ca: None, // LoopbackPlain has no CA
        profile: PROFILE.to_string(),
        agent: None,
    };
    let p = expect_profile(parse_remote(PROFILE, &def, None));
    let client = Client::new(&p, short_opts()).await.expect("build");

    client.list().await.expect("plain http list");
    drop(client.dial().await.expect("plain ws dial"));

    assert!(
        rec.hits().iter().any(|h| h.uri == "/api/conversations"),
        "the plain remote saw the list call"
    );
}

// ---------------------------------------------------------------------------
// §5.12 failure rows the client owns.
// ---------------------------------------------------------------------------

// §5.12 E-DOWN: refused/unreachable at dial ⇒ `Can't reach remote profile
// 'L' at H: <reason>.`, bounded (a refused connect fails immediately).
#[tokio::test]
async fn closed_port_is_bounded_edown() {
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("port probe");
        probe.local_addr().expect("port").port()
    }; // dropped here: the port is now refused
    let url = format!("https://127.0.0.1:{port}");
    let p = profile(&url, &mint_ca().fingerprint, REMOTE_TOKEN);

    let started = std::time::Instant::now();
    let err = Client::new(&p, short_opts())
        .await
        .expect_err("a closed port is unreachable");

    let msg = err.to_string();
    assert!(
        msg.starts_with(&format!(
            "Can't reach remote profile '{PROFILE}' at 127.0.0.1:{port}: "
        )),
        "E-DOWN names profile and host:port: {msg}"
    );
    assert!(
        msg.ends_with('.'),
        "locked string ends with a period: {msg}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a refused connect fails well inside the bound, took {:?}",
        started.elapsed()
    );
}

// §5.12 E-TIMEOUT: TCP accepted but no TLS/upgrade ⇒ `Remote profile 'L' at
// H didn't answer in time.`, bounded by the (injected) dial timeout. Paused
// time keeps the test instantaneous in wall clock.
#[tokio::test(start_paused = true)]
async fn blackhole_listener_is_etimeout_within_the_bound() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("blackhole");
    let addr = listener.local_addr().expect("blackhole addr");
    // Accept every connection and answer nothing.
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _held = socket;
                std::future::pending::<()>().await;
            });
        }
    });
    let url = format!("https://{addr}");
    let p = profile(&url, &mint_ca().fingerprint, REMOTE_TOKEN);

    let started = tokio::time::Instant::now();
    let err = Client::new(&p, short_opts())
        .await
        .expect_err("a blackhole must time out");
    let elapsed = started.elapsed();

    assert_eq!(
        err.to_string(),
        format!("Remote profile '{PROFILE}' at {addr} didn't answer in time."),
        "E-TIMEOUT is the locked string"
    );
    assert!(
        elapsed <= short_opts().dial_timeout + Duration::from_secs(1),
        "bounded by the dial timeout, took {elapsed:?}"
    );
}

// §5.12 E-AUTH: a 401 on the HTTP call and on the WS upgrade ⇒ the locked
// auth error, with no retry storm (exactly one request per call).
#[tokio::test]
async fn remote_401_is_eauth_with_no_retry() {
    let ca = mint_ca();
    let rec = Recorder::new();
    // The remote's real token differs from the one the profile carries.
    let url = start_tls_server(
        &ca,
        &["127.0.0.1"],
        FakeRemote::new("server-token-real", &ca),
        rec.clone(),
    )
    .await;
    let p = profile(&url, &ca.fingerprint, "stale-token-from-config");
    let client = Client::new(&p, short_opts())
        .await
        .expect("bootstrap is tokenless");

    let err = client.list().await.expect_err("401 on the HTTP call");
    assert_eq!(
        err.to_string(),
        format!(
            "Remote profile '{PROFILE}' rejected its token. \
             Update the token in Settings → Profiles."
        ),
        "E-AUTH is the locked string"
    );
    assert_eq!(
        rec.hits()
            .iter()
            .filter(|h| h.uri == "/api/conversations")
            .count(),
        1,
        "no retry storm: exactly one list request"
    );

    let err = client.dial().await.expect_err("401 on the WS upgrade");
    assert_eq!(
        err.to_string(),
        "Remote profile 'dev' rejected its token. Update the token in Settings → Profiles."
    );
    assert_eq!(
        rec.hits().iter().filter(|h| h.uri == "/ws").count(),
        1,
        "no retry storm: exactly one /ws upgrade"
    );
}

// ---------------------------------------------------------------------------
// The remote HTTP calls (T4 shapes).
// ---------------------------------------------------------------------------

// §6.2: `list` is a `GET /api/conversations` (token-gated) and parses the T4
// `{"items":[ConversationSummaryWire]}` shape.
#[tokio::test]
async fn list_is_get_api_conversations_and_parses_the_t4_shape() {
    let ca = mint_ca();
    let rec = Recorder::new();
    let url = start_tls_server(
        &ca,
        &["127.0.0.1"],
        FakeRemote::new(REMOTE_TOKEN, &ca),
        rec.clone(),
    )
    .await;
    let p = profile(&url, &ca.fingerprint, REMOTE_TOKEN);
    let client = Client::new(&p, short_opts()).await.expect("build");

    let items = client.list().await.expect("list");

    let hits = rec.hits();
    let hit = hits
        .iter()
        .find(|h| h.uri == "/api/conversations")
        .expect("the remote saw GET /api/conversations");
    assert_eq!(hit.method, "GET", "list is a GET");

    assert_eq!(
        serde_json::to_value(&items).expect("serialize the parsed items"),
        expected_items(),
        "the T4 response shape round-trips, incl. the #[serde(default)] profile"
    );
}

// §6.2: `kill` is a `POST /api/conversations/{id}/kill` (token-gated); a clean
// 200 maps to `None`, and the remote's `error` field maps to `Some(msg)`.
#[tokio::test]
async fn kill_is_post_api_conversations_id_kill_and_maps_the_remote_error() {
    let ca = mint_ca();
    let rec = Recorder::new();
    let mut fr = FakeRemote::new(REMOTE_TOKEN, &ca);
    fr.kill = serde_json::json!({ "items": [], "error": "kill refused: a turn is running" });
    let url = start_tls_server(&ca, &["127.0.0.1"], fr, rec.clone()).await;
    let p = profile(&url, &ca.fingerprint, REMOTE_TOKEN);
    let client = Client::new(&p, short_opts()).await.expect("build");

    let id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
    let out = client.kill(&id).await.expect("kill");

    assert_eq!(
        out,
        Some("kill refused: a turn is running".to_string()),
        "the remote's `error` field is surfaced, for T7's mapping"
    );

    let want_uri = format!("/api/conversations/{id}/kill");
    let hits = rec.hits();
    let hit = hits
        .iter()
        .find(|h| h.uri == want_uri)
        .unwrap_or_else(|| panic!("the remote saw POST {want_uri}"));
    assert_eq!(hit.method, "POST", "kill is a POST");
    assert_eq!(
        hit_cookie(hit),
        Some(format!("{COOKIE_NAME}={REMOTE_TOKEN}").as_str()),
        "kill carries the token cookie"
    );
}

#[tokio::test]
async fn kill_without_a_remote_error_maps_to_none() {
    let ca = mint_ca();
    let rec = Recorder::new();
    let url = start_tls_server(
        &ca,
        &["127.0.0.1"],
        FakeRemote::new(REMOTE_TOKEN, &ca),
        rec.clone(),
    )
    .await;
    let p = profile(&url, &ca.fingerprint, REMOTE_TOKEN);
    let client = Client::new(&p, short_opts()).await.expect("build");

    let id = Uuid::parse_str("66666666-6666-6666-6666-666666666666").unwrap();
    assert_eq!(
        client.kill(&id).await.expect("kill"),
        None,
        "clean kill → None"
    );
}

// ---------------------------------------------------------------------------
// Directory — config freshness + client cache.
// ---------------------------------------------------------------------------

/// Points `XDG_CONFIG_HOME` at a tempdir holding `yaml` as the global config,
/// under the crate-wide env lock; restores on drop (same pattern as T4).
struct XdgHome {
    prev: Option<std::ffi::OsString>,
    _dir: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl XdgHome {
    fn new(yaml: &str) -> Self {
        let lock = CONFIG_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("XDG tempdir");
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let cfg_dir = crate::config::get_config_dir().expect("config dir under XDG_CONFIG_HOME");
        std::fs::create_dir_all(&cfg_dir).expect("config dir");
        std::fs::write(cfg_dir.join("config.yaml"), yaml).expect("write global config");
        Self {
            prev,
            _dir: dir,
            _lock: lock,
        }
    }
}

impl Drop for XdgHome {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
    }
}

fn remote_yaml(url: &str, pin: &str, token: &str) -> String {
    format!(
        r#"
providers:
  - name: ollama
    type: ollama
    models:
      - {{ name: llama3, alias: local }}
general:
  context:
    enabled: false
profiles:
  local:
    agents:
      team:
        model: local
  home-dev:
    remote:
      url: {url}
      token: {token}
      ca: {pin}
      profile: dev
"#
    )
}

// §5.8 / §9-T5: `Directory` re-parses the global config on demand and caches
// the `Client` per (profile, definition) — a changed token rebuilds it.
#[tokio::test]
async fn directory_rebuilds_the_client_when_the_token_changes() {
    let ca = mint_ca();
    let rec = Recorder::new();
    let url = start_tls_server(
        &ca,
        &["127.0.0.1"],
        FakeRemote::new("token-A", &ca),
        rec.clone(),
    )
    .await;
    let _home = XdgHome::new(&remote_yaml(&url, &ca.fingerprint, "token-A"));

    let dir = Directory::new();
    dir.client("home-dev")
        .await
        .expect("first client")
        .list()
        .await
        .expect("list with token A");
    assert_eq!(
        last_cookie_on(&rec, "/api/conversations"),
        Some(format!("{COOKIE_NAME}=token-A")),
        "the first client carried token A"
    );

    // The user rotated the remote token in config.yaml.
    let cfg_dir = crate::config::get_config_dir().expect("config dir");
    std::fs::write(
        cfg_dir.join("config.yaml"),
        remote_yaml(&url, &ca.fingerprint, "token-B"),
    )
    .expect("rewrite config");

    // The fake still only accepts token A, so this list is a 401 — the
    // cookie on the wire is what's asserted.
    let _ = dir
        .client("home-dev")
        .await
        .expect("rebuilt client")
        .list()
        .await;
    assert_eq!(
        last_cookie_on(&rec, "/api/conversations"),
        Some(format!("{COOKIE_NAME}=token-B")),
        "the changed definition rebuilt the client — the new token is on the wire"
    );
}
