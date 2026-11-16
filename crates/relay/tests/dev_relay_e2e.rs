//! End-to-end test of the dev_relay harness: drives the full OIDC login
//! chain through the FakeIdp's /authorize endpoint and verifies the final
//! redirect is to the app scheme URL with a one-time code.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64U;
use sha2::{Digest, Sha256};
use url::Url;

// Reuse the test harness infrastructure
use peakbot_relay::config::{ClientId, ClientSecret, Config, Origin};
use rand::RngCore;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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

// ------------------------------------------------------------------- FakeIdp

struct IdpInner {
    url: String,
    client_id: String,
    client_secret: String,
    es: p256::ecdsa::SigningKey,
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
) -> axum::response::Redirect {
    let mut st = st.lock().unwrap();

    if q.get("client_id").map(|s| s.as_str()) != Some(st.client_id.as_str()) {
        panic!("authorize: wrong client_id");
    }

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

    let code = b64u(&rand_bytes(16));
    st.codes.insert(code.clone(), (nonce, challenge));

    let mut url = Url::parse(&redirect_uri).expect("valid redirect_uri");
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

    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let want = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", st.client_id, st.client_secret))
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

async fn spawn_idp() -> (String, String, String) {
    let es = p256::ecdsa::SigningKey::from_slice(&rand_bytes(32)).expect("p256 key");
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
        codes: HashMap::new(),
    }));
    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(discovery),
        )
        .route("/jwks", axum::routing::get(jwks))
        .route("/authorize", axum::routing::get(authorize))
        .route("/token", axum::routing::post(token_endpoint))
        .with_state(inner.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (url, client_id, client_secret)
}

#[tokio::test]
async fn dev_relay_e2e_login_chain() {
    // Spawn FakeIdp
    let (idp_url, client_id, client_secret) = spawn_idp().await;

    // Spawn relay with app_scheme
    let tmp = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cfg = Config {
        bind: addr,
        origin: Origin(format!("http://{addr}")),
        db: tmp.path().join("relay.db"),
        app_scheme: Some("sh.shifu.mobile".to_string()),
        issuers: vec![peakbot_relay::config::Issuer {
            name: "fake".to_string(),
            url: idp_url.clone(),
            client_id: ClientId(client_id.clone()),
            client_secret: ClientSecret::new(client_secret.clone()),
        }],
    };
    let relay = peakbot_relay::serve(cfg, listener).await.expect("serve");

    // PKCE pair
    let verifier = b64u(&rand_bytes(32));
    let challenge = b64u(Sha256::digest(verifier.as_bytes()).as_slice());

    // Step 1: Device opens relay's /login
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let resp = http
        .get(format!("http://{addr}/login"))
        .query(&[
            ("code_challenge", challenge.as_str()),
            ("redirect_uri", "sh.shifu.mobile:/cb"),
            ("state", "dev-state-1"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 302, "step 1: /login should 302");
    let loc = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(loc.starts_with(&idp_url), "step 1: should redirect to IdP");

    // Step 2: FakeIdp /authorize auto-approves and redirects to relay's /oidc/callback
    let resp = http.get(loc).send().await.unwrap();
    assert!(
        resp.status() == 302 || resp.status() == 303,
        "step 2: /authorize should redirect, got {}",
        resp.status()
    );
    let loc = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        loc.contains("/oidc/callback"),
        "step 2: should redirect to relay callback"
    );

    // Step 3: Relay exchanges code, issues one-time code, redirects to app scheme
    let resp = http.get(loc).send().await.unwrap();
    assert_eq!(resp.status(), 302, "step 3: /oidc/callback should 302");
    let final_loc = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        final_loc.starts_with("sh.shifu.mobile:/cb"),
        "step 3: should redirect to app scheme, got {final_loc}"
    );

    // Parse the one-time code from the final redirect
    let u = Url::parse(final_loc).unwrap();
    let code = u
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
        .expect("final redirect should have code");
    assert!(!code.is_empty(), "code should not be empty");

    // Step 4: Device POSTs code + verifier to relay's /token
    let resp = http
        .post(format!("http://{addr}/token"))
        .json(&serde_json::json!({
            "code": code,
            "code_verifier": verifier,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "step 4: /token should 200");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["token"].is_string(), "should have token field");

    relay.shutdown().await;
}
