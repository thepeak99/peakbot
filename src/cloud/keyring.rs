//! Host secret stores (p3-design §C): OS keyring first, 0600 file
//! fallback when it is unavailable (headless servers, locked keychains).
//!
//! The choice is made ONCE at boot via [`probe`] so the two stores never
//! split-brain. If the keyring later errors, that is a CloudLink error
//! state — no silent per-call fallback.
//!
//! Host-only (the Local edition gets a P5 phone-keystore impl).

use std::io;
use std::path::{Path, PathBuf};

use super::store::{Seed, SeedStore, SessionToken};

const SERVICE: &str = "peakbot";

/// `<dir>/secrets.json` — `{"seed_b64": …, "token": …}`. Created
/// `O_CREAT|O_EXCL` mode 0600, written atomically (tmp + rename, 0600).
/// On load: refuse if the mode is looser than 0600, with the fix in the
/// error (`chmod 600 <path>`).
pub struct FileSeedStore {
    pub dir: PathBuf,
}

impl FileSeedStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `<dir>/secrets.json`.
    pub fn path(&self) -> PathBuf {
        self.dir.join("secrets.json")
    }
}

impl SeedStore for FileSeedStore {
    fn load_seed(&self) -> io::Result<Option<Seed>> {
        todo!("p3 PR-B: FileSeedStore::load_seed (0600 check, b64 decode)")
    }

    fn store_seed(&self, seed: &Seed) -> io::Result<()> {
        todo!("p3 PR-B: FileSeedStore::store_seed (atomic 0600 write)")
    }

    fn load_token(&self) -> io::Result<Option<SessionToken>> {
        todo!("p3 PR-B: FileSeedStore::load_token")
    }

    fn store_token(&self, token: &SessionToken) -> io::Result<()> {
        todo!("p3 PR-B: FileSeedStore::store_token")
    }

    fn wipe(&self) -> io::Result<()> {
        todo!("p3 PR-B: FileSeedStore::wipe (idempotent)")
    }
}

/// The OS-keyring store: service `"peakbot"`, accounts
/// `"cloud-seed:<relay origin>"` / `"cloud-token:<relay origin>"` (keying
/// by origin so a relay change never reuses a token for the wrong relay).
/// The seed is stored b64 (keyring stores strings portably).
///
/// SKELETON: no `keyring` crate import yet — the green phase adds the
/// crate (its `sync-secret-service` backend needs the system
/// `libdbus-1-dev`) and fills these bodies.
pub struct KeyringSeedStore {
    pub origin: String,
}

impl SeedStore for KeyringSeedStore {
    fn load_seed(&self) -> io::Result<Option<Seed>> {
        todo!("p3 PR-B green: KeyringSeedStore::load_seed (keyring crate)")
    }

    fn store_seed(&self, seed: &Seed) -> io::Result<()> {
        todo!("p3 PR-B green: KeyringSeedStore::store_seed (keyring crate)")
    }

    fn load_token(&self) -> io::Result<Option<SessionToken>> {
        todo!("p3 PR-B green: KeyringSeedStore::load_token (keyring crate)")
    }

    fn store_token(&self, token: &SessionToken) -> io::Result<()> {
        todo!("p3 PR-B green: KeyringSeedStore::store_token (keyring crate)")
    }

    fn wipe(&self) -> io::Result<()> {
        todo!("p3 PR-B green: KeyringSeedStore::wipe (keyring crate)")
    }
}

/// The boot-time probe result (p3-design §C: "the choice is made once at
/// boot, not per call").
pub enum Probed {
    Keyring(KeyringSeedStore),
    File(FileSeedStore),
}

/// The boot-time choice (p3-design §C). `keyring_available` is the OS
/// probe (green phase); tests inject `false` to force the file fallback —
/// the real OS keyring is never touched in tests. `File` is the headless
/// fallback with the one boot warning
/// `⚠ no OS keyring (Secret Service unavailable); cloud identity stored in <path> (0600)`.
pub fn probe(keyring_available: bool, origin: &str, dir: &Path) -> Probed {
    if keyring_available {
        Probed::Keyring(KeyringSeedStore {
            origin: origin.to_string(),
        })
    } else {
        Probed::File(FileSeedStore::new(dir))
    }
}
