//! The router: live sockets, `DeviceId → Conn` (design §6.2). No persistence,
//! no membership copy — routing asks the chain, then the Hub.
//!
//! SKELETON: signatures are the locked interface; bodies are `todo!()`.

use cloud::DeviceId;

use crate::store::Role;

/// One live socket (design §3.3). Closing goes through `close`, never `tx`,
/// so a full queue can still be closed (§6.2).
#[non_exhaustive]
pub struct Conn;

/// The router (design §6.2). The lock is never held across `.await`.
#[non_exhaustive]
pub struct Hub;

impl Hub {
    /// New.
    pub fn new() -> Self {
        todo!("P2 task 6")
    }

    /// Per-account socket permit at upgrade; `None` at the cap → 429.
    /// `Drop` decrements (RAII) (design §6.5).
    pub fn permit(&self, acct: crate::store::AccountId) -> Option<SocketPermit> {
        todo!("P2 task 6")
    }

    /// Register after a successful Hello; replaces + closes an older Conn for
    /// the id with 1008 (newest wins).
    pub fn register(&self, id: DeviceId, conn: Conn) {
        todo!("P2 task 6")
    }

    /// Only if `conn_id` still matches (a replaced socket never evicts its successor).
    pub fn unregister(&self, id: DeviceId, conn_id: u64) {
        todo!("P2 task 6")
    }

    /// The Hub's current role for `id` — the single source of truth for roles.
    pub fn role(&self, id: DeviceId) -> Option<Role> {
        todo!("P2 task 6")
    }

    /// `try_send`; `Full` ⇒ close(1013) + remove ⇒ `Gone` (design §6.2).
    pub fn send(&self, id: DeviceId, msg: cloud::codec::FromRelay) -> Sent {
        todo!("P2 task 6")
    }

    /// Visit every live conn of one account (ChainChanged, promotion, revoke).
    pub fn for_account(&self, acct: crate::store::AccountId, f: impl FnMut(DeviceId, &mut Conn)) {
        todo!("P2 task 6")
    }

    /// Close every registered socket opened with this session (design §6.1).
    pub fn close_session(&self, hash: [u8; 32], code: u16) {
        todo!("P2 task 6")
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII socket permit (design §6.5): drops decrement the per-account counter.
#[non_exhaustive]
pub struct SocketPermit;

/// Outcome of `Hub::send` (design §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    Ok,
    Gone,
}
