//! Contract for PeakBot's built-in CA + TLS (`peakbot::ui::web::tls`).
//!
//! The whole point of the CA indirection: you install the CA on the phone
//! ONCE, and every leaf the CA signs is then trusted. These tests pin that
//! contract end-to-end — the strongest proof (a real localhost TLS handshake
//! rooted on the generated CA) needs no X.509 parser: if rustls completes the
//! handshake, the leaf is genuinely chained to the CA *and* its SAN matched the
//! host we dialed.

use std::net::SocketAddr;

use peakbot::ui::web::tls;

/// The CA is generated once and then reused. Regenerating it silently would
/// break every phone that already trusts it — so a second call must return the
/// byte-identical CA cert.
#[test]
fn ca_is_generated_once_and_reused() {
    let dir = tempfile::tempdir().unwrap();

    let first = tls::ca_cert_pem(dir.path()).unwrap();
    let second = tls::ca_cert_pem(dir.path()).unwrap();

    assert!(
        first.contains("BEGIN CERTIFICATE"),
        "ca_cert_pem should return a PEM certificate"
    );
    assert_eq!(first, second, "the CA must be reused, never regenerated");
}

/// The CA private key is the crown jewel. On Unix it must be written 0600 —
/// same guarantee the mcp-auth credential store makes.
#[cfg(unix)]
#[test]
fn ca_private_key_is_written_0600() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    tls::ca_cert_pem(dir.path()).unwrap();

    let key = dir.path().join("ca-key.pem");
    assert!(
        key.exists(),
        "ca-key.pem must be persisted next to the cert"
    );
    let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "CA key must be private (0600), got {mode:o}");
}

/// `server_config` must produce a TLS config rustls will actually serve — i.e.
/// the minted leaf and its key are valid and matched. If the pair were bad,
/// building the config would fail.
#[test]
fn server_config_builds_a_usable_leaf() {
    let dir = tempfile::tempdir().unwrap();
    let sans = vec!["localhost".to_string(), "127.0.0.1".to_string()];

    let cfg = tls::server_config(dir.path(), &sans);
    assert!(
        cfg.is_ok(),
        "server_config must yield a serveable rustls config: {:?}",
        cfg.err()
    );
}

/// SANs always include loopback so the operator can reach the UI locally over
/// HTTPS regardless of the bind address.
#[test]
fn local_sans_always_include_loopback() {
    let bind: SocketAddr = "0.0.0.0:7823".parse().unwrap();
    let sans = tls::local_sans(bind, &[]);
    assert!(sans.iter().any(|s| s == "localhost"));
    assert!(sans.iter().any(|s| s == "127.0.0.1"));
}

/// A concrete non-loopback bind IP must end up in the SAN list, so a browser
/// dialing that exact IP gets a name-matched cert.
#[test]
fn local_sans_include_a_concrete_bind_ip() {
    let bind: SocketAddr = "192.168.7.7:7823".parse().unwrap();
    let sans = tls::local_sans(bind, &[]);
    assert!(
        sans.iter().any(|s| s == "192.168.7.7"),
        "a concrete bind IP must be a SAN, got {sans:?}"
    );
}

/// The machine's mDNS `<hostname>.local` name is added automatically so a phone
/// can reach the UI by a name that survives DHCP IP changes. (Skipped only in
/// the pathological case of a host whose name is empty or `localhost`.)
#[test]
fn local_sans_include_mdns_local_name() {
    let bind: SocketAddr = "127.0.0.1:7823".parse().unwrap();
    let sans = tls::local_sans(bind, &[]);
    let raw = gethostname::gethostname().into_string().unwrap_or_default();
    let short = raw
        .split('.')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if short.is_empty() || short == "localhost" {
        return; // no derivable mDNS name on this host
    }
    let expected = format!("{short}.local");
    assert!(
        sans.contains(&expected),
        "expected mDNS name {expected:?} in SANs, got {sans:?}"
    );
}

/// Operator-supplied extra names (from `--tls-name`) are folded into the leaf's
/// SANs alongside the auto-discovered ones, and duplicates collapse.
#[test]
fn local_sans_include_extra_names_and_dedup() {
    let bind: SocketAddr = "127.0.0.1:7823".parse().unwrap();
    let extra = vec![
        "peakbot.lan".to_string(),
        "10.0.0.9".to_string(),
        "localhost".to_string(), // duplicate of an auto SAN
    ];
    let sans = tls::local_sans(bind, &extra);
    assert!(sans.contains(&"peakbot.lan".to_string()), "got {sans:?}");
    assert!(sans.contains(&"10.0.0.9".to_string()), "got {sans:?}");
    assert_eq!(
        sans.iter().filter(|s| *s == "localhost").count(),
        1,
        "localhost must not be duplicated, got {sans:?}"
    );
}

/// The end-to-end proof: an HTTPS server presenting a CA-signed leaf is trusted
/// by a client whose ONLY root is that CA — and rejected by a client using the
/// system roots. This exercises the real crypto path (CA → leaf → handshake →
/// SAN match) without a browser and without an X.509 parser.
#[tokio::test]
async fn https_leaf_is_trusted_by_its_ca_and_untrusted_otherwise() {
    use axum::{Router, routing::get};
    use axum_server::tls_rustls::RustlsConfig;
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let ca_pem = tls::ca_cert_pem(dir.path()).unwrap();
    let sans = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    let server_config = tls::server_config(dir.path(), &sans).unwrap();

    let app = Router::new().route("/", get(|| async { "ok" }));
    // Bind an ephemeral port, then let axum-server own its listener (it manages
    // the non-blocking registration itself; handing it a std listener panics).
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);
    let rustls_cfg = RustlsConfig::from_config(Arc::new(server_config));
    tokio::spawn(async move {
        axum_server::bind_rustls(addr, rustls_cfg)
            .serve(app.into_make_service())
            .await
            .ok();
    });

    // Give the server a moment to start accepting.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Client trusting only our CA → handshake + SAN check succeed.
    let ca = reqwest::Certificate::from_pem(ca_pem.as_bytes()).unwrap();
    let trusting = reqwest::Client::builder()
        .add_root_certificate(ca)
        .build()
        .unwrap();
    let resp = trusting
        .get(format!("https://localhost:{}/", addr.port()))
        .send()
        .await
        .expect("client trusting the CA must complete the TLS handshake");
    assert!(resp.status().is_success());

    // Client using system roots → the CA is unknown → handshake must fail.
    let untrusting = reqwest::Client::builder().build().unwrap();
    let err = untrusting
        .get(format!("https://localhost:{}/", addr.port()))
        .send()
        .await;
    assert!(
        err.is_err(),
        "a client without the CA must reject the self-signed chain"
    );
}
