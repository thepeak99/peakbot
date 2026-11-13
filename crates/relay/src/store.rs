//! The `Store`: one SQLite connection, one lock, the `VerifiedChain` cache
//! (design §3.2–§3.4, §5.5). Every method is sync, takes `now: u64`, finishes
//! its own transaction, and is called only via `Db::run` (spawn_blocking).
//!
//! Pass A: open/migrations, accounts, sessions, sweep, reset-account.
//! Pass B: chain cache, fetch, hello_pending, append, post, ack, letters_for.

use cloud::card::DeviceCard;
use cloud::chain::{ChainEntry, ChainOp, VerifiedChain, extend};
use cloud::codec;
use cloud::constants::{MAX_CHAIN_BATCH, MAX_PENDING_DEVICES, MAX_PENDING_LETTERS, TTL_LETTER};
use cloud::{DeviceId, LetterId};
use rusqlite::OptionalExtension as _;
use std::collections::HashMap;
use std::path::Path;

use crate::limits::SESSION_TTL;

/// Account row id (design §3.3).
pub type AccountId = i64;

/// The single source of truth (design §3.3). Lives behind ONE `std::sync::Mutex`.
#[non_exhaustive]
pub struct Store {
    conn: rusqlite::Connection,
    // The chain cache (design §3.3); loaded lazily and swapped on append.
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
    /// A cancelled task (runtime teardown) is an error, not a panic.
    pub async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Store) -> T + Send + 'static,
    ) -> Result<T, DbError> {
        let db = self.0.clone();
        // A panic in SQL code is a bug: propagate it (design §3.4).
        match tokio::task::spawn_blocking(move || {
            f(&mut db.lock().unwrap_or_else(|p| p.into_inner()))
        })
        .await
        {
            Ok(v) => Ok(v),
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(_) => Err(DbError::Cancelled),
        }
    }
}

/// The store task was cancelled (runtime shutdown); not a SQL failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbError {
    Cancelled,
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
    /// Session gone at Hello time (design §4.3).
    Unauthorized,
    /// DB failure or a corrupt stored chain: fail closed (design §6.3).
    Internal,
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
    if let rusqlite::Error::SqliteFailure(info, _) = &e
        && matches!(
            info.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        )
    {
        return OpenError::Locked;
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
    /// `None` = empty account (not cached). A stored chain that fails
    /// `extend` is `Corrupt`: fail closed, no repair (design §6.3).
    pub fn chain(&mut self, acct: AccountId) -> Result<Option<&VerifiedChain>, Reject> {
        if !self.chains.contains_key(&acct) {
            match read_chain(&self.conn, acct)? {
                Some(c) => {
                    self.chains.insert(acct, c);
                }
                None => return Ok(None),
            }
        }
        Ok(self.chains.get(&acct))
    }

    /// Routing check: `id` is an active device of `acct` (design §4.5).
    pub fn is_active(&mut self, acct: AccountId, id: DeviceId) -> Result<bool, Reject> {
        Ok(self
            .chain(acct)?
            .is_some_and(|c| c.active.contains_key(&id)))
    }

    /// ≤ `MAX_CHAIN_BATCH` rows with `seq >= from_seq`; a short batch means
    /// "caught up" (design §4.2).
    pub fn fetch(&self, acct: AccountId, from_seq: u64) -> Result<Vec<Vec<u8>>, Reject> {
        read_rows(
            &self.conn,
            acct,
            i64::try_from(from_seq).unwrap_or(i64::MAX),
        )
    }

    /// Both Hellos in one place (design §4.3): session re-check, signature
    /// over the connection's nonce, then chain membership / pending row.
    pub fn hello(
        &mut self,
        acct: AccountId,
        session: [u8; 32],
        req: HelloReq,
        origin: &str,
        nonce: [u8; 32],
        now: u64,
    ) -> Result<HelloOk, Reject> {
        if !self.session_live(session, now) {
            return Err(Reject::Unauthorized);
        }
        match req {
            HelloReq::Active { device, sig } => {
                let chain = self.chain(acct)?.ok_or(Reject::BadHello)?;
                let card = chain.active.get(&device).ok_or(Reject::BadHello)?;
                let pk = card.public_key().map_err(|_| Reject::BadHello)?;
                codec::verify_hello(&pk, &codec::hello_tbs(origin, nonce, device, 0), &sig)
                    .map_err(|_| Reject::BadHello)?;
                Ok(HelloOk {
                    id: device,
                    head_seq: Some(chain.head.seq),
                    role: Role::Active,
                    card: None,
                })
            }
            HelloReq::Pending { card, sig } => {
                let card = DeviceCard::parse(&card).map_err(|_| Reject::BadHello)?;
                let id = card.device_id();
                let pk = card.public_key().map_err(|_| Reject::BadHello)?;
                codec::verify_hello(&pk, &codec::hello_tbs(origin, nonce, id, 1), &sig)
                    .map_err(|_| Reject::BadHello)?;
                self.hello_pending(acct, &card, now)?;
                Ok(HelloOk {
                    id,
                    head_seq: self.chain(acct)?.map(|c| c.head.seq),
                    role: Role::Pending,
                    card: Some(card.encode()),
                })
            }
        }
    }

    /// `HelloPending` registration with the §4.3 P2-edit refresh rule.
    /// Returns the (absolute) expiry of the pending row.
    pub fn hello_pending(
        &mut self,
        acct: AccountId,
        card: &DeviceCard,
        now: u64,
    ) -> Result<u64, Reject> {
        let id = card.device_id();
        if self
            .chain(acct)?
            .is_some_and(|c| c.active.contains_key(&id) || c.revoked.contains(&id))
        {
            return Err(Reject::BadHello);
        }
        let row: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT account_id, expires_at FROM pending_devices WHERE device_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        // Same live row: keep it (and its mailbox); the 15-min window is absolute.
        if let Some((a, exp)) = row
            && a == acct
            && exp as u64 > now
        {
            return Ok(exp as u64);
        }
        let others: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM pending_devices
             WHERE account_id = ?1 AND device_id != ?2 AND expires_at > ?3",
            rusqlite::params![acct, id, now as i64],
            |r| r.get(0),
        )?;
        if others as usize >= MAX_PENDING_DEVICES {
            return Err(Reject::TooManyPending);
        }
        let expires = now + TTL_LETTER;
        let tx = self.conn.transaction()?;
        // Never INSERT OR REPLACE: its implicit delete would cascade the mailbox.
        tx.execute("DELETE FROM pending_devices WHERE device_id = ?1", [id])?;
        tx.execute(
            "INSERT INTO pending_devices (device_id, account_id, card, expires_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, acct, card.encode(), expires as i64],
        )?;
        tx.commit()?;
        Ok(expires)
    }

    /// `ChainAppend` (`genesis_card = None`) / `EnrollGenesis` (the socket's
    /// HelloPending card) in one `BEGIN IMMEDIATE` (design §4.4, §6.3).
    pub fn append(
        &mut self,
        acct: AccountId,
        entry: &[u8],
        genesis_card: Option<&[u8]>,
        now: u64,
    ) -> Result<Appended, Reject> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let cur = match self.chains.get(&acct) {
            Some(c) => Some(c.clone()),
            None => read_chain(&tx, acct)?,
        };
        let raw = [entry.to_vec()];
        let (parsed, next) = match genesis_card {
            Some(gc) => {
                if cur.is_some() {
                    return Err(Reject::Conflict);
                }
                let e = ChainEntry::parse(entry).map_err(|_| Reject::InvalidEntry)?;
                if e.card.as_deref() != Some(gc) {
                    return Err(Reject::NotPermitted);
                }
                let next = extend(None, &raw, None).map_err(|_| Reject::InvalidEntry)?;
                (e, next)
            }
            None => {
                let cur = cur.ok_or(Reject::Conflict)?;
                let e = ChainEntry::parse(entry).map_err(|_| Reject::InvalidEntry)?;
                // Compare-and-set on the head seq; a fork at the right seq fails in `extend`.
                if cur.head.seq.checked_add(1) != Some(e.seq) {
                    return Err(Reject::Conflict);
                }
                let genesis = cur.genesis;
                let next =
                    extend(Some(cur), &raw, Some(genesis)).map_err(|_| Reject::InvalidEntry)?;
                (e, next)
            }
        };
        let (mut added, mut revoked) = (None, None);
        match (parsed.op, genesis_card.is_some()) {
            (ChainOp::Genesis, true) => {
                let id = card_id(parsed.card.as_deref())?;
                tx.execute("DELETE FROM pending_devices WHERE device_id = ?1", [id])?;
                added = Some(id);
            }
            (ChainOp::Add, false) => {
                let id = card_id(parsed.card.as_deref())?;
                let live: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM pending_devices
                     WHERE device_id = ?1 AND account_id = ?2 AND expires_at > ?3",
                    rusqlite::params![id, acct, now as i64],
                    |r| r.get(0),
                )?;
                if live == 0 {
                    return Err(Reject::NotPermitted);
                }
                tx.execute("DELETE FROM pending_devices WHERE device_id = ?1", [id])?;
                added = Some(id);
            }
            (ChainOp::Revoke, false) => {
                let t = parsed.target.ok_or(Reject::InvalidEntry)?;
                tx.execute("DELETE FROM letters WHERE active_id = ?1", [t])?;
                revoked = Some(t);
            }
            _ => return Err(Reject::InvalidEntry),
        }
        // The PK (account_id, seq) is the last compare-and-set guard.
        tx.execute(
            "INSERT INTO chain_entries (account_id, seq, bytes) VALUES (?1, ?2, ?3)",
            rusqlite::params![acct, parsed.seq as i64, entry],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(i, _)
                if i.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Reject::Conflict
            }
            other => other.into(),
        })?;
        tx.commit()?;
        let head_seq = next.head.seq;
        self.chains.insert(acct, next);
        Ok(Appended {
            head_seq,
            revoked,
            added,
        })
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
        let (me, role) = from;
        let chain = self.chain(acct)?;
        let in_chain = |d: &DeviceId| chain.is_some_and(|c| c.active.contains_key(d));
        // Pending → active device, or active → a live pending device of the account.
        let (pending_id, active_id, to_pending) = match role {
            Role::Pending if in_chain(&to) => (me, to, 0),
            Role::Active if in_chain(&me) => (to, me, 1),
            _ => return Err(Reject::NotPermitted),
        };
        let live: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM pending_devices
             WHERE device_id = ?1 AND account_id = ?2 AND expires_at > ?3",
            rusqlite::params![pending_id, acct, now as i64],
            |r| r.get(0),
        )?;
        if live == 0 {
            return Err(Reject::NotPermitted);
        }
        let recipient = if to_pending == 1 {
            pending_id
        } else {
            active_id
        };
        let held: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM letters l
             JOIN pending_devices p ON p.device_id = l.pending_id
             WHERE p.expires_at > ?2
               AND ((l.to_pending = 1 AND l.pending_id = ?1) OR (l.to_pending = 0 AND l.active_id = ?1))",
            rusqlite::params![recipient, now as i64],
            |r| r.get(0),
        )?;
        if held as usize >= MAX_PENDING_LETTERS {
            return Err(Reject::MailboxFull);
        }
        self.conn
            .execute(
                "INSERT INTO letters (id, pending_id, active_id, to_pending, sealed)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, pending_id, active_id, to_pending, sealed],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(i, _)
                    if i.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    Reject::NotPermitted
                }
                other => other.into(),
            })?;
        Ok(())
    }

    /// Recipient-only, idempotent, silent (design §4.6).
    pub fn ack(&mut self, me: DeviceId, id: LetterId) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM letters WHERE id = ?1
             AND ((to_pending = 1 AND pending_id = ?2) OR (to_pending = 0 AND active_id = ?2))",
            rusqlite::params![id, me],
        )?;
        Ok(())
    }

    /// Un-acked letters for `me`, oldest first; expired pending ⇒ none (design §6.4).
    pub fn letters_for(
        &self,
        me: DeviceId,
        now: u64,
    ) -> Result<Vec<(DeviceId, LetterId, Vec<u8>)>, Reject> {
        let mut stmt = self.conn.prepare(
            "SELECT l.id, l.to_pending, l.pending_id, l.active_id, l.sealed FROM letters l
             JOIN pending_devices p ON p.device_id = l.pending_id
             WHERE p.expires_at > ?2
               AND ((l.to_pending = 1 AND l.pending_id = ?1) OR (l.to_pending = 0 AND l.active_id = ?1))
             ORDER BY l.rowid",
        )?;
        let rows = stmt.query_map(rusqlite::params![me, now as i64], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get::<_, Vec<u8>>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, to_pending, pending, active, sealed) = row?;
            let from = if to_pending == 1 { active } else { pending };
            if let (Ok(id), Ok(from)) = (LetterId::try_from(id), DeviceId::try_from(from)) {
                out.push((from, id, sealed));
            }
        }
        Ok(out)
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

/// A device's Hello as received (design §4.3).
pub enum HelloReq {
    Active { device: DeviceId, sig: Vec<u8> },
    Pending { card: Vec<u8>, sig: Vec<u8> },
}

/// A verified Hello: who the socket is, its role, and the chain head to
/// greet with (the HelloPending card is kept for the `EnrollGenesis` check).
pub struct HelloOk {
    pub id: DeviceId,
    pub head_seq: Option<u64>,
    pub role: Role,
    pub card: Option<Vec<u8>>,
}

impl From<rusqlite::Error> for Reject {
    fn from(_: rusqlite::Error) -> Self {
        Reject::Internal
    }
}

/// Device id of a CE card carried by a chain entry.
fn card_id(card: Option<&[u8]>) -> Result<DeviceId, Reject> {
    let raw = card.ok_or(Reject::InvalidEntry)?;
    Ok(DeviceCard::parse(raw)
        .map_err(|_| Reject::InvalidEntry)?
        .device_id())
}

/// ≤ `MAX_CHAIN_BATCH` stored entries with `seq >= from_seq`, in order.
fn read_rows(
    conn: &rusqlite::Connection,
    acct: AccountId,
    from: i64,
) -> Result<Vec<Vec<u8>>, Reject> {
    let mut stmt = conn.prepare_cached(
        "SELECT bytes FROM chain_entries WHERE account_id = ?1 AND seq >= ?2
         ORDER BY seq LIMIT ?3",
    )?;
    let rows = stmt.query_map(rusqlite::params![acct, from, MAX_CHAIN_BATCH as i64], |r| {
        r.get::<_, Vec<u8>>(0)
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Re-verify the whole stored chain (the cache is equivalent to this, §6.2).
/// Rows that fail `extend` mean disk corruption: `Internal`, fail closed.
fn read_chain(
    conn: &rusqlite::Connection,
    acct: AccountId,
) -> Result<Option<VerifiedChain>, Reject> {
    let mut all: Vec<Vec<u8>> = Vec::new();
    loop {
        let batch = read_rows(conn, acct, all.len() as i64)?;
        let n = batch.len();
        all.extend(batch);
        if n < MAX_CHAIN_BATCH {
            break;
        }
    }
    if all.is_empty() {
        return Ok(None);
    }
    extend(None, &all, None)
        .map(Some)
        .map_err(|_| Reject::Internal)
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
