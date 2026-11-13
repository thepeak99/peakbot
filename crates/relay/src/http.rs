//! The HTTP boundary: `/login`, `/oidc/callback`, `/token`, `/logout`, `/ws`
//! (design §5.1). Holds the in-memory `Logins` (OIDC flows + one-time codes).
//!
//! SKELETON: signatures are the locked interface; bodies are `todo!()`.

use std::net::SocketAddr;

use crate::config::Config;
use crate::hub::Hub;
use crate::store::Db;

/// One in-flight `/login` flow or one-time code (design §3.3). Both maps are
/// capped at `limits::MAX_LOGINS_IN_FLIGHT`; `now` is injected for tests.
#[non_exhaustive]
pub struct Logins;

impl Logins {
    pub fn new() -> Self {
        todo!("P2 task 7")
    }
}

impl Default for Logins {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the axum router (design §5.1). All bodies JSON unless stated.
pub fn router(cfg: Config, db: Db, hub: Hub, logins: Logins) -> axum::Router {
    let _ = (cfg, db, hub, logins);
    todo!("P2 task 7: /login, /oidc/callback, /token, /logout, /ws")
}

/// The `/ws` upgrade: 401 (token), 429 (per-account cap), 503 (global
/// `limits::MAX_SOCKETS` cap) as plain responses, else the 101 upgrade
/// response with the frame caps (design §5.1).
pub async fn ws_upgrade(addr: SocketAddr, token: Option<String>) -> axum::response::Response {
    let _ = (addr, token);
    todo!("P2 task 7: bearer check + caps + upgrade with frame caps")
}
