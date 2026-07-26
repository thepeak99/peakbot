//! Shared reqwest client construction — one TLS policy and one timeout policy
//! for every HTTP client.
//!
//! On Android, reqwest's default `rustls-platform-verifier` panics (no JVM to
//! reach the system trust store, reqwest#2968), so there we root TLS in the
//! bundled Mozilla `webpki-roots`. Desktop keeps the OS platform verifier.
//!
//! Always build clients via `client()` / `client_builder()` (rig providers:
//! `.http_client(http::client())`); a bare `reqwest::Client::new()` crashes on
//! Android and, worse, has no timeouts — an upstream that accepts a request and
//! never answers would wedge the turn until the process dies.

use std::sync::OnceLock;
use std::time::Duration;

use crate::config::HttpConfig;

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
/// OS verifier) and the configured timeouts. Callers add user-agent / their own
/// (shorter) total timeout then `.build()`.
pub fn client_builder() -> reqwest::ClientBuilder {
    #[cfg(target_os = "android")]
    let base = reqwest::Client::builder().use_preconfigured_tls(tls_config());
    #[cfg(not(target_os = "android"))]
    let base = reqwest::Client::builder();

    apply_timeouts(base, configured())
}

/// Timeouts set at boot. Unset (tests, examples) reads as the defaults, so
/// there is no "unconfigured" state that behaves differently.
static TIMEOUTS: OnceLock<HttpConfig> = OnceLock::new();

/// Publish the boot config's timeouts. Called once from `main`; later calls are
/// ignored, which is why this is boot-only and not reloaded by session verbs.
pub fn init_timeouts(cfg: HttpConfig) {
    let _ = TIMEOUTS.set(cfg);
}

fn configured() -> &'static HttpConfig {
    TIMEOUTS.get_or_init(HttpConfig::default)
}

/// Apply `cfg` to a builder; `0` means "no timeout" for either knob.
fn apply_timeouts(mut b: reqwest::ClientBuilder, cfg: &HttpConfig) -> reqwest::ClientBuilder {
    if cfg.connect_timeout_secs > 0 {
        b = b.connect_timeout(Duration::from_secs(cfg.connect_timeout_secs));
    }
    if cfg.read_timeout_secs > 0 {
        b = b.read_timeout(Duration::from_secs(cfg.read_timeout_secs));
    }
    b
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

    /// A server that accepts the connection and then never answers is exactly
    /// how a turn wedged for 56 minutes (upstream took the request and sent no
    /// response headers). The read timeout is what bounds it.
    #[tokio::test]
    async fn read_timeout_bounds_a_server_that_never_responds() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // Hold the accepted socket open, reply with nothing.
        std::thread::spawn(move || {
            let _held = listener.accept();
            std::thread::sleep(std::time::Duration::from_secs(30));
        });

        let cfg = HttpConfig {
            connect_timeout_secs: 5,
            read_timeout_secs: 1,
        };
        let client = apply_timeouts(client_builder(), &cfg).build().unwrap();

        let started = std::time::Instant::now();
        let err = client
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect_err("a silent server must not hang forever");

        assert!(err.is_timeout(), "expected a timeout, got: {err}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "timeout took too long: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn zero_disables_a_timeout() {
        let cfg = HttpConfig {
            connect_timeout_secs: 0,
            read_timeout_secs: 0,
        };
        // Nothing to assert on the built client (reqwest exposes no getters);
        // the contract worth pinning is that 0 is accepted, not rejected.
        assert!(apply_timeouts(client_builder(), &cfg).build().is_ok());
    }
}
