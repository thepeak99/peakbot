//! Secrets and local state (p3-design §C). Two secrets (seed, session
//! token) go in secure storage; the two public artifacts (genesis pin,
//! cached chain) go in 0600 files under `<state_dir>/cloud/<origin-slug>/`.
//!
//! The store is synchronous and tiny — the calls are rare (boot, login,
//! logout) and the actor reaches it via `spawn_blocking`.

use std::io;
use std::path::Path;
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
        Ok(self
            .seed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .map(Seed::new))
    }

    fn store_seed(&self, seed: &Seed) -> io::Result<()> {
        *self.seed.lock().unwrap_or_else(|p| p.into_inner()) = Some(*seed.as_bytes());
        Ok(())
    }

    fn load_token(&self) -> io::Result<Option<SessionToken>> {
        Ok(self
            .token
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .map(SessionToken::new))
    }

    fn store_token(&self, token: &SessionToken) -> io::Result<()> {
        *self.token.lock().unwrap_or_else(|p| p.into_inner()) = Some(token.as_str().to_string());
        Ok(())
    }

    fn wipe(&self) -> io::Result<()> {
        *self.seed.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *self.token.lock().unwrap_or_else(|p| p.into_inner()) = None;
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
    pub fn new(state_dir: &Path, origin: &url::Url) -> Self {
        Self {
            dir: state_dir.join(crate::cloud::config::origin_slug(origin)),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn save_genesis(&self, genesis: [u8; 32]) -> io::Result<()> {
        write_atomic_0600(&self.dir, "genesis", &genesis)
    }

    /// `Ok(None)` when absent; a wrong-length file is `InvalidData`.
    pub fn load_genesis(&self) -> io::Result<Option<[u8; 32]>> {
        let bytes = read_optional(&self.dir, "genesis")?;
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        if bytes.len() != 32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "genesis file is not 32 bytes",
            ));
        }
        let mut genesis = [0u8; 32];
        genesis.copy_from_slice(&bytes);
        Ok(Some(genesis))
    }

    pub fn save_chain(&self, entries: &[Vec<u8>]) -> io::Result<()> {
        let mut buf = Vec::new();
        for entry in entries {
            buf.extend_from_slice(&(entry.len() as u32).to_be_bytes());
            buf.extend_from_slice(entry);
        }
        write_atomic_0600(&self.dir, "chain", &buf)
    }

    /// Empty when absent; a truncated file is `InvalidData`.
    pub fn load_chain(&self) -> io::Result<Vec<Vec<u8>>> {
        let bytes = read_optional(&self.dir, "chain")?;
        let Some(bytes) = bytes else {
            return Ok(Vec::new());
        };
        let mut entries = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if i + 4 > bytes.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "truncated chain length prefix",
                ));
            }
            let len =
                u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]) as usize;
            i += 4;
            if i + len > bytes.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "truncated chain entry",
                ));
            }
            entries.push(bytes[i..i + len].to_vec());
            i += len;
        }
        Ok(entries)
    }

    /// Delete genesis + chain + the slug dir; idempotent.
    pub fn wipe(&self) -> io::Result<()> {
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Logout wipe (§G.6 step 3): seed + token in the store, then genesis +
/// chain. Idempotent; both halves always run — a locked keychain must not
/// leave a pin behind — and the first failure is reported.
pub fn wipe_all(store: &dyn SeedStore, public: &PublicState) -> io::Result<()> {
    let mut first: Option<io::Error> = None;
    for result in [store.wipe(), public.wipe()] {
        if let Err(e) = result
            && first.is_none()
        {
            first = Some(e);
        }
    }
    match first {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// `O_CREAT|O_EXCL` tmp file at 0600, then `rename` over the target — no
/// partial file is ever visible at the final path.
pub(crate) fn write_atomic_0600(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{name}.tmp"));
    // A crash can leave a stale tmp; EXCL would then wedge every later write.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut file = opts.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, dir.join(name))
}

/// `NotFound` → `Ok(None)`.
fn read_optional(dir: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(dir.join(name)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
