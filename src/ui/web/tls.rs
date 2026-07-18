//! PeakBot's built-in certificate authority for `peakbot --web --tls`.
//!
//! PeakBot owns the whole PKI so HTTPS is batteries-included: it self-signs a
//! long-lived **CA** (install it on your phone ONCE), then mints a fresh
//! **leaf** every boot whose SANs follow the machine's current addresses — its
//! loopback names, primary LAN IP, and mDNS `<hostname>.local` name (plus any
//! operator-supplied `--tls-name` extras). The phone trusts the CA, so every
//! leaf the CA signs gets a padlock — no phone action when the LAN IP changes.
//!
//! The CA lives in `dirs::cache_dir()/peakbot/tls/` (same home as the mcp-auth
//! credential store); its private key is written `0600`. It is generated once
//! and never silently regenerated — doing so would break every phone that
//! already trusts it. To rotate, delete the `tls/` directory.
//!
//! We deliberately avoid rcgen's `x509-parser` feature (a heavy ASN.1 parser
//! that bloats the cross-builds). Instead the CA's identity is fixed — a
//! constant distinguished name plus the persisted key — so on reload we rebuild
//! an equivalent [`Issuer`] from the key alone, without parsing the stored cert.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, date_time_ymd,
};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

/// Public CA certificate — served at `/peakbot-ca.crt`, installed on the phone.
const CA_CERT_FILE: &str = "ca-cert.pem";
/// CA private key — the crown jewel, written `0600`.
const CA_KEY_FILE: &str = "ca-key.pem";
/// The CA's subject/issuer name. Fixed so the issuer can be rebuilt from the
/// key alone (no cert parsing) and still chain to the persisted certificate.
const CA_COMMON_NAME: &str = "PeakBot Local CA";

/// The directory holding the CA material: `dirs::cache_dir()/peakbot/tls`.
pub fn default_tls_dir() -> Result<PathBuf> {
    let cache =
        dirs::cache_dir().context("no cache directory available to store the PeakBot CA")?;
    Ok(cache.join("peakbot").join("tls"))
}

/// The SANs a leaf must cover: loopback always, plus the machine's mDNS
/// `<hostname>.local` name and primary LAN IP, any concrete (non-wildcard) bind
/// IP, and any operator-supplied `extra` names — so a phone dialing that exact
/// host gets a name-matched certificate. `.local` is the most durable of these:
/// it survives DHCP lease changes that shuffle the LAN IP.
pub fn local_sans(bind: SocketAddr, extra: &[String]) -> Vec<String> {
    let mut sans = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    if let Some(name) = mdns_hostname() {
        sans.push(name);
    }
    if let Some(ip) = primary_lan_ip() {
        sans.push(ip.to_string());
    }
    let bind_ip = bind.ip();
    if !bind_ip.is_loopback() && !bind_ip.is_unspecified() {
        sans.push(bind_ip.to_string());
    }
    sans.extend(extra.iter().cloned());
    sans.sort();
    sans.dedup();
    sans
}

/// The host to advertise in the CA-install URL: the machine's LAN IP when the
/// bind is loopback/wildcard (so a phone can reach it), else the bind IP.
pub fn primary_lan_host(bind: SocketAddr) -> String {
    let ip = bind.ip();
    if !ip.is_loopback() && !ip.is_unspecified() {
        return ip.to_string();
    }
    primary_lan_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| ip.to_string())
}

/// The CA's public certificate in PEM, generating+persisting the CA on first
/// use. This is what the operator installs on the phone.
pub fn ca_cert_pem(tls_dir: &Path) -> Result<String> {
    Ok(load_or_create_ca(tls_dir)?.cert_pem)
}

/// A rustls server config presenting a freshly-minted leaf (signed by the CA)
/// for `sans`. Building it proves the leaf and its key are valid and matched.
pub fn server_config(tls_dir: &Path, sans: &[String]) -> Result<ServerConfig> {
    let ca = load_or_create_ca(tls_dir)?;
    let leaf = mint_leaf(&ca, sans)?;

    let chain = vec![
        CertificateDer::from_pem_slice(leaf.cert_pem.as_bytes())
            .context("leaf certificate PEM did not parse")?,
    ];
    let key = PrivateKeyDer::from_pem_slice(leaf.key_pem.as_bytes())
        .context("leaf private key PEM did not parse")?;

    // Provide the aws-lc-rs provider explicitly rather than relying on a
    // process-wide default being installed — otherwise `builder()` panics if
    // nothing called `install_default()` first.
    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("no supported TLS protocol versions")?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .context("leaf certificate and key did not form a usable pair")?;
    Ok(config)
}

/// The CA: its public certificate (for serving) plus a signing issuer.
struct Ca {
    cert_pem: String,
    issuer: Issuer<'static, KeyPair>,
}

/// A minted leaf: certificate + private key, both PEM.
struct Leaf {
    cert_pem: String,
    key_pem: String,
}

/// Load the CA from `tls_dir`, or generate and persist it (key `0600`) if
/// absent. The issuer is always rebuilt from the persisted key — never from the
/// stored certificate — so the certificate bytes the phone trusts stay stable.
fn load_or_create_ca(tls_dir: &Path) -> Result<Ca> {
    let cert_path = tls_dir.join(CA_CERT_FILE);
    let key_path = tls_dir.join(CA_KEY_FILE);

    if cert_path.exists() && key_path.exists() {
        let cert_pem = std::fs::read_to_string(&cert_path)
            .with_context(|| format!("reading {}", cert_path.display()))?;
        let key_pem = std::fs::read_to_string(&key_path)
            .with_context(|| format!("reading {}", key_path.display()))?;
        let key = KeyPair::from_pem(&key_pem).context("CA private key did not parse")?;
        return Ok(Ca {
            cert_pem,
            issuer: Issuer::new(ca_params()?, key),
        });
    }

    let key = KeyPair::generate().context("generating CA key pair")?;
    let cert_pem = ca_params()?
        .self_signed(&key)
        .context("self-signing the CA certificate")?
        .pem();

    std::fs::create_dir_all(tls_dir).with_context(|| format!("creating {}", tls_dir.display()))?;
    std::fs::write(&cert_path, &cert_pem)
        .with_context(|| format!("writing {}", cert_path.display()))?;
    write_private(&key_path, &key.serialize_pem())?;

    Ok(Ca {
        cert_pem,
        issuer: Issuer::new(ca_params()?, key),
    })
}

/// Mint a leaf certificate for `sans`, signed by the CA. `CertificateParams::new`
/// classifies each SAN string as an IP or DNS name automatically.
fn mint_leaf(ca: &Ca, sans: &[String]) -> Result<Leaf> {
    let key = KeyPair::generate().context("generating leaf key pair")?;
    let mut params =
        CertificateParams::new(sans.to_vec()).context("building leaf certificate params")?;
    params.not_before = date_time_ymd(2020, 1, 1);
    params.not_after = date_time_ymd(2035, 1, 1);
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params
        .distinguished_name
        .push(DnType::CommonName, "PeakBot");

    let cert = params
        .signed_by(&key, &ca.issuer)
        .context("signing the leaf certificate")?;
    Ok(Leaf {
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
    })
}

/// The CA's certificate parameters. Fixed identity so the issuer can be
/// reconstructed from the key alone.
fn ca_params() -> Result<CertificateParams> {
    let mut params =
        CertificateParams::new(Vec::<String>::new()).context("building CA certificate params")?;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.not_before = date_time_ymd(2020, 1, 1);
    params.not_after = date_time_ymd(2035, 1, 1);
    params
        .distinguished_name
        .push(DnType::CommonName, CA_COMMON_NAME);
    Ok(params)
}

/// Write a secret file with owner-only permissions (`0600` on Unix), matching
/// the mcp-auth credential store's guarantee.
fn write_private(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting 0600 on {}", path.display()))?;
    }
    Ok(())
}

/// The machine's mDNS name, `<short-hostname>.local` — the most stable way for
/// a phone to reach this host, since it survives DHCP IP changes. Returns
/// `None` when the hostname is missing, non-UTF-8, or already `localhost`.
/// We take only the first label (before any dot) so an FQDN hostname still
/// yields the mDNS single-label form.
fn mdns_hostname() -> Option<String> {
    let raw = gethostname::gethostname().into_string().ok()?;
    let short = raw.split('.').next()?.trim().to_ascii_lowercase();
    if short.is_empty() || short == "localhost" {
        return None;
    }
    Some(format!("{short}.local"))
}

/// The machine's primary LAN IP, discovered via the classic UDP-connect trick:
/// connecting a UDP socket sends no packet but makes the kernel pick the source
/// address it would route from. `None` when offline / no route.
fn primary_lan_ip() -> Option<IpAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    sock.local_addr().ok().map(|addr| addr.ip())
}
