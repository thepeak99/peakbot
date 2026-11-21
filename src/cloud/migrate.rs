//! Keystore migration (P5 design §2, C4): wraps `{keystore, legacy
//! FileSeedStore}` and moves a legacy 0600-file seed into the OS keystore
//! once, verify-then-delete. A keystore error is a hard error — never
//! "no seed" (a fresh genesis would orphan the account).
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
    /// Run the one-shot migration (design §2 table). Never logs secrets.
    pub fn open(
        primary: Arc<dyn SeedStore>,
        legacy: FileSeedStore,
    ) -> Result<Self, MigrationError> {
        // A keystore error is NEVER "empty": checked first, file untouched.
        let ks_seed = primary.load_seed().map_err(MigrationError::Keystore)?;
        let file_seed = legacy.load_seed().map_err(MigrationError::Legacy)?;
        match (ks_seed, file_seed) {
            (_, None) => {}
            (Some(k), Some(f)) => {
                if k.as_bytes() != f.as_bytes() {
                    return Err(MigrationError::Conflict);
                }
                legacy.wipe().map_err(MigrationError::Legacy)?;
            }
            (None, Some(f)) => {
                let token = legacy.load_token().map_err(MigrationError::Legacy)?;
                primary.store_seed(&f).map_err(MigrationError::Keystore)?;
                if let Some(t) = &token {
                    primary.store_token(t).map_err(MigrationError::Keystore)?;
                }
                let back = primary.load_seed().map_err(MigrationError::Keystore)?;
                if back.as_ref().map(|s| s.as_bytes()) != Some(f.as_bytes()) {
                    return Err(MigrationError::VerifyMismatch);
                }
                if let Some(t) = &token {
                    let back = primary.load_token().map_err(MigrationError::Keystore)?;
                    if back.as_ref().map(|b| b.as_str()) != Some(t.as_str()) {
                        return Err(MigrationError::VerifyMismatch);
                    }
                }
                legacy.wipe().map_err(MigrationError::Legacy)?;
            }
        }
        Ok(Self { primary })
    }
}

impl SeedStore for MigratingSeedStore {
    fn load_seed(&self) -> io::Result<Option<Seed>> {
        self.primary.load_seed()
    }
    fn store_seed(&self, seed: &Seed) -> io::Result<()> {
        self.primary.store_seed(seed)
    }
    fn load_token(&self) -> io::Result<Option<SessionToken>> {
        self.primary.load_token()
    }
    fn store_token(&self, token: &SessionToken) -> io::Result<()> {
        self.primary.store_token(token)
    }
    fn clear_token(&self) -> io::Result<()> {
        self.primary.clear_token()
    }
    fn wipe(&self) -> io::Result<()> {
        self.primary.wipe()
    }
}
