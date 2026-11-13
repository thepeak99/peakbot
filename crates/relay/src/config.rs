//! `relay.yaml` parsing and validation (design §3.1). This is the only place
//! config is parsed.

use std::net::SocketAddr;
use std::path::Path;

/// RFC 6454 ASCII origin string (`scheme://host[:port]`), used for the Hello
/// `tbs` and as the OIDC `redirect_uri` origin (design §3.1, §5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin(pub String);

/// OIDC `client_id`. Not secret; plain `Debug`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientId(pub String);

/// OIDC client secret, read from `client_secret_file` at load. Its value is
/// never logged; `Debug` is redacted by construction (design §3.1).
#[derive(Clone, PartialEq, Eq)]
pub struct ClientSecret(String);

impl ClientSecret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ClientSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClientSecret(<redacted>)")
    }
}

/// One configured IdP (design §3.1). `name` is only the `/login?issuer=` selector.
#[derive(Debug, Clone)]
pub struct Issuer {
    pub name: String,
    /// Issuer URL; must equal the ID token `iss` byte-for-byte (design §5.3).
    pub url: String,
    pub client_id: ClientId,
    pub client_secret: ClientSecret,
}

/// Validated relay configuration (design §3.1). Fields are public so the test
/// harness can build a config in code; validation happens in `from_file`.
#[derive(Debug, Clone)]
pub struct Config {
    /// Plain-HTTP bind address; the reverse proxy terminates TLS.
    pub bind: SocketAddr,
    /// Origin used for the Hello tbs AND the OIDC redirect_uri.
    pub origin: Origin,
    /// SQLite file (WAL + `locking_mode=EXCLUSIVE`, design §3.2).
    pub db: std::path::PathBuf,
    /// Optional reverse-DNS app scheme enabling the mobile deep-link redirect.
    pub app_scheme: Option<String>,
    /// ≥ 1, unique names (design §3.1).
    pub issuers: Vec<Issuer>,
}

/// Wire shape of `relay.yaml`; `deny_unknown_fields` is the load-time gate
/// (design §3.1).
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    bind: SocketAddr,
    public_url: String,
    db: std::path::PathBuf,
    #[serde(default)]
    app_scheme: Option<String>,
    issuers: Vec<RawIssuer>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIssuer {
    name: String,
    url: String,
    client_id: String,
    client_secret_file: std::path::PathBuf,
}

impl Config {
    /// Parse `relay.yaml`, validate (design §3.1), read secret files.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
        let raw: RawConfig = serde_yaml::from_str(&text).map_err(ConfigError::Yaml)?;
        let origin = validate_origin(&raw.public_url)?;
        let app_scheme = match &raw.app_scheme {
            Some(s) => {
                validate_app_scheme(s)?;
                Some(s.clone())
            }
            None => None,
        };
        let mut seen = std::collections::BTreeSet::new();
        let mut issuers = Vec::with_capacity(raw.issuers.len());
        for r in raw.issuers {
            validate_issuer_name(&r.name)?;
            if !seen.insert(r.name.clone()) {
                return Err(ConfigError::Invalid("issuer names must be unique".into()));
            }
            // Trailing whitespace is trimmed; an empty secret is a config error.
            let secret = std::fs::read_to_string(&r.client_secret_file)
                .map_err(ConfigError::Io)?
                .trim_end()
                .to_string();
            if secret.is_empty() {
                return Err(ConfigError::Invalid(
                    "client_secret_file must be non-empty".into(),
                ));
            }
            issuers.push(Issuer {
                name: r.name,
                url: r.url,
                client_id: ClientId(r.client_id),
                client_secret: ClientSecret(secret),
            });
        }
        if issuers.is_empty() {
            return Err(ConfigError::Invalid(
                "at least one issuer is required".into(),
            ));
        }
        Ok(Config {
            bind: raw.bind,
            origin,
            db: raw.db,
            app_scheme,
            issuers,
        })
    }
}

/// `public_url`: https, or http only on loopback; origin only (design §3.1).
fn validate_origin(raw: &str) -> Result<Origin, ConfigError> {
    let u =
        url::Url::parse(raw).map_err(|_| ConfigError::Invalid("public_url: not a URL".into()))?;
    let loopback = matches!(
        u.host_str(),
        Some("127.0.0.1") | Some("::1") | Some("localhost")
    );
    match u.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => {
            return Err(ConfigError::Invalid(
                "public_url: https, or http on loopback only".into(),
            ));
        }
    }
    if u.path() != "/" || u.query().is_some() || u.fragment().is_some() {
        return Err(ConfigError::Invalid(
            "public_url: must be an origin (no path, query or fragment)".into(),
        ));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(ConfigError::Invalid("public_url: no userinfo".into()));
    }
    Ok(Origin(u.origin().ascii_serialization()))
}

/// `^[a-z][a-z0-9+.-]*\.[a-z0-9+.-]+$` minus `http`/`https` (design §3.1).
fn validate_app_scheme(s: &str) -> Result<(), ConfigError> {
    let err = || ConfigError::Invalid("app_scheme: reverse-DNS shape required".into());
    let bytes = s.as_bytes();
    if s == "http" || s == "https" {
        return Err(err());
    }
    let charset = !bytes.is_empty()
        && bytes[0].is_ascii_lowercase()
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.')
        });
    // The dot must have a non-empty part on both sides (the `x.y` shape).
    let dotted = s.split('.').all(|part| !part.is_empty()) && s.split('.').count() >= 2;
    if !charset || !dotted {
        return Err(err());
    }
    Ok(())
}

fn validate_issuer_name(name: &str) -> Result<(), ConfigError> {
    let ok = (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !ok {
        return Err(ConfigError::Invalid(
            "issuer name: [a-z0-9-]{1,32} required".into(),
        ));
    }
    Ok(())
}

/// Config parse/validation failure. `Debug` must never print a secret value.
#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Yaml(serde_yaml::Error),
    Invalid(String),
}

/// Device redirect URI, parsed once at `/login` (design §5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectUri {
    /// `http://127.0.0.1:<port>/<path>` — explicit port, no userinfo/fragment;
    /// a query is allowed and preserved.
    Loopback(url::Url),
    /// `<app_scheme>:…` — only when `app_scheme` is configured; no fragment.
    App(url::Url),
}

impl RedirectUri {
    /// Accept exactly the two shapes of design §5.1; reject everything else.
    pub fn parse(raw: &str, app_scheme: Option<&str>) -> Result<Self, RedirectUriError> {
        let u = url::Url::parse(raw).map_err(|_| RedirectUriError::Rejected)?;
        if u.fragment().is_some() {
            return Err(RedirectUriError::Rejected);
        }
        if u.scheme() == "http" {
            // Host is literally 127.0.0.1 with an explicit port; no userinfo.
            if u.host_str() == Some("127.0.0.1")
                && u.port().is_some()
                && u.username().is_empty()
                && u.password().is_none()
            {
                return Ok(RedirectUri::Loopback(u));
            }
            return Err(RedirectUriError::Rejected);
        }
        if let Some(s) = app_scheme
            && u.scheme() == s
        {
            return Ok(RedirectUri::App(u));
        }
        Err(RedirectUriError::Rejected)
    }
}

/// `RedirectUri::parse` failure (design §5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectUriError {
    Rejected,
}
