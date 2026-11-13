//! TDD harness for the `peakbot-relay` integration tests (design §9.1).
//!
//! Helpers are real code; the relay under test is a `todo!()` skeleton, so
//! every integration test in `tests/` fails on purpose (RED) until the
//! implementation lands.
//!
//! Components:
//! - [`FakeIdp`]: in-test axum OIDC issuer (discovery, JWKS, token endpoint).
//!   The token endpoint enforces client auth and `S256(code_verifier) ==
//!   code_challenge`, so relay↔IdP PKCE is exercised on every login.
//! - [`TestRelay`]: a relay on `127.0.0.1:0` with a temp DB, plus the
//!   no-browser login dance and `restart()`/`sweep_at()` seams.
//! - [`FakeDevice`]: a device built purely from the `cloud` crate (fixed-seed
//!   keys, card, chain entries, Hello) over a `tokio-tungstenite` client.
//! - [`LogCapture`]: global TRACE subscriber into a shared buffer
//!   (used only by `tests/logs.rs`).
//!
//! No sleeps: "nothing arrives" is asserted with a sentinel afterwards, and
//! expiry goes through `sweep_at(t)` (design §9.1, R7: log in with real time,
//! then `tokio::time::pause()` if a paused clock is needed).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD as B64S, URL_SAFE_NO_PAD as B64U};
use cloud::chain::{ChainEntry, ChainOp};
use cloud::codec::{FromRelay, ToRelay};
use cloud::{DeviceId, LetterId};
use futures_util::{SinkExt as _, StreamExt as _};
use perma_crypto::v1;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

/// Default device redirect URI for the login dance (explicit port, §5.1).
pub const DEFAULT_REDIRECT: &str = "http://127.0.0.1:9/cb";
/// Sentinel pipe payload used to prove "nothing else arrived first".
pub const SENTINEL: &[u8] = b"PBSENTINEL";

// ---------------------------------------------------------------- time/misc

/// Unix seconds, for the `now` arguments.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub fn b64u(bytes: &[u8]) -> String {
    B64U.encode(bytes)
}

pub fn rand_bytes(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    rand::rng().fill_bytes(&mut b);
    b
}

/// RFC 7636 pair: (verifier, S256 challenge), both b64url.
pub fn pkce() -> (String, String) {
    let verifier = b64u(&rand_bytes(32));
    let challenge = b64u(Sha256::digest(verifier.as_bytes()).as_slice());
    (verifier, challenge)
}

pub fn letter_id() -> LetterId {
    let b = rand_bytes(16);
    let mut id = [0u8; 16];
    id.copy_from_slice(&b);
    id
}

// ------------------------------------------------------------------- FakeIdp

/// ID-token mint knobs: one thing overridden per test (design §9.1).
#[derive(Debug, Clone, Default)]
pub struct Mint {
    pub alg: Option<IdpAlg>,
    /// Default: the IdP URL.
    pub iss: Option<String>,
    /// Default: the client id as a string aud.
    pub aud: Option<Aud>,
    /// `None` = omit; `Some(None)` = omit; `Some(Some(x))` = set.
    pub azp: Option<Option<String>>,
    /// Added to the default `exp = now + 3600`.
    pub exp_offset: i64,
    /// Added to the default `iat = now`.
    pub iat_offset: i64,
    /// `None` = the flow nonce; `Some(None)` = drop the claim.
    pub nonce: Option<Option<String>>,
    /// Default: "user-1".
    pub sub: Option<String>,
    /// Default: the algorithm's kid.
    pub kid: Option<String>,
    /// Sign with a *different* key, same kid (bad_signature).
    pub alt_key: bool,
}

/// Signing algorithm of the minted ID token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdpAlg {
    Es256,
    Rs256,
    /// `alg:none` (unsigned).
    None,
    /// HS256 keyed with the client secret.
    Hs256,
}

/// `aud` claim shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Aud {
    Str(String),
    Vec(Vec<String>),
}

struct IdpInner {
    /// This IdP's base URL (= the issuer, unless `discovery_issuer` overrides).
    url: String,
    client_id: String,
    client_secret: String,
    es: p256::ecdsa::SigningKey,
    es_alt: p256::ecdsa::SigningKey,
    rs: Option<rsa::RsaPrivateKey>,
    /// Discovery `issuer` override (discovery_issuer_mismatch_rejected).
    discovery_issuer: Option<String>,
    /// The most recent minted ID token (logs_are_redacted needle).
    last_id_token: Option<String>,
    mint: Mint,
    /// code -> (flow nonce, relay S256 challenge).
    codes: HashMap<String, (String, String)>,
}

/// In-test OIDC issuer (design §9.1).
#[derive(Clone)]
pub struct FakeIdp {
    pub url: String,
    pub client_id: String,
    pub client_secret: String,
    state: Arc<Mutex<IdpInner>>,
}

impl FakeIdp {
    /// Start on `127.0.0.1:0`; `rs256` additionally generates an RSA-2048 key.
    pub async fn spawn(rs256: bool) -> Self {
        let es = p256::ecdsa::SigningKey::from_slice(&rand_bytes(32)).expect("p256 key");
        let es_alt = p256::ecdsa::SigningKey::from_slice(&rand_bytes(32)).expect("p256 key");
        let rs = if rs256 {
            // rsa 0.9 uses rand_core 0.6; its re-exported OsRng satisfies it.
            Some(
                rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).expect("rsa-2048 keygen"),
            )
        } else {
            None
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        let client_id = "relay-client-id".to_string();
        let client_secret = "s3cret-client-secret-value".to_string();
        let inner = Arc::new(Mutex::new(IdpInner {
            url: url.clone(),
            client_id: client_id.clone(),
            client_secret: client_secret.clone(),
            es,
            es_alt,
            rs,
            discovery_issuer: None,
            mint: Mint::default(),
            codes: HashMap::new(),
            last_id_token: None,
        }));
        let app = axum::Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/token", post(token_endpoint))
            .with_state(inner.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            url,
            client_id,
            client_secret,
            state: inner,
        }
    }

    /// Override the mint knobs for the next `issue_code`/token exchange.
    pub fn set_mint(&self, mint: Mint) {
        self.state.lock().unwrap().mint = mint;
    }

    /// IdP-side half of the browser step: issue an authorization code bound to
    /// the flow's `nonce` and the relay's `code_challenge` (design §9.1).
    pub fn issue_code(&self, nonce: &str, challenge: &str) -> String {
        let code = b64u(&rand_bytes(16));
        self.state
            .lock()
            .unwrap()
            .codes
            .insert(code.clone(), (nonce.to_string(), challenge.to_string()));
        code
    }

    pub fn set_discovery_issuer(&self, issuer: Option<String>) {
        self.state.lock().unwrap().discovery_issuer = issuer;
    }

    /// The last minted ID token (for redaction needles).
    pub fn last_id_token(&self) -> Option<String> {
        self.state.lock().unwrap().last_id_token.clone()
    }
}

async fn discovery(
    axum::extract::State(st): axum::extract::State<Arc<Mutex<IdpInner>>>,
) -> axum::Json<serde_json::Value> {
    let st = st.lock().unwrap();
    // The issuer is the server URL; the override tests the byte-for-byte check.
    let issuer = st
        .discovery_issuer
        .clone()
        .unwrap_or_else(|| st.url.clone());
    axum::Json(serde_json::json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{}/token", st.url),
        "jwks_uri": format!("{}/jwks", st.url),
        "token_endpoint_auth_methods_supported": ["client_secret_basic"],
    }))
}

fn es_jwk(key: &p256::ecdsa::SigningKey, kid: &str) -> serde_json::Value {
    let point = key.verifying_key().to_encoded_point(false);
    let pt = point.as_bytes();
    serde_json::json!({
        "kty": "EC", "crv": "P-256",
        "x": B64U.encode(&pt[1..33]),
        "y": B64U.encode(&pt[33..65]),
        "kid": kid, "use": "sig", "alg": "ES256",
    })
}

fn rs_jwk(key: &rsa::RsaPrivateKey, kid: &str) -> serde_json::Value {
    use rsa::traits::PublicKeyParts as _;
    serde_json::json!({
        "kty": "RSA",
        "n": B64U.encode(key.n().to_bytes_be().as_slice()),
        "e": B64U.encode(key.e().to_bytes_be().as_slice()),
        "kid": kid, "use": "sig", "alg": "RS256",
    })
}

async fn jwks(
    axum::extract::State(st): axum::extract::State<Arc<Mutex<IdpInner>>>,
) -> axum::Json<serde_json::Value> {
    let st = st.lock().unwrap();
    let mut keys = vec![es_jwk(&st.es, "idp-es256")];
    if let Some(rs) = &st.rs {
        keys.push(rs_jwk(rs, "idp-rs256"));
    }
    axum::Json(serde_json::json!({ "keys": keys }))
}

#[derive(serde::Deserialize)]
struct TokenForm {
    grant_type: String,
    code: String,
    #[serde(default)]
    redirect_uri: String,
    code_verifier: String,
}

async fn token_endpoint(
    axum::extract::State(st): axum::extract::State<Arc<Mutex<IdpInner>>>,
    headers: axum::http::HeaderMap,
    form: Result<axum::extract::Form<TokenForm>, axum::extract::rejection::FormRejection>,
) -> Result<axum::Json<serde_json::Value>, (axum::http::StatusCode, axum::Json<serde_json::Value>)>
{
    let form = form.map_err(|e| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({"error": "invalid_request", "detail": format!("{e}")})),
        )
    })?;
    let mut st = st.lock().unwrap();

    // client_secret_basic (RFC 6749 §2.3.1), enforced.
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let want = format!(
        "Basic {}",
        B64S.encode(format!("{}:{}", st.client_id, st.client_secret))
    );
    if auth != want {
        return Err((
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": "invalid_client"})),
        ));
    }
    if form.grant_type != "authorization_code" {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({"error": "unsupported_grant_type"})),
        ));
    }
    // PKCE is enforced relay↔IdP on every login (design §9.1).
    let Some((nonce, challenge)) = st.codes.remove(&form.code) else {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({"error": "invalid_grant"})),
        ));
    };
    let got = B64U.encode(Sha256::digest(form.code_verifier.as_bytes()).as_slice());
    if got != challenge {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({"error": "invalid_grant"})),
        ));
    }
    let id_token = mint_id_token(&st, &nonce);
    st.last_id_token = Some(id_token.clone());
    Ok(axum::Json(serde_json::json!({
        "token_type": "Bearer",
        "id_token": id_token,
    })))
}

/// Mint the ID token per the current `Mint` knobs (design §9.1).
fn mint_id_token(st: &IdpInner, nonce: &str) -> String {
    let m = &st.mint;
    let t = now();
    let mut claims = serde_json::Map::new();
    claims.insert(
        "iss".into(),
        serde_json::json!(m.iss.clone().unwrap_or_else(|| st.url.clone())),
    );
    let aud = match &m.aud {
        Some(Aud::Str(s)) => serde_json::json!(s),
        Some(Aud::Vec(v)) => serde_json::json!(v),
        None => serde_json::json!(st.client_id),
    };
    claims.insert("aud".into(), aud);
    if let Some(azp) = m.azp.clone().flatten() {
        claims.insert("azp".into(), serde_json::json!(azp));
    }
    claims.insert(
        "exp".into(),
        serde_json::json!(t as i64 + 3600 + m.exp_offset),
    );
    claims.insert("iat".into(), serde_json::json!(t as i64 + m.iat_offset));
    let nonce_claim: Option<String> = match &m.nonce {
        Some(inner) => inner.clone(),
        None => Some(nonce.to_string()),
    };
    if let Some(n) = nonce_claim {
        claims.insert("nonce".into(), serde_json::json!(n));
    }
    claims.insert(
        "sub".into(),
        serde_json::json!(m.sub.clone().unwrap_or_else(|| "user-1".into())),
    );

    let alg = m.alg.unwrap_or(IdpAlg::Es256);
    let alg_str = match alg {
        IdpAlg::Es256 => "ES256",
        IdpAlg::Rs256 => "RS256",
        IdpAlg::None => "none",
        IdpAlg::Hs256 => "HS256",
    };
    let kid = m.kid.clone().unwrap_or_else(|| match alg {
        IdpAlg::Rs256 => "idp-rs256".to_string(),
        _ => "idp-es256".to_string(),
    });
    let header = serde_json::json!({ "alg": alg_str, "typ": "JWT", "kid": kid });
    let h = B64U.encode(serde_json::to_string(&header).unwrap().as_bytes());
    let p = B64U.encode(serde_json::to_string(&claims).unwrap().as_bytes());
    let input = format!("{h}.{p}");
    match alg {
        IdpAlg::Es256 => {
            use p256::ecdsa::signature::hazmat::PrehashSigner as _;
            let key = if m.alt_key { &st.es_alt } else { &st.es };
            let sig: p256::ecdsa::Signature = key
                .sign_prehash(Sha256::digest(input.as_bytes()).as_slice())
                .expect("es256 sign");
            format!("{input}.{}", B64U.encode(sig.to_bytes().as_slice()))
        }
        IdpAlg::Rs256 => {
            use rsa::signature::{SignatureEncoding as _, Signer as _};
            let rs = st.rs.as_ref().expect("rs256 key generated");
            let signer = rsa::pkcs1v15::SigningKey::<Sha256>::new(rs.clone());
            let sig = signer.sign(input.as_bytes());
            format!("{input}.{}", B64U.encode(sig.to_bytes()))
        }
        IdpAlg::None => format!("{input}."),
        IdpAlg::Hs256 => {
            let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
            header.kid = Some(kid);
            jsonwebtoken::encode(
                &header,
                &claims,
                &jsonwebtoken::EncodingKey::from_secret(st.client_secret.as_bytes()),
            )
            .expect("hs256 encode")
        }
    }
}

// ------------------------------------------------------------------ TestRelay

pub struct TestRelay {
    pub idp: FakeIdp,
    relay: Option<peakbot_relay::RelayHandle>,
    pub tmp: tempfile::TempDir,
    cfg: peakbot_relay::config::Config,
    http: reqwest::Client,
}

impl TestRelay {
    /// Raise the soft RLIMIT_NOFILE toward the hard limit so the test process
    /// can hold `MAX_SOCKETS` loopback sockets (client + server fd each).
    /// Fails loudly when the hard limit is too low — never passes vacuously.
    fn raise_fd_limit() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let need = (peakbot_relay::limits::MAX_SOCKETS * 2 + 256) as libc::rlim_t;
            unsafe {
                let mut lim: libc::rlimit = std::mem::zeroed();
                libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim);
                if lim.rlim_cur < need {
                    lim.rlim_cur = need.min(lim.rlim_max);
                    let rc = libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
                    let _ = rc; // the re-read below reports the effective value
                    libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim);
                }
                assert!(
                    lim.rlim_cur as usize >= need as usize,
                    "socket_cap test cannot run: soft RLIMIT_NOFILE is {} but the test needs >= {need} (raise the hard limit)",
                    lim.rlim_cur
                );
            }
        });
    }

    /// Bind `127.0.0.1:0`, build the `Config` in code (loopback http allowed),
    /// temp DB + secret file, and `serve` (design §9.1).
    pub async fn spawn() -> Self {
        Self::spawn_idp(FakeIdp::spawn(false).await).await
    }

    pub async fn spawn_idp(idp: FakeIdp) -> Self {
        Self::raise_fd_limit();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let secret_file = tmp.path().join("idp.secret");
        // Trailing newline on purpose: config load must trim it.
        std::fs::write(&secret_file, format!("{}\n", idp.client_secret)).unwrap();
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
        let relay = Some(
            peakbot_relay::serve(cfg.clone(), listener)
                .await
                .expect("serve"),
        );
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        Self {
            idp,
            relay,
            tmp,
            cfg,
            http,
        }
    }

    pub fn handle(&self) -> &peakbot_relay::RelayHandle {
        self.relay.as_ref().expect("relay running")
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.handle().addr)
    }

    /// A bare authed `/ws` client: no keys, no Hello. Used to fill socket caps
    /// without paying ML-DSA keygen per device.
    pub async fn raw_ws(
        &self,
        token: &str,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let mut req = self.ws_url().into_client_request().expect("ws request");
        req.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}").parse().expect("header value"),
        );
        let (ws, _) = tokio_tungstenite::connect_async(req)
            .await
            .expect("raw ws connect");
        ws
    }

    pub fn ws_url(&self) -> String {
        format!("ws://{}/ws", self.handle().addr)
    }

    /// The origin string used in the Hello tbs.
    pub fn origin(&self) -> String {
        self.cfg.origin.0.clone()
    }

    pub fn db_path(&self) -> &Path {
        &self.cfg.db
    }

    /// Stop the relay (and release the DB lock) without restarting.
    pub async fn shutdown_all(&mut self) {
        if let Some(relay) = self.relay.take() {
            relay.shutdown().await;
        }
    }

    /// `restart()` = shutdown + serve on the same DB (design §9.1).
    pub async fn restart(&mut self) {
        if let Some(relay) = self.relay.take() {
            relay.shutdown().await;
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut cfg = self.cfg.clone();
        cfg.bind = addr;
        self.relay = Some(
            peakbot_relay::serve(cfg, listener)
                .await
                .expect("serve after restart"),
        );
    }

    pub async fn sweep_at(&self, now: u64) {
        self.handle().sweep_at(now).await;
    }

    // ------------------------------------------------- the no-browser login

    /// `GET /login` (redirects off); on 302, parse the IdP URL into the flow.
    #[allow(clippy::result_large_err)]
    pub async fn begin_login(&self, redirect_uri: &str) -> Result<LoginFlow, reqwest::Response> {
        let (verifier, challenge) = pkce();
        let resp = self
            .http
            .get(format!("{}/login", self.url()))
            .query(&[
                ("code_challenge", challenge.as_str()),
                ("redirect_uri", redirect_uri),
                ("state", "dev-state-1"),
            ])
            .send()
            .await
            .unwrap();
        if resp.status() != axum::http::StatusCode::FOUND {
            return Err(resp);
        }
        let loc = resp
            .headers()
            .get(axum::http::header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let u = url::Url::parse(&loc).unwrap();
        let qp: HashMap<String, String> = u.query_pairs().into_owned().collect();
        Ok(LoginFlow {
            idp_url: loc,
            relay_state: qp.get("state").cloned().unwrap_or_default(),
            nonce: qp.get("nonce").cloned().unwrap_or_default(),
            challenge: qp.get("code_challenge").cloned().unwrap_or_default(),
            device_state: "dev-state-1".to_string(),
            device_verifier: verifier,
            redirect_uri: redirect_uri.to_string(),
        })
    }

    /// `idp.issue_code(nonce, challenge)` + `GET /oidc/callback?code&state`.
    pub async fn finish_login(&self, flow: &LoginFlow, mint: &Mint) -> Callback {
        self.idp.set_mint(mint.clone());
        let code = self.idp.issue_code(&flow.nonce, &flow.challenge);
        let resp = self
            .http
            .get(format!("{}/oidc/callback", self.url()))
            .query(&[
                ("code", code.as_str()),
                ("state", flow.relay_state.as_str()),
            ])
            .send()
            .await
            .unwrap();
        Callback::parse(resp)
    }

    /// `POST /token` with the device's PKCE verifier.
    pub async fn get_token(&self, flow: &LoginFlow, code: &str) -> reqwest::Response {
        self.http
            .post(format!("{}/token", self.url()))
            .json(&serde_json::json!({ "code": code, "code_verifier": flow.device_verifier }))
            .send()
            .await
            .unwrap()
    }

    /// `GET /ws` over plain HTTP with an optional bearer token. Used for the
    /// pre-upgrade contract (401/429/503); a live upgrade goes through
    /// `FakeDevice::connect`.
    pub async fn ws_status(&self, token: Option<&str>) -> reqwest::Response {
        let mut req = self.http.get(format!("{}/ws", self.url()));
        if let Some(tok) = token {
            req = req.header("Authorization", format!("Bearer {tok}"));
        }
        req.send().await.unwrap()
    }

    /// `POST /logout` (empty body) with an optional bearer token.
    pub async fn logout(&self, token: Option<&str>) -> reqwest::Response {
        let mut req = self.http.post(format!("{}/logout", self.url()));
        if let Some(tok) = token {
            req = req.header("Authorization", format!("Bearer {tok}"));
        }
        req.send().await.unwrap()
    }

    /// `GET /oidc/callback?code&state` (raw, for single-use-state tests).
    pub async fn http_get_callback(&self, code: &str, state: &str) -> reqwest::Response {
        self.http
            .get(format!("{}/oidc/callback", self.url()))
            .query(&[("code", code), ("state", state)])
            .send()
            .await
            .unwrap()
    }

    /// Convenience: full login for `sub` (design §9.1). Different `sub` ⇒
    /// different account.
    pub async fn login(&self, sub: &str) -> String {
        let flow = self
            .begin_login(DEFAULT_REDIRECT)
            .await
            .expect("GET /login → 302");
        let cb = self
            .finish_login(
                &flow,
                &Mint {
                    sub: Some(sub.to_string()),
                    ..Default::default()
                },
            )
            .await;
        assert!(
            cb.response.status() == axum::http::StatusCode::FOUND && cb.code.is_some(),
            "callback should 302 with a code, got error={:?}",
            cb.error
        );
        let resp = self.get_token(&flow, cb.code.as_ref().unwrap()).await;
        assert_eq!(resp.status(), axum::http::StatusCode::OK, "POST /token");
        let v: serde_json::Value = resp.json().await.unwrap();
        v["token"].as_str().unwrap().to_string()
    }
}

/// One in-flight device-side login (design §9.1).
#[derive(Debug, Clone)]
pub struct LoginFlow {
    pub idp_url: String,
    pub relay_state: String,
    pub nonce: String,
    pub challenge: String,
    pub device_state: String,
    pub device_verifier: String,
    pub redirect_uri: String,
}

/// Parsed `GET /oidc/callback` response.
pub struct Callback {
    pub response: reqwest::Response,
    pub code: Option<String>,
    pub error: Option<String>,
    pub device_state: Option<String>,
}

impl Callback {
    fn parse(response: reqwest::Response) -> Self {
        let mut code = None;
        let mut error = None;
        let mut device_state = None;
        if let Some(loc) = response
            .headers()
            .get(axum::http::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            && let Ok(u) = url::Url::parse(loc)
        {
            for (k, v) in u.query_pairs() {
                match k.as_ref() {
                    "code" => code = Some(v.to_string()),
                    "error" => error = Some(v.to_string()),
                    "state" => device_state = Some(v.to_string()),
                    _ => {}
                }
            }
        }
        Self {
            response,
            code,
            error,
            device_state,
        }
    }
}

// --------------------------------------------------------------- FakeDevice

/// A fake device built with the `cloud` crate only (design §9.1).
pub struct FakeDevice {
    pub id: DeviceId,
    pub card: Vec<u8>,
    pub sk: v1::PrivateKey,
    pub pk: v1::PublicKey,
    ws: Option<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    nonce: [u8; 32],
    stalled: bool,
}

impl FakeDevice {
    /// Deterministic keypair from a fixed seed (design §9.1).
    pub fn new(seed: u64) -> Self {
        let mut seed_bytes = [0u8; 64];
        seed_bytes[..8].copy_from_slice(&seed.to_be_bytes());
        let kp = v1::KeyPair::from_seed(&v1::SecretSeed::from_bytes(seed_bytes)).expect("seed");
        let card = cloud::card::DeviceCard::new(&kp, &format!("device-{seed}"), 1).expect("card");
        Self {
            id: card.device_id(),
            card: card.encode(),
            sk: kp.private().clone(),
            pk: kp.public().clone(),
            ws: None,
            nonce: [0; 32],
            stalled: false,
        }
    }

    /// `ws://…/ws` with the bearer token; the first frame must be
    /// `Challenge{nonce}` (design §4.2).
    pub async fn connect(&mut self, ws_url: &str, token: &str) {
        let mut req = ws_url.into_client_request().expect("ws request");
        req.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}").parse().expect("header value"),
        );
        // Clients must accept 1 + MAX_ENVELOPE_CHAIN inbound (design §8 R3).
        let config = Some(
            WebSocketConfig::default()
                .max_message_size(Some(1 + cloud::constants::MAX_ENVELOPE_CHAIN))
                .max_frame_size(Some(1 + cloud::constants::MAX_ENVELOPE_CHAIN)),
        );
        let (mut ws, _) = tokio_tungstenite::connect_async_with_config(req, config, true)
            .await
            .expect("ws connect");
        match Self::first_frame(&mut ws).await {
            FromRelay::Challenge { nonce } => {
                self.nonce = nonce;
                self.stalled = false;
                self.ws = Some(ws);
            }
            other => panic!("first frame must be Challenge, got {other:?}"),
        }
    }

    /// `Hello` for an enrolled (active) device, signed for the given origin.
    pub async fn hello(&mut self, origin: impl AsRef<str>) {
        let tbs = cloud::codec::hello_tbs(origin.as_ref(), self.nonce, self.id, 0);
        let sig = cloud::codec::sign_hello(&self.sk, &tbs, &mut rand::rng()).expect("sig");
        self.send(ToRelay::Hello {
            device: self.id,
            sig,
        })
        .await;
    }

    /// `Hello` signed for a *different* origin (hello_signed_for_other_origin).
    pub async fn hello_with_origin(&mut self, origin: impl AsRef<str>) {
        let tbs = cloud::codec::hello_tbs(origin.as_ref(), self.nonce, self.id, 0);
        let sig = cloud::codec::sign_hello(&self.sk, &tbs, &mut rand::rng()).expect("sig");
        self.send(ToRelay::Hello {
            device: self.id,
            sig,
        })
        .await;
    }

    /// A `Hello` whose signature is garbage (hello_bad_sig).
    pub async fn hello_bad_sig(&mut self) {
        self.send(ToRelay::Hello {
            device: self.id,
            sig: rand_bytes(64),
        })
        .await;
    }

    /// `HelloPending` with this device's card.
    pub async fn hello_pending(&mut self, origin: impl AsRef<str>) {
        let tbs = cloud::codec::hello_tbs(origin.as_ref(), self.nonce, self.id, 1);
        let sig = cloud::codec::sign_hello(&self.sk, &tbs, &mut rand::rng()).expect("sig");
        self.send(ToRelay::HelloPending {
            card: self.card.clone(),
            sig,
        })
        .await;
    }

    pub async fn send(&mut self, msg: ToRelay) {
        let bytes = cloud::codec::encode_to_relay(&msg);
        self.ws
            .as_mut()
            .expect("connected")
            .send(Message::Binary(bytes.into()))
            .await
            .expect("send");
    }

    /// Send a raw frame (text / oversize tests).
    pub async fn send_raw(&mut self, msg: Message) {
        self.ws
            .as_mut()
            .expect("connected")
            .send(msg)
            .await
            .expect("send");
    }

    /// Next `FromRelay`; 5 s timeout = failure (design §9.1).
    pub async fn recv(&mut self) -> FromRelay {
        assert!(!self.stalled, "recv on a stalled device");
        let ws = self.ws.as_mut().expect("connected");
        let msg = tokio::time::timeout(Duration::from_secs(5), Self::next_raw_stream(ws))
            .await
            .expect("recv: 5 s timeout");
        match msg {
            Message::Binary(b) => cloud::codec::decode_from_relay(&b).expect("decode FromRelay"),
            Message::Close(_) => panic!("unexpected close frame"),
            other => panic!("unexpected frame {other:?}"),
        }
    }

    /// Read until the close frame; `None` on clean EOF. Ignores `stall()`.
    pub async fn close_code(&mut self) -> Option<u16> {
        let ws = self.ws.as_mut()?;
        while let Some(Ok(msg)) = ws.next().await {
            if let Message::Close(Some(frame)) = msg {
                return Some(u16::from(frame.code));
            }
        }
        None
    }

    pub async fn expect_close(&mut self, code: u16) {
        assert_eq!(self.close_code().await, Some(code), "expected close {code}");
    }

    /// Stop reading (slow_consumer tests); `recv()` panics, `close_code()` works.
    pub fn stall(&mut self) {
        self.stalled = true;
    }

    /// Go offline: send Close and wait for EOF. The relay unregisters the
    /// device before it drops the socket, so EOF proves the device is gone.
    pub async fn go_offline(&mut self) {
        if let Some(mut ws) = self.ws.take() {
            let _ = ws.close(None).await;
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                while let Some(Ok(_)) = ws.next().await {}
            })
            .await;
        }
    }

    async fn next_raw_stream(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Message {
        match ws.next().await {
            Some(Ok(msg)) => msg,
            other => panic!("socket closed: {other:?}"),
        }
    }

    async fn first_frame(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> FromRelay {
        let msg = Self::next_raw_stream(ws).await;
        match msg {
            Message::Binary(b) => cloud::codec::decode_from_relay(&b).expect("decode"),
            other => panic!("first frame {other:?}"),
        }
    }
}

// ------------------------------------------------------------- enrollment

/// Build a signed chain entry with a fresh ThreadRng.
pub fn entry(
    seq: u64,
    prev: [u8; 32],
    op: ChainOp,
    card: Option<Vec<u8>>,
    target: Option<DeviceId>,
    signer: &FakeDevice,
) -> ChainEntry {
    cloud::chain::build_entry(
        seq,
        prev,
        op,
        card,
        target,
        signer.id,
        signer.sk(),
        &mut rand::rng(),
    )
    .expect("build_entry")
}

impl FakeDevice {
    pub fn sk(&self) -> &v1::PrivateKey {
        &self.sk
    }
}

/// Connect + `HelloPending` + `EnrollGenesis` ⇒ this device is the account's
/// single active device at seq 0.
pub async fn enroll_genesis(t: &TestRelay, token: &str, d: &mut FakeDevice) {
    d.connect(&t.ws_url(), token).await;
    d.hello_pending(t.origin()).await;
    expect_welcome(&d.recv().await, None, true).await;
    let e = entry(0, [0; 32], ChainOp::Genesis, Some(d.card.clone()), None, d);
    d.send(ToRelay::EnrollGenesis { entry: e.encode() }).await;
    expect_chain_changed(&d.recv().await, 0).await;
}

/// Connect `new` (HelloPending) and `ChainAppend` an `Add` from `active`;
/// both sockets see `ChainChanged{seq}`.
pub async fn enroll_add(
    t: &TestRelay,
    token: &str,
    seq: u64,
    prev: [u8; 32],
    active: &mut FakeDevice,
    new: &mut FakeDevice,
) {
    new.connect(&t.ws_url(), token).await;
    new.hello_pending(t.origin()).await;
    expect_welcome(&new.recv().await, Some(seq - 1), true).await;
    let e = entry(
        seq,
        prev,
        ChainOp::Add,
        Some(new.card.clone()),
        None,
        active,
    );
    active
        .send(ToRelay::ChainAppend { entry: e.encode() })
        .await;
    expect_chain_changed(&active.recv().await, seq).await;
    expect_chain_changed(&new.recv().await, seq).await;
}

/// Connect `d` (already enrolled) with `Hello`; expects the Welcome for the
/// given chain head.
pub async fn connect_active(t: &TestRelay, token: &str, d: &mut FakeDevice, head: Option<u64>) {
    d.connect(&t.ws_url(), token).await;
    d.hello(t.origin()).await;
    expect_welcome(&d.recv().await, head, false).await;
}

/// `Post` from `d` to `to`; returns the letter id (no success reply exists,
/// design §4.6).
pub async fn post_to(d: &mut FakeDevice, to: DeviceId, sealed: Vec<u8>) -> LetterId {
    let id = letter_id();
    d.send(ToRelay::Post {
        to,
        letter_id: id,
        sealed,
    })
    .await;
    id
}

/// `ChainFetch` and the expected single `Chain` reply.
pub async fn fetch_chain(d: &mut FakeDevice, from_seq: u64) -> Vec<Vec<u8>> {
    d.send(ToRelay::ChainFetch { from_seq }).await;
    match d.recv().await {
        FromRelay::Chain { entries } => entries,
        other => panic!("expected Chain, got {other:?}"),
    }
}

/// Add `count` active devices after `seq` (prev = `prev`); returns the devices
/// and the new head hash. All sockets stay open.
pub async fn add_devices(
    t: &TestRelay,
    token: &str,
    seq: u64,
    prev: [u8; 32],
    active: &mut FakeDevice,
    count: u64,
) -> (Vec<FakeDevice>, [u8; 32]) {
    let mut out = Vec::new();
    let mut cur = prev;
    for i in 0..count {
        let s = seq + i;
        let mut d = FakeDevice::new(5000 + s);
        d.connect(&t.ws_url(), token).await;
        d.hello_pending(t.origin()).await;
        expect_welcome(&d.recv().await, Some(s - 1), true).await;
        let e = entry(s, cur, ChainOp::Add, Some(d.card.clone()), None, active);
        let raw = e.encode();
        active
            .send(ToRelay::ChainAppend { entry: raw.clone() })
            .await;
        expect_chain_changed(&active.recv().await, s).await;
        expect_chain_changed(&d.recv().await, s).await;
        cur = cloud::chain::entry_hash(&raw);
        // Drop the socket: 256 live ones would exceed MAX_SOCKETS_PER_ACCOUNT (§6.5).
        d.ws = None;
        out.push(d);
    }
    (out, cur)
}

// ------------------------------------------------------------- assertions

pub async fn expect_welcome(m: &FromRelay, head: Option<u64>, pending: bool) -> Option<u64> {
    match m {
        FromRelay::Welcome {
            head_seq,
            pending: p,
        } => {
            assert_eq!(*head_seq, head, "welcome head_seq");
            assert_eq!(*p, pending, "welcome pending flag");
            *head_seq
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
}

pub async fn expect_chain_changed(m: &FromRelay, seq: u64) {
    match m {
        FromRelay::ChainChanged { head_seq } => assert_eq!(*head_seq, seq, "ChainChanged head"),
        other => panic!("expected ChainChanged{seq}, got {other:?}"),
    }
}

pub async fn expect_error(m: &FromRelay, code: u16) {
    match m {
        FromRelay::Error { code: c } => assert_eq!(*c, code, "error code"),
        other => panic!("expected Error{code}, got {other:?}"),
    }
}

pub async fn expect_unreachable(m: &FromRelay, to: DeviceId) {
    match m {
        FromRelay::Unreachable { to: t } => assert_eq!(*t, to, "Unreachable target"),
        other => panic!("expected Unreachable{to:?}, got {other:?}"),
    }
}

pub fn expect_pipe(m: &FromRelay, from: DeviceId, bytes: &[u8]) {
    match m {
        FromRelay::Pipe { from: f, bytes: b } => {
            assert_eq!(*f, from, "pipe from");
            assert_eq!(b, bytes, "pipe bytes");
        }
        other => panic!("expected Pipe, got {other:?}"),
    }
}

pub fn expect_letter(m: &FromRelay, from: DeviceId, letter_id: LetterId, sealed: &[u8]) {
    match m {
        FromRelay::Letter {
            from: f,
            letter_id: id,
            sealed: s,
        } => {
            assert_eq!(*f, from, "letter from");
            assert_eq!(*id, letter_id, "letter id");
            assert_eq!(s, sealed, "letter sealed");
        }
        other => panic!("expected Letter, got {other:?}"),
    }
}

// ---------------------------------------------------------------- LogCapture

/// Global TRACE subscriber into a shared buffer (design §9.1). Used only by
/// `tests/logs.rs`, which is its own test binary.
pub struct LogCapture {
    buf: Arc<Mutex<Vec<u8>>>,
}

struct Writer(Arc<Mutex<Vec<u8>>>);

struct GuardWriter<'a>(std::sync::MutexGuard<'a, Vec<u8>>);

impl std::io::Write for GuardWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Writer {
    type Writer = GuardWriter<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        GuardWriter(self.0.lock().unwrap())
    }
}

impl LogCapture {
    pub fn install() -> Self {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let _ = tracing_subscriber::fmt()
            // The production filter at TRACE: dependency frame logs stay off.
            .with_env_filter(peakbot_relay::log_filter("trace"))
            .with_ansi(false)
            .with_writer(Writer(buf.clone()))
            .try_init();
        Self { buf }
    }

    pub fn contains(&self, needle: &str) -> bool {
        let bytes = self.buf.lock().unwrap();
        String::from_utf8_lossy(&bytes).contains(needle)
    }
}
