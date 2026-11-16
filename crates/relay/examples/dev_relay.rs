//! Dev OIDC harness for on-device e2e testing (P5 T1).
//!
//! Spawns a FakeIdp on 0.0.0.0:8099 and a relay on 0.0.0.0:8080 with
//! app_scheme: sh.shifu.mobile. The FakeIdp auto-approves every authorize
//! request (no UI) and redirects with a code.
//!
//! Redirect chain (mobile):
//!   1. Device opens relay's /login?code_challenge=…&redirect_uri=sh.shifu.mobile:/cb&state=…
//!   2. Relay 302s to IdP's /authorize (with relay's own state/nonce/challenge)
//!   3. FakeIdp auto-approves, 302s to relay's /oidc/callback?code=…&state=…
//!   4. Relay exchanges code for id_token, issues one-time code, 302s to sh.shifu.mobile:/cb?code=…&state=…
//!   5. Device receives callback via custom scheme, POSTs code+verifier to relay's /token
//!
//! To run:
//!   cargo run -p peakbot-relay --example dev_relay
//!
//! To run the relay against it (production binary):
//!   # In relay's relay.yaml, set:
//!   #   app_scheme: sh.shifu.mobile
//!   #   issuers:
//!   #     - name: fake
//!   #       url: http://0.0.0.0:8099
//!   #       client_id: relay-client-id
//!   #       client_secret: s3cret-client-secret-value
//!
//! This example is TEST-ONLY and never ships in production builds.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD as B64S, URL_SAFE_NO_PAD as B64U};
use rand::RngCore;
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

struct IdpInner {
    url: String,
    client_id: String,
    client_secret: String,
    es: p256::ecdsa::SigningKey,
    // code -> (flow nonce, relay S256 challenge)
    codes: HashMap<String, (String, String)>,
}

async fn discovery(
    axum::extract::State(st): axum::extract::State<Arc<Mutex<IdpInner>>>,
) -> axum::Json<serde_json::Value> {
    let st = st.lock().unwrap();
    axum::Json(serde_json::json!({
        "issuer": st.url.clone(),
        "authorization_endpoint": format!("{}/authorize", st.url),
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

async fn jwks(
    axum::extract::State(st): axum::extract::State<Arc<Mutex<IdpInner>>>,
) -> axum::Json<serde_json::Value> {
    let st = st.lock().unwrap();
    axum::Json(serde_json::json!({ "keys": [es_jwk(&st.es, "idp-es256")] }))
}

async fn authorize(
    axum::extract::State(st): axum::extract::State<Arc<Mutex<IdpInner>>>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> axum::response::Html<String> {
    let st = st.lock().unwrap();

    // Validate client_id
    if q.get("client_id").map(|s| s.as_str()) != Some(st.client_id.as_str()) {
        panic!("authorize: wrong client_id");
    }

    // Build an HTML page with a "Sign in" button (form).
    // This simulates the user gesture required on Chrome Android to
    // trigger a custom-scheme redirect (sh.shifu.mobile:/cb).
    let mut fields = String::new();
    for (k, v) in q.iter() {
        fields.push_str(&format!(
            "<input type=\"hidden\" name=\"{k}\" value=\"{v}\">"
        ));
    }
    let html = format!(
        "<html><head><title>Sign In</title></head><body><h1>Sign In</h1>\
         <p>Click to approve the login request.</p>\
         <form method=\"get\" action=\"/authorize/continue\">\
         {fields}\
         <button type=\"submit\">Sign in</button>\
         </form></body></html>"
    );
    axum::response::Html(html)
}

async fn authorize_continue(
    axum::extract::State(st): axum::extract::State<Arc<Mutex<IdpInner>>>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> axum::response::Redirect {
    let mut st = st.lock().unwrap();

    // Validate client_id
    if q.get("client_id").map(|s| s.as_str()) != Some(st.client_id.as_str()) {
        panic!("authorize: wrong client_id");
    }

    // Extract required params
    let redirect_uri = q
        .get("redirect_uri")
        .expect("authorize: missing redirect_uri")
        .clone();
    let state = q.get("state").cloned().unwrap_or_default();
    let nonce = q.get("nonce").cloned().unwrap_or_default();
    let challenge = q
        .get("code_challenge")
        .expect("authorize: missing code_challenge")
        .clone();

    // Auto-approve: generate code and redirect
    let code = b64u(&rand_bytes(16));
    st.codes.insert(code.clone(), (nonce, challenge));

    // Build redirect URL with code and state
    let mut url = url::Url::parse(&redirect_uri).expect("valid redirect_uri");
    url.query_pairs_mut()
        .clear()
        .append_pair("code", &code)
        .append_pair("state", &state);

    axum::response::Redirect::to(url.as_str())
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
    let mut st = st.lock().unwrap();

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

async fn spawn_idp(bind: &str) -> String {
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
    url
}

#[tokio::main]
async fn main() {
    let idp_url = spawn_idp("0.0.0.0:8099").await;
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
    println!("The FakeIdp will auto-approve and redirect to sh.shifu.mobile:/cb?code=…&state=dev");

    let handle = peakbot_relay::serve(cfg, listener).await.unwrap();
    println!("Relay handle obtained, waiting for shutdown...");
    // Wait for shutdown signal (never comes in this example, so runs forever)
    tokio::signal::ctrl_c().await.unwrap();
    println!("Shutting down...");
    handle.shutdown().await;
}
