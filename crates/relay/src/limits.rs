//! Relay-only knobs (design §5.6). Protocol constants live in `cloud::constants`
//! and are reused, never copied.
//!
//! SKELETON: values are locked by the design; only the use-sites are `todo!()`.

/// 14 days **idle**, sliding on each `/ws` upgrade (design §5.6, §6.1).
pub const SESSION_TTL: u64 = 1_209_600;
/// OIDC flows in flight (design §5.6).
pub const FLOW_TTL: u64 = 600;
/// One-time `/token` codes, single use (design §5.6).
pub const CODE_TTL: u64 = 60;
/// In-flight logins per map; `/login` → 503 when full (expired evicted first).
pub const MAX_LOGINS_IN_FLIGHT: usize = 1024;
/// Outbound messages per socket; `Full` ⇒ slow consumer ⇒ close 1013 (design §6.2).
pub const OUTBOX_CAP: usize = 32;
/// Posts per socket, fixed 60 s window (design §5.6).
pub const POSTS_PER_MINUTE: u64 = 30;
/// Sockets per account, counted from upgrade (pre-Hello included) → 429 (design §6.5).
pub const MAX_SOCKETS_PER_ACCOUNT: usize = 32;
/// Global socket cap — added on top of the design (design §8 R4 left it open).
/// When the process-wide count of live `/ws` sockets reaches this, the upgrade is
/// refused with HTTP 503 **before** the handshake.
pub const MAX_SOCKETS: usize = 4096;
/// Sweeper period (design §5.6).
pub const SWEEP_INTERVAL: u64 = 60;
/// Per-IdP-request timeout (design §5.6).
pub const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
