//! `FailingSeedStore` (P5): a `MemSeedStore` with injectable failures and
//! call counters. Lets tests prove "keystore error ≠ empty" and "no seed
//! was ever written" (T2/T3/T6).

use peakbot::cloud::store::{MemSeedStore, Seed, SeedStore, SessionToken};
use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};

#[derive(Default)]
pub struct FailingSeedStore {
    pub inner: MemSeedStore,
    pub fail_load_seed: AtomicBool,
    pub fail_store_seed: AtomicBool,
    pub fail_load_token: AtomicBool,
    pub fail_store_token: AtomicBool,
    /// After `store_seed`, `load_seed` returns this instead (verify mismatch).
    pub readback_override: Mutex<Option<[u8; 64]>>,
    pub store_seed_calls: AtomicUsize,
    pub store_token_calls: AtomicUsize,
}

fn boom(what: &str) -> io::Error {
    io::Error::other(format!("injected keystore failure: {what}"))
}

impl FailingSeedStore {
    pub fn new() -> Self {
        Self::default()
    }
    /// Every read AND write fails (a locked / unavailable keystore).
    pub fn broken() -> Self {
        let s = Self::new();
        s.fail_load_seed.store(true, SeqCst);
        s.fail_store_seed.store(true, SeqCst);
        s.fail_load_token.store(true, SeqCst);
        s.fail_store_token.store(true, SeqCst);
        s
    }
    pub fn seeds_written(&self) -> usize {
        self.store_seed_calls.load(SeqCst)
    }
}

impl SeedStore for FailingSeedStore {
    fn load_seed(&self) -> io::Result<Option<Seed>> {
        if self.fail_load_seed.load(SeqCst) {
            return Err(boom("load_seed"));
        }
        if self.store_seed_calls.load(SeqCst) > 0
            && let Some(b) = *self.readback_override.lock().unwrap()
        {
            return Ok(Some(Seed::new(b)));
        }
        self.inner.load_seed()
    }
    fn store_seed(&self, seed: &Seed) -> io::Result<()> {
        self.store_seed_calls.fetch_add(1, SeqCst);
        if self.fail_store_seed.load(SeqCst) {
            return Err(boom("store_seed"));
        }
        self.inner.store_seed(seed)
    }
    fn load_token(&self) -> io::Result<Option<SessionToken>> {
        if self.fail_load_token.load(SeqCst) {
            return Err(boom("load_token"));
        }
        self.inner.load_token()
    }
    fn store_token(&self, token: &SessionToken) -> io::Result<()> {
        self.store_token_calls.fetch_add(1, SeqCst);
        if self.fail_store_token.load(SeqCst) {
            return Err(boom("store_token"));
        }
        self.inner.store_token(token)
    }
    fn wipe(&self) -> io::Result<()> {
        self.inner.wipe()
    }
}
