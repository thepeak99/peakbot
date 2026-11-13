//! `general.cloud` — p3-design §B.
//!
//! ```yaml
//! general:
//!   cloud:                 # present ⇒ Cloud Mode on; absent ⇒ Disabled
//!     relay: https://relay.example.com
//!     ca: system           # or sha256:<64 lower hex>
//! ```
//!
//! Reuses `config::remote::{Endpoint, Trust}` so "https without a trust
//! decision" cannot be represented; validation is the same code path as
//! `parse_remote` (`parse_endpoint`). Errors are keyed
//! `general.cloud.relay` / `general.cloud.ca` with the remote-profile
//! messages verbatim (the CA-missing noun parameterised: "server's" →
//! "relay's").

use std::path::PathBuf;

use serde::Deserialize;
use url::Url;

use crate::config::remote::{Endpoint, RemoteFieldError};

/// The raw `general.cloud` block. `deny_unknown_fields`: there are no other
/// knobs (backoff, timeouts and limits are constants). There is no
/// `enabled: false` — delete the block to disable.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CloudDef {
    pub relay: String,
    #[serde(default)]
    pub ca: Option<String>,
}

/// The parsed cloud config. `state_dir` is derived at the boundary
/// (`<data>/peakbot/cloud`) in production and injected in tests
/// (p3-design I.2: `paths::init_roots` is process-global, so per-core
/// state must be injected, never derived).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudConfig {
    pub relay: Endpoint,
    pub state_dir: PathBuf,
    /// Name on this device's card (green phase: the hostname) — the card is
    /// built from the seed + name + platform (design gap: not specified).
    pub device_name: String,
    /// Card platform byte (spec §4.1; 1 = desktop, 4 = phone in the vectors).
    pub platform: u8,
}

/// Absent ⇒ `Ok(None)` (CloudState::Disabled, the actor is not spawned);
/// present ⇒ parsed + validated endpoint. Semantic violations are
/// collected, keyed `relay` / `ca` (the resolve layer prefixes
/// `general.cloud.`).
pub fn parse_cloud(def: Option<&CloudDef>) -> Result<Option<CloudConfig>, Vec<RemoteFieldError>> {
    let Some(def) = def else {
        return Ok(None);
    };
    // Same rule as remote profiles by construction (p3-design §B).
    let relay =
        crate::config::remote::parse_endpoint(&def.relay, def.ca.as_deref(), "relay", "relay's")?;
    Ok(Some(CloudConfig {
        relay,
        device_name: "device".to_string(),
        platform: 1,
        state_dir: crate::paths::app_data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("cloud"),
    }))
}

/// The origin slug for the public state dir: `short_id(sha256(origin))`
/// (p3-design §C): the first 12 lowercase hex chars (6 digest bytes) of the
/// SHA-256 of the origin string without its trailing slash.
pub fn origin_slug(origin: &Url) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(origin.as_str().trim_end_matches('/').as_bytes());
    digest[..6].iter().map(|b| format!("{b:02x}")).collect()
}
