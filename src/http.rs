//! Shared reqwest client construction.
//!
//! Every HTTP client in PeakBot is built here so they all share one TLS policy.
//!
//! On **Android** we hand reqwest a rustls config rooted in the bundled Mozilla
//! `webpki-roots` instead of letting it default to `rustls-platform-verifier`,
//! which panics on Android/Termux where no JVM is available to reach the system
//! trust store (reqwest#2968). The roots are static and the binary is rebuilt
//! every release, so a frozen trust set is the right trade-off there.
//!
//! On **desktop** (Linux/macOS/Windows) we change nothing: reqwest keeps its
//! default platform verifier and the OS system trust store, exactly as before.
//!
//! New HTTP clients MUST be built through `client()` / `client_builder()` (or,
//! for rig-core providers, `.http_client(http::client())`) so the Android TLS
//! policy is applied consistently. A bare `reqwest::Client::new()` works on
//! desktop but crashes on Android.

#[cfg(target_os = "android")]
use std::sync::Arc;

/// A rustls config trusting the bundled Mozilla webpki roots, using the
/// aws-lc-rs provider already linked via reqwest's `rustls` feature.
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

/// A reqwest builder. On Android it is preconfigured with webpki-root TLS; on
/// desktop it is reqwest's default (OS platform verifier). Callers add their own
/// timeout / user-agent and then `.build()`.
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

/// A ready reqwest client with the platform-appropriate TLS policy and no extra
/// options.
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
