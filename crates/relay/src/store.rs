//! The `Store`: one SQLite connection, one lock, the `VerifiedChain` cache
//! (design §3.2–§3.4, §5.5). Every method is sync, takes `now: u64`, finishes
//! its own transaction, and is called only via `Db::run` (spawn_blocking).
//!
//! Pass A: open/migrations, accounts, sessions, sweep, reset-account.
//! Pass B: chain cache, fetch, hello_pending, append, post, ack, letters_for.

use cloud::chain::VerifiedChain;
use cloud::{DeviceId, LetterId};
use std::collections::HashMap;
use std::path::Path;

use crate::limits::SESSION_TTL;

/// Account row id (design §3.3).
pub type AccountId = i64;

/// The single source of truth (design §3.3). Lives behind ONE `std::sync::Mutex`.
#[non_exhaustive]
pub struct Store {
    conn: rusqlite::Connection,
    // The pass-B chain cache (design §3.3); loaded lazily and swapped on append.
    #[allow(dead_code)] // pass B
    chains: HashMap<AccountId, VerifiedChain>,
}

/// The process-wide handle to the store: one connection, one lock (design §3.4).
#[derive(Clone)]
pub struct Db(std::sync::Arc<std::sync::Mutex<Store>>);

impl Db {
    /// Wrap an opened store in the one lock (design §3.4).
    pub fn new(store: Store) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(store)))
    }

    /// Drop this handle and wait (bounded) for every other clone to go, so the
    /// exclusive DB lock is released when it returns (restart/reset-account).
    pub async fn release(self, timeout: std::time::Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        while std::sync::Arc::strong_count(&self.0) > 1 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// Run `f` under the single store lock, off the async runtime.
    pub async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Store) -> T + Send + 'static,
    ) -> T {
        let db = self.0.clone();
        // A panic in SQL code is a bug: crash loudly (design §3.4).
        match tokio::task::spawn_blocking(move || {
            f(&mut db.lock().unwrap_or_else(|p| p.into_inner()))
        })
        .await
        {
            Ok(v) => v,
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(_) => panic!("store task cancelled"),
        }
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

/// Schema version the binary knows (design §3.2: migration 1).
const SCHEMA_VERSION: i64 = 1;

const MIGRATION_1: &str = r#"
CREATE TABLE schema_version (version INTEGER NOT NULL);
INSERT INTO schema_version VALUES (0);

CREATE TABLE accounts (
  id       INTEGER PRIMARY KEY,
  issuer   TEXT NOT NULL,
  subject  TEXT NOT NULL,
  UNIQUE (issuer, subject));

CREATE TABLE sessions (
  token_hash BLOB PRIMARY KEY CHECK (length(token_hash) = 32),
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  expires_at INTEGER NOT NULL) WITHOUT ROWID;
CREATE INDEX sessions_by_account ON sessions(account_id);

CREATE TABLE chain_entries (
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  seq        INTEGER NOT NULL CHECK (seq >= 0),
  bytes      BLOB NOT NULL,
  PRIMARY KEY (account_id, seq)) WITHOUT ROWID;

CREATE TABLE pending_devices (
  device_id  BLOB PRIMARY KEY CHECK (length(device_id) = 32),
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  card       BLOB NOT NULL,
  expires_at INTEGER NOT NULL);
CREATE INDEX pending_by_account ON pending_devices(account_id);

CREATE TABLE letters (
  id         BLOB PRIMARY KEY CHECK (length(id) = 16),
  pending_id BLOB NOT NULL REFERENCES pending_devices(device_id) ON DELETE CASCADE,
  active_id  BLOB NOT NULL CHECK (length(active_id) = 32),
  to_pending INTEGER NOT NULL CHECK (to_pending IN (0,1)),
  sealed     BLOB NOT NULL CHECK (length(sealed) <= 8192));
CREATE INDEX letters_by_active ON letters(active_id);

UPDATE schema_version SET version = 1;
"#;

/// Map a rusqlite failure: `SQLITE_BUSY` under the exclusive lock is the
/// "another relay owns the DB" condition (design §1).
fn map_open_err(e: rusqlite::Error) -> OpenError {
    if let rusqlite::Error::SqliteFailure(info, _) = &e {
        if info.code == rusqlite::ErrorCode::DatabaseBusy
            || info.code == rusqlite::ErrorCode::DatabaseLocked
        {
            return OpenError::Locked;
        }
    }
    OpenError::Db(e.to_string())
}

impl Store {
    /// Open the DB: pragmas (WAL, `locking_mode=EXCLUSIVE`), migrations
    /// (design §3.2). Fails with `Locked` when another relay owns the file.
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        let mut conn = rusqlite::Connection::open(path).map_err(map_open_err)?;
        // Fail fast: a held exclusive lock means another relay is running.
        conn.busy_timeout(std::time::Duration::ZERO)
            .map_err(map_open_err)?;
        // The exclusive lock makes "one process per DB" true by construction.
        conn.pragma_update(None, "locking_mode", "EXCLUSIVE")
            .map_err(map_open_err)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(map_open_err)?;
        conn.pragma_update(None, "secure_delete", "ON")
            .map_err(map_open_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(map_open_err)?;
        let has_table: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .map_err(map_open_err)?;
        let version: i64 = if has_table == 0 {
            0
        } else {
            conn.query_row("SELECT version FROM schema_version", [], |r| r.get(0))
                .map_err(map_open_err)?
        };
        if version > SCHEMA_VERSION {
            return Err(OpenError::Migrate(format!(
                "db schema {version} is newer than the binary ({SCHEMA_VERSION})"
            )));
        }
        if version < SCHEMA_VERSION {
            let tx = conn
                .transaction()
                .map_err(|e| OpenError::Migrate(e.to_string()))?;
            tx.execute_batch(MIGRATION_1)
                .and_then(|_| tx.commit())
                .map_err(|e| OpenError::Migrate(e.to_string()))?;
        }
        Ok(Self {
            conn,
            chains: HashMap::new(),
        })
    }

    /// SQL failures are returned, never panicked on (no unwrap/expect rule).
    pub fn upsert_account(&mut self, issuer: &str, subject: &str) -> rusqlite::Result<AccountId> {
        self.conn.execute(
            "INSERT INTO accounts (issuer, subject) VALUES (?1, ?2)
             ON CONFLICT (issuer, subject) DO NOTHING",
            rusqlite::params![issuer, subject],
        )?;
        self.conn.query_row(
            "SELECT id FROM accounts WHERE issuer = ?1 AND subject = ?2",
            rusqlite::params![issuer, subject],
            |r| r.get(0),
        )
    }

    pub fn create_session(
        &mut self,
        acct: AccountId,
        token_hash: [u8; 32],
        now: u64,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (token_hash, account_id, expires_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![token_hash, acct, (now + SESSION_TTL) as i64],
        )?;
        Ok(())
    }

    /// Look up a session; slides the expiry (design §5.5, §6.1). A DB error
    /// reads as "no session": fail closed.
    pub fn session(&mut self, token_hash: [u8; 32], now: u64) -> Option<AccountId> {
        let acct: i64 = self
            .conn
            .query_row(
                "SELECT account_id FROM sessions WHERE token_hash = ?1 AND expires_at > ?2",
                rusqlite::params![token_hash, now as i64],
                |r| r.get(0),
            )
            .ok()?;
        self.conn
            .execute(
                "UPDATE sessions SET expires_at = ?1 WHERE token_hash = ?2",
                rusqlite::params![(now + SESSION_TTL) as i64, token_hash],
            )
            .ok()?;
        Some(acct)
    }

    /// Hello re-check inside the Hello transaction; does not slide (design §6.1).
    pub fn session_live(&self, token_hash: [u8; 32], now: u64) -> bool {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE token_hash = ?1 AND expires_at > ?2",
                rusqlite::params![token_hash, now as i64],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    /// `/logout`; idempotent (design §6.1).
    pub fn revoke_session(&mut self, token_hash: [u8; 32]) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM sessions WHERE token_hash = ?1",
            rusqlite::params![token_hash],
        )?;
        Ok(())
    }

    /// Lazy load + cache of the account's verified chain (design §3.3, §6.2).
    pub fn chain(&mut self, acct: AccountId) -> Option<&VerifiedChain> {
        let _ = acct;
        todo!("P2 pass B: lazy load via cloud::chain::extend + cache")
    }

    /// ≤ `cloud::constants::MAX_CHAIN_BATCH` rows with `seq >= from_seq`.
    pub fn fetch(&self, acct: AccountId, from_seq: u64) -> Vec<Vec<u8>> {
        let _ = (acct, from_seq);
        todo!("P2 pass B")
    }

    /// `HelloPending` registration with the §4.3 P2-edit refresh rule.
    pub fn hello_pending(
        &mut self,
        acct: AccountId,
        card: &cloud::card::DeviceCard,
        now: u64,
    ) -> Result<u64, Reject> {
        let _ = (acct, card, now);
        todo!("P2 pass B: R2 membership check + MAX_PENDING_DEVICES cap")
    }

    /// `ChainAppend` / `EnrollGenesis` in one `BEGIN IMMEDIATE` (design §4.4, §6.3).
    pub fn append(
        &mut self,
        acct: AccountId,
        entry: &[u8],
        genesis_card: Option<&[u8]>,
        now: u64,
    ) -> Result<Appended, Reject> {
        let _ = (acct, entry, genesis_card, now);
        todo!("P2 pass B: extend() + compare-and-set + cache swap")
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
        let _ = (acct, from, to, id, sealed, now);
        todo!("P2 pass B")
    }

    /// Recipient-only, idempotent, silent (design §4.6).
    pub fn ack(&mut self, me: DeviceId, id: LetterId) {
        let _ = (me, id);
        todo!("P2 pass B")
    }

    /// Un-acked letters for `me`, oldest first; expired pending ⇒ none (design §6.4).
    pub fn letters_for(&self, me: DeviceId, now: u64) -> Vec<(DeviceId, LetterId, Vec<u8>)> {
        let _ = (me, now);
        todo!("P2 pass B")
    }

    /// Sweeper transaction (design §4.7): expired pending + sessions. Returns
    /// the expired pending device ids.
    pub fn sweep(&mut self, now: u64) -> rusqlite::Result<Vec<DeviceId>> {
        let tx = self.conn.transaction()?;
        let mut expired: Vec<DeviceId> = Vec::new();
        {
            let mut stmt =
                tx.prepare("SELECT device_id FROM pending_devices WHERE expires_at <= ?1")?;
            let rows = stmt.query_map([now as i64], |r| r.get::<_, Vec<u8>>(0))?;
            for row in rows {
                if let Ok(d) = DeviceId::try_from(row?.as_slice()) {
                    expired.push(d);
                }
            }
        }
        tx.execute(
            "DELETE FROM pending_devices WHERE expires_at <= ?1",
            [now as i64],
        )?;
        tx.execute("DELETE FROM sessions WHERE expires_at <= ?1", [now as i64])?;
        tx.commit()?;
        Ok(expired)
    }
}

/// Socket role inside the Hub (design §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Active,
    Pending,
}

/// `peakbot-relay reset-account` (design §4.8, §5.4). The relay must be stopped.
/// The account row is kept; chain, pending devices, letters and sessions go.
pub fn reset_account(db: &Path, issuer: &str, subject: &str) -> Result<ResetReport, ResetError> {
    let db_err = |e: rusqlite::Error| ResetError::Db(e.to_string());
    let mut store = Store::open(db).map_err(|e| match e {
        OpenError::Locked => ResetError::Locked,
        other => ResetError::Db(format!("{other:?}")),
    })?;
    let id: Option<i64> = store
        .conn
        .query_row(
            "SELECT id FROM accounts WHERE issuer = ?1 AND subject = ?2",
            rusqlite::params![issuer, subject],
            |r| r.get(0),
        )
        .ok();
    let Some(id) = id else {
        return Err(ResetError::NoSuchAccount);
    };
    let count = |sql: &str| -> u64 {
        store
            .conn
            .query_row(sql, rusqlite::params![id], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as u64
    };
    let report = ResetReport {
        chain_entries: count("SELECT COUNT(*) FROM chain_entries WHERE account_id = ?1"),
        pending_devices: count("SELECT COUNT(*) FROM pending_devices WHERE account_id = ?1"),
        letters: count(
            "SELECT COUNT(*) FROM letters l
             JOIN pending_devices p ON l.pending_id = p.device_id
             WHERE p.account_id = ?1",
        ),
        sessions: count("SELECT COUNT(*) FROM sessions WHERE account_id = ?1"),
    };
    let tx = store.conn.transaction().map_err(db_err)?;
    tx.execute("DELETE FROM chain_entries WHERE account_id = ?1", [id])
        .map_err(db_err)?;
    // The cascade removes the pending device's letters (design §3.2).
    tx.execute("DELETE FROM pending_devices WHERE account_id = ?1", [id])
        .map_err(db_err)?;
    tx.execute("DELETE FROM sessions WHERE account_id = ?1", [id])
        .map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    Ok(report)
}
