//! The `Store`: one SQLite connection, one lock, the `VerifiedChain` cache
//! (design §3.2–§3.4, §5.5). Every method is sync, takes `now: u64`, finishes
//! its own transaction, and is called only via `Db::run` (spawn_blocking).
//!
//! SKELETON: signatures are the locked interface; bodies are `todo!()`.

use cloud::chain::VerifiedChain;
use cloud::{DeviceId, LetterId};
use std::path::Path;

/// Account row id (design §3.3).
pub type AccountId = i64;

/// The single source of truth (design §3.3). Lives behind ONE `std::sync::Mutex`.
pub struct Store {
    _priv: (),
}

/// The process-wide handle to the store: one connection, one lock (design §3.4).
#[derive(Clone)]
pub struct Db(std::sync::Arc<std::sync::Mutex<Store>>);

impl Db {
    /// Run `f` under the single store lock, off the async runtime.
    pub async fn run<T: Send + 'static>(&self, f: impl FnOnce(&mut Store) -> T + Send + 'static) -> T {
        todo!("P2 task 3: spawn_blocking over the one Mutex<Store>")
    }
}

/// `Store::open` failure (design §5.5).
#[derive(Debug)]
pub enum OpenError {
    /// `SQLITE_BUSY` from the exclusive lock: another relay owns the DB.
    Locked,
    /// Migration failure, or a DB newer than the binary.
    Migrate(String),
    Db(String),
}

/// Store-level rejection, mapped to `cloud::constants::relay_error::*` (design §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    NotPermitted,
    Conflict,
    InvalidEntry,
    MailboxFull,
    TooManyPending,
    BadHello,
}

/// Result of a committed `append` (design §5.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Appended {
    pub head_seq: u64,
    pub revoked: Option<DeviceId>,
    pub added: Option<DeviceId>,
}

/// `reset-account` outcome (design §4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResetReport {
    pub chain_entries: u64,
    pub pending_devices: u64,
    pub letters: u64,
    pub sessions: u64,
}

/// `reset-account` failure (design §5.4): `Locked` → exit 2, the rest → exit 1.
#[derive(Debug)]
pub enum ResetError {
    /// The relay is running (exclusive DB lock held).
    Locked,
    NoSuchAccount,
    Db(String),
}

impl Store {
    /// Open the DB: pragmas (WAL, `locking_mode=EXCLUSIVE`), migrations (design §3.2).
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        todo!("P2 task 3: pragmas + EXCLUSIVE lock + migrations")
    }

    pub fn upsert_account(&mut self, issuer: &str, subject: &str) -> AccountId {
        todo!("P2 task 3")
    }

    pub fn create_session(&mut self, acct: AccountId, token_hash: [u8; 32], now: u64) {
        todo!("P2 task 3")
    }

    /// Look up a session; slides the expiry (design §5.5).
    pub fn session(&mut self, token_hash: [u8; 32], now: u64) -> Option<AccountId> {
        todo!("P2 task 3")
    }

    /// Hello re-check inside the Hello transaction; does not slide (design §6.1).
    pub fn session_live(&self, token_hash: [u8; 32], now: u64) -> bool {
        todo!("P2 task 3")
    }

    /// `/logout`; idempotent (design §6.1).
    pub fn revoke_session(&mut self, token_hash: [u8; 32]) {
        todo!("P2 task 3")
    }

    /// Lazy load + cache of the account's verified chain (design §3.3, §6.2).
    pub fn chain(&mut self, acct: AccountId) -> Option<&VerifiedChain> {
        todo!("P2 task 4")
    }

    /// ≤ `cloud::constants::MAX_CHAIN_BATCH` rows with `seq >= from_seq`.
    pub fn fetch(&self, acct: AccountId, from_seq: u64) -> Vec<Vec<u8>> {
        todo!("P2 task 4")
    }

    /// `HelloPending` registration with the §4.3 P2-edit refresh rule.
    pub fn hello_pending(
        &mut self,
        acct: AccountId,
        card: &cloud::card::DeviceCard,
        now: u64,
    ) -> Result<u64, Reject> {
        todo!("P2 task 4")
    }

    /// `ChainAppend` / `EnrollGenesis` in one `BEGIN IMMEDIATE` (design §4.4, §6.3).
    pub fn append(
        &mut self,
        acct: AccountId,
        entry: &[u8],
        genesis_card: Option<&[u8]>,
        now: u64,
    ) -> Result<Appended, Reject> {
        todo!("P2 task 4")
    }

    /// Letter insert with the mailbox caps (design §4.6, §6.4).
    pub fn post(
        &mut self,
        acct: AccountId,
        from: (DeviceId, Role),
        to: DeviceId,
        id: LetterId,
        sealed: &[u8],
        now: u64,
    ) -> Result<(), Reject> {
        todo!("P2 task 4")
    }

    /// Recipient-only, idempotent, silent (design §4.6).
    pub fn ack(&mut self, me: DeviceId, id: LetterId) {
        todo!("P2 task 4")
    }

    /// Un-acked letters for `me`, oldest first; expired pending ⇒ none (design §6.4).
    pub fn letters_for(&self, me: DeviceId, now: u64) -> Vec<(DeviceId, LetterId, Vec<u8>)> {
        todo!("P2 task 4")
    }

    /// Sweeper transaction (design §4.7): expired pending + sessions.
    pub fn sweep(&mut self, now: u64) -> Vec<DeviceId> {
        todo!("P2 task 3/4")
    }
}

/// Socket role inside the Hub (design §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Active,
    Pending,
}

/// `peakbot-relay reset-account` (design §4.8, §5.4). The relay must be stopped.
pub fn reset_account(db: &Path, issuer: &str, subject: &str) -> Result<ResetReport, ResetError> {
    todo!("P2 task 4: open (Locked → err) + one delete-all transaction")
}
