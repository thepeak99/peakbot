//! The router: live sockets, `DeviceId → Conn` (design §6.2). No persistence,
//! no membership copy — routing asks the chain, then the Hub.
//!
//! The lock is a std `Mutex`, never held across `.await`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use cloud::DeviceId;
use cloud::codec::FromRelay;
use tokio::sync::{mpsc, watch};

use crate::limits::{MAX_SOCKETS, MAX_SOCKETS_PER_ACCOUNT};
use crate::store::{AccountId, Role};

/// One live socket (design §3.3). Closing goes through `close`, never `tx`,
/// so a full queue can still be closed (§6.2).
#[non_exhaustive]
pub struct Conn {
    /// Unique per socket; a replaced socket never evicts its successor.
    pub id: u64,
    pub account: AccountId,
    /// `sha256(token)` the socket was opened with (for `/logout`).
    pub session: [u8; 32],
    pub role: Role,
    pub tx: mpsc::Sender<FromRelay>,
    pub close: watch::Sender<Option<u16>>,
}

#[derive(Default)]
struct HubInner {
    conns: HashMap<DeviceId, Conn>,
    /// Sockets per account counted from upgrade, pre-Hello included (§6.5).
    per_account: HashMap<AccountId, usize>,
    total: usize,
}

/// The router (design §6.2). Cheap to clone; clones share state.
#[derive(Clone, Default)]
pub struct Hub(Arc<Mutex<HubInner>>);

fn lock(m: &Mutex<HubInner>) -> MutexGuard<'_, HubInner> {
    // Every op is a few map edits; a poisoned lock holds consistent data.
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Permit at upgrade (design §6.5): global cap → 503, per-account → 429.
    /// `Drop` of the permit decrements both counters (RAII).
    pub fn permit(&self, acct: AccountId) -> Result<SocketPermit, PermitDenied> {
        let mut g = lock(&self.0);
        if g.total >= MAX_SOCKETS {
            return Err(PermitDenied::Global);
        }
        let n = g.per_account.entry(acct).or_insert(0);
        if *n >= MAX_SOCKETS_PER_ACCOUNT {
            return Err(PermitDenied::Account);
        }
        *n += 1;
        g.total += 1;
        Ok(SocketPermit {
            hub: self.0.clone(),
            acct,
        })
    }

    /// Register after a successful Hello; replaces + closes an older Conn for
    /// the id with 1008 (newest wins).
    pub fn register(&self, id: DeviceId, conn: Conn) {
        let old = lock(&self.0).conns.insert(id, conn);
        if let Some(old) = old {
            old.close.send_replace(Some(1008));
        }
    }

    /// Only if `conn_id` still matches (a replaced socket never evicts its successor).
    pub fn unregister(&self, id: DeviceId, conn_id: u64) {
        let mut g = lock(&self.0);
        if g.conns.get(&id).is_some_and(|c| c.id == conn_id) {
            g.conns.remove(&id);
        }
    }

    /// The Hub's current role for `id` — the single source of truth for roles.
    pub fn role(&self, id: DeviceId) -> Option<Role> {
        lock(&self.0).conns.get(&id).map(|c| c.role)
    }

    /// `try_send`; `Full` ⇒ close(1013) + remove ⇒ `Gone` (design §6.2).
    pub fn send(&self, id: DeviceId, msg: FromRelay) -> Sent {
        let mut g = lock(&self.0);
        let Some(conn) = g.conns.get(&id) else {
            return Sent::Gone;
        };
        match conn.tx.try_send(msg) {
            Ok(()) => Sent::Ok,
            Err(mpsc::error::TrySendError::Full(_)) => {
                conn.close.send_replace(Some(1013));
                g.conns.remove(&id);
                Sent::Gone
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                g.conns.remove(&id);
                Sent::Gone
            }
        }
    }

    /// Visit every live conn of one account (ChainChanged, promotion, revoke).
    pub fn for_account(&self, acct: AccountId, mut f: impl FnMut(DeviceId, &mut Conn)) {
        let mut g = lock(&self.0);
        for (id, conn) in g.conns.iter_mut().filter(|(_, c)| c.account == acct) {
            f(*id, conn);
        }
    }

    /// Close every registered socket opened with this session (design §6.1).
    pub fn close_session(&self, hash: [u8; 32], code: u16) {
        let mut g = lock(&self.0);
        g.conns.retain(|_, c| {
            let hit = crate::ct_eq(&c.session, &hash);
            if hit {
                c.close.send_replace(Some(code));
            }
            !hit
        });
    }

    /// Close a registered Pending socket whose pending row expired (§4.7).
    pub fn close_pending(&self, id: DeviceId, code: u16) {
        let mut g = lock(&self.0);
        if g.conns.get(&id).is_some_and(|c| c.role == Role::Pending)
            && let Some(c) = g.conns.remove(&id)
        {
            c.close.send_replace(Some(code));
        }
    }
}

/// Why `Hub::permit` refused (design §6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermitDenied {
    /// Process-wide `MAX_SOCKETS` reached → HTTP 503.
    Global,
    /// `MAX_SOCKETS_PER_ACCOUNT` reached → HTTP 429.
    Account,
}

/// RAII socket permit (design §6.5): drops decrement the per-account counter.
#[must_use = "dropping the permit releases the socket slot"]
pub struct SocketPermit {
    hub: Arc<Mutex<HubInner>>,
    acct: AccountId,
}

impl Drop for SocketPermit {
    fn drop(&mut self) {
        let mut g = lock(&self.hub);
        g.total = g.total.saturating_sub(1);
        if let Some(n) = g.per_account.get_mut(&self.acct) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                g.per_account.remove(&self.acct);
            }
        }
    }
}

/// Outcome of `Hub::send` (design §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    Ok,
    Gone,
}
