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
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use cloud::frames::StreamMsg;
use futures::stream::BoxStream;
use futures::{Sink, Stream, StreamExt};
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
use tokio_tungstenite::tungstenite::Message as TMsg;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use url::{Host, Url};
use uuid::Uuid;

use crate::config::remote::{CaFingerprint, Endpoint, RemoteProfile, Trust};
use crate::config::resolve::{CatalogEntry, Config, ParseCtx, ProfileInfo, Source};
use crate::image_cache::valid_image_id;
use crate::paths;
use crate::ui::wire::ConversationSummaryWire;

pub(super) mod listing;
pub(super) mod proxy;
pub(super) mod relay;

// Every remote test drives a config `remote:` profile, which every
// edition parses — the relay runs in the Local edition too.
#[cfg(test)]
mod client_tests;

#[cfg(test)]
mod cloud_body_tests;

#[cfg(test)]
mod relay_tests;

#[cfg(test)]
mod test_support;

// T7/T8 (merged list, kill routing, `/r/` proxy): shared fixture + tests.
#[cfg(test)]
mod t78_support;

#[cfg(test)]
mod t7_tests;

#[cfg(test)]
mod t8_tests;

#[cfg(test)]
mod t9_tests;

// P3 PR-A A6: the typed GetTarget boundary (RED until A6 lands).
#[cfg(test)]
mod t11_tests;

// Unpinned attach on a relay-only config (remotes through the on-device
// core).
#[cfg(test)]
mod t10_tests;

/// The WS stream a `Client` dials — an enum so the cloud-tunnel variant can
/// land above the transport without forking the callers (p3-design §F.2).
#[derive(Debug)]
pub enum WsStream {
    Direct(Box<tokio_tungstenite::WebSocketStream<MaybeTlsStream<TcpStream>>>),
    /// The Cloud Mode tunnel (p3-design §F.2): `Frame(s)` ↔ `Text(s)`,
    /// `End` → stream end, `Reset(code)` → Io error; outgoing `Ping`/`Pong`
    /// are swallowed (the session layer keeps liveness), `Binary` is a bug
    /// (the attach protocol is text-only), `Close` → `End`.
    Cloud(crate::cloud::stream::CloudStream),
}

impl Stream for WsStream {
    type Item = Result<TMsg, WsError>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut() {
            Self::Direct(inner) => Pin::new(inner).poll_next(cx),
            // §F.2: Frame ↔ Text; End → stream end; Reset → Io error.
            Self::Cloud(inner) => match Pin::new(inner).poll_next(cx) {
                Poll::Ready(Some(StreamMsg::Frame(s))) => {
                    Poll::Ready(Some(Ok(TMsg::Text(s.into()))))
                }
                Poll::Ready(Some(StreamMsg::End)) | Poll::Ready(None) => Poll::Ready(None),
                Poll::Ready(Some(StreamMsg::Reset(code))) => {
                    Poll::Ready(Some(Err(WsError::Io(io::Error::other(reset_text(code))))))
                }
                // Head/Chunk/Open or a raw fragment record on an Attach
                // stream is a protocol bug: the actor delivers whole Frames.
                Poll::Ready(Some(_)) => Poll::Ready(Some(Err(WsError::Io(io::Error::other(
                    "unexpected message on an attach stream",
                ))))),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

impl Sink<TMsg> for WsStream {
    type Error = WsError;
    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            Self::Direct(inner) => Pin::new(inner).poll_ready(cx),
            Self::Cloud(inner) => Pin::new(inner).poll_ready(cx).map_err(ws_sink_err),
        }
    }
    fn start_send(self: Pin<&mut Self>, item: TMsg) -> Result<(), Self::Error> {
        match (self.get_mut(), item) {
            (Self::Direct(inner), item) => Pin::new(inner).start_send(item),
            // §F.2: Text → Frame; Ping/Pong swallowed (the session layer
            // keeps liveness); Binary is a bug (text-only protocol);
            // Close → End.
            (Self::Cloud(inner), TMsg::Text(t)) => Pin::new(inner)
                .start_send(StreamMsg::Frame(t.to_string()))
                .map_err(ws_sink_err),
            (Self::Cloud(_), TMsg::Ping(_) | TMsg::Pong(_)) => Ok(()),
            (Self::Cloud(_), TMsg::Binary(_)) => {
                Err(WsError::Protocol(ProtocolError::InvalidOpcode(2)))
            }
            // Raw frames never leave tungstenite; the attach protocol is text-only.
            (Self::Cloud(_), TMsg::Frame(_)) => Ok(()),
            (Self::Cloud(inner), TMsg::Close(_)) => Pin::new(inner)
                .start_send(StreamMsg::End)
                .map_err(ws_sink_err),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            Self::Direct(inner) => Pin::new(inner).poll_flush(cx),
            Self::Cloud(inner) => Pin::new(inner).poll_flush(cx).map_err(ws_sink_err),
        }
    }
    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            Self::Direct(inner) => Pin::new(inner).poll_close(cx),
            Self::Cloud(inner) => Pin::new(inner).poll_close(cx).map_err(ws_sink_err),
        }
    }
}

/// A `CloudStream` sink fault, surfaced as a tungstenite Io error (§F.2).
fn ws_sink_err(err: crate::cloud::CloudError) -> WsError {
    WsError::Io(io::Error::other(err.to_string()))
}

/// The §F.2 `reset_text(code)` a `Reset` carries in an Io error.
fn reset_text(code: u16) -> String {
    use cloud::constants::reset;
    match code {
        reset::UNSUPPORTED => "not supported".to_string(),
        reset::NOT_FOUND => "not found".to_string(),
        reset::TOO_LARGE => "too large".to_string(),
        reset::INTERNAL => "internal error".to_string(),
        reset::CANCELLED => "cancelled".to_string(),
        reset::PROTOCOL => "protocol error".to_string(),
        other => format!("reset {other}"),
    }
}

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

/// The T7 list merge groups by origin (one fetch per server, RP4).
pub struct RemoteRef {
    /// L — the local profile name.
    pub name: String,
    /// `scheme://host[:port]` without a trailing slash.
    pub origin: String,
    /// P — the profile on the remote server.
    pub target: String,
}

/// An upload body in flight: the request body's data frames (§F.1).
pub type BodyStream = BoxStream<'static, Result<Bytes, axum::Error>>;

/// The remote's answer to a proxied request (T8): its status, its response
/// headers (the proxy copies an allowlist of them) and its body, left for
/// streaming. The body is an enum (p3-design §F.3); the proxy only ever
/// calls `bytes_stream()`, so it never learns which transport it holds.
pub struct Upstream {
    pub status: StatusCode,
    pub headers: reqwest::header::HeaderMap,
    body: UpstreamBody,
}

enum UpstreamBody {
    /// The response plus the peer its body errors are classified against.
    Direct(reqwest::Response, Peer),
    /// The Cloud Mode tunnel body (§F.3): `Chunk`s until `End`, `Reset` →
    /// error. `head` = `None` while the `Head` is still unread; `Some(len)`
    /// once it has been read (`len` is the exact declared body length, when
    /// the device supplied one).
    Cloud {
        stream: crate::cloud::stream::CloudStream,
        head: Option<Option<u64>>,
    },
}

impl Upstream {
    /// The Cloud Mode body constructor (§F.3): the first message the
    /// stream yields must be `Head{status,content_type,length}` — anything
    /// else is a protocol fault. `bytes_stream()` then yields the `Chunk`s.
    pub fn from_cloud(stream: crate::cloud::stream::CloudStream) -> Self {
        Self {
            status: StatusCode::OK,
            headers: reqwest::header::HeaderMap::new(),
            body: UpstreamBody::Cloud { stream, head: None },
        }
    }

    /// The Cloud Mode body whose `Head` the `Client` already read (so the
    /// proxy gets the device's real status): the body starts at the `Chunk`s.
    fn from_cloud_headed(
        stream: crate::cloud::stream::CloudStream,
        status: u16,
        content_type: Option<String>,
        length: Option<u64>,
    ) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(ct) = content_type
            && let Ok(v) = reqwest::header::HeaderValue::from_str(&ct)
        {
            headers.insert(reqwest::header::CONTENT_TYPE, v);
        }
        if let Some(len) = length
            && let Ok(v) = reqwest::header::HeaderValue::from_str(&len.to_string())
        {
            headers.insert(reqwest::header::CONTENT_LENGTH, v);
        }
        Self {
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            headers,
            body: UpstreamBody::Cloud {
                stream,
                head: Some(length),
            },
        }
    }

    pub fn bytes_stream(self) -> BoxStream<'static, Result<Bytes, RemoteError>> {
        match self.body {
            UpstreamBody::Direct(resp, peer) => Box::pin(
                resp.bytes_stream()
                    .map(move |chunk| chunk.map_err(|e| peer.classify_reqwest(&e))),
            ),
            UpstreamBody::Cloud { stream, head } => Box::pin(cloud_body(stream, head)),
        }
    }
}

/// A protocol fault in a cloud body (the proxy renders it as a 502 like
/// any other upstream failure).
fn cloud_fault(message: &str) -> RemoteError {
    cloud_down("cloud", message)
}

/// §F.3: `Head` first (unless already read), then `Chunk`s paced by credit,
/// `End` ends. The receiver validates the exact declared length (short or
/// overlong is an error, never a clean truncated body) and returns credit
/// for bytes it has actually yielded. Per-record receive timeouts apply;
/// a steadily progressing transfer is never capped.
struct CloudBodyState {
    headed: bool,
    expected: Option<u64>,
    seen: u64,
    since_grant: u32,
    done: bool,
}

fn cloud_body(
    stream: crate::cloud::stream::CloudStream,
    head: Option<Option<u64>>,
) -> impl Stream<Item = Result<Bytes, RemoteError>> {
    let state = match head {
        Some(expected) => CloudBodyState {
            headed: true,
            expected,
            seen: 0,
            since_grant: 0,
            done: false,
        },
        None => CloudBodyState {
            headed: false,
            expected: None,
            seen: 0,
            since_grant: 0,
            done: false,
        },
    };
    futures::stream::unfold((stream, state), |(mut stream, mut st)| async move {
        if st.done {
            return None;
        }
        loop {
            // Return credit for the previous chunk(s) only when the
            // consumer asks for more: that is consumption, not delivery.
            if st.since_grant >= cloud::constants::CREDIT_GRANT {
                let grant = std::mem::take(&mut st.since_grant);
                if stream
                    .send_timed(
                        StreamMsg::Credit { bytes: grant },
                        Duration::from_secs(cloud::constants::CREDIT_STALL_SECS),
                    )
                    .await
                    .is_err()
                {
                    st.done = true;
                    return Some((
                        Err(cloud_down(
                            "cloud",
                            "the download stalled waiting for credit",
                        )),
                        (stream, st),
                    ));
                }
            }
            match stream
                .next_timed(Duration::from_secs(cloud::constants::RECV_STALL_SECS))
                .await
            {
                Ok(Some(StreamMsg::Head { length, .. })) if !st.headed => {
                    st.headed = true;
                    st.expected = length;
                }
                Ok(Some(StreamMsg::Chunk(c))) if st.headed => {
                    let len = c.len() as u64;
                    if let Some(expected) = st.expected
                        && st.seen + len > expected
                    {
                        st.done = true;
                        return Some((
                            Err(cloud_fault(
                                "the cloud body is longer than its declared length",
                            )),
                            (stream, st),
                        ));
                    }
                    st.seen += len;
                    st.since_grant = st.since_grant.saturating_add(len as u32);
                    return Some((Ok(Bytes::from(c)), (stream, st)));
                }
                Ok(Some(StreamMsg::End)) => {
                    st.done = true;
                    if !st.headed {
                        return Some((Err(cloud_fault("End before the Head")), (stream, st)));
                    }
                    if let Some(expected) = st.expected
                        && st.seen != expected
                    {
                        return Some((
                            Err(cloud_fault(
                                "the cloud body is shorter than its declared length",
                            )),
                            (stream, st),
                        ));
                    }
                    return None;
                }
                Ok(Some(StreamMsg::Reset(code))) => {
                    st.done = true;
                    return Some((Err(reset_error(code, "cloud")), (stream, st)));
                }
                // The actor consumes Credit; ignore defensively.
                Ok(Some(StreamMsg::Credit { .. })) => continue,
                Ok(Some(_)) => {
                    st.done = true;
                    return Some((
                        Err(cloud_fault("unexpected message in a cloud body")),
                        (stream, st),
                    ));
                }
                Ok(None) => {
                    st.done = true;
                    return Some((
                        Err(cloud_fault(if st.headed {
                            "the cloud body ended before its End"
                        } else {
                            "the cloud body ended without a Head"
                        })),
                        (stream, st),
                    ));
                }
                Err(_) => {
                    st.done = true;
                    return Some((
                        Err(cloud_down("cloud", "the download stalled waiting for data")),
                        (stream, st),
                    ));
                }
            }
        }
    })
}

/// The typed `GET` target at the `/r/{profile}/…` boundary (p3-design
/// §F.4, task A6): the only two shapes the proxy forwards. The boundary
/// parses the tail once into a `GetTarget`; anything that does not parse is
/// a **local 404 — the upstream is never contacted** (this narrows the
/// Direct path, which today forwards any shape: intended). `Direct` renders
/// `path()` to a URL; `Cloud` (PR-C) maps to `Op::Get(…)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GetTarget {
    /// `images/{id}` — the image cache's validated id (64 lower-hex + ext).
    Image(String),
    /// `api/uploads/{convo}/{id}` — both UUIDs.
    Upload { convo: Uuid, id: Uuid },
}

impl GetTarget {
    /// Parse the tail after `/r/{profile}/` (segments already
    /// percent-decoded by the boundary) into a target. `None` → local 404.
    /// The per-shape grammar checks are the traversal defence: a `..`
    /// segment can never match either shape.
    pub fn parse(path_after_r: &str) -> Option<GetTarget> {
        let segs: Vec<&str> = path_after_r.split('/').collect();
        match segs.as_slice() {
            ["images", id] if valid_image_id(id) => Some(Self::Image(id.to_string())),
            ["api", "uploads", convo, id] => Some(Self::Upload {
                convo: Uuid::parse_str(convo).ok()?,
                id: Uuid::parse_str(id).ok()?,
            }),
            _ => None,
        }
    }

    /// Render the target back to the upstream path (e.g. `/images/{id}`).
    pub(crate) fn path(&self) -> String {
        match self {
            Self::Image(id) => format!("/images/{id}"),
            Self::Upload { convo, id } => format!("/api/uploads/{convo}/{id}"),
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
    /// Cloud Mode (p3-design §F.7, new in PR-C): the trust chain rejected
    /// the connection (`Reset(TRUST)` / `Close{TRUST}` / `TrustError`).
    /// HTTP 502 with its text, like `Auth`.
    Trust(String),
    /// Cloud Mode (§F.7): `UnknownDevice` — the same generic 404 an
    /// unknown remote profile gets.
    NotFound {
        name: String,
    },
    /// Cloud Mode: the device refused an upload because the file exceeds
    /// its `max_file_mb` limit. The proxy answers 413 (never a generic 502).
    PayloadTooLarge {
        profile: String,
    },
}

/// Android 17 drops LAN dials silently without the "Nearby devices" grant; name the fix.
const ANDROID_LNP_HINT: &str = " On Android 17+, allow PeakBot \"Nearby devices\" in App info → Permissions so it can reach your local network.";

fn timeout_hint(os: &str) -> &'static str {
    if os == "android" {
        ANDROID_LNP_HINT
    } else {
        ""
    }
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
                    "Remote profile '{profile}' at {host} didn't answer in time.{}",
                    timeout_hint(std::env::consts::OS)
                )
            }
            Self::TlsPin { profile, want, got } => write!(
                f,
                "Remote profile '{profile}': the server's CA is {got}, but the config pins {want}. \
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
            // `@` cannot occur in a config profile name, so it marks a cloud peer.
            Self::Auth { profile } if profile.contains('@') => write!(
                f,
                "{profile}: device refused the connection (not enrolled on that device)"
            ),
            Self::Auth { profile } => write!(
                f,
                "Remote profile '{profile}' rejected its token. Update the token in Settings → Profiles."
            ),
            Self::Config { message } => write!(
                f,
                "config.yaml has an error, so remote profiles are unavailable: {message}"
            ),
            // §F.7 user texts (locked strings land with the green phase).
            Self::Trust(e) => write!(f, "cloud trust failure: {e} — re-check devices"),
            Self::NotFound { name } => write!(f, "no such remote: {name}"),
            Self::PayloadTooLarge { profile } => write!(
                f,
                "{profile}: the file is larger than that device's upload limit"
            ),
        }
    }
}

/// Map a stream reset code to the proxy-facing error: `TOO_LARGE` is the
/// device's upload limit (HTTP 413); everything else is a normal cloud
/// failure (HTTP 502).
fn reset_error(code: u16, name: &str) -> RemoteError {
    if code == cloud::constants::reset::TOO_LARGE {
        RemoteError::PayloadTooLarge {
            profile: name.to_string(),
        }
    } else {
        cloud_down(name, &reset_text(code))
    }
}

/// The §F.7 mapping `CloudError` → `RemoteError` (PR-C, where `Trust` is
/// first used): offline → `Down("device offline")`, trust refusals →
/// `Auth`/`Trust`, `UnknownDevice` → `NotFound`.
pub fn from_cloud(err: &crate::cloud::CloudError, name: &str) -> RemoteError {
    use crate::cloud::CloudError;
    match err {
        // §F.7: the relay couldn't reach the peer (or the handshake timed out).
        CloudError::Offline => cloud_down(name, "device offline"),
        // A chain-level trust failure: the new `Trust` variant (§F.7).
        CloudError::Trust(e) => RemoteError::Trust(format!("{e:?}")),
        // `open()` folds a peer's handshake `Close{TRUST|PROTOCOL}` into
        // `Reset(PROTOCOL)` (actor::reset_for_reason): the peer refused us.
        // Only `open()` errors come through here; mid-stream resets don't.
        CloudError::Reset(code) if *code == cloud::constants::reset::PROTOCOL => {
            RemoteError::Auth {
                profile: name.to_string(),
            }
        }
        CloudError::NotActive(kind) => cloud_down(name, &format!("cloud not connected ({kind:?})")),
        CloudError::Disabled => cloud_down(name, "cloud not connected (disabled)"),
        CloudError::UnknownDevice => RemoteError::NotFound {
            name: name.to_string(),
        },
        CloudError::Reset(code) if *code == cloud::constants::reset::TOO_LARGE => {
            RemoteError::PayloadTooLarge {
                profile: name.to_string(),
            }
        }
        CloudError::SelfTarget => cloud_down(name, "that is this device itself"),
        CloudError::Reset(code) => cloud_down(name, &reset_text(*code)),
        CloudError::Io(msg) => cloud_down(name, msg),
        CloudError::Flow(e) => cloud_down(name, &format!("{e:?}")),
    }
}

/// A §F.7 `Down` for a cloud failure (no host:port to carry — `cloud`).
fn cloud_down(name: &str, reason: &str) -> RemoteError {
    RemoteError::Down {
        profile: name.to_string(),
        host: "cloud".to_string(),
        reason: reason.to_string(),
    }
}

impl StdError for RemoteError {}

/// Who a failure is about: L, H (`host:port`) and the bare host the TLS
/// name check and `--tls-name` use. Exists before the `Client` does, so the
/// bootstrap can report errors too. `pub` because `Client::peer()` hands
/// it out (the `remote` module itself stays private to `web`).
#[derive(Debug, Clone)]
pub struct Peer {
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
#[derive(Clone)]
pub struct DirectClient {
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
impl fmt::Debug for DirectClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirectClient")
            .field("profile", &self.peer.profile)
            .field("host", &self.peer.host_port)
            .finish_non_exhaustive()
    }
}

impl DirectClient {
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
            Ok((ws, _response)) => Ok(WsStream::Direct(Box::new(ws))),
            Err(WsError::Http(response)) if response.status() == StatusCode::UNAUTHORIZED => {
                Err(peer.auth())
            }
            Err(e) => Err(peer.down(e.to_string())),
        }
    }

    /// `GET /api/conversations` — the remote's local rows (T4 shape).
    pub(crate) async fn list(&self) -> Result<Vec<ConversationSummaryWire>, RemoteError> {
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

    /// `GET <origin>/<path>` for the `/r/` proxy (T8): the token cookie is
    /// the only credential (§7), a 401 is E-AUTH, and the body is left for
    /// the proxy to stream.
    pub async fn get(&self, path: &str) -> Result<Upstream, RemoteError> {
        let resp = self
            .http
            .get(self.url(path))
            .header(COOKIE, self.cookie.clone())
            .send()
            .await
            .map_err(|e| self.peer.classify_reqwest(&e))?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            return Err(self.peer.auth());
        }
        Ok(Upstream {
            status: resp.status(),
            headers: resp.headers().clone(),
            body: UpstreamBody::Direct(resp, self.peer.clone()),
        })
    }

    /// `POST /api/uploads?convo=&name=` (T8): the query is rebuilt from the
    /// parsed parts only (§6.2), and the browser's `Content-Type` is the one
    /// browser header copied (§7). The body streams through.
    pub async fn post_upload(
        &self,
        convo: &Uuid,
        name: &str,
        content_type: Option<HeaderValue>,
        length: Option<u64>,
        body: BodyStream,
    ) -> Result<Upstream, RemoteError> {
        let body = reqwest::Body::wrap_stream(body);
        let mut url = self.url("api/uploads");
        url.query_pairs_mut()
            .append_pair("convo", &convo.as_hyphenated().to_string())
            .append_pair("name", name);
        let mut request = self
            .http
            .post(url)
            .header(COOKIE, self.cookie.clone())
            .body(body);
        if let Some(content_type) = content_type {
            request = request.header(reqwest::header::CONTENT_TYPE, content_type);
        }
        if let Some(len) = length {
            request = request.header(reqwest::header::CONTENT_LENGTH, len);
        }
        let resp = request
            .send()
            .await
            .map_err(|e| self.peer.classify_reqwest(&e))?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            return Err(self.peer.auth());
        }
        Ok(Upstream {
            status: resp.status(),
            headers: resp.headers().clone(),
            body: UpstreamBody::Direct(resp, self.peer.clone()),
        })
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

/// The remote-profile controller — an enum over transports (p3-design
/// §F.1): `Direct` is today's client, unchanged; the `Cloud` variant lands
/// in PR-C. Nothing above the transport learns which one it holds.
#[derive(Clone)]
pub enum Client {
    Direct(DirectClient),
    /// Cloud Mode (p3-design §F.1): a profile on a paired device, dialed
    /// through the verified chain (`handle.open(device, Op::Attach)` →
    /// `WsStream::Cloud`). Nothing above the transport learns this exists.
    Cloud {
        handle: crate::cloud::CloudHandle,
        device: cloud::DeviceId,
        profile: String,
    },
}

impl Client {
    pub async fn new(profile: &RemoteProfile, opts: ClientOpts) -> Result<Self, RemoteError> {
        Ok(Self::Direct(DirectClient::new(profile, opts).await?))
    }

    /// The connection policy the timeout/auth errors carry (§F.1 `peer()`).
    /// Cloud: `"profile@device"` (the slug from the verified chain).
    pub fn peer(&self) -> Peer {
        match self {
            Self::Direct(d) => d.peer.clone(),
            Self::Cloud {
                handle,
                device,
                profile,
            } => {
                let slug = cloud_slug(handle, device).unwrap_or_default();
                Peer {
                    profile: format!("{profile}@{slug}"),
                    host_port: slug.clone(),
                    host: slug,
                    port: 0,
                }
            }
        }
    }

    /// The error label for a cloud op: `profile@slug` from the verified
    /// chain (the slug is derived, never stored), the profile alone when
    /// the state is not `Active`.
    fn cloud_label(
        &self,
        handle: &crate::cloud::CloudHandle,
        device: &cloud::DeviceId,
        profile: &str,
    ) -> String {
        match cloud_slug(handle, device) {
            Some(slug) => format!("{profile}@{slug}"),
            None => profile.to_string(),
        }
    }

    /// Dial the transport (§F.1): `Direct` = today's WS upgrade; `Cloud` =
    /// `handle.open(device, Op::Attach)` → `WsStream::Cloud` (CloudServe
    /// forces `relayed` server-side, §E.1).
    pub async fn dial(&self) -> Result<WsStream, RemoteError> {
        match self {
            Self::Direct(d) => d.dial().await,
            Self::Cloud {
                handle,
                device,
                profile,
            } => {
                let name = self.cloud_label(handle, device, profile);
                handle
                    .open(*device, cloud::frames::Op::Attach)
                    .await
                    .map(WsStream::Cloud)
                    .map_err(|e| from_cloud(&e, &name))
            }
        }
    }

    pub(crate) async fn list(&self) -> Result<Vec<ConversationSummaryWire>, RemoteError> {
        match self {
            Self::Direct(d) => d.list().await,
            Self::Cloud {
                handle,
                device,
                profile,
            } => {
                let name = self.cloud_label(handle, device, profile);
                let mut stream = handle
                    .open(*device, cloud::frames::Op::List)
                    .await
                    .map_err(|e| from_cloud(&e, &name))?;
                let (status, body) = cloud_op_reply(&mut stream, &name).await?;
                if status != 200 {
                    return Err(cloud_down(&name, &format!("the device answered {status}")));
                }
                #[derive(Deserialize)]
                struct Items {
                    items: Vec<ConversationSummaryWire>,
                }
                let parsed: Items = serde_json::from_slice(&body)
                    .map_err(|e| cloud_down(&name, &format!("unexpected list reply: {e}")))?;
                Ok(parsed.items)
            }
        }
    }

    pub async fn kill(&self, id: &Uuid) -> Result<Option<String>, RemoteError> {
        match self {
            Self::Direct(d) => d.kill(id).await,
            Self::Cloud {
                handle,
                device,
                profile,
            } => {
                let name = self.cloud_label(handle, device, profile);
                let mut stream = handle
                    .open(*device, cloud::frames::Op::Kill { id: *id.as_bytes() })
                    .await
                    .map_err(|e| from_cloud(&e, &name))?;
                let (status, body) = cloud_op_reply(&mut stream, &name).await?;
                if status != 200 {
                    return Err(cloud_down(&name, &format!("the device answered {status}")));
                }
                #[derive(Deserialize)]
                struct KillReply {
                    #[serde(default)]
                    error: Option<String>,
                }
                let parsed: KillReply = serde_json::from_slice(&body)
                    .map_err(|e| cloud_down(&name, &format!("unexpected kill reply: {e}")))?;
                Ok(parsed.error)
            }
        }
    }

    /// The typed `GET` at the `/r/` boundary (p3-design §F.4): `Direct`
    /// renders `path()` to a URL; `Cloud` maps to `Op::Get(…)`.
    pub async fn get(&self, target: GetTarget) -> Result<Upstream, RemoteError> {
        match self {
            Self::Direct(d) => d.get(&target.path()).await,
            Self::Cloud {
                handle,
                device,
                profile,
            } => {
                let name = self.cloud_label(handle, device, profile);
                let target = match &target {
                    GetTarget::Image(id) => cloud::frames::GetTarget::Image { id: id.clone() },
                    GetTarget::Upload { convo, id } => cloud::frames::GetTarget::Upload {
                        convo: *convo.as_bytes(),
                        id: id.to_string(),
                    },
                };
                let mut stream = handle
                    .open(*device, cloud::frames::Op::Get(target))
                    .await
                    .map_err(|e| from_cloud(&e, &name))?;
                let (status, ct, len) = cloud_head(&mut stream, &name).await?;
                Ok(Upstream::from_cloud_headed(stream, status, ct, len))
            }
        }
    }

    pub async fn post_upload(
        &self,
        convo: &Uuid,
        name: &str,
        content_type: Option<HeaderValue>,
        length: Option<u64>,
        body: BodyStream,
    ) -> Result<Upstream, RemoteError> {
        match self {
            Self::Direct(d) => d.post_upload(convo, name, content_type, length, body).await,
            Self::Cloud {
                handle,
                device,
                profile,
            } => {
                let label = self.cloud_label(handle, device, profile);
                let content_type = content_type
                    .map(|h| h.to_str().unwrap_or_default().to_string())
                    .unwrap_or_default();
                let mut stream = handle
                    .open(
                        *device,
                        cloud::frames::Op::Upload {
                            convo: *convo.as_bytes(),
                            name: name.to_string(),
                            content_type,
                            length,
                        },
                    )
                    .await
                    .map_err(|e| from_cloud(&e, &label))?;
                // The body streams through in ≤ MAX_CHUNK pieces, paced by
                // the device's credit grants; every send is bounded by the
                // approved credit-stall budget. A closed/refused tunnel ends
                // the upload with the device's real error (413 for TOO_LARGE).
                let mut body = body;
                let send_stall = Duration::from_secs(cloud::constants::CREDIT_STALL_SECS);
                while let Some(chunk) = body.next().await {
                    let bytes = chunk.map_err(|e| cloud_down(&label, &e.to_string()))?;
                    for piece in bytes.chunks(cloud::constants::MAX_CHUNK) {
                        if let Err(e) = stream
                            .send_timed(StreamMsg::Chunk(piece.to_vec()), send_stall)
                            .await
                        {
                            return cloud_upload_error(stream, &label, &e).await;
                        }
                    }
                }
                if let Err(e) = stream.send_timed(StreamMsg::End, send_stall).await {
                    return cloud_upload_error(stream, &label, &e).await;
                }
                let (status, ct, len) = cloud_head(&mut stream, &label).await?;
                Ok(Upstream::from_cloud_headed(stream, status, ct, len))
            }
        }
    }
}

/// A refused upload's real reason: a peer `Reset(TOO_LARGE)` is queued on
/// the receive side after the local sink closes. Read it so the proxy can
/// answer 413 instead of a generic 502; if the device queued an early
/// `Head` (e.g. 415 Unacceptable Media Type), return it as `Ok(Upstream)`
/// so the phone surfaces the device's actual HTTP status.
async fn cloud_upload_error(
    mut stream: crate::cloud::stream::CloudStream,
    label: &str,
    err: &crate::cloud::CloudError,
) -> Result<Upstream, RemoteError> {
    match stream.next_timed(Duration::from_secs(2)).await {
        Ok(Some(StreamMsg::Head {
            status,
            content_type,
            length,
        })) => Ok(Upstream::from_cloud_headed(
            stream,
            status,
            Some(content_type),
            length,
        )),
        Ok(Some(StreamMsg::Reset(code))) => Err(reset_error(code, label)),
        _ => Err(cloud_down(
            label,
            &format!("the upload stream closed: {err}"),
        )),
    }
}

/// The device's Directory slug from the verified chain (§F.5); `None` when
/// the state is not `Active` or the device is gone.
fn cloud_slug(handle: &crate::cloud::CloudHandle, device: &cloud::DeviceId) -> Option<String> {
    let crate::cloud::CloudState::Active { devices, .. } = handle.state().borrow().clone() else {
        return None;
    };
    let slugs = crate::cloud::link::device_slugs(&devices);
    devices
        .iter()
        .zip(slugs)
        .find(|(d, _)| d.id == *device)
        .map(|(_, s)| s)
}

/// Read a request/response op `Head` (§E.2): the first message must be a
/// `Head`, else a protocol fault.
async fn cloud_head(
    stream: &mut crate::cloud::stream::CloudStream,
    name: &str,
) -> Result<(u16, Option<String>, Option<u64>), RemoteError> {
    match stream
        .next_timed(Duration::from_secs(cloud::constants::RECV_STALL_SECS))
        .await
    {
        Ok(Some(StreamMsg::Head {
            status,
            content_type,
            length,
        })) => Ok((status, Some(content_type), length)),
        Ok(Some(StreamMsg::Reset(code))) => Err(reset_error(code, name)),
        Ok(Some(_)) | Ok(None) => Err(cloud_down(name, "a cloud op reply must start with a Head")),
        Err(_) => Err(cloud_down(name, "the device stalled before answering")),
    }
}

/// Read a request/response op body to completion: `Head`, `Chunk`s until
/// `End` (§E.2). The receiver grants credit for consumed bytes and rejects
/// a body whose length differs from the `Head`'s declared length.
async fn cloud_op_reply(
    stream: &mut crate::cloud::stream::CloudStream,
    name: &str,
) -> Result<(u16, Vec<u8>), RemoteError> {
    let (status, _, expected) = cloud_head(stream, name).await?;
    let mut body = Vec::new();
    let mut since_grant = 0u32;
    loop {
        match stream
            .next_timed(Duration::from_secs(cloud::constants::RECV_STALL_SECS))
            .await
        {
            Ok(Some(StreamMsg::Chunk(c))) => {
                let len = c.len();
                if let Some(expected) = expected
                    && (body.len() + len) as u64 > expected
                {
                    return Err(cloud_down(
                        name,
                        "the reply body exceeds its declared length",
                    ));
                }
                body.extend(c);
                since_grant = since_grant.saturating_add(len as u32);
                if since_grant >= cloud::constants::CREDIT_GRANT {
                    stream
                        .send_timed(
                            StreamMsg::Credit { bytes: since_grant },
                            Duration::from_secs(cloud::constants::CREDIT_STALL_SECS),
                        )
                        .await
                        .map_err(|_| cloud_down(name, "stalled returning credit"))?;
                    since_grant = 0;
                }
            }
            Ok(Some(StreamMsg::End)) => {
                if let Some(expected) = expected
                    && body.len() as u64 != expected
                {
                    return Err(cloud_down(
                        name,
                        "the reply body is shorter than its declared length",
                    ));
                }
                return Ok((status, body));
            }
            Ok(Some(StreamMsg::Reset(code))) => return Err(reset_error(code, name)),
            Ok(Some(_)) => {
                return Err(cloud_down(name, "protocol fault in a cloud op body"));
            }
            Ok(None) => {
                return Err(cloud_down(name, "the cloud op stream ended before its End"));
            }
            Err(_) => {
                return Err(cloud_down(
                    name,
                    "the device stalled while sending the reply",
                ));
            }
        }
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct(d) => d.fmt(f),
            Self::Cloud { profile, .. } => f
                .debug_struct("Client::Cloud")
                .field("profile", profile)
                .finish_non_exhaustive(),
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
    /// The process-wide Cloud Mode handle (PR-C, §F.5): `None` when
    /// `general.cloud` is absent (no `profile@device` names resolvable).
    /// Set once at boot wiring; the LazyLock global picks it up.
    cloud: Option<crate::cloud::CloudHandle>,
}

/// The `/r/{name}` URL segment, parsed at the boundary (§F.5): split on
/// the LAST `@`. No `@` → `Profile` (profile names cannot contain `@` —
/// already validated in config — so a `profile@device` can never collide
/// with a config name). `device` is a slug `[a-z0-9-]{1,32}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteName {
    Profile(String),
    Cloud { profile: String, device: String },
}

impl RemoteName {
    /// Parse a URL segment (percent-decoded) into a name. §F.5: last `@`.
    pub fn parse(segment: &str) -> Self {
        match segment.rsplit_once('@') {
            // Last `@` wins: the profile passes through as-is (the host
            // validates it against its local profiles).
            Some((profile, device)) => Self::Cloud {
                profile: profile.to_string(),
                device: device.to_string(),
            },
            None => Self::Profile(segment.to_string()),
        }
    }
}

impl Directory {
    pub fn new() -> Self {
        Self {
            cloud: crate::cloud::boot_handle(),
            ..Self::default()
        }
    }

    /// Test/edge seam: a Directory bound to a specific cloud handle
    /// (the LazyLock global binds the boot handle at construction).
    pub fn with_cloud(mut self, cloud: Option<crate::cloud::CloudHandle>) -> Self {
        self.cloud = cloud;
        self
    }

    /// The cloud handle this directory resolves `profile@device` with
    /// (single source of truth: the verified chain in `CloudState`).
    pub fn cloud(&self) -> Option<&crate::cloud::CloudHandle> {
        self.cloud.as_ref()
    }

    /// §F.5: resolve a name to a `Client`. `Profile` = today's
    /// `client(name)`; `Cloud` = state must be `Active` (else `Down`/
    /// `Trust`) → slug lookup in `devices` (missing → `NotFound`) →
    /// `Client::Cloud`. Self is never listed.
    pub async fn resolve(&self, name: &RemoteName) -> Result<Client, RemoteError> {
        match name {
            RemoteName::Profile(p) => Ok((*self.client(p).await?).clone()),
            RemoteName::Cloud { profile, device } => {
                let label = format!("{profile}@{device}");
                let Some(handle) = &self.cloud else {
                    // No cloud configured: the name is as unknown as a
                    // profile never defined in the config.
                    return Err(RemoteError::NotFound { name: label });
                };
                let state = handle.state().borrow().clone();
                match state {
                    crate::cloud::CloudState::Trust(e) => Err(RemoteError::Trust(format!("{e:?}"))),
                    crate::cloud::CloudState::Active { devices, .. } => {
                        let self_id = handle.device_id();
                        let slugs = crate::cloud::link::device_slugs(&devices);
                        // Self is never listed (F.5): the verified chain
                        // is the single source of truth.
                        let Some(id) = devices
                            .iter()
                            .zip(slugs)
                            .find(|(d, s)| s == device && Some(d.id) != self_id)
                            .map(|(d, _)| d.id)
                        else {
                            return Err(RemoteError::NotFound { name: label });
                        };
                        Ok(Client::Cloud {
                            handle: handle.clone(),
                            device: id,
                            profile: profile.clone(),
                        })
                    }
                    other => Err(from_cloud(
                        &crate::cloud::CloudError::NotActive(other.kind()),
                        &label,
                    )),
                }
            }
        }
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

    /// Every remote profile in a fresh global-config parse (§5.11) — the
    /// T7 list merge groups these by origin (one fetch per server, RP4).
    pub fn remotes(&self) -> Result<Vec<RemoteRef>, RemoteError> {
        Ok(load_config()?
            .catalog
            .entries()
            .filter_map(|entry| match entry {
                CatalogEntry::Remote(r) => Some(RemoteRef {
                    name: r.name.clone(),
                    origin: r.endpoint.origin_without_slash(),
                    target: r.target.profile.clone(),
                }),
                _ => None,
            })
            .collect())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ClientCache> {
        self.clients.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// The global config only (repo files never define remotes, §5.11).
fn load_config() -> Result<Config, RemoteError> {
    let ctx = ParseCtx { known_skills: &[] };
    // Use the roots config dir (set by tests via paths::init_roots), not the
    // process cwd, so a fresh parse reads the same file the server booted with.
    let config_path = paths::config_dir().unwrap_or_else(|| PathBuf::from("."));
    let global_path = config_path.join("config.yaml");
    if !global_path.exists() {
        return Err(RemoteError::Config {
            message: format!("{} does not exist", global_path.display()),
        });
    }
    let text = std::fs::read_to_string(&global_path).map_err(|e| RemoteError::Config {
        message: format!("cannot read {}: {e}", global_path.display()),
    })?;
    let global = Source {
        path: &global_path,
        text: &text,
    };
    Config::parse(global, None, &ctx).map_err(|errors| RemoteError::Config {
        message: crate::config_errors_summary(&errors),
    })
}

fn snapshot(name: &str) -> Result<Arc<RemoteProfile>, RemoteError> {
    load_config()?
        .catalog
        .remote(name)
        .cloned()
        .ok_or_else(|| RemoteError::Config {
            message: format!("no remote profile named '{name}'"),
        })
}

/// The attach-time remote decision (design §5.11), from ONE fresh
/// global-config parse — the same path `Directory` uses on every call:
/// is `name` a remote profile *right now* (added or edited after boot),
/// and if so its definition, plus the fresh `infos()` a relay's
/// `LocalView.catalog` swaps into the SPA catalogue. `Err` is the locked
/// E-CONFIG string.
pub fn fresh_attach(
    name: &str,
) -> Result<(Option<Arc<RemoteProfile>>, Vec<ProfileInfo>), RemoteError> {
    let cfg = load_config()?;
    Ok((cfg.catalog.remote(name).cloned(), cfg.catalog.infos()))
}

/// The unpinned-attach counterpart of [`fresh_attach`]: the remote the
/// config homes *right now* (a relay-only config has no local session to
/// mint), plus the fresh catalogue. `None` when the fresh home is local or
/// there is no home at all (cloud-only config with zero profiles, P5).
pub fn fresh_home_attach() -> Result<(Option<Arc<RemoteProfile>>, Vec<ProfileInfo>), RemoteError> {
    let cfg = load_config()?;
    let remote = match cfg.catalog.home() {
        Some(crate::config::resolve::Home::Remote(r)) => Some(r),
        Some(crate::config::resolve::Home::Local(_)) | None => None,
    };
    Ok((remote, cfg.catalog.infos()))
}
