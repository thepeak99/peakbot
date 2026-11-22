//! Dev OIDC harness for on-device e2e testing (P5 T1).
//!
//! Spawns a FakeIdp and a relay with app_scheme: sh.shifu.mobile. The FakeIdp
//! auto-approves every authorize request (no UI) and redirects with a code.
//!
//! Redirect chain (mobile):
//!   1. Device opens relay's /login?code_challenge=…&redirect_uri=sh.shifu.mobile:/cb&state=…
//!   2. Relay 302s to the IdP's authorize endpoint (with relay's own state/nonce/challenge)
//!   3. FakeIdp auto-approves, 302s to relay's /oidc/callback?code=…&state=…
//!   4. Relay exchanges code for id_token, issues one-time code, 302s to sh.shifu.mobile:/cb?code=…&state=…
//!   5. Device receives callback via custom scheme, POSTs code+verifier to relay's /token
//!
//! Two ways to run it:
//!
//!   # 1. Plain HTTP for local testing (FakeIdp :8099, relay :8080).
//!   cargo run -p peakbot-relay --example dev_relay
//!
//!   # 2. HTTPS on a LAN so a real phone can reach it (TLS listener on :8443).
//!   cargo run -p peakbot-relay --example dev_relay -- --tls --host 10.1.2.3 [--port 8443]
//!
//! In TLS mode the example mints a self-signed CA (reused across runs from
//! ~/.cache/peakbot/dev-relay-tls, pin printed on startup) and a leaf covering
//! `--host` plus loopback, and serves a single HTTPS listener. The FakeIdp's
//! issuer/discovery/token/JWKS stay loopback HTTP (only its authorize endpoint
//! is exposed publicly at /idp/authorize on the relay origin), so the relay
//! never has to trust its own self-signed cert. It prints the relay URL, the CA
//! URL (`/peakbot-ca.crt`) and the exact `general.cloud` lines to paste:
//!
//!   relay: https://10.1.2.3:8443
//!   ca: sha256:<64 hex>
//!
//! Restart the phone app after changing `general.cloud`. Browser/OS trust of
//! the CA is a separate step from pinning it via `ca`.
//!
//! This example is TEST-ONLY and never ships in production builds.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD as B64S, URL_SAFE_NO_PAD as B64U};
use rand::RngCore;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, date_time_ymd,
};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use sha2::{Digest, Sha256};

// ------------------------------------------------------------------- FakeIdp

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn b64u(bytes: &[u8]) -> String {
    B64U.encode(bytes)
}

fn rand_bytes(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    rand::rng().fill_bytes(&mut b);
    b
}

/// OIDC authorize request query — typed so missing required fields are 400s.
/// state/nonce default to empty; unknown fields are allowed.
#[derive(serde::Deserialize)]
struct AuthorizeQuery {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    scope: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    nonce: String,
    code_challenge: String,
    #[serde(default)]
    code_challenge_method: String,
}

/// Recover from a poisoned mutex. Single-statement codes.insert mutation
/// can never leave the IdpInner struct corrupted, so the values are still
/// usable after another task panicked under the lock.
fn idp_lock(m: &Mutex<IdpInner>) -> std::sync::MutexGuard<'_, IdpInner> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct IdpInner {
    url: String,
    client_id: String,
    client_secret: String,
    es: p256::ecdsa::SigningKey,
    // code -> (flow nonce, relay S256 challenge)
    codes: HashMap<String, (String, String)>,
    // Public HTTPS URL for the authorize endpoint (set in TLS mode).
    // The FakeIdp itself still listens on loopback HTTP; only this endpoint URL is public.
    public_authorize_url: Option<String>,
}

async fn discovery(
    axum::extract::State(st): axum::extract::State<Arc<Mutex<IdpInner>>>,
) -> axum::Json<serde_json::Value> {
    let st = idp_lock(&st);
    // In TLS mode, discovery's authorization_endpoint points at the public HTTPS
    // URL (on the same origin as the relay) so the phone can reach it. Issuer,
    // token_endpoint and jwks_uri stay loopback HTTP — the relay's Oidc client
    // validates endpoints, and they must not use TLS for the self-signed cert.
    let auth_endpoint = st
        .public_authorize_url
        .as_deref()
        .unwrap_or(&format!("{}/authorize", st.url))
        .to_string();
    let url = st.url.clone();
    axum::Json(serde_json::json!({
        "issuer": url,
        "authorization_endpoint": auth_endpoint,
        "token_endpoint": format!("{}/token", url),
        "jwks_uri": format!("{}/jwks", url),
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

async fn jwks(
    axum::extract::State(st): axum::extract::State<Arc<Mutex<IdpInner>>>,
) -> axum::Json<serde_json::Value> {
    let st = idp_lock(&st);
    axum::Json(serde_json::json!({ "keys": [es_jwk(&st.es, "idp-es256")] }))
}

async fn authorize(
    axum::extract::State(st): axum::extract::State<Arc<Mutex<IdpInner>>>,
    q: Result<axum::extract::Query<AuthorizeQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<axum::response::Html<String>, (axum::http::StatusCode, &'static str)> {
    let q = q.map_err(|_| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            "missing required query params: client_id, redirect_uri, code_challenge",
        )
    })?;
    let st = idp_lock(&st);

    // Validate client_id
    if q.0.client_id != st.client_id {
        return Err((axum::http::StatusCode::BAD_REQUEST, "invalid client_id"));
    }

    // Build an HTML page with a "Sign in" button (form).
    // This simulates the user gesture required on Chrome Android to
    // trigger a custom-scheme redirect (sh.shifu.mobile:/cb).
    // Use a relative action so it works at both / and /idp/ prefixes.
    let mut fields = String::new();
    let q0 = &q.0;
    for (k, v) in [
        ("response_type", q0.response_type.clone()),
        ("client_id", q0.client_id.clone()),
        ("redirect_uri", q0.redirect_uri.clone()),
        ("scope", q0.scope.clone()),
        ("state", q0.state.clone()),
        ("nonce", q0.nonce.clone()),
        ("code_challenge", q0.code_challenge.clone()),
        ("code_challenge_method", q0.code_challenge_method.clone()),
    ] {
        fields.push_str(&format!(
            "<input type=\"hidden\" name=\"{k}\" value=\"{v}\">"
        ));
    }
    let html = format!(
        "<html><head><title>Sign In</title></head><body><h1>Sign In</h1>\
         <p>Click to approve the login request.</p>\
         <form method=\"get\" action=\"./authorize/continue\">\
         {fields}\
         <button type=\"submit\">Sign in</button>\
         </form></body></html>"
    );
    Ok(axum::response::Html(html))
}

async fn authorize_continue(
    axum::extract::State(st): axum::extract::State<Arc<Mutex<IdpInner>>>,
    q: Result<axum::extract::Query<AuthorizeQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<axum::response::Redirect, (axum::http::StatusCode, &'static str)> {
    let q = q.map_err(|_| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            "missing required query params: client_id, redirect_uri, code_challenge",
        )
    })?;

    // Parse redirect_uri before locking — invalid URL is a client error
    let mut url = match url::Url::parse(&q.0.redirect_uri) {
        Ok(u) => u,
        Err(_) => {
            return Err((axum::http::StatusCode::BAD_REQUEST, "invalid redirect_uri"));
        }
    };

    // Validate client_id and mint code (both require lock)
    let mut st = idp_lock(&st);
    if q.0.client_id != st.client_id {
        return Err((axum::http::StatusCode::BAD_REQUEST, "invalid client_id"));
    }

    // Auto-approve: generate code and redirect
    let code = b64u(&rand_bytes(16));
    st.codes.insert(
        code.clone(),
        (q.0.nonce.clone(), q.0.code_challenge.clone()),
    );

    url.query_pairs_mut()
        .clear()
        .append_pair("code", &code)
        .append_pair("state", &q.0.state);

    Ok(axum::response::Redirect::to(url.as_str()))
}

#[derive(serde::Deserialize)]
#[allow(dead_code)]
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
    let mut st = idp_lock(&st);

    // client_secret_basic
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

    // Mint ID token (ES256)
    let t = now();
    let mut claims = serde_json::Map::new();
    claims.insert("iss".into(), serde_json::json!(st.url.clone()));
    claims.insert("aud".into(), serde_json::json!(st.client_id));
    claims.insert("exp".into(), serde_json::json!(t + 3600));
    claims.insert("iat".into(), serde_json::json!(t));
    claims.insert("nonce".into(), serde_json::json!(nonce));
    claims.insert("sub".into(), serde_json::json!("user-1"));

    let header = serde_json::json!({ "alg": "ES256", "typ": "JWT", "kid": "idp-es256" });
    let h = B64U.encode(serde_json::to_string(&header).unwrap().as_bytes());
    let p = B64U.encode(serde_json::to_string(&claims).unwrap().as_bytes());
    let input = format!("{h}.{p}");

    use p256::ecdsa::signature::hazmat::PrehashSigner as _;
    let sig: p256::ecdsa::Signature = st
        .es
        .sign_prehash(Sha256::digest(input.as_bytes()).as_slice())
        .expect("es256 sign");
    let id_token = format!("{input}.{}", B64U.encode(sig.to_bytes().as_slice()));

    Ok(axum::Json(serde_json::json!({
        "token_type": "Bearer",
        "id_token": id_token,
    })))
}

async fn spawn_idp(
    bind: &str,
    public_authorize_url: Option<String>,
) -> (String, Arc<Mutex<IdpInner>>) {
    let es = p256::ecdsa::SigningKey::from_slice(&rand_bytes(32)).expect("p256 key");
    let listener = tokio::net::TcpListener::bind(bind).await.unwrap();
    let addr = listener.local_addr().unwrap();
    // Use 127.0.0.1 for the URL so the relay can reach it (0.0.0.0 is not routable)
    let url = format!("http://127.0.0.1:{}", addr.port());
    let client_id = "relay-client-id".to_string();
    let client_secret = "s3cret-client-secret-value".to_string();
    let inner = Arc::new(Mutex::new(IdpInner {
        url: url.clone(),
        client_id: client_id.clone(),
        client_secret: client_secret.clone(),
        es,
        codes: HashMap::new(),
        public_authorize_url,
    }));
    let app = axum::Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/jwks", get(jwks))
        .route("/authorize", get(authorize))
        .route("/authorize/continue", get(authorize_continue))
        .route("/token", post(token_endpoint))
        .with_state(inner.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (url, inner)
}

// ── DevCa: stable self-signed CA for the dev relay ────────────────────────

/// The dev relay's CA, generated once and reused. The CA is persisted on disk
/// so its fingerprint is stable across loads. The CA is self-signed (not a
/// production CA), but it lets the phone trust the relay's leaf certificate.
struct DevCa {
    /// The CA certificate in PEM.
    pub cert_pem: String,
    /// The CA fingerprint: `sha256:` + 64 lowercase hex chars over the DER.
    pub fingerprint: String,
    /// The CA issuer (key material for signing the leaf).
    issuer: Issuer<'static, KeyPair>,
}

const DEV_CA_CERT: &str = "cert.pem";
const DEV_CA_KEY: &str = "key.pem";
const DEV_CA_COMMON_NAME: &str = "PeakBot Dev Relay CA";

/// Load the dev CA from `dir`, or generate it if absent. The fingerprint is
/// computed from the persisted certificate, so it is stable across loads.
impl DevCa {
    fn load_or_create(dir: &Path) -> Result<Self> {
        let cert_path = dir.join(DEV_CA_CERT);
        let key_path = dir.join(DEV_CA_KEY);

        if cert_path.exists() && key_path.exists() {
            let cert_pem = fs::read_to_string(&cert_path)
                .with_context(|| format!("reading {}", cert_path.display()))?;
            let key_pem = fs::read_to_string(&key_path)
                .with_context(|| format!("reading {}", key_path.display()))?;
            let key = KeyPair::from_pem(&key_pem).context("CA key did not parse")?;
            // Rebuild the issuer from the persisted key (the cert is loaded).
            Ok(Self {
                cert_pem: cert_pem.clone(),
                fingerprint: der_fingerprint(&cert_pem)?,
                issuer: Issuer::new(ca_params()?, key),
            })
        } else {
            let key = KeyPair::generate().context("generating CA key pair")?;
            let cert = ca_params()?
                .self_signed(&key)
                .context("self-signing the CA")?;
            let cert_pem = cert.pem();
            let cert_der = cert.der();
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            fs::write(&cert_path, &cert_pem)
                .with_context(|| format!("writing {}", cert_path.display()))?;
            write_private(&key_path, &key.serialize_pem())?;
            Ok(Self {
                cert_pem,
                fingerprint: der_fingerprint_bytes(cert_der.as_ref()),
                issuer: Issuer::new(ca_params()?, key),
            })
        }
    }
}

fn ca_params() -> Result<CertificateParams> {
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.not_before = date_time_ymd(2020, 1, 1);
    params.not_after = date_time_ymd(2035, 1, 1);
    params
        .distinguished_name
        .push(DnType::CommonName, DEV_CA_COMMON_NAME);
    Ok(params)
}

/// Derive the CA fingerprint from a PEM string (used when loading the cert).
fn der_fingerprint(pem: &str) -> Result<String> {
    let der = CertificateDer::from_pem_slice(pem.as_bytes())?;
    Ok(der_fingerprint_bytes(der.as_ref()))
}

/// `sha256:` + 64 lowercase hex chars of the DER bytes.
fn der_fingerprint_bytes(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// Write a secret file with owner-only permissions (0600 on Unix).
fn write_private(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting 0600 on {}", path.display()))?;
    }
    Ok(())
}

/// Generate a leaf certificate signed by the dev CA for the given SANs.
fn mint_leaf(ca: &DevCa, sans: Vec<String>) -> Result<(String, String)> {
    let key = KeyPair::generate()?;
    let mut params = CertificateParams::new(sans)?;
    // A rolling one-year window, like the production CA helper: freshly minted
    // each boot, and short enough for Apple's 825-day server-leaf rule.
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(365);
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params
        .distinguished_name
        .push(DnType::CommonName, "PeakBot Dev Relay");
    let cert = params.signed_by(&key, &ca.issuer)?;
    Ok((cert.pem(), key.serialize_pem()))
}

/// SANs for the dev relay: the host (from --host), localhost, and loopback IPs.
fn dev_sans(host: &str) -> Vec<String> {
    let mut sans = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    // Only add the host if it isn't already covered by the loopback names.
    if host != "localhost" && host != "127.0.0.1" && host != "::1" {
        sans.push(host.to_string());
    }
    sans
}

// ── Argument parsing ─────────────────────────────────────────────────────

struct Args {
    tls: bool,
    host: String,
    port: u16,
}

fn parse_args() -> Args {
    let mut args = Args {
        tls: false,
        host: String::new(),
        port: 8443,
    };
    let mut host_given = false;
    let mut port_given = false;
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--tls" => {
                args.tls = true;
            }
            "--host" => {
                if i + 1 >= argv.len() {
                    usage_error("--host requires a value");
                }
                args.host = argv[i + 1].clone();
                host_given = true;
                i += 1;
            }
            "--port" => {
                if i + 1 >= argv.len() {
                    usage_error("--port requires a value");
                }
                args.port = argv[i + 1]
                    .parse()
                    .unwrap_or_else(|_| usage_error("port must be a number"));
                port_given = true;
                i += 1;
            }
            _ => {
                usage_error(&format!("unknown argument: {}", argv[i]));
            }
        }
        i += 1;
    }
    if (host_given || port_given) && !args.tls {
        usage_error("--host and --port require --tls");
    }
    if args.tls && !host_given {
        // We require --host rather than guessing the interface: a wrong guess
        // makes the printed relay URL and certificate SANs silently unusable.
        usage_error("--tls requires --host <ip-or-hostname>");
    }
    args
}

fn usage_error(msg: &str) -> ! {
    eprintln!("Error: {msg}");
    eprintln!("Usage: dev_relay [--tls --host <ip-or-hostname> [--port <port>]]");
    std::process::exit(1);
}

// ── TLS relay ────────────────────────────────────────────────────────────

async fn run_tls(host: &str, port: u16) -> Result<()> {
    // Create the CA and derive the fingerprint before any listeners are up
    // so the printed pin and the served cert are from the same bytes.
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("peakbot/dev-relay-tls");
    let ca = DevCa::load_or_create(&cache_dir)?;
    println!("CA dir: {}", cache_dir.display());

    let leaf_sans = dev_sans(host);
    let (leaf_pem, leaf_key_pem) = mint_leaf(&ca, leaf_sans)?;
    let leaf_cert = CertificateDer::from_pem_slice(leaf_pem.as_bytes())
        .context("leaf cert PEM did not parse")?;
    let leaf_key = PrivateKeyDer::from_pem_slice(leaf_key_pem.as_bytes())
        .context("leaf key PEM did not parse")?;

    // Build the rustls server config from the leaf.
    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("no supported TLS protocol versions")?
        .with_no_client_auth()
        .with_single_cert(vec![leaf_cert], leaf_key)
        .context("leaf cert and key did not form a usable pair")?;
    let tls_config =
        axum_server::tls_rustls::RustlsConfig::from_config(std::sync::Arc::new(config));

    // Build the relay config for the public HTTPS origin.
    let public_origin = format!("https://{host}:{port}");

    // Spawn the FakeIdp on loopback HTTP (it stays tokenless).
    // Only the authorize endpoint URL is public; discovery/issuer/token/jwks
    // stay loopback HTTP because the relay talks to them directly.
    let public_authorize_url = format!("{public_origin}/idp/authorize");
    let (idp_url, idp_state) = spawn_idp("0.0.0.0:8099", Some(public_authorize_url)).await;
    println!("FakeIdp listening at {idp_url} (loopback HTTP only)");
    let cfg = peakbot_relay::config::Config {
        bind: format!("0.0.0.0:{port}").parse().unwrap(),
        origin: peakbot_relay::config::Origin(public_origin.clone()),
        db: "/tmp/peakbot-dev-relay-tls.db".into(),
        app_scheme: Some("sh.shifu.mobile".to_string()),
        issuers: vec![peakbot_relay::config::Issuer {
            name: "fake".to_string(),
            url: idp_url.clone(),
            client_id: peakbot_relay::config::ClientId("relay-client-id".to_string()),
            client_secret: peakbot_relay::config::ClientSecret::new(
                "s3cret-client-secret-value".to_string(),
            ),
        }],
    };

    // Build the relay app with the build_app seam so we can add the FakeIdp
    // and CA routes on the same origin.
    let (mut app, handle) = peakbot_relay::build_app(cfg).await?;

    // Mount the FakeIdp routes at /idp/* on the same origin.
    // The relay's Oidp client talks to the loopback FakeIdp directly for
    // discovery, jwks, and token. Only the browser-facing authorize endpoints
    // are exposed via HTTPS on the relay's public origin.
    let idp_router = axum::Router::new()
        .route("/authorize", axum::routing::get(authorize))
        .route(
            "/authorize/continue",
            axum::routing::get(authorize_continue),
        )
        .with_state(idp_state.clone());
    app = app.nest("/idp", idp_router);

    // CA endpoint: serve the CA PEM as application/x-x509-ca-cert.
    app = app.route(
        "/peakbot-ca.crt",
        get({
            let ca_pem = ca.cert_pem.clone();
            move || {
                let pem = ca_pem.clone();
                async move {
                    (
                        [
                            ("Content-Type", "application/x-x509-ca-cert"),
                            ("Cache-Control", "public, max-age=31536000"),
                        ],
                        pem,
                    )
                }
            }
        }),
    );

    // Print startup info.
    println!();
    println!("Relay (TLS) listening at {public_origin}");
    println!("CA URL: {public_origin}/peakbot-ca.crt");
    println!("CA pin: {}", ca.fingerprint);
    println!();
    println!("To configure your phone (general.cloud):");
    println!("  relay: {public_origin}");
    println!("  ca: {}", ca.fingerprint);
    println!();
    println!("Restart the phone app after changing general.cloud.");
    println!("Browser OS trust is separate from the CA pin.");

    // Serve over TLS with axum-server.
    let socket =
        std::net::TcpListener::bind(format!("0.0.0.0:{port}")).context("binding TLS listener")?;
    socket
        .set_nonblocking(true)
        .context("setting nonblocking")?;
    let server_handle = axum_server::Handle::new();
    let server = axum_server::from_tcp_rustls(socket, tls_config)
        .context("from_tcp_rustls")?
        .handle(server_handle.clone())
        .serve(app.into_make_service());
    let server_task = tokio::spawn(server);

    // Wait for shutdown, then stop the relay and drain the HTTPS listener.
    tokio::signal::ctrl_c().await?;
    println!("Shutting down...");
    handle.shutdown().await;
    server_handle.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
    let _ = server_task.await;
    Ok(())
}

// ── Main ─────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let args = parse_args();

    if args.tls {
        // TLS mode with self-signed CA.
        run_tls(&args.host, args.port).await.unwrap();
    } else {
        // HTTP mode: the original dev relay behavior.
        let (idp_url, _idp_state) = spawn_idp("0.0.0.0:8099", None).await;
        println!("FakeIdp listening at {idp_url}");

        // Build relay config
        let cfg = peakbot_relay::config::Config {
            bind: "0.0.0.0:8080".parse().unwrap(),
            origin: peakbot_relay::config::Origin("http://localhost:8080".to_string()),
            db: "/tmp/peakbot-dev-relay.db".into(),
            app_scheme: Some("sh.shifu.mobile".to_string()),
            issuers: vec![peakbot_relay::config::Issuer {
                name: "fake".to_string(),
                url: idp_url.clone(),
                client_id: peakbot_relay::config::ClientId("relay-client-id".to_string()),
                client_secret: peakbot_relay::config::ClientSecret::new(
                    "s3cret-client-secret-value".to_string(),
                ),
            }],
        };

        let listener = tokio::net::TcpListener::bind(cfg.bind).await.unwrap();
        println!("Relay listening at http://0.0.0.0:8080");
        println!("app_scheme: sh.shifu.mobile");
        println!();
        println!("To test, open in browser:");
        println!(
            "  http://0.0.0.0:8080/login?code_challenge=…&redirect_uri=sh.shifu.mobile:/cb&state=dev"
        );
        println!();
        println!(
            "The FakeIdp will auto-approve and redirect to sh.shifu.mobile:/cb?code=…&state=dev"
        );

        let handle = peakbot_relay::serve(cfg, listener).await.unwrap();
        println!("Relay handle obtained, waiting for shutdown...");
        tokio::signal::ctrl_c().await.unwrap();
        println!("Shutting down...");
        handle.shutdown().await;
    }
}

// ── DevCa contract tests ─────────────────────────────────────────
// Exercise the example-private DevCa helper the TLS mode depends on:
// stable PEM + `sha256:<64 hex>` fingerprint over the CA DER across loads.

#[cfg(test)]
mod devca_tests {
    use super::*;
    use std::fs;

    /// Test 1: stability — two loads from the same dir yield identical cert
    /// and fingerprint (proves the CA is persisted and reloaded, not
    /// regenerated on each call).
    #[test]
    fn devca_load_or_create_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let ca1 = DevCa::load_or_create(dir.path()).unwrap();
        let ca2 = DevCa::load_or_create(dir.path()).unwrap();
        assert_eq!(
            ca1.cert_pem, ca2.cert_pem,
            "second load should return the same PEM"
        );
        assert_eq!(
            ca1.fingerprint, ca2.fingerprint,
            "second load should return the same fingerprint"
        );
    }

    /// Test 2: fingerprint format — `sha256:` + 64 lowercase hex chars.
    #[test]
    fn devca_fingerprint_format() {
        let dir = tempfile::tempdir().unwrap();
        let ca = DevCa::load_or_create(dir.path()).unwrap();
        let fp = &ca.fingerprint;
        assert!(
            fp.starts_with("sha256:"),
            "fingerprint must start with 'sha256:'"
        );
        let hex = &fp[7..];
        assert_eq!(hex.len(), 64, "fingerprint hex part must be 64 chars");
        for ch in hex.chars() {
            assert!(
                ch.is_ascii_hexdigit(),
                "fingerprint hex part must be lowercase hex"
            );
            // Check lowercase (no uppercase hex digits).
            assert!(
                !ch.is_ascii_uppercase(),
                "fingerprint hex must be lowercase"
            );
        }
    }

    /// Test 3: fingerprint is SHA-256 of the cert DER. Decode PEM → DER,
    /// hash, lowercase-hex encode, and compare to the fingerprint.
    #[test]
    fn devca_fingerprint_matches_sha256_of_cert_der() {
        let dir = tempfile::tempdir().unwrap();
        let ca = DevCa::load_or_create(dir.path()).unwrap();

        // Parse PEM to DER (cert only).
        let cert_pem = &ca.cert_pem;
        let lines: Vec<&str> = cert_pem.lines().collect();
        let mut pem_body = String::new();
        for line in lines {
            if line.starts_with("-----BEGIN CERTIFICATE-----")
                || line.starts_with("-----END CERTIFICATE-----")
            {
                continue;
            }
            pem_body.push_str(line);
        }
        let der = B64S
            .decode(pem_body.trim())
            .expect("PEM body must be valid base64");

        // Compute SHA-256 of DER and compare to fingerprint (manual lowercase
        // hex encoding to avoid depending on the hex crate).
        let digest = Sha256::digest(&der);
        let hex_part: String = digest.iter().map(|b| format!("{:02x}", b)).collect();
        let expected_fp = format!("sha256:{}", hex_part);
        assert_eq!(
            ca.fingerprint, expected_fp,
            "fingerprint must be sha256: of the DER bytes"
        );
    }

    /// Edge case: loading from a nonexistent but writable path creates a new
    /// CA and writes the files there.
    #[test]
    fn devca_load_or_create_creates_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let fresh = tmp.path().join("nonexistent-subdir");
        fs::create_dir_all(&fresh).unwrap();
        let ca = DevCa::load_or_create(&fresh).unwrap();
        assert!(!ca.cert_pem.is_empty(), "PEM must not be empty");
        assert!(
            ca.fingerprint.starts_with("sha256:"),
            "fingerprint must be sha256-prefixed"
        );
        // Verify files were created on disk.
        let cert_path = fresh.join("cert.pem");
        let key_path = fresh.join("key.pem");
        assert!(
            cert_path.exists() || key_path.exists(),
            "CA files must be written to the directory"
        );
    }
}

// ── IdP handler contract tests (TDD) ────────────────────────────────
// Exercise the real IdP handlers via HTTP to verify they return proper
// 400 responses (not panics/connection errors) for malformed requests,
// and that a valid flow still works after rejected requests (no-poison
// regression). These are written BEFORE the fix and should all be RED.

#[cfg(test)]
mod idp_tests {
    use super::*;

    /// Build a valid authorize query string with all OIDC fields.
    fn valid_authorize_query(base: &str) -> String {
        format!(
            "{}?response_type=code&client_id=relay-client-id&redirect_uri=http://localhost:8080/oidc/callback&scope=openid&state=abc&nonce=def&code_challenge=iMnq5o6zALKXGivsnlom_0F5_WYda32GHkxlV7mq7hQ&code_challenge_method=S256",
            base
        )
    }

    /// Build an authorize query with a wrong client_id.
    fn wrong_client_authorize_query(base: &str) -> String {
        format!(
            "{}?response_type=code&client_id=wrong-id&redirect_uri=http://localhost:8080/oidc/callback&scope=openid&state=abc&nonce=def&code_challenge=iMnq5o6zALKXGivsnlom_0F5_WYda32GHkxlV7mq7hQ&code_challenge_method=S256",
            base
        )
    }

    /// Build an authorize query missing code_challenge.
    fn missing_challenge_authorize_query(base: &str) -> String {
        format!(
            "{}?response_type=code&client_id=relay-client-id&redirect_uri=http://localhost:8080/oidc/callback&scope=openid&state=abc&nonce=def",
            base
        )
    }

    /// Build a valid authorize_continue query string with all OIDC fields.
    fn valid_continue_query(base: &str) -> String {
        format!(
            "{}?response_type=code&client_id=relay-client-id&redirect_uri=http://localhost:8080/oidc/callback&scope=openid&state=abc&nonce=def&code_challenge=iMnq5o6zALKXGivsnlom_0F5_WYda32GHkxlV7mq7hQ&code_challenge_method=S256",
            base
        )
    }

    /// Build an authorize_continue query missing redirect_uri.
    fn missing_redirect_continue_query(base: &str) -> String {
        format!(
            "{}?response_type=code&client_id=relay-client-id&scope=openid&state=abc&nonce=def&code_challenge=iMnq5o6zALKXGivsnlom_0F5_WYda32GHkxlV7mq7hQ&code_challenge_method=S256",
            base
        )
    }

    /// Build an authorize_continue query with an invalid redirect_uri.
    fn invalid_redirect_continue_query(base: &str) -> String {
        format!(
            "{}?response_type=code&client_id=relay-client-id&redirect_uri=not-a-url&scope=openid&state=abc&nonce=def&code_challenge=iMnq5o6zALKXGivsnlom_0F5_WYda32GHkxlV7mq7hQ&code_challenge_method=S256",
            base
        )
    }

    /// Build an authorize_continue query with a wrong client_id.
    fn wrong_client_continue_query(base: &str) -> String {
        format!(
            "{}?response_type=code&client_id=wrong-id&redirect_uri=http://localhost:8080/oidc/callback&scope=openid&state=abc&nonce=def&code_challenge=iMnq5o6zALKXGivsnlom_0F5_WYda32GHkxlV7mq7hQ&code_challenge_method=S256",
            base
        )
    }

    /// Extract the code from a redirect URL.
    fn extract_code(url: &str) -> Option<String> {
        let parsed = url::Url::parse(url).ok()?;
        for (k, v) in parsed.query_pairs() {
            if k == "code" {
                return Some(v.to_string());
            }
        }
        None
    }

    #[tokio::test]
    async fn authorize_without_params_returns_400() {
        let (url, _state) = spawn_idp("127.0.0.1:0", None).await;
        // No query params at all — should be 400 not a panic/connection error
        let resp = reqwest::get(&format!("{}/authorize", url)).await.unwrap();
        assert_eq!(
            resp.status(),
            400,
            "/authorize without params should return 400"
        );
    }

    #[tokio::test]
    async fn authorize_wrong_client_id_returns_400() {
        let (url, _state) = spawn_idp("127.0.0.1:0", None).await;
        let resp = reqwest::get(&wrong_client_authorize_query(&format!("{}/authorize", url)))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            400,
            "/authorize with wrong client_id should return 400"
        );
    }

    #[tokio::test]
    async fn authorize_missing_code_challenge_returns_400() {
        let (url, _state) = spawn_idp("127.0.0.1:0", None).await;
        let resp = reqwest::get(&missing_challenge_authorize_query(&format!(
            "{}/authorize",
            url
        )))
        .await
        .unwrap();
        assert_eq!(
            resp.status(),
            400,
            "/authorize missing code_challenge should return 400"
        );
    }

    #[tokio::test]
    async fn authorize_continue_missing_redirect_uri_returns_400() {
        let (url, _state) = spawn_idp("127.0.0.1:0", None).await;
        let resp = reqwest::get(&missing_redirect_continue_query(&format!(
            "{}/authorize/continue",
            url
        )))
        .await
        .unwrap();
        assert_eq!(
            resp.status(),
            400,
            "/authorize/continue missing redirect_uri should return 400"
        );
    }

    #[tokio::test]
    async fn authorize_continue_invalid_redirect_uri_returns_400() {
        let (url, _state) = spawn_idp("127.0.0.1:0", None).await;
        let resp = reqwest::get(&invalid_redirect_continue_query(&format!(
            "{}/authorize/continue",
            url
        )))
        .await
        .unwrap();
        assert_eq!(
            resp.status(),
            400,
            "/authorize/continue with invalid redirect_uri should return 400"
        );
    }

    #[tokio::test]
    async fn authorize_continue_wrong_client_id_returns_400() {
        let (url, _state) = spawn_idp("127.0.0.1:0", None).await;
        let resp = reqwest::get(&wrong_client_continue_query(&format!(
            "{}/authorize/continue",
            url
        )))
        .await
        .unwrap();
        assert_eq!(
            resp.status(),
            400,
            "/authorize/continue with wrong client_id should return 400"
        );
    }

    #[tokio::test]
    async fn full_flow_works_after_rejected_requests() {
        // Spawn IdP and issue several malformed requests that currently panic.
        // After the fix, the IdP should survive and a valid flow should complete.
        let (url, _state) = spawn_idp("127.0.0.1:0", None).await;

        // First, hit the IdP with the malformed requests (these currently panic).
        // Even if they fail, the IdP task keeps running (it's spawned separately).
        // We don't assert on these — we're testing that they don't poison the IdP.
        let _ = reqwest::get(&format!("{}/authorize", url)).await;
        let _ = reqwest::get(&wrong_client_authorize_query(&format!("{}/authorize", url))).await;
        let _ = reqwest::get(&wrong_client_continue_query(&format!(
            "{}/authorize/continue",
            url
        )))
        .await;

        // Now test the valid full flow.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();

        // Step 1: GET /authorize with valid params → 200 HTML form
        let resp = client
            .get(&valid_authorize_query(&format!("{}/authorize", url)))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "authorize should return 200 HTML form");
        let html = resp.text().await.unwrap();
        assert!(
            html.contains("Sign In"),
            "authorize should return HTML with 'Sign In' form"
        );

        // Step 2: GET /authorize/continue with valid params → 303 redirect with code
        let resp = client
            .get(&valid_continue_query(&format!(
                "{}/authorize/continue",
                url
            )))
            .send()
            .await
            .unwrap();
        assert!(
            resp.status().is_redirection(),
            "authorize/continue should return redirect"
        );
        let location = resp
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let code = extract_code(&location).expect("redirect URL should contain code param");

        // Step 3: POST /token with the code → 200 JSON with id_token
        let resp = client
            .post(&format!("{}/token", url))
            .basic_auth("relay-client-id", Some("s3cret-client-secret-value"))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("code_verifier", "verifier"),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "token endpoint should return 200");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(
            body.get("id_token").is_some(),
            "token response should contain id_token"
        );

        // Verify the id_token contains the expected nonce
        let id_token = body["id_token"].as_str().unwrap();
        let parts: Vec<&str> = id_token.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT should have 3 parts");
        let payload_b64 = parts[1];
        let payload_json: serde_json::Value = serde_json::from_slice(
            &base64::Engine::decode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                payload_b64,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            payload_json["nonce"], "def",
            "id_token nonce should match the authorize nonce"
        );
    }

    /// Runtime test: deliberately poison the IdP's mutex and verify that
    /// subsequent requests still work (lock helper recovers from poison).
    /// Currently this will fail because the handlers use .lock().unwrap()
    /// which panics on a poisoned mutex.
    #[tokio::test]
    async fn lock_helper_recovers_from_poisoned_mutex() {
        let (url, state) = spawn_idp("127.0.0.1:0", None).await;

        // Poison the mutex by panicking while holding the lock in a separate task.
        let poison_task = tokio::spawn({
            let s = state.clone();
            async move {
                let _locked = s.lock().unwrap();
                // Panic while holding the lock to poison it.
                panic!("poison the mutex");
            }
        });
        // Wait for the poison task; the JoinError indicates the panic.
        let _ = poison_task.await;

        // Small delay to let the poison propagate.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Now make a request to the IdP — should still work after the fix.
        let resp = reqwest::get(&format!("{}/jwks", url)).await.unwrap();
        assert_eq!(
            resp.status(),
            200,
            "jwks endpoint should still work after mutex is poisoned"
        );
    }
}
