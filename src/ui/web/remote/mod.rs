//! Remote profiles (T5) — `remote::Directory` + `remote::Client`.
//!
//! Every byte the local server sends to one remote: TLS trust (pinned CA or
//! explicit `system` roots — never plaintext off loopback, no fallbacks),
//! the `peakbot_token` cookie, the `/ws` dial, and the remote HTTP calls
//! (`list`, `kill`). Built from `RemoteProfile` alone (design §3, §5.8,
//! §5.12, §7).

use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{COOKIE, HeaderValue};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use url::{Host, Url};
use uuid::Uuid;

use crate::config::remote::{CaFingerprint, Endpoint, RemoteProfile, Trust};
use crate::config::resolve::{Config, LoadError, ParseCtx, RepoMode};
use crate::ui::wire::ConversationSummaryWire;

#[cfg(test)]
mod client_tests;

pub type WsStream = tokio_tungstenite::WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The bounds of §5.12, injectable so tests run in seconds.
#[derive(Clone, Copy, Debug)]
pub struct ClientOpts {
    /// The tokenless CA bootstrap fetch (E-TLS-PIN row: ≤ 10 s).
    pub ca_fetch_timeout: Duration,
    /// TCP connect + TLS + WS upgrade, and HTTP connects (E-TIMEOUT: ≤ 20 s).
    pub dial_timeout: Duration,
}

impl Default for ClientOpts {
    fn default() -> Self {
        Self {
            ca_fetch_timeout: Duration::from_secs(10),
            dial_timeout: Duration::from_secs(20),
        }
    }
}

/// A remote failure; `Display` is the §5.12 locked string. Carries
/// `host:port` only — never a URL or the token (§7).
#[derive(Debug)]
pub enum RemoteError {
    Down {
        profile: String,
        host: String,
        reason: String,
    },
    Timeout {
        profile: String,
        host: String,
    },
    TlsPin {
        profile: String,
        want: String,
        got: String,
    },
    TlsName {
        profile: String,
        host: String,
    },
    Tls {
        profile: String,
        reason: String,
    },
    Auth {
        profile: String,
    },
    Config {
        message: String,
    },
}

impl fmt::Display for RemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Down {
                profile,
                host,
                reason,
            } => {
                write!(
                    f,
                    "Can't reach remote profile '{profile}' at {host}: {reason}."
                )
            }
            Self::Timeout { profile, host } => {
                write!(
                    f,
                    "Remote profile '{profile}' at {host} didn't answer in time."
                )
            }
            // No space after "pins {want}." — matches the locked test string
            // (client_tests.rs); the design doc has one. See the T5 report.
            Self::TlsPin { profile, want, got } => write!(
                f,
                "Remote profile '{profile}': the server's CA is {got}, but the config pins {want}.\
                 Refusing to connect. If the server's CA was rotated, update `ca` in Settings → Profiles."
            ),
            Self::TlsName { profile, host } => write!(
                f,
                "Remote profile '{profile}': the certificate isn't valid for '{host}'. \
                 On the server, add it with --tls-name {host}."
            ),
            Self::Tls { profile, reason } => {
                write!(
                    f,
                    "Remote profile '{profile}': TLS handshake failed: {reason}."
                )
            }
            Self::Auth { profile } => write!(
                f,
                "Remote profile '{profile}' rejected its token. Update the token in Settings → Profiles."
            ),
            Self::Config { message } => write!(
                f,
                "config.yaml has an error, so remote profiles are unavailable: {message}"
            ),
        }
    }
}

impl StdError for RemoteError {}

/// Who a failure is about: L, H (`host:port`) and the bare host the TLS
/// name check and `--tls-name` use. Exists before the `Client` does, so the
/// bootstrap can report errors too.
#[derive(Debug, Clone)]
struct Peer {
    profile: String,
    host_port: String,
    host: String,
    port: u16,
}

impl Peer {
    fn new(profile: &str, origin: &Url) -> Self {
        // `RemoteProfile` origins always have a host and a known-scheme port.
        let host = match origin.host() {
            Some(Host::Ipv6(ip)) => ip.to_string(),
            Some(h) => h.to_string(),
            None => String::new(),
        };
        let port = origin.port_or_known_default().unwrap_or(443);
        let host_port = match origin.host() {
            Some(Host::Ipv6(_)) => format!("[{host}]:{port}"),
            _ => format!("{host}:{port}"),
        };
        Self {
            profile: profile.to_string(),
            host_port,
            host,
            port,
        }
    }

    fn down(&self, reason: impl Into<String>) -> RemoteError {
        RemoteError::Down {
            profile: self.profile.clone(),
            host: self.host_port.clone(),
            reason: reason.into(),
        }
    }

    fn timeout(&self) -> RemoteError {
        RemoteError::Timeout {
            profile: self.profile.clone(),
            host: self.host_port.clone(),
        }
    }

    fn tls(&self, reason: impl Into<String>) -> RemoteError {
        RemoteError::Tls {
            profile: self.profile.clone(),
            reason: reason.into(),
        }
    }

    fn auth(&self) -> RemoteError {
        RemoteError::Auth {
            profile: self.profile.clone(),
        }
    }

    fn classify_rustls(&self, err: &rustls::Error) -> RemoteError {
        match err {
            rustls::Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            ) => RemoteError::TlsName {
                profile: self.profile.clone(),
                host: self.host.clone(),
            },
            other => self.tls(other.to_string()),
        }
    }

    /// reqwest buries the rustls error a few `source()`s deep; a TLS cause
    /// wins over the timeout/connect classification it also carries.
    fn classify_reqwest(&self, err: &reqwest::Error) -> RemoteError {
        if let Some(tls) = find_rustls(err) {
            return self.classify_rustls(tls);
        }
        if err.is_timeout() {
            return self.timeout();
        }
        self.down(root_cause(err))
    }
}

/// `io::Error::source()` skips its payload (and payloads nest), so step into
/// it explicitly.
fn find_rustls<'e>(err: &'e (dyn StdError + 'static)) -> Option<&'e rustls::Error> {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if let Some(tls) = e.downcast_ref::<rustls::Error>() {
            return Some(tls);
        }
        cur = match e.downcast_ref::<std::io::Error>() {
            Some(io) => io.get_ref().map(|inner| inner as &(dyn StdError + 'static)),
            None => e.source(),
        };
    }
    None
}

/// The innermost cause ("Connection refused (os error 111)"): reqwest's top
/// message is just "error sending request for url".
fn root_cause(err: &(dyn StdError + 'static)) -> String {
    let mut cur = err;
    while let Some(next) = cur.source() {
        cur = next;
    }
    cur.to_string()
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// Accepts any server certificate (signatures still checked). Used for the
/// ONE CA bootstrap GET, whose response is then authenticated by the pin.
#[derive(Debug)]
struct NoVerify(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
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
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// The reqwest settings every remote call shares. No proxy: the WS dial
/// can't use one, so HTTP mustn't either. No redirects: a remote can't
/// bounce the cookie somewhere else.
fn http_builder(opts: ClientOpts) -> reqwest::ClientBuilder {
    crate::http::client_builder()
        .connect_timeout(opts.dial_timeout)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
}

/// §5.8: fetch `/peakbot-ca.crt` without verification and without
/// credentials, check it against the pin, and trust only that CA after.
async fn bootstrap_pinned(
    peer: &Peer,
    origin: &Url,
    pin: &CaFingerprint,
    opts: ClientOpts,
) -> Result<ClientConfig, RemoteError> {
    let no_verify = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| peer.tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify(provider())))
        .with_no_client_auth();
    let fetch = http_builder(opts)
        .tls_backend_preconfigured(no_verify)
        .timeout(opts.ca_fetch_timeout)
        .build()
        .map_err(|e| peer.tls(e.to_string()))?;
    let url = origin
        .join("peakbot-ca.crt")
        .expect("a static path joins any origin");
    let resp = fetch
        .get(url)
        .send()
        .await
        .map_err(|e| peer.classify_reqwest(&e))?;
    if !resp.status().is_success() {
        return Err(peer.down(format!("GET /peakbot-ca.crt answered {}", resp.status())));
    }
    let pem = resp.text().await.map_err(|e| peer.classify_reqwest(&e))?;
    let der = CertificateDer::from_pem_slice(pem.as_bytes())
        .map_err(|_| peer.tls("the server's /peakbot-ca.crt is not a PEM certificate"))?;

    let got = crate::ui::web::tls::der_fingerprint(&der);
    let want = pin.to_string();
    if got != want {
        return Err(RemoteError::TlsPin {
            profile: peer.profile.clone(),
            want,
            got,
        });
    }

    let mut roots = RootCertStore::empty();
    roots.add(der).map_err(|e| peer.tls(e.to_string()))?;
    Ok(ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| peer.tls(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

/// One remote profile's connection policy, built once (CA bootstrap
/// included) and reused for every dial and HTTP call.
pub struct Client {
    peer: Peer,
    origin: Url,
    /// `peakbot_token=<remote token>`, marked sensitive.
    cookie: HeaderValue,
    /// `None` only for `LoopbackPlain`.
    tls: Option<Arc<ClientConfig>>,
    http: reqwest::Client,
    opts: ClientOpts,
}

// Hand-written so the cookie (the token) can never reach a Debug print.
impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("profile", &self.peer.profile)
            .field("host", &self.peer.host_port)
            .finish_non_exhaustive()
    }
}

impl Client {
    pub async fn new(profile: &RemoteProfile, opts: ClientOpts) -> Result<Self, RemoteError> {
        let origin = match &profile.endpoint {
            Endpoint::Tls { origin, .. } | Endpoint::LoopbackPlain { origin } => origin,
        };
        let peer = Peer::new(&profile.name, origin);
        let tls = match &profile.endpoint {
            Endpoint::LoopbackPlain { .. } => None,
            Endpoint::Tls {
                trust: Trust::System,
                ..
            } => Some(crate::http::system_tls_config().map_err(|e| peer.tls(e.to_string()))?),
            Endpoint::Tls {
                trust: Trust::PinnedCa(pin),
                ..
            } => Some(bootstrap_pinned(&peer, origin, pin, opts).await?),
        };
        let tls = tls.map(Arc::new);
        let mut cookie =
            HeaderValue::from_str(&format!("peakbot_token={}", profile.token.expose())).map_err(
                |_| RemoteError::Config {
                    message: format!(
                        "the token of remote profile '{}' can't be sent as a cookie",
                        profile.name
                    ),
                },
            )?;
        cookie.set_sensitive(true);

        let mut builder = http_builder(opts);
        if let Some(config) = &tls {
            builder = builder.tls_backend_preconfigured(ClientConfig::clone(config));
        }
        let http = builder.build().map_err(|e| peer.tls(e.to_string()))?;

        Ok(Self {
            peer,
            origin: origin.clone(),
            cookie,
            tls,
            http,
            opts,
        })
    }

    /// TCP → TLS → WS upgrade of `/ws` with the token cookie, all within
    /// `dial_timeout`. The `ready{v}` check is the relay's (T6).
    pub async fn dial(&self) -> Result<WsStream, RemoteError> {
        tokio::time::timeout(self.opts.dial_timeout, self.dial_inner())
            .await
            .map_err(|_| self.peer.timeout())?
    }

    async fn dial_inner(&self) -> Result<WsStream, RemoteError> {
        let peer = &self.peer;
        let tcp = TcpStream::connect((peer.host.as_str(), peer.port))
            .await
            .map_err(|e| peer.down(e.to_string()))?;
        let (stream, scheme) = match &self.tls {
            None => (MaybeTlsStream::Plain(tcp), "ws"),
            Some(config) => {
                let name =
                    ServerName::try_from(peer.host.clone()).map_err(|e| peer.tls(e.to_string()))?;
                let tls = tokio_rustls::TlsConnector::from(Arc::clone(config))
                    .connect(name, tcp)
                    .await
                    .map_err(|e| match find_rustls(&e) {
                        Some(tls) => peer.classify_rustls(tls),
                        None => peer.tls(e.to_string()),
                    })?;
                (MaybeTlsStream::Rustls(tls), "wss")
            }
        };

        let mut request = format!("{scheme}://{}/ws", peer.host_port)
            .into_client_request()
            .map_err(|e| peer.down(e.to_string()))?;
        request.headers_mut().insert(COOKIE, self.cookie.clone());
        // tungstenite's defaults (64 MiB message / 16 MiB frame) are the §7 bounds.
        match tokio_tungstenite::client_async(request, stream).await {
            Ok((ws, _response)) => Ok(ws),
            Err(WsError::Http(response)) if response.status() == StatusCode::UNAUTHORIZED => {
                Err(peer.auth())
            }
            Err(e) => Err(peer.down(e.to_string())),
        }
    }

    /// `GET /api/conversations` — the remote's local rows (T4 shape).
    pub async fn list(&self) -> Result<Vec<ConversationSummaryWire>, RemoteError> {
        #[derive(Deserialize)]
        struct Items {
            items: Vec<ConversationSummaryWire>,
        }
        let body = self
            .call(self.http.get(self.url("api/conversations")))
            .await?;
        let parsed: Items = serde_json::from_str(&body).map_err(|e| {
            self.peer
                .down(format!("unexpected reply to GET /api/conversations: {e}"))
        })?;
        Ok(parsed.items)
    }

    /// `POST /api/conversations/{id}/kill`; `Some(msg)` is the remote's
    /// refusal (`error`, §6.2), `None` a clean kill.
    pub async fn kill(&self, id: &Uuid) -> Result<Option<String>, RemoteError> {
        #[derive(Deserialize)]
        struct KillReply {
            #[serde(default)]
            error: Option<String>,
        }
        let path = format!("api/conversations/{id}/kill");
        let body = self.call(self.http.post(self.url(&path))).await?;
        let parsed: KillReply = serde_json::from_str(&body).map_err(|e| {
            self.peer
                .down(format!("unexpected reply to POST /{path}: {e}"))
        })?;
        Ok(parsed.error)
    }

    fn url(&self, path: &str) -> Url {
        self.origin
            .join(path)
            .expect("a relative path joins any origin")
    }

    /// Send with the token cookie; one attempt, 401 ⇒ E-AUTH.
    async fn call(&self, request: reqwest::RequestBuilder) -> Result<String, RemoteError> {
        let resp = request
            .header(COOKIE, self.cookie.clone())
            .send()
            .await
            .map_err(|e| self.peer.classify_reqwest(&e))?;
        match resp.status() {
            StatusCode::UNAUTHORIZED => Err(self.peer.auth()),
            status if !status.is_success() => {
                Err(self.peer.down(format!("the remote answered {status}")))
            }
            _ => resp
                .text()
                .await
                .map_err(|e| self.peer.classify_reqwest(&e)),
        }
    }
}

/// L → the `RemoteProfile` a client was built from, and that client.
type ClientCache = HashMap<String, (Arc<RemoteProfile>, Arc<Client>)>;

/// "What is remote profile L right now?" — a fresh global-config parse per
/// call (§5.11) and one cached `Client` per profile definition (§5.8).
#[derive(Default)]
pub struct Directory {
    clients: Mutex<ClientCache>,
    opts: ClientOpts,
}

impl Directory {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn client(&self, name: &str) -> Result<Arc<Client>, RemoteError> {
        let profile = snapshot(name)?;
        if let Some((built_from, client)) = self.lock().get(name)
            && **built_from == *profile
        {
            return Ok(Arc::clone(client));
        }
        // Built outside the lock (the CA bootstrap can take seconds); two
        // racing first calls just both build, and the last insert wins.
        let client = Arc::new(Client::new(&profile, self.opts).await?);
        self.lock()
            .insert(name.to_string(), (profile, Arc::clone(&client)));
        Ok(client)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ClientCache> {
        self.clients.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// The global config only (repo files never define remotes, §5.11).
fn snapshot(name: &str) -> Result<Arc<RemoteProfile>, RemoteError> {
    let ctx = ParseCtx { known_skills: &[] };
    let config =
        Config::load(Path::new("."), RepoMode::Ignore, &ctx).map_err(|e| RemoteError::Config {
            message: match e {
                LoadError::Invalid(errors) => crate::config_errors_summary(&errors),
                LoadError::Missing(path) => format!("{} does not exist", path.display()),
                LoadError::Legacy(path) => {
                    format!("{}: {}", path.display(), crate::config::resolve::L1_MESSAGE)
                }
                LoadError::Io(path, e) => format!("cannot read {}: {e}", path.display()),
            },
        })?;
    config
        .catalog
        .remote(name)
        .cloned()
        .ok_or_else(|| RemoteError::Config {
            message: format!("no remote profile named '{name}'"),
        })
}
