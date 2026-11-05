//! The resolved remote-profile layer (remote-profiles design §4.2): the
//! types the rest of the code sees and the rules that turn a file-layer
//! `RemoteDef` into a trusted `RemoteProfile`. Built only by
//! `Config::parse` (via `parse_remote`), so the RP1–RP3 invariants hold
//! by construction.

use std::fmt;

use super::file::{RemoteDef, TabColor};
use super::named::{invalid_name_error, valid_name};
use url::Url;

/// The trust policy of an https endpoint (RP2: https always has one —
/// there is no "https with default trust" state).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// Only this CA is a root (the pin from the pairing link).
    PinnedCa(CaFingerprint),
    /// Explicit opt-in to the system root store.
    System,
}

/// A pinned CA: 32 digest bytes, displayed as `sha256:<64 lower hex>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaFingerprint([u8; 32]);

impl fmt::Display for CaFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:")?;
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// The remote server's web token (RP6). Debug and Display are redacted so
/// the value never reaches a log line, a Debug print, or an error string.
#[derive(Clone, PartialEq, Eq)]
pub struct RemoteToken(String);

impl RemoteToken {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The raw value. Only `ui::web::remote::Client` reads it, to build the
    /// `peakbot_token` cookie (design §7).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RemoteToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RemoteToken(***)")
    }
}

impl fmt::Display for RemoteToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RemoteToken(***)")
    }
}

/// The profile — and optionally one agent of it — on the remote server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTarget {
    pub profile: String,
    pub agent: Option<String>,
}

/// A remote endpoint (RP2/RP3): https with an explicit trust policy, or
/// plaintext on loopback only. The origin is the normalised
/// `scheme://host[:port]/` — nothing else is expressible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Tls { origin: Url, trust: Trust },
    LoopbackPlain { origin: Url },
}

impl Endpoint {
    /// The origin without its trailing slash, for messages (RP4).
    pub fn origin_without_slash(&self) -> String {
        let origin = match self {
            Endpoint::Tls { origin, .. } | Endpoint::LoopbackPlain { origin } => origin,
        };
        origin.as_str().trim_end_matches('/').to_string()
    }
}

/// A fully resolved remote profile (RP1). `color` is always set (RP8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteProfile {
    /// L — the local profile name (same name rule as profiles).
    pub name: String,
    pub endpoint: Endpoint,
    pub token: RemoteToken,
    pub target: RemoteTarget,
    pub color: TabColor,
}

/// One §4.4 violation of a `remote:` block: the field the error is keyed
/// at and the locked message. `Config::parse` prefixes the path with
/// `profiles.<n>.remote`.
pub struct RemoteFieldError {
    pub field: &'static str,
    pub message: String,
}

const URL_SCHEME_MESSAGE: &str = "must be an https:// URL, e.g. https://10.0.0.5:7823";
const URL_SHAPE_MESSAGE: &str = "only scheme://host[:port] — put the token in `token`";
const URL_LOOPBACK_MESSAGE: &str =
    "remote profiles connect over HTTPS — http is allowed only for 127.0.0.1 / localhost";
const TOKEN_MESSAGE: &str = "the remote's web token (as printed by its `?token=…` link)";
const CA_MISSING_MESSAGE: &str = "pin the server's CA (`ca: sha256:…` from its pairing link) or use `ca: system` for a publicly trusted certificate";
const CA_HTTP_MESSAGE: &str = "`ca` applies to https only";
const CA_SHAPE_MESSAGE: &str = "expected `system` or `sha256:<64 hex chars>`";

/// The `remote:` block → `RemoteProfile`, collecting every §4.4 violation
/// (one per case; the resolve layer keys them at `profiles.<n>.remote.*`).
pub fn parse_remote(
    name: &str,
    def: &RemoteDef,
    color: Option<TabColor>,
) -> Result<RemoteProfile, Vec<RemoteFieldError>> {
    let mut errors: Vec<RemoteFieldError> = Vec::new();
    let mut endpoint: Option<Endpoint> = None;

    match parse_origin(&def.url) {
        Ok(origin) => {
            if origin.scheme() == "https" {
                // RP2: https needs a pin or an explicit `system`.
                match def.ca.as_deref().map(parse_ca) {
                    Some(Ok(trust)) => endpoint = Some(Endpoint::Tls { origin, trust }),
                    Some(Err(message)) => errors.push(RemoteFieldError {
                        field: "ca",
                        message,
                    }),
                    None => errors.push(RemoteFieldError {
                        field: "ca",
                        message: CA_MISSING_MESSAGE.to_string(),
                    }),
                }
            } else if def.ca.is_some() {
                // RP3: plaintext is loopback-only, and it has no CA.
                errors.push(RemoteFieldError {
                    field: "ca",
                    message: CA_HTTP_MESSAGE.to_string(),
                });
            } else {
                endpoint = Some(Endpoint::LoopbackPlain { origin });
            }
        }
        Err(message) => errors.push(RemoteFieldError {
            field: "url",
            message,
        }),
    }

    if !cookie_safe_token(&def.token) {
        errors.push(RemoteFieldError {
            field: "token",
            message: TOKEN_MESSAGE.to_string(),
        });
    }
    if !valid_name(&def.profile) {
        errors.push(RemoteFieldError {
            field: "profile",
            message: invalid_name_error(&def.profile),
        });
    }
    if let Some(agent) = &def.agent
        && !valid_name(agent)
    {
        errors.push(RemoteFieldError {
            field: "agent",
            message: invalid_name_error(agent),
        });
    }

    match endpoint {
        Some(endpoint) if errors.is_empty() => Ok(RemoteProfile {
            name: name.to_string(),
            endpoint,
            token: RemoteToken::new(def.token.clone()),
            target: RemoteTarget {
                profile: def.profile.clone(),
                agent: def.agent.clone(),
            },
            color: color.unwrap_or_else(|| TabColor::derived(name)),
        }),
        _ => Err(errors),
    }
}

/// `scheme://host[:port]` only (row 4); http is loopback-only (row 3).
fn parse_origin(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| URL_SCHEME_MESSAGE.to_string())?;
    if !matches!(url.scheme(), "https" | "http") {
        return Err(URL_SCHEME_MESSAGE.to_string());
    }
    if !url.username().is_empty()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(URL_SHAPE_MESSAGE.to_string());
    }
    if url.scheme() == "http" && !url.host().is_some_and(|h| is_loopback(&h)) {
        return Err(URL_LOOPBACK_MESSAGE.to_string());
    }
    Ok(url)
}

/// 127.0.0.0/8, `::1` and `localhost` — nothing else (RP3). A prefixed or
/// suffixed look-alike (`127.0.0.1.example.com`) is NOT loopback.
fn is_loopback(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(d) => *d == "localhost",
        url::Host::Ipv4(v4) => v4.octets()[0] == 127,
        url::Host::Ipv6(v6) => v6.is_loopback(),
    }
}

/// `system` or `sha256:<64 lowercase hex>` (row 8) — nothing else.
fn parse_ca(ca: &str) -> Result<Trust, String> {
    if ca == "system" {
        return Ok(Trust::System);
    }
    let hex = ca
        .strip_prefix("sha256:")
        .filter(|h| h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .ok_or_else(|| CA_SHAPE_MESSAGE.to_string())?;
    let mut bytes = [0u8; 32];
    for (out, pair) in bytes.iter_mut().zip(hex.as_bytes().chunks(2)) {
        *out = u8::from_str_radix(std::str::from_utf8(pair).expect("ascii-checked"), 16)
            .expect("ascii hex pair");
    }
    Ok(Trust::PinnedCa(CaFingerprint(bytes)))
}

/// Non-empty, cookie-safe: no whitespace, `;`, `,`, `"` or control chars
/// (row 5) — the token travels as a Cookie value.
fn cookie_safe_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && !matches!(c, ';' | ',' | '"'))
}
