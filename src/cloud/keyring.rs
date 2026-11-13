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
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

use super::store::{Seed, SeedStore, SessionToken, write_atomic_0600};

const SERVICE: &str = "peakbot";
const SECRETS_FILE: &str = "secrets.json";

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
        self.dir.join(SECRETS_FILE)
    }

    /// `Ok(None)` when the file is absent. A mode looser than 0600 is a
    /// `PermissionDenied` naming the fix; corrupt JSON is `InvalidData`.
    fn read(&self) -> io::Result<Option<SecretsFile>> {
        let path = self.path();
        // Mode check BEFORE the contents: a loosened file is a policy break.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let meta = match std::fs::metadata(&path) {
                Ok(meta) => meta,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            };
            if meta.permissions().mode() & 0o777 != 0o600 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "refusing to read {} — it is looser than 0600 (run: chmod 600 {})",
                        path.display(),
                        path.display()
                    ),
                ));
            }
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    }

    fn write(&self, file: &SecretsFile) -> io::Result<()> {
        let json = serde_json::to_vec(file).map_err(|e| io::Error::other(e.to_string()))?;
        write_atomic_0600(&self.dir, SECRETS_FILE, &json)
    }

    fn decode_seed(b64: &str) -> io::Result<Seed> {
        let bytes = B64
            .decode(b64)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        if bytes.len() != 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "seed is not 64 bytes",
            ));
        }
        let mut seed = [0u8; 64];
        seed.copy_from_slice(&bytes);
        Ok(Seed::new(seed))
    }
}

/// The on-disk shape. `Option` fields keep `{}` and partial files parseable
/// so an existing entry is never clobbered by storing the other one.
#[derive(Debug, serde::Serialize, serde::Deserialize, Default)]
struct SecretsFile {
    #[serde(default)]
    seed_b64: Option<String>,
    #[serde(default)]
    token: Option<String>,
}

impl SeedStore for FileSeedStore {
    fn load_seed(&self) -> io::Result<Option<Seed>> {
        let Some(file) = self.read()? else {
            return Ok(None);
        };
        file.seed_b64.as_deref().map(Self::decode_seed).transpose()
    }

    fn store_seed(&self, seed: &Seed) -> io::Result<()> {
        let mut file = self.read()?.unwrap_or_default();
        file.seed_b64 = Some(B64.encode(seed.as_bytes()));
        self.write(&file)
    }

    fn load_token(&self) -> io::Result<Option<SessionToken>> {
        let Some(file) = self.read()? else {
            return Ok(None);
        };
        Ok(file.token.map(SessionToken::new))
    }

    fn store_token(&self, token: &SessionToken) -> io::Result<()> {
        let mut file = self.read()?.unwrap_or_default();
        file.token = Some(token.as_str().to_string());
        self.write(&file)
    }

    fn wipe(&self) -> io::Result<()> {
        match std::fs::remove_file(self.path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// The OS-keyring store: service `"peakbot"`, accounts
/// `"cloud-seed:<relay origin>"` / `"cloud-token:<relay origin>"` (keying
/// by origin so a relay change never reuses a token for the wrong relay).
/// The seed is stored b64 (keyring stores strings portably).
pub struct KeyringSeedStore {
    pub origin: String,
}

impl KeyringSeedStore {
    fn entry(&self, what: &str) -> io::Result<keyring::Entry> {
        let account = format!("{what}:{}", self.origin);
        keyring::Entry::new(SERVICE, &account).map_err(keyring_io)
    }
}

fn keyring_io(e: keyring::Error) -> io::Error {
    io::Error::other(e.to_string())
}

impl SeedStore for KeyringSeedStore {
    fn load_seed(&self) -> io::Result<Option<Seed>> {
        match self.entry("cloud-seed")?.get_password() {
            Ok(b64) => FileSeedStore::decode_seed(&b64).map(Some),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keyring_io(e)),
        }
    }

    fn store_seed(&self, seed: &Seed) -> io::Result<()> {
        self.entry("cloud-seed")?
            .set_password(&B64.encode(seed.as_bytes()))
            .map_err(keyring_io)
    }

    fn load_token(&self) -> io::Result<Option<SessionToken>> {
        match self.entry("cloud-token")?.get_password() {
            Ok(token) => Ok(Some(SessionToken::new(token))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keyring_io(e)),
        }
    }

    fn store_token(&self, token: &SessionToken) -> io::Result<()> {
        self.entry("cloud-token")?
            .set_password(token.as_str())
            .map_err(keyring_io)
    }

    fn wipe(&self) -> io::Result<()> {
        // Both accounts, missing = Ok (idempotent).
        for what in ["cloud-seed", "cloud-token"] {
            match self.entry(what)?.delete_credential() {
                Ok(()) => {}
                Err(keyring::Error::NoEntry) => {}
                Err(e) => return Err(keyring_io(e)),
            }
        }
        Ok(())
    }
}

/// Which store the boot probe chose (p3-design §C: made ONCE at boot, so
/// the two stores never split-brain).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    Keyring,
    File,
}

/// The seam through which tests mock keyring availability — the real OS
/// keyring is never touched in tests. Production wires the `keyring`
/// crate behind it (green phase); `Err` = unavailable (no Secret Service,
/// locked keychain).
pub trait KeyringProbe {
    fn probe(&self) -> Result<(), String>;
}

/// The boot-time choice + the one warning to print.
pub struct Probed {
    pub store: std::sync::Arc<dyn SeedStore>,
    pub kind: StoreKind,
    /// `Some("⚠ no OS keyring (Secret Service unavailable); cloud identity
    /// stored in <path> (0600)")` when the file fallback was chosen.
    pub warning: Option<String>,
}

/// Probe the keyring through `probe`; unavailable ⇒ `FileSeedStore` in
/// `dir` + the warning (never refuse: the headless server is the primary
/// host use case).
pub fn probe(probe: &dyn KeyringProbe, origin: &str, dir: &Path) -> Probed {
    match probe.probe() {
        Ok(()) => Probed {
            store: Arc::new(KeyringSeedStore {
                origin: origin.to_string(),
            }),
            kind: StoreKind::Keyring,
            warning: None,
        },
        // The probe reason is intentionally not echoed: the locked message
        // names the fallback, not the OS state.
        Err(_) => {
            let file = Arc::new(FileSeedStore::new(dir.to_path_buf()));
            let path = file.path();
            Probed {
                store: file,
                kind: StoreKind::File,
                warning: Some(format!(
                    "⚠ no OS keyring (Secret Service unavailable); cloud identity stored in {} (0600)",
                    path.display()
                )),
            }
        }
    }
}
