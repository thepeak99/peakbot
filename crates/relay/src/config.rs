//! `relay.yaml` parsing and validation (design §3.1). This is the only place
//! config is parsed.
//!
//! SKELETON: types are the locked interface (the test harness builds `Config`
//! in code); `from_file` and `RedirectUri::parse` are `todo!()`.

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

impl Config {
    /// Parse `relay.yaml`, validate (design §3.1), read secret files.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        todo!("P2 task 2: serde_yaml + deny_unknown_fields + validation rules")
    }
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
        todo!("P2 task 7: the §5.1 redirect-URI table")
    }
}

/// `RedirectUri::parse` failure (design §5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectUriError {
    Rejected,
}
