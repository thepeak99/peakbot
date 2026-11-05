//! Shared test harness for the remote-profiles tasks (T5 client, T6 relay,
//! T7 list, T8 proxy).
//!
//! What it provides:
//! * `mint_ca` / `leaf_server_config` — a fresh rcgen test CA + leaf (the
//!   same minting as `ui::web::tls`), and the `sha256:<hex>` pin.
//! * `Recorder` — a recording middleware capturing every request line and
//!   header, so the §7 token rules are asserted on the wire.
//! * `start_tls_app` / `start_plain_app` — in-process axum servers (TLS via
//!   `axum-server`, plain via `axum::serve`), returning the base URL.
//! * `XdgHome` — points `XDG_CONFIG_HOME` at a tempdir holding the given
//!   global config, under `CONFIG_ENV_LOCK`, restored on drop.
//!
//! The T5 client tests (client_tests.rs) keep their own copy of the
//! drain-style fake remote; the T6 relay tests (relay_tests.rs) build a
//! SCRIPTED `/ws` remote on top of this module.

#![allow(dead_code)] // shared T5–T8 harness: each task's tests use a subset

use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::Request;
use axum::http::header;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum_server::tls_rustls::RustlsConfig;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, date_time_ymd,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use sha2::{Digest, Sha256};

use super::ClientOpts;

/// The cookie name the remote token travels in (§7: `Cookie:
/// peakbot_token=<remote>`; accepted by `require_token` on the remote).
pub const COOKIE_NAME: &str = "peakbot_token";

/// The §5.12 bounds, injectable so tests run in seconds.
pub fn short_opts() -> ClientOpts {
    ClientOpts {
        ca_fetch_timeout: Duration::from_secs(2),
        dial_timeout: Duration::from_secs(2),
    }
}

// ── Test CA / leaf minting (mirrors `ui::web::tls`, in-test only) ───────

pub struct TestCa {
    /// `sha256:<64 lower hex>` of the CA DER — what a `ca:` pin compares.
    pub fingerprint: String,
    pub ca_pem: String,
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
        .push(DnType::CommonName, "PeakBot Remote Test CA");
    params
}

pub fn mint_ca() -> TestCa {
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
pub fn leaf_server_config(ca: &TestCa, sans: &[&str]) -> rustls::ServerConfig {
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

// ── Recording middleware ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Captured {
    pub method: String,
    /// The full request URI (path + query) as the remote saw it.
    pub uri: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Recorder {
    hits: Arc<Mutex<Vec<Captured>>>,
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            hits: Arc::new(Mutex::new(Vec::new())),
        }
    }
    pub fn hits(&self) -> Vec<Captured> {
        self.hits.lock().expect("recorder lock").clone()
    }
    pub fn count(&self) -> usize {
        self.hits().len()
    }
}

/// Capture every request line and header before routing (the §7 assertions).
pub async fn record(State(rec): State<Recorder>, req: Request<Body>, next: Next) -> Response {
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

/// The `peakbot_token` cookie value, if any.
pub fn cookie_token(req: &Request<Body>) -> Option<String> {
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

// ── Server starters ─────────────────────────────────────────────────────

/// An in-process TLS axum server (leaf for `sans`, signed by `ca`).
/// Returns the base URL (`https://127.0.0.1:port`).
pub async fn start_tls_app(ca: &TestCa, sans: &[&str], app: Router, rec: Recorder) -> String {
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
            .serve(
                app.layer(middleware::from_fn_with_state(rec, record))
                    .into_make_service(),
            )
            .await
            .ok();
    });
    format!("https://{addr}")
}

/// An in-process plain-HTTP axum server. Returns `http://127.0.0.1:port`.
pub async fn start_plain_app(app: Router, rec: Recorder) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("plain listener");
    let addr = listener.local_addr().expect("plain addr");
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.layer(middleware::from_fn_with_state(rec, record)),
        )
        .await
        .ok();
    });
    format!("http://{addr}")
}

// ── Global config under XDG_CONFIG_HOME ─────────────────────────────────

/// Points `XDG_CONFIG_HOME` at a tempdir holding `yaml` as the global
/// config, under the crate-wide env lock; restores on drop (same pattern as
/// T4/T5). `Directory` re-reads this file on every call (§5.11).
pub struct XdgHome {
    prev: Option<OsString>,
    _dir: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl XdgHome {
    pub fn new(yaml: &str) -> Self {
        let lock = crate::config::CONFIG_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
