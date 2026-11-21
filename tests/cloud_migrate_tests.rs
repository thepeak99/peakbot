//! P5 T2 RED tests — `MigratingSeedStore` (design §2 migration table).
//! Runs in both editions (`--no-default-features` too).
//!
//! RED today: `MigratingSeedStore::open` is a `todo!()` skeleton.

mod cloud_common;

use cloud_common::failing_store::FailingSeedStore;
use peakbot::cloud::keyring::FileSeedStore;
use peakbot::cloud::migrate::{MigratingSeedStore, MigrationError};
use peakbot::cloud::store::{MemSeedStore, Seed, SeedStore, SessionToken};
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

const S1: [u8; 64] = [1; 64];
const S2: [u8; 64] = [2; 64];

fn file_with(seed: Option<[u8; 64]>, token: Option<&str>) -> (tempfile::TempDir, FileSeedStore) {
    let dir = tempfile::tempdir().unwrap();
    let f = FileSeedStore::new(dir.path());
    if let Some(s) = seed {
        f.store_seed(&Seed::new(s)).unwrap();
    }
    if let Some(t) = token {
        f.store_token(&SessionToken::new(t.to_string())).unwrap();
    }
    (dir, f)
}

fn seed_of(s: &dyn SeedStore) -> Option<[u8; 64]> {
    s.load_seed().unwrap().map(|s| *s.as_bytes())
}

fn token_of(s: &dyn SeedStore) -> Option<String> {
    s.load_token().unwrap().map(|t| t.as_str().to_string())
}

// ── None/Some(file): copy, verify, delete ───────────────────────────────

#[test]
fn file_seed_and_token_are_copied_to_empty_keystore_and_file_deleted() {
    let (_d, file) = file_with(Some(S1), Some("tok-1"));
    let path = file.path();
    let ks = Arc::new(MemSeedStore::new());
    let m = MigratingSeedStore::open(ks.clone(), file).expect("migrate");
    assert_eq!(seed_of(&*ks), Some(S1), "seed copied");
    assert_eq!(token_of(&*ks).as_deref(), Some("tok-1"), "token copied");
    assert!(!path.exists(), "legacy file deleted after verify");
    assert_eq!(seed_of(&m), Some(S1), "wrapper serves the keystore seed");
}

#[test]
fn file_seed_without_token_migrates_seed_only() {
    let (_d, file) = file_with(Some(S1), None);
    let path = file.path();
    let ks = Arc::new(MemSeedStore::new());
    MigratingSeedStore::open(ks.clone(), file).expect("migrate");
    assert_eq!(seed_of(&*ks), Some(S1));
    assert_eq!(token_of(&*ks), None, "no token invented");
    assert!(!path.exists());
}

#[test]
fn second_open_after_migration_is_a_noop() {
    let (d, file) = file_with(Some(S1), None);
    let ks = Arc::new(MemSeedStore::new());
    MigratingSeedStore::open(ks.clone(), file).expect("first");
    MigratingSeedStore::open(ks.clone(), FileSeedStore::new(d.path())).expect("second");
    assert_eq!(seed_of(&*ks), Some(S1));
}

// ── both present ────────────────────────────────────────────────────────

#[test]
fn identical_seed_in_both_deletes_file_crash_resume() {
    let (_d, file) = file_with(Some(S1), None);
    let path = file.path();
    let ks = Arc::new(MemSeedStore::new());
    ks.store_seed(&Seed::new(S1)).unwrap();
    MigratingSeedStore::open(ks.clone(), file).expect("resume");
    assert!(!path.exists(), "file deleted");
    assert_eq!(seed_of(&*ks), Some(S1));
}

#[test]
fn different_seeds_in_both_is_conflict_and_nothing_is_deleted_or_overwritten() {
    let (_d, file) = file_with(Some(S2), None);
    let path = file.path();
    let ks = Arc::new(MemSeedStore::new());
    ks.store_seed(&Seed::new(S1)).unwrap();
    let r = MigratingSeedStore::open(ks.clone(), file);
    assert!(matches!(r, Err(MigrationError::Conflict)), "{:?}", r.err());
    assert!(path.exists(), "file kept");
    assert_eq!(seed_of(&*ks), Some(S1), "keystore seed untouched");
}

// ── keystore only / neither ─────────────────────────────────────────────

#[test]
fn keystore_only_changes_nothing() {
    let (_d, file) = file_with(None, None);
    let ks = Arc::new(FailingSeedStore::new());
    ks.inner.store_seed(&Seed::new(S1)).unwrap();
    let m = MigratingSeedStore::open(ks.clone(), file).expect("open");
    assert_eq!(seed_of(&m), Some(S1));
    assert_eq!(ks.seeds_written(), 0, "no write");
}

#[test]
fn neither_present_yields_none_and_writes_nothing() {
    let (_d, file) = file_with(None, None);
    let path = file.path();
    let ks = Arc::new(FailingSeedStore::new());
    let m = MigratingSeedStore::open(ks.clone(), file).expect("fresh install is not an error");
    assert!(
        m.load_seed().unwrap().is_none(),
        "None, not an invented seed"
    );
    assert_eq!(ks.seeds_written(), 0, "open must not generate a seed");
    assert!(!path.exists());
}

// ── error ≠ empty ───────────────────────────────────────────────────────

#[test]
fn keystore_read_error_with_file_present_is_err_keystore_and_file_kept() {
    let (_d, file) = file_with(Some(S1), Some("t"));
    let path = file.path();
    let ks = Arc::new(FailingSeedStore::new());
    ks.fail_load_seed.store(true, SeqCst);
    let r = MigratingSeedStore::open(ks.clone(), file);
    assert!(
        matches!(r, Err(MigrationError::Keystore(_))),
        "{:?}",
        r.err()
    );
    assert!(
        path.exists(),
        "file must NOT be deleted on a keystore read error"
    );
    assert_eq!(ks.seeds_written(), 0, "nothing written blind");
}

#[test]
fn keystore_read_error_with_no_file_is_err_never_ok_none() {
    let (_d, file) = file_with(None, None);
    let ks = Arc::new(FailingSeedStore::broken());
    let r = MigratingSeedStore::open(ks.clone(), file);
    assert!(
        matches!(r, Err(MigrationError::Keystore(_))),
        "a broken keystore must not look like a fresh install"
    );
    assert_eq!(ks.seeds_written(), 0);
}

#[test]
fn keystore_write_error_is_err_and_file_kept() {
    let (_d, file) = file_with(Some(S1), None);
    let path = file.path();
    let ks = Arc::new(FailingSeedStore::new());
    ks.fail_store_seed.store(true, SeqCst);
    let r = MigratingSeedStore::open(ks.clone(), file);
    assert!(
        matches!(r, Err(MigrationError::Keystore(_))),
        "{:?}",
        r.err()
    );
    assert!(path.exists(), "file kept so the seed is not lost");
}

#[test]
fn keystore_token_write_error_is_err_and_file_kept() {
    let (_d, file) = file_with(Some(S1), Some("tok"));
    let path = file.path();
    let ks = Arc::new(FailingSeedStore::new());
    ks.fail_store_token.store(true, SeqCst);
    let r = MigratingSeedStore::open(ks.clone(), file);
    assert!(r.is_err(), "partial migration must not succeed");
    assert!(path.exists(), "file kept");
}

#[test]
fn verify_mismatch_is_err_and_file_kept() {
    let (_d, file) = file_with(Some(S1), None);
    let path = file.path();
    let ks = Arc::new(FailingSeedStore::new());
    *ks.readback_override.lock().unwrap() = Some(S2);
    let r = MigratingSeedStore::open(ks.clone(), file);
    assert!(
        matches!(r, Err(MigrationError::VerifyMismatch)),
        "{:?}",
        r.err()
    );
    assert!(path.exists(), "file kept on verify mismatch");
}

#[test]
fn corrupt_legacy_file_is_err_legacy_and_left_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let file = FileSeedStore::new(dir.path());
    // Seed a valid file first (0600), then corrupt its contents in place.
    file.store_seed(&Seed::new(S1)).unwrap();
    std::fs::write(file.path(), b"{not json").unwrap();
    let path = file.path();
    let ks = Arc::new(FailingSeedStore::new());
    let r = MigratingSeedStore::open(ks.clone(), file);
    assert!(matches!(r, Err(MigrationError::Legacy(_))), "{:?}", r.err());
    assert!(path.exists(), "corrupt file kept for the user");
    assert_eq!(ks.seeds_written(), 0);
}

// ── delegation after open ───────────────────────────────────────────────

#[test]
fn wrapper_delegates_seed_token_and_wipe_to_the_keystore() {
    let (_d, file) = file_with(None, None);
    let ks = Arc::new(MemSeedStore::new());
    let m = MigratingSeedStore::open(ks.clone(), file).expect("open");
    m.store_seed(&Seed::new(S2)).unwrap();
    m.store_token(&SessionToken::new("t2".into())).unwrap();
    assert_eq!(seed_of(&*ks), Some(S2));
    assert_eq!(token_of(&*ks).as_deref(), Some("t2"));
    m.clear_token().unwrap();
    assert_eq!(token_of(&*ks), None, "clear_token drops the token only");
    assert_eq!(seed_of(&*ks), Some(S2), "seed survives clear_token");
    m.wipe().unwrap();
    assert_eq!(seed_of(&*ks), None, "wipe reaches the keystore");
}
