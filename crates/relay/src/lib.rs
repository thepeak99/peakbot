//! `peakbot-relay` — Shifu Cloud Mode relay (design: /tmp/peakbot/design/relay-design.md).
//!
//! Logs users in with generic OIDC, authenticates device WebSockets with a
//! session token plus a signed Hello, stores each account's public device chain
//! and short-lived enrollment letters in one SQLite file, and forwards opaque
//! Pipe bytes between live sockets of the same account. All protocol logic
//! comes from the `cloud` crate.
//!
//! Pass A (config, store sessions, OIDC, HTTP routes, `/ws` upgrade auth) is
//! implemented. `conn` is a pass-B placeholder: it only sends the Challenge.

pub mod config;
// Pass-B placeholder: the conn state machine lands there.
#[allow(dead_code, unused_variables)]
pub mod conn;
pub mod http;
pub mod hub;
pub mod limits;
pub mod oidc;
pub mod store;

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use rand_core::TryRngCore as _;
use subtle::ConstantTimeEq as _;
use tokio::sync::watch;

/// Unix seconds; a clock before the epoch reads as 0 (fails closed on expiry).
pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 32 bytes from the OS RNG (tokens, codes, states, nonces: ≥ 256 bits).
pub(crate) fn random_32() -> Result<[u8; 32], rand_core::OsError> {
    let mut b = [0u8; 32];
    rand_core::OsRng.try_fill_bytes(&mut b)?;
    Ok(b)
}

/// Constant-time equality for secrets and their hashes.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// Serve the relay on the given listener (design §5.4). The test seam is the
/// production entry point; spawns the 60 s sweeper (design §4.7).
pub async fn serve(
    cfg: config::Config,
    listener: tokio::net::TcpListener,
) -> anyhow::Result<RelayHandle> {
    let addr = listener.local_addr()?;
    let path = cfg.db.clone();
    let store = tokio::task::spawn_blocking(move || store::Store::open(&path))
        .await?
        .map_err(|e| anyhow::anyhow!("open db: {e:?}"))?;
    let db = store::Db::new(store);
    let hub = hub::Hub::new();
    let logins = http::Logins::new();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let state = http::AppState {
        oidc: std::sync::Arc::new(
            oidc::Oidc::new(cfg.issuers.clone(), &cfg.origin.0)
                .map_err(|e| anyhow::anyhow!("oidc client: {e:?}"))?,
        ),
        origin: cfg.origin.0.clone(),
        app_scheme: cfg.app_scheme.clone(),
        db: db.clone(),
        hub: hub.clone(),
        logins: logins.clone(),
        shutdown: shutdown_rx.clone(),
    };
    let app = http::router(state);
    let server_stop = shutdown_rx.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(stopped(server_stop))
            .await;
    });
    let sweeper = {
        let (db, hub, logins) = (db.clone(), hub.clone(), logins.clone());
        let stop = shutdown_rx;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(limits::SWEEP_INTERVAL));
            loop {
                tokio::select! {
                    _ = tick.tick() => sweep(&db, &hub, &logins, now_secs()).await,
                    _ = stopped(stop.clone()) => break,
                }
            }
        })
    };
    Ok(RelayHandle {
        addr,
        db,
        hub,
        logins,
        shutdown: shutdown_tx,
        tasks: vec![server, sweeper],
    })
}

/// One sweeper pass (design §4.7), shared by the timer and `sweep_at`.
async fn sweep(db: &store::Db, hub: &hub::Hub, logins: &http::Logins, now: u64) {
    match db.run(move |s| s.sweep(now)).await {
        Ok(expired) => {
            // A pending socket must always have a pending row.
            for id in expired {
                hub.close_pending(id, 1008);
            }
        }
        Err(e) => tracing::error!(error = %e, "sweep failed"),
    }
    logins.purge(now);
}

/// Handle to a running relay (design §5.4).
#[non_exhaustive]
pub struct RelayHandle {
    /// The bound address of the listener.
    pub addr: SocketAddr,
    db: store::Db,
    hub: hub::Hub,
    logins: http::Logins,
    shutdown: watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl RelayHandle {
    /// Run one sweeper pass at `now` (design §4.7). The periodic sweeper
    /// calls the same code; tests call this directly (no sleeps).
    pub async fn sweep_at(&self, now: u64) {
        sweep(&self.db, &self.hub, &self.logins, now).await;
    }

    /// Stop the relay and release the DB lock.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        // Sockets watch the same signal; bound the wait so a stuck peer
        // cannot hang shutdown.
        for t in self.tasks {
            let _ = tokio::time::timeout(Duration::from_secs(5), t).await;
        }
        self.db.release(Duration::from_secs(5)).await;
    }
}

/// `peakbot-relay reset-account <issuer> <subject>` (design §4.8, §5.4).
/// The relay must be stopped: `Locked` when the exclusive DB lock is held.
pub fn reset_account(
    db: &Path,
    issuer: &str,
    subject: &str,
) -> Result<store::ResetReport, store::ResetError> {
    store::reset_account(db, issuer, subject)
}

/// Resolves once shutdown is signalled. The `watch` guard is dropped before
/// returning so the future stays `Send`.
pub(crate) async fn stopped(mut rx: watch::Receiver<bool>) {
    let _ = rx.wait_for(|s| *s).await;
}
