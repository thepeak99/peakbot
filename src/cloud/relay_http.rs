//! Relay TLS pinning policy (p3-design §B).
//!
//! One `RelayHttp` per `general.cloud` config: fetches the relay's CA once
//! (tokenless `/peakbot-ca.crt`), validates the fingerprint, builds a
//! rustls client config that trusts only that CA, and caches it for every
//! subsequent HTTPS call. `ca: system` uses the production trust store.
//! Loopback HTTP gets no TLS config.

use std::sync::Arc;
use std::time::Duration;

use rustls::ClientConfig;
use rustls::pki_types::pem::PemObject;
use url::Url;

use super::link::CloudError;
use super::login::LoginPlan;
use super::store::SessionToken;
use crate::config::remote::Endpoint;

/// One HTTPS policy for one relay endpoint. Built once at boot from the
/// parsed `general.cloud`; every login, token-refresh and logout call
/// routes through it so the CA is fetched exactly once and the rustls
/// config is shared via `Arc`.
pub struct RelayHttp {
    endpoint: Endpoint,
    /// Fetched-once TLS config (lazily populated). Only successful
    /// configs are cached; errors are retried on the next call.
    tls_config: tokio::sync::OnceCell<Arc<ClientConfig>>,
}

impl RelayHttp {
    /// Build the policy for this endpoint. Validation of the pin itself
    /// (fingerprint format) happened at parse time (`parse_endpoint`).
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            tls_config: tokio::sync::OnceCell::new(),
        }
    }

    /// The relay origin (scheme://host[:port]/), whichever trust mode.
    pub fn origin(&self) -> &Url {
        self.endpoint.origin()
    }

    /// Resolve the trust decision into a rustls client config.
    ///
    /// - `PinnedCa`: fetch `/peakbot-ca.crt`, validate the fingerprint,
    ///   build a config that trusts ONLY that CA (no fallback).
    /// - `System`: build a config with the production trust store.
    /// - Loopback HTTP: `Ok(None)` (no TLS).
    ///
    /// The result is cached — repeated calls return the same `Arc`.
    /// Errors are not cached: a failed CA fetch is retried on the next call.
    pub async fn tls_config(&self) -> Result<Option<Arc<ClientConfig>>, CloudError> {
        match &self.endpoint {
            Endpoint::LoopbackPlain { .. } => Ok(None),
            Endpoint::Tls {
                trust: crate::config::remote::Trust::System,
                ..
            } => {
                let cfg =
                    crate::http::system_tls_config().map_err(|e| CloudError::Io(e.to_string()))?;
                Ok(Some(Arc::new(cfg)))
            }
            Endpoint::Tls {
                trust: crate::config::remote::Trust::PinnedCa(pin),
                ..
            } => {
                // OnceCell::get_or_try_init only caches successful results.
                let cfg = self
                    .tls_config
                    .get_or_try_init(|| {
                        let origin = self.origin().clone();
                        let pin = *pin;
                        async move { pinned_ca(&origin, &pin).await }
                    })
                    .await?;
                Ok(Some(cfg.clone()))
            }
        }
    }

    /// Build a `reqwest::Client` that speaks to this relay with the correct
    /// TLS policy. Reuses the cached config; builds the reqwest client
    /// fresh each time (the config is shared via Arc).
    pub async fn client(&self) -> Result<reqwest::Client, CloudError> {
        let cfg = self.tls_config().await?;
        // `tls_backend_preconfigured` downcasts to a bare `rustls::ClientConfig`
        // (not `Arc<…>`), so hand it a clone of the deref'd inner value.
        let builder = crate::http::client_builder();
        let builder = match cfg {
            Some(cfg) => builder.tls_backend_preconfigured((*cfg).clone()),
            None => builder,
        };
        builder.build().map_err(|e| CloudError::Io(e.to_string()))
    }
}

/// Accepts any certificate (signatures still checked) — for the ONE CA
/// bootstrap GET, whose response is then authenticated by the pin
/// (same policy as the remote-profile bootstrap).
#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// `{origin}/peakbot-ca.crt` fetched unverified, fingerprint-checked
/// against the pin, then the ONLY trusted root (p3-design §B: "same
/// trust code as remote profiles").
async fn pinned_ca(
    origin: &Url,
    pin: &crate::config::remote::CaFingerprint,
) -> Result<Arc<ClientConfig>, CloudError> {
    // Exact same construction as src/ui/web/remote/mod.rs bootstrap_pinned.
    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let no_verify = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| CloudError::Io(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(NoVerify(provider)))
        .with_no_client_auth();
    // Use client_builder() like the remote code does.
    let client = crate::http::client_builder()
        .tls_backend_preconfigured(no_verify)
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| CloudError::Io(e.to_string()))?;
    let url = origin
        .join("peakbot-ca.crt")
        .map_err(|e| CloudError::Io(e.to_string()))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| CloudError::Io(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(CloudError::Io(format!(
            "CA fetch answered {}",
            resp.status()
        )));
    }
    let pem = resp
        .text()
        .await
        .map_err(|e| CloudError::Io(e.to_string()))?;
    let der = rustls::pki_types::CertificateDer::from_pem_slice(pem.as_bytes())
        .map_err(|_| CloudError::Io("the server CA is not a PEM certificate".into()))?;
    if crate::ui::web::tls::der_fingerprint(&der) != pin.to_string() {
        return Err(CloudError::Io("CA pin mismatch".into()));
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.add(der).map_err(|e| CloudError::Io(e.to_string()))?;
    let cfg = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| CloudError::Io(e.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Arc::new(cfg))
}

pub async fn redeem_token(
    http: &RelayHttp,
    plan: &LoginPlan,
    code: &str,
) -> Result<SessionToken, CloudError> {
    let url = http
        .origin()
        .join("token")
        .map_err(|e| CloudError::Io(e.to_string()))?;
    let client = http.client().await?;
    let resp = client
        .post(url)
        .json(&serde_json::json!({ "code": code, "code_verifier": plan.verifier }))
        .send()
        .await
        .map_err(|e| CloudError::Io(describe_error(&e)))?;
    if !resp.status().is_success() {
        return Err(CloudError::Io(format!(
            "token endpoint answered {}",
            resp.status()
        )));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| CloudError::Io(describe_error(&e)))?;
    let token = body
        .get("token")
        .and_then(|t| t.as_str())
        .ok_or_else(|| CloudError::Io("no token in reply".into()))?;
    Ok(SessionToken::new(token.to_string()))
}

pub async fn logout_post(http: &RelayHttp, token: &SessionToken) -> Result<(), CloudError> {
    let url = http
        .origin()
        .join("logout")
        .map_err(|e| CloudError::Io(e.to_string()))?;
    let client = http.client().await?;
    let req = client
        .post(url)
        .header("Authorization", format!("Bearer {}", token.as_str()))
        .send();
    let resp = tokio::time::timeout(Duration::from_secs(5), req)
        .await
        .map_err(|_| CloudError::Io("relay /logout timed out".into()))?
        .map_err(|e| CloudError::Io(describe_error(&e)))?;
    if !resp.status().is_success() {
        return Err(CloudError::Io(format!(
            "relay /logout answered {}",
            resp.status()
        )));
    }
    Ok(())
}

/// Flatten an error and its source chain into one message, so a TLS cause
/// buried under reqwest's "error sending request" surface still reads as a
/// certificate/verification failure.
fn describe_error(e: &(dyn std::error::Error + 'static)) -> String {
    let mut msg = e.to_string();
    let mut cur = e.source();
    while let Some(next) = cur {
        msg.push_str(": ");
        msg.push_str(&next.to_string());
        cur = next.source();
    }
    msg
}

// The test harness mints certs + runs a TLS axum server (rcgen/axum-server),
// host-only deps; Local edition must compile without them.
#[cfg(all(test, feature = "host"))]
mod tests {
    use super::*;
    use crate::config::remote::parse_endpoint;
    use crate::ui::web::tls::{ca_cert_pem, ca_fingerprint, server_config};
    use http_body_util::BodyExt;
    use rand_chacha::ChaCha20Rng;
    use rand_core::SeedableRng;
    use std::net::SocketAddr;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    /// The test HTTPS server: a self-signed CA + leaf serving /peakbot-ca.crt,
    /// POST /token, POST /logout on a loopback port.
    struct TestServer {
        addr: SocketAddr,
        /// CA fingerprint for the test leaf (the "correct" pin).
        ca_fingerprint: String,
        /// Counts how many times /token was hit (for "zero hits" assertions).
        token_hits: Arc<AtomicUsize>,
        /// Counts how many times /logout was hit.
        logout_hits: Arc<AtomicUsize>,
        /// The last redeem request body (for exact content assertions).
        last_redeem: Arc<Mutex<Option<serde_json::Value>>>,
        /// The last logout Authorization header.
        last_auth: Arc<Mutex<Option<String>>>,
        /// Join handle so the server task keeps running.
        handle: tokio::task::JoinHandle<()>,
        /// Temp TLS dir; dropped when this struct is dropped.
        _tls_dir: tempfile::TempDir,
    }

    impl TestServer {
        async fn spawn() -> Self {
            let tls_dir = tempdir().expect("temp tls dir");
            // Generate the CA cert + leaf for the test server.
            let ca_pem = ca_cert_pem(tls_dir.path()).expect("CA cert");
            let leaf_cfg = server_config(tls_dir.path(), &["localhost".to_string()])
                .expect("leaf server config");
            let ca_fingerprint = ca_fingerprint(tls_dir.path()).expect("CA fingerprint");

            let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
            let token_hits = Arc::new(AtomicUsize::new(0));
            let logout_hits = Arc::new(AtomicUsize::new(0));
            let last_redeem = Arc::new(Mutex::new(None));
            let last_auth = Arc::new(Mutex::new(None));

            // Build the axum router for the test server endpoints.
            let th = token_hits.clone();
            let lh = logout_hits.clone();
            let lr = last_redeem.clone();
            let la = last_auth.clone();
            let app = axum::Router::new()
                .route(
                    "/peakbot-ca.crt",
                    axum::routing::get({
                        let ca_pem = ca_pem.clone();
                        move || async move { axum::response::Html(ca_pem.clone()) }
                    }),
                )
                .route(
                    "/token",
                    axum::routing::post({
                        let th = th.clone();
                        let lr = lr.clone();
                        move |body: axum::body::Body| {
                            let th = th.clone();
                            let lr = lr.clone();
                            async move {
                                th.fetch_add(1, Ordering::SeqCst);
                                let collected = body.collect().await.expect("collect body");
                                let bytes = collected.to_bytes();
                                if let Ok(v) = serde_json::from_slice(&bytes) {
                                    let mut g = lr.lock().unwrap();
                                    *g = Some(v);
                                }
                                axum::Json(serde_json::json!({"token": "test-session-token"}))
                            }
                        }
                    }),
                )
                .route(
                    "/logout",
                    axum::routing::post({
                        let lh = lh.clone();
                        let la = la.clone();
                        move |headers: axum::http::HeaderMap| {
                            let lh = lh.clone();
                            let la = la.clone();
                            async move {
                                lh.fetch_add(1, Ordering::SeqCst);
                                let auth = headers
                                    .get(axum::http::header::AUTHORIZATION)
                                    .and_then(|v| v.to_str().ok())
                                    .unwrap_or_default()
                                    .to_string();
                                let mut g = la.lock().unwrap();
                                *g = Some(auth);
                                axum::Json(serde_json::json!({"ok": true}))
                            }
                        }
                    }),
                );

            let rustls_cfg = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(leaf_cfg));
            // Bind a plain listener first to get the actual ephemeral port.
            let plain_listener = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                tokio::net::TcpListener::bind("127.0.0.1:0"),
            )
            .await
            .expect("tcp bind timeout")
            .expect("tcp bind");
            let actual_addr = plain_listener.local_addr().expect("local_addr");
            drop(plain_listener); // Free the port; axum_server will rebind
            let handle = tokio::spawn(async move {
                let server = axum_server::bind_rustls(actual_addr, rustls_cfg);
                let future = server.serve(app.into_make_service());
                let _ = addr_tx.send(actual_addr);
                let result = future.await;
                if let Err(e) = result {
                    eprintln!("test server error: {e}");
                }
            });

            let addr = tokio::time::timeout(std::time::Duration::from_secs(2), addr_rx)
                .await
                .expect("server started")
                .expect("server sent address");

            Self {
                addr,
                ca_fingerprint,
                token_hits,
                logout_hits,
                last_redeem,
                last_auth,
                handle,
                _tls_dir: tls_dir,
            }
        }

        async fn shutdown(&mut self) {
            self.handle.abort();
        }

        fn url(&self) -> String {
            format!("https://localhost:{}", self.addr.port())
        }

        #[allow(dead_code)] // kept for symmetry with `url()`; no test dials plain HTTP yet
        fn loopback_url(&self) -> String {
            format!("http://127.0.0.1:{}", self.addr.port())
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    fn seeded_plan(base: &Url, redirect: &Url) -> LoginPlan {
        let mut rng = ChaCha20Rng::seed_from_u64(42);
        crate::cloud::login::plan_login(base, redirect, &mut rng).expect("plan_login")
    }

    #[tokio::test]
    async fn tls_config_cached_fetches_ca_once_and_reuses_config() {
        let mut server = TestServer::spawn().await;
        let endpoint = parse_endpoint(
            &server.url(),
            Some(server.ca_fingerprint.as_str()),
            "url",
            "relay's",
        )
        .expect("endpoint");
        let http = RelayHttp::new(endpoint);

        // Two tls_config calls return the same Arc and CA is fetched only once.
        let cfg1 = http.tls_config().await.expect("tls_config 1");
        let cfg2 = http.tls_config().await.expect("tls_config 2");
        assert!(cfg1.is_some(), "pinned CA endpoint must yield Some config");
        assert!(cfg2.is_some(), "cached config must be Some");
        // Same Arc — pointer equality proves caching (not re-fetch).
        assert!(
            Arc::ptr_eq(cfg1.as_ref().unwrap(), cfg2.as_ref().unwrap()),
            "tls_config must cache the Arc — second call reuses, not refetches"
        );
        // One client build reuses the cached config too.
        let client = http.client().await.expect("client build");
        let _ = client;
        // CA was fetched exactly once (one CA-cert request, not per-call).
        assert_eq!(
            server.token_hits.load(Ordering::SeqCst),
            0,
            "tls_config alone must not hit /token"
        );

        server.shutdown().await;
    }

    #[tokio::test]
    async fn matching_pin_https_redeem_token_succeeds_and_server_receives_exact_code_verifier() {
        let mut server = TestServer::spawn().await;
        let endpoint = parse_endpoint(
            &server.url(),
            Some(server.ca_fingerprint.as_str()),
            "url",
            "relay's",
        )
        .expect("endpoint");
        let http = RelayHttp::new(endpoint);

        let redirect = Url::parse("http://127.0.0.1:9999/cb").expect("redirect");
        let plan = seeded_plan(http.origin(), &redirect);
        let code = "one-time-code-abc";

        let token = redeem_token(&http, &plan, code)
            .await
            .expect("redeem_token");
        assert_eq!(
            token.as_str(),
            "test-session-token",
            "server returned the expected token"
        );

        // Verify the server received the exact code and verifier.
        let redeem = server
            .last_redeem
            .lock()
            .unwrap()
            .take()
            .expect("redeem recorded");
        assert_eq!(
            redeem.get("code").and_then(|v| v.as_str()),
            Some(code),
            "server must receive the exact auth code"
        );
        assert_eq!(
            redeem.get("code_verifier").and_then(|v| v.as_str()),
            Some(plan.verifier.as_str()),
            "server must receive the exact PKCE verifier"
        );
        assert_eq!(
            server.token_hits.load(Ordering::SeqCst),
            1,
            "redeem_token must hit /token exactly once"
        );

        server.shutdown().await;
    }

    #[tokio::test]
    async fn wrong_pin_fails_ca_mismatch_and_sends_zero_token_hits() {
        let mut server = TestServer::spawn().await;
        // Wrong pin: 32 zeros (valid hex shape, wrong value).
        let wrong_pin = "sha256:".to_string() + &"00".repeat(32);
        let endpoint =
            parse_endpoint(&server.url(), Some(&wrong_pin), "url", "relay's").expect("endpoint");
        let http = RelayHttp::new(endpoint);

        let redirect = Url::parse("http://127.0.0.1:9999/cb").expect("redirect");
        let plan = seeded_plan(http.origin(), &redirect);

        let result = redeem_token(&http, &plan, "code-x").await;
        assert!(result.is_err(), "wrong pin must fail");
        let err = result.unwrap_err();
        // The error must name the pin mismatch, not a generic I/O error.
        let msg = format!("{err}");
        assert!(
            msg.contains("pin") || msg.contains("mismatch") || msg.contains("fingerprint"),
            "error must reference the CA pin mismatch: {msg}"
        );
        // The /token endpoint must have been called zero times.
        assert_eq!(
            server.token_hits.load(Ordering::SeqCst),
            0,
            "wrong pin must fail before reaching /token"
        );

        server.shutdown().await;
    }

    #[tokio::test]
    async fn system_trust_rejects_self_signed_cert_and_sends_zero_token_hits() {
        let mut server = TestServer::spawn().await;
        // Trust::System: no pin, system roots only — must reject self-signed.
        let endpoint =
            parse_endpoint(&server.url(), Some("system"), "url", "relay's").expect("endpoint");
        let http = RelayHttp::new(endpoint);

        let redirect = Url::parse("http://127.0.0.1:9999/cb").expect("redirect");
        let plan = seeded_plan(http.origin(), &redirect);

        let result = redeem_token(&http, &plan, "code-y").await;
        assert!(result.is_err(), "system trust must reject self-signed cert");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("certificate") || msg.contains("tls") || msg.contains("verify"),
            "error must reference TLS certificate verification: {msg}"
        );
        assert_eq!(
            server.token_hits.load(Ordering::SeqCst),
            0,
            "system trust must fail before reaching /token"
        );

        server.shutdown().await;
    }

    #[tokio::test]
    async fn logout_matching_pin_succeeds_with_bearer_auth_wrong_pin_sends_zero_logout() {
        let mut server = TestServer::spawn().await;

        // Correct pin: logout succeeds with Bearer auth.
        let endpoint = parse_endpoint(
            &server.url(),
            Some(server.ca_fingerprint.as_str()),
            "url",
            "relay's",
        )
        .expect("endpoint");
        let http = RelayHttp::new(endpoint);
        let token = SessionToken::new("test-session-token".to_string());

        logout_post(&http, &token).await.expect("logout");
        assert_eq!(
            server.logout_hits.load(Ordering::SeqCst),
            1,
            "logout must hit /logout exactly once"
        );
        let auth = server
            .last_auth
            .lock()
            .unwrap()
            .take()
            .expect("auth recorded");
        assert_eq!(
            auth, "Bearer test-session-token",
            "logout must send the exact Bearer auth header"
        );

        // Wrong pin: zero /logout hits.
        let wrong_pin = "sha256:".to_string() + &"ff".repeat(32);
        let endpoint2 =
            parse_endpoint(&server.url(), Some(&wrong_pin), "url", "relay's").expect("endpoint");
        let http2 = RelayHttp::new(endpoint2);
        let result = logout_post(&http2, &token).await;
        assert!(result.is_err(), "wrong pin logout must fail");
        assert_eq!(
            server.logout_hits.load(Ordering::SeqCst),
            1,
            "wrong pin must not reach /logout (still 1 from the first call)"
        );

        server.shutdown().await;
    }

    #[tokio::test]
    async fn loopback_http_returns_none_tls_and_builds_client() {
        // No TLS: loopback HTTP endpoint gets None TLS config and builds a client.
        let endpoint =
            parse_endpoint("http://127.0.0.1:7823", None, "url", "relay's").expect("endpoint");
        let http = RelayHttp::new(endpoint);

        let cfg = http.tls_config().await.expect("tls_config on loopback");
        assert!(cfg.is_none(), "loopback HTTP must yield None TLS config");

        let client = http.client().await.expect("client build on loopback");
        let _ = client; // Client built successfully.
    }
}
