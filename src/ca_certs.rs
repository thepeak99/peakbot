//! Self-contained CA roots for environments with no system trust store.
//!
//! Compiled in **only** under the `embed-ca-certs` feature, which
//! `Dockerfile.android` enables for the static musl/Termux binary. reqwest
//! 0.13 uses `rustls-platform-verifier`, whose Unix backend reads roots from
//! the system store (honouring `SSL_CERT_FILE`/`SSL_CERT_DIR`). A bare
//! Android/Termux device has neither, so `Client::new()` panics ("No CA
//! certificates were loaded from the system"). We write the compiled-in
//! Mozilla bundle to the cache dir once and point `SSL_CERT_FILE` at it —
//! before any TLS client is built — so every reqwest client finds roots.
//!
//! Without the feature (every desktop/Windows/macOS build) this is a no-op
//! and the CA data is not compiled in at all: those platforms have a real OS
//! trust store, so shipping ~250 KB of roots would be dead weight.

/// No-op build: the host already has an OS trust store / `SSL_CERT_FILE`.
#[cfg(not(feature = "embed-ca-certs"))]
pub fn ensure_ca_bundle() {}

#[cfg(feature = "embed-ca-certs")]
pub use embedded::ensure_ca_bundle;

#[cfg(feature = "embed-ca-certs")]
mod embedded {
    use base64::Engine as _;
    use std::io::Write as _;
    use std::path::PathBuf;

    /// Ensure a CA bundle is available to rustls via `SSL_CERT_FILE`.
    ///
    /// No-op when the operator already set `SSL_CERT_FILE` or `SSL_CERT_DIR`,
    /// or when a standard system bundle exists — we never override real roots.
    pub fn ensure_ca_bundle() {
        if std::env::var_os("SSL_CERT_FILE").is_some() || std::env::var_os("SSL_CERT_DIR").is_some()
        {
            return;
        }
        if system_bundle_exists() {
            return;
        }

        let Some(path) = bundle_path() else { return };
        if write_bundle_if_needed(&path).is_ok() {
            // SAFETY: called once at startup before any client/thread is spawned.
            unsafe { std::env::set_var("SSL_CERT_FILE", &path) };
        }
    }

    /// Common distro locations for a system CA bundle. If one exists, the
    /// device already has roots and platform-verifier finds them on its own.
    fn system_bundle_exists() -> bool {
        const CANDIDATES: &[&str] = &[
            "/etc/ssl/certs/ca-certificates.crt", // Debian/Ubuntu/Alpine
            "/etc/pki/tls/certs/ca-bundle.crt",   // Fedora/RHEL
            "/etc/ssl/cert.pem",                  // macOS/BSD/Termux
        ];
        CANDIDATES.iter().any(|p| std::path::Path::new(p).exists())
    }

    fn bundle_path() -> Option<PathBuf> {
        let dir = dirs::cache_dir()?.join("peakbot");
        Some(dir.join("cacert.pem"))
    }

    /// Write the embedded bundle to `path` unless an identically-sized copy is
    /// already there (cheap idempotency — the bundle only changes on rebuild).
    fn write_bundle_if_needed(path: &std::path::Path) -> std::io::Result<()> {
        let pem = pem_bundle();
        if let Ok(meta) = std::fs::metadata(path)
            && meta.len() == pem.len() as u64
        {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::File::create(path)?;
        f.write_all(pem.as_bytes())
    }

    /// Render the compiled-in DER roots as a PEM bundle.
    fn pem_bundle() -> String {
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut out = String::new();
        for cert in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
            let encoded = b64.encode(cert.as_ref());
            out.push_str("-----BEGIN CERTIFICATE-----\n");
            for chunk in encoded.as_bytes().chunks(64) {
                out.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
                out.push('\n');
            }
            out.push_str("-----END CERTIFICATE-----\n");
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn pem_bundle_is_wellformed() {
            let pem = pem_bundle();
            let begins = pem.matches("-----BEGIN CERTIFICATE-----").count();
            let ends = pem.matches("-----END CERTIFICATE-----").count();
            assert!(
                begins > 100,
                "expected the full Mozilla bundle, got {begins}"
            );
            assert_eq!(begins, ends, "every BEGIN must have a matching END");
            assert_eq!(begins, webpki_root_certs::TLS_SERVER_ROOT_CERTS.len());
        }

        #[test]
        fn write_is_idempotent_by_size() {
            let dir = std::env::temp_dir().join(format!("peakbot-catest-{}", std::process::id()));
            let path = dir.join("cacert.pem");
            let _ = std::fs::remove_dir_all(&dir);

            write_bundle_if_needed(&path).expect("first write");
            let mtime1 = std::fs::metadata(&path).unwrap().modified().unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), pem_bundle());

            // Second call must skip (same size) — file untouched.
            write_bundle_if_needed(&path).expect("second write");
            let mtime2 = std::fs::metadata(&path).unwrap().modified().unwrap();
            assert_eq!(mtime1, mtime2, "identical bundle must not be rewritten");

            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
