//! Keystore migration (P5 design §2, C4): wraps `{keystore, legacy
//! FileSeedStore}` and moves a legacy 0600-file seed into the OS keystore
//! once, verify-then-delete. A keystore error is a hard error — never
//! "no seed" (a fresh genesis would orphan the account).
//!
//! SKELETON (RED phase): signatures only, no behaviour.
use super::keyring::FileSeedStore;
use super::store::{Seed, SeedStore, SessionToken};
use std::io;
use std::sync::Arc;

/// Why `MigratingSeedStore::open` refused to boot.
#[derive(Debug)]
pub enum MigrationError {
    /// The keystore failed to read/write. Never treated as empty.
    Keystore(io::Error),
    /// The legacy file is unreadable/corrupt (kept untouched).
    Legacy(io::Error),
    /// Written to the keystore but the read-back differs (file kept).
    VerifyMismatch,
    /// Keystore and file hold DIFFERENT seeds (nothing deleted).
    Conflict,
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keystore(e) => write!(f, "keystore: {e}"),
            Self::Legacy(e) => write!(f, "legacy seed file: {e}"),
            Self::VerifyMismatch => f.write_str("keystore read-back differs"),
            Self::Conflict => f.write_str("keystore and legacy file hold different seeds"),
        }
    }
}

impl std::error::Error for MigrationError {}

/// After `open`, the legacy file is gone; every call goes to `primary`.
pub struct MigratingSeedStore {
    primary: Arc<dyn SeedStore>,
}

impl MigratingSeedStore {
    pub fn open(
        primary: Arc<dyn SeedStore>,
        legacy: FileSeedStore,
    ) -> Result<Self, MigrationError> {
        let _ = (&primary, &legacy);
        todo!("P5 T2: migration table (design §2)")
    }
}

impl SeedStore for MigratingSeedStore {
    fn load_seed(&self) -> io::Result<Option<Seed>> {
        let _ = &self.primary;
        todo!("P5 T2: delegate to primary")
    }
    fn store_seed(&self, _seed: &Seed) -> io::Result<()> {
        todo!("P5 T2: delegate to primary")
    }
    fn load_token(&self) -> io::Result<Option<SessionToken>> {
        todo!("P5 T2: delegate to primary")
    }
    fn store_token(&self, _token: &SessionToken) -> io::Result<()> {
        todo!("P5 T2: delegate to primary")
    }
    fn clear_token(&self) -> io::Result<()> {
        todo!("P5 T2: delegate to primary")
    }
    fn wipe(&self) -> io::Result<()> {
        todo!("P5 T2: delegate to primary")
    }
}
