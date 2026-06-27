//! Shared reqwest client construction — one TLS policy for every HTTP client.
//!
//! On Android, reqwest's default `rustls-platform-verifier` panics (no JVM to
//! reach the system trust store, reqwest#2968), so there we root TLS in the
//! bundled Mozilla `webpki-roots`. Desktop keeps the OS platform verifier.
//!
//! Always build clients via `client()` / `client_builder()` (rig providers:
//! `.http_client(http::client())`); a bare `reqwest::Client::new()` crashes on
//! Android.

#[cfg(target_os = "android")]
use std::sync::Arc;

/// rustls config trusting the bundled Mozilla webpki roots (aws-lc-rs provider,
/// already linked via reqwest's `rustls` feature).
#[cfg(target_os = "android")]
fn tls_config() -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("aws-lc-rs supports the default protocol versions")
    .with_root_certificates(roots)
    .with_no_client_auth()
}

/// reqwest builder with the platform TLS policy (Android: webpki roots; desktop:
/// OS verifier). Callers add timeout / user-agent then `.build()`.
pub fn client_builder() -> reqwest::ClientBuilder {
    #[cfg(target_os = "android")]
    {
        reqwest::Client::builder().use_preconfigured_tls(tls_config())
    }
    #[cfg(not(target_os = "android"))]
    {
        reqwest::Client::builder()
    }
}

/// Ready reqwest client with the platform TLS policy and no extra options.
pub fn client() -> reqwest::Client {
    client_builder()
        .build()
        .expect("reqwest client builds with the platform TLS policy")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_client_without_panicking() {
        // On desktop this exercises the default reqwest path; on Android it
        // exercises tls_config() (root loading + aws-lc-rs provider).
        let _ = client();
    }
}
