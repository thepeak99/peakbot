//! `peakbot-relay` — Shifu Cloud Mode relay (design: /tmp/peakbot/design/relay-design.md).
//!
//! Logs users in with generic OIDC, authenticates device WebSockets with a
//! session token plus a signed Hello, stores each account's public device chain
//! and short-lived enrollment letters in one SQLite file, and forwards opaque
//! Pipe bytes between live sockets of the same account. All protocol logic
//! comes from the `cloud` crate.
//!
//! SKELETON (TDD): the public interface is locked by the design; every
//! behaviour is `todo!()`. The integration tests in `tests/` are RED on
//! purpose. **Remove the crate-level `allow`s below as implementation lands.**

// SKELETON ONLY: the implementer removes these as each module fills in.
#![allow(dead_code, unused_variables)]

pub mod config;
pub mod conn;
pub mod hub;
pub mod http;
pub mod limits;
pub mod oidc;
pub mod store;

use std::net::SocketAddr;
use std::path::Path;

/// Serve the relay on the given listener (design §5.4). The test seam is the
/// production entry point; spawns the 60 s sweeper (design §4.7).
pub async fn serve(
    cfg: config::Config,
    listener: tokio::net::TcpListener,
) -> anyhow::Result<RelayHandle> {
    let _ = cfg;
    todo!("P2 task 7/8: store + hub + Logins + axum router + sweeper")
}

/// Handle to a running relay (design §5.4).
pub struct RelayHandle {
    /// The bound address of the listener.
    pub addr: SocketAddr,
    // private: store, hub, logins, shutdown
    _priv: (),
}

impl RelayHandle {
    /// Run one sweeper pass at `now` (design §4.7). The periodic sweeper
    /// calls the same code; tests call this directly (no sleeps).
    pub async fn sweep_at(&self, now: u64) {
        let _ = now;
        todo!("P2 task 7: store.sweep + login purge + close expired pending sockets")
    }

    /// Stop the relay and release the DB lock.
    pub async fn shutdown(self) {
        todo!("P2 task 7: graceful shutdown")
    }
}

/// `peakbot-relay reset-account <issuer> <subject>` (design §4.8, §5.4).
/// The relay must be stopped: `Locked` when the exclusive DB lock is held.
pub fn reset_account(db: &Path, issuer: &str, subject: &str) -> Result<store::ResetReport, store::ResetError> {
    let _ = (db, issuer, subject);
    todo!("P2 task 4: thin wrapper over store::reset_account")
}
