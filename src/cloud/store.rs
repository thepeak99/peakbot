//! Secrets and local state (p3-design §C). Two secrets (seed, session
//! token) go in secure storage; the two public artifacts (genesis pin,
//! cached chain) go in 0600 files under `<state_dir>/cloud/<origin-slug>/`.
//!
//! The store is synchronous and tiny — the calls are rare (boot, login,
//! logout) and the actor reaches it via `spawn_blocking`.

use std::io;
use std::sync::Mutex;

use zeroize::Zeroizing;

/// The 64-byte long-term seed. Device identity is derived ONLY from this
/// (cloud-crate keypair derivation); the keypair is never persisted.
/// No `Clone`, no `Display`; `Debug` is redacted.
pub struct Seed(Zeroizing<[u8; 64]>);

impl Seed {
    pub fn new(bytes: [u8; 64]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

impl std::fmt::Debug for Seed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Seed(***)")
    }
}

/// The relay session token (b64url). Same redaction pattern as
/// `RemoteToken`. No `Clone`.
pub struct SessionToken(Zeroizing<String>);

impl SessionToken {
    pub fn new(token: String) -> Self {
        Self(Zeroizing::from(token))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionToken(***)")
    }
}

/// Stores both cloud secrets. `wipe` removes BOTH entries and is
/// idempotent (missing = `Ok`). Implementations must never log secret
/// material (p3-design B9).
pub trait SeedStore: Send + Sync + 'static {
    fn load_seed(&self) -> io::Result<Option<Seed>>;
    fn store_seed(&self, seed: &Seed) -> io::Result<()>;
    fn load_token(&self) -> io::Result<Option<SessionToken>>;
    fn store_token(&self, token: &SessionToken) -> io::Result<()>;
    fn wipe(&self) -> io::Result<()>;
}

/// In-memory store for tests and the `mock` harness (p3-design I.1).
/// Fully implemented — it is the test double for the trait, not behaviour.
/// Copies through `as_bytes`/`as_str` because the secret types are
/// deliberately not `Clone` (p3-design §C).
pub struct MemSeedStore {
    seed: Mutex<Option<[u8; 64]>>,
    token: Mutex<Option<String>>,
}

impl MemSeedStore {
    pub fn new() -> Self {
        Self {
            seed: Mutex::new(None),
            token: Mutex::new(None),
        }
    }
}

impl Default for MemSeedStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SeedStore for MemSeedStore {
    fn load_seed(&self) -> io::Result<Option<Seed>> {
        Ok(self.seed.lock().expect("mem store lock").map(Seed::new))
    }

    fn store_seed(&self, seed: &Seed) -> io::Result<()> {
        *self.seed.lock().expect("mem store lock") = Some(*seed.as_bytes());
        Ok(())
    }

    fn load_token(&self) -> io::Result<Option<SessionToken>> {
        Ok(self
            .token
            .lock()
            .expect("mem store lock")
            .clone()
            .map(SessionToken::new))
    }

    fn store_token(&self, token: &SessionToken) -> io::Result<()> {
        *self.token.lock().expect("mem store lock") = Some(token.as_str().to_string());
        Ok(())
    }

    fn wipe(&self) -> io::Result<()> {
        *self.seed.lock().expect("mem store lock") = None;
        *self.token.lock().expect("mem store lock") = None;
        Ok(())
    }
}

/// The two PUBLIC artifacts (p3-design §C): the pinned genesis hash (the
/// trust anchor, TOFU at first login) and the cached verified chain
/// (length-prefixed raw entries; re-verified with `chain::sync` from the
/// pin on load, so a corrupt file means a refetch, never trust).
/// Lives at `<state_dir>/<origin-slug>/{genesis,chain}`; both files 0600,
/// written atomically (tmp + rename).
pub struct PublicState {
    dir: std::path::PathBuf,
}

impl PublicState {
    /// `<state_dir>/<origin_slug(origin)>/`.
    pub fn new(state_dir: &std::path::Path, origin: &url::Url) -> Self {
        todo!("p3 PR-B: PublicState::new")
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    pub fn save_genesis(&self, genesis: [u8; 32]) -> io::Result<()> {
        todo!("p3 PR-B: save_genesis")
    }

    /// `Ok(None)` when absent; a wrong-length file is `InvalidData`.
    pub fn load_genesis(&self) -> io::Result<Option<[u8; 32]>> {
        todo!("p3 PR-B: load_genesis")
    }

    pub fn save_chain(&self, entries: &[Vec<u8>]) -> io::Result<()> {
        todo!("p3 PR-B: save_chain")
    }

    /// Empty when absent; a truncated file is `InvalidData`.
    pub fn load_chain(&self) -> io::Result<Vec<Vec<u8>>> {
        todo!("p3 PR-B: load_chain")
    }

    /// Delete genesis + chain + the slug dir; idempotent.
    pub fn wipe(&self) -> io::Result<()> {
        todo!("p3 PR-B: PublicState::wipe")
    }
}

/// Logout wipe (§G.6 step 3): seed + token in the store, then genesis +
/// chain. Idempotent; continues past the first failure and reports it.
pub fn wipe_all(store: &dyn SeedStore, public: &PublicState) -> io::Result<()> {
    todo!("p3 PR-B: wipe_all")
}
