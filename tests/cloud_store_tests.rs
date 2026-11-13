//! P3 PR-B — RED tests for the SeedStore / secrets layer (p3-design §C, B2).
//!
//! Interfaces under test (skeleton in `src/cloud/store.rs`, `keyring.rs`):
//!   `SeedStore` (trait), `MemSeedStore`, `Seed`, `SessionToken`,
//!   `PublicState` (genesis pin + chain cache), `wipe_all`,
//!   host-only: `FileSeedStore` (0600 fallback), `probe(&dyn KeyringProbe,..)`.
//!
//! The real OS keyring is NEVER touched: availability is mocked through the
//! `KeyringProbe` trait. No test prints or asserts on secret values in a
//! failure message beyond equality of opaque bytes.
//!
//! Test → spec map:
//!   mem_store_*                       §C trait contract (round-trip, wipe idempotent)
//!   secrets_debug_is_redacted         §C `Debug` redacted, no leak
//!   file_store_*  (host, unix)        §C 0600 file fallback, atomic, refuse loose mode
//!   probe_*       (host)              §C one-time choice + the one warning
//!   public_state_*                    §C genesis/chain files, 0600, atomic, slug dir
//!   wipe_all_*                        §C/§G.6 logout wipe: seed+token+chain+pin

use std::io;
use std::sync::Arc;

use peakbot::cloud::store::{MemSeedStore, PublicState, Seed, SeedStore, SessionToken, wipe_all};

fn seed(n: u8) -> Seed {
    Seed::new([n; 64])
}

fn token(s: &str) -> SessionToken {
    SessionToken::new(s.to_string())
}

fn relay_origin() -> url::Url {
    url::Url::parse("https://relay.example.com/").expect("url")
}

/// The generic SeedStore contract, run against every implementation.
fn contract(store: &dyn SeedStore) {
    assert!(
        store.load_seed().expect("load").is_none(),
        "fresh store: no seed"
    );
    assert!(
        store.load_token().expect("load").is_none(),
        "fresh store: no token"
    );

    store.store_seed(&seed(7)).expect("store seed");
    store.store_token(&token("tok-abc")).expect("store token");
    assert_eq!(
        store.load_seed().expect("load").expect("seed").as_bytes(),
        &[7u8; 64],
        "seed round-trips byte-exact"
    );
    assert_eq!(
        store.load_token().expect("load").expect("token").as_str(),
        "tok-abc"
    );

    store.store_seed(&seed(9)).expect("overwrite seed");
    assert_eq!(
        store.load_seed().expect("load").expect("seed").as_bytes(),
        &[9u8; 64],
        "a second store replaces the first"
    );

    store.wipe().expect("wipe");
    assert!(
        store.load_seed().expect("load").is_none(),
        "wipe removes the seed"
    );
    assert!(
        store.load_token().expect("load").is_none(),
        "wipe removes the token"
    );
    store.wipe().expect("wipe is idempotent (missing = Ok)");
}

#[test]
fn mem_store_satisfies_the_store_contract() {
    contract(&MemSeedStore::new());
}

#[test]
fn mem_store_token_can_be_replaced_without_touching_the_seed() {
    let store = MemSeedStore::new();
    store.store_seed(&seed(1)).expect("seed");
    store.store_token(&token("old")).expect("token");
    store.store_token(&token("new")).expect("token");
    assert_eq!(store.load_token().expect("l").expect("t").as_str(), "new");
    assert_eq!(
        store.load_seed().expect("l").expect("s").as_bytes(),
        &[1u8; 64]
    );
}

#[test]
fn secrets_debug_is_redacted() {
    let s = format!("{:?}", seed(0x5a));
    assert!(
        !s.contains("5a") && !s.contains("90"),
        "no seed bytes in Debug: {s}"
    );
    let t = format!("{:?}", token("super-secret-token"));
    assert!(
        !t.contains("super-secret-token"),
        "no token text in Debug: {t}"
    );
}

// ── file fallback + probe (host only: keyring.rs is host-gated) ────────────

#[cfg(feature = "host")]
mod host {
    use super::*;
    use peakbot::cloud::keyring::{FileSeedStore, KeyringProbe, StoreKind, probe};

    struct Probe(Result<(), String>);
    impl KeyringProbe for Probe {
        fn probe(&self) -> Result<(), String> {
            self.0.clone()
        }
    }

    #[test]
    fn file_store_satisfies_the_store_contract() {
        let dir = tempfile::tempdir().expect("tmp");
        contract(&FileSeedStore::new(dir.path()));
    }

    #[test]
    fn file_store_lives_at_secrets_json_and_does_not_hold_the_seed_in_clear() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = FileSeedStore::new(dir.path());
        store.store_seed(&seed(0xab)).expect("store");
        assert_eq!(store.path(), dir.path().join("secrets.json"));
        let raw = std::fs::read_to_string(store.path()).expect("file exists");
        assert!(raw.contains("seed_b64"), "documented field name: {raw}");
        assert!(
            !raw.contains(&"ab".repeat(8)),
            "the seed is b64, never raw hex"
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_store_writes_secrets_with_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tmp");
        let store = FileSeedStore::new(dir.path());
        store.store_seed(&seed(1)).expect("seed");
        assert_eq!(
            std::fs::metadata(store.path())
                .expect("meta")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "created 0600"
        );
        store.store_token(&token("t")).expect("token");
        assert_eq!(
            std::fs::metadata(store.path())
                .expect("meta")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "still 0600 after the atomic rewrite (tmp + rename)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_store_leaves_no_tmp_files_behind() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = FileSeedStore::new(dir.path());
        store.store_seed(&seed(1)).expect("seed");
        store.store_token(&token("t")).expect("token");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .expect("dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["secrets.json"],
            "atomic write cleans its tmp: {names:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_store_refuses_to_load_when_the_mode_is_looser_than_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tmp");
        let store = FileSeedStore::new(dir.path());
        store.store_seed(&seed(1)).expect("seed");
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644))
            .expect("chmod");

        let err = store.load_seed().expect_err("0644 must be refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            err.to_string().contains("chmod 600"),
            "the error tells the user the fix: {err}"
        );
        assert!(
            !err.to_string().contains("AQEB"),
            "and never echoes secret material: {err}"
        );
        assert!(store.load_token().is_err(), "the token load refuses too");
    }

    #[test]
    fn file_store_corrupt_file_is_invalid_data_not_a_panic() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = FileSeedStore::new(dir.path());
        store.store_seed(&seed(1)).expect("seed");
        std::fs::write(store.path(), b"{ not json").expect("corrupt");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o600))
                .expect("chmod");
        }
        let err = store.load_seed().expect_err("corrupt");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn file_store_wrong_length_seed_is_invalid_data() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = FileSeedStore::new(dir.path());
        store.store_token(&token("t")).expect("token");
        std::fs::write(store.path(), br#"{"seed_b64":"AAAA","token":"t"}"#).expect("short seed");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o600))
                .expect("chmod");
        }
        let err = store.load_seed().expect_err("a 3-byte seed is not a seed");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn file_store_wipe_removes_the_file_and_is_idempotent() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = FileSeedStore::new(dir.path());
        store.store_seed(&seed(1)).expect("seed");
        store.store_token(&token("t")).expect("token");
        store.wipe().expect("wipe");
        assert!(!store.path().exists(), "no secret remains on disk");
        store.wipe().expect("second wipe is Ok");
    }

    #[test]
    fn probe_falls_back_to_the_file_store_with_the_documented_warning() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = probe(
            &Probe(Err("no Secret Service".into())),
            "https://relay.example.com",
            dir.path(),
        );
        assert_eq!(
            p.kind,
            StoreKind::File,
            "keyring unavailable ⇒ file fallback, not refusal"
        );
        let path = dir.path().join("secrets.json");
        assert_eq!(
            p.warning.as_deref(),
            Some(
                format!(
                    "⚠ no OS keyring (Secret Service unavailable); cloud identity stored in {} (0600)",
                    path.display()
                )
                .as_str()
            ),
            "the one boot warning, naming the path"
        );
        // And the chosen store really is the file store.
        p.store
            .store_seed(&seed(3))
            .expect("store via the probed store");
        assert!(path.exists(), "the fallback writes <dir>/secrets.json");
    }

    #[test]
    fn probe_uses_the_keyring_without_a_warning_when_available() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = probe(&Probe(Ok(())), "https://relay.example.com", dir.path());
        assert_eq!(p.kind, StoreKind::Keyring);
        assert!(p.warning.is_none(), "no warning on the happy path");
        assert!(
            !dir.path().join("secrets.json").exists(),
            "probing alone writes no fallback file"
        );
    }
}

// ── PublicState: genesis pin + chain cache ─────────────────────────────────

#[test]
fn public_state_lives_under_the_origin_slug_dir() {
    let root = tempfile::tempdir().expect("tmp");
    let ps = PublicState::new(root.path(), &relay_origin());
    let slug = peakbot::cloud::config::origin_slug(&relay_origin());
    assert_eq!(
        ps.dir(),
        root.path().join(slug),
        "<state_dir>/<origin-slug>/"
    );
}

#[test]
fn public_state_genesis_round_trips_and_is_absent_before_first_login() {
    let root = tempfile::tempdir().expect("tmp");
    let ps = PublicState::new(root.path(), &relay_origin());
    assert_eq!(
        ps.load_genesis().expect("load"),
        None,
        "no pin before first login (TOFU)"
    );
    ps.save_genesis([0x42; 32]).expect("save");
    assert_eq!(ps.load_genesis().expect("load"), Some([0x42; 32]));
    ps.save_genesis([0x43; 32]).expect("overwrite");
    assert_eq!(ps.load_genesis().expect("load"), Some([0x43; 32]));
}

#[test]
fn public_state_genesis_of_wrong_length_is_invalid_data() {
    let root = tempfile::tempdir().expect("tmp");
    let ps = PublicState::new(root.path(), &relay_origin());
    ps.save_genesis([1; 32]).expect("save");
    std::fs::write(ps.dir().join("genesis"), [0u8; 31]).expect("truncate");
    let err = ps.load_genesis().expect_err("31 bytes is not a pin");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn public_state_chain_round_trips_length_prefixed_entries() {
    let root = tempfile::tempdir().expect("tmp");
    let ps = PublicState::new(root.path(), &relay_origin());
    assert!(ps.load_chain().expect("load").is_empty(), "absent ⇒ empty");
    let entries = vec![vec![1u8, 2, 3], vec![], vec![9u8; 5000]];
    ps.save_chain(&entries).expect("save");
    assert_eq!(
        ps.load_chain().expect("load"),
        entries,
        "order and bytes kept, incl. empty"
    );
}

#[test]
fn public_state_truncated_chain_file_is_invalid_data() {
    let root = tempfile::tempdir().expect("tmp");
    let ps = PublicState::new(root.path(), &relay_origin());
    ps.save_chain(&[vec![7u8; 100]]).expect("save");
    let raw = std::fs::read(ps.dir().join("chain")).expect("read");
    std::fs::write(ps.dir().join("chain"), &raw[..raw.len() - 10]).expect("truncate");
    let err = ps
        .load_chain()
        .expect_err("a torn cache must not be trusted");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[cfg(unix)]
#[test]
fn public_state_files_are_0600_because_the_pin_is_integrity_critical() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().expect("tmp");
    let ps = PublicState::new(root.path(), &relay_origin());
    ps.save_genesis([1; 32]).expect("g");
    ps.save_chain(&[vec![1]]).expect("c");
    for f in ["genesis", "chain"] {
        let mode = std::fs::metadata(ps.dir().join(f))
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "{f}");
    }
}

#[test]
fn public_state_for_two_relays_does_not_collide() {
    let root = tempfile::tempdir().expect("tmp");
    let a = PublicState::new(
        root.path(),
        &url::Url::parse("https://a.example.com/").expect("u"),
    );
    let b = PublicState::new(
        root.path(),
        &url::Url::parse("https://b.example.com/").expect("u"),
    );
    a.save_genesis([0xaa; 32]).expect("a");
    assert_eq!(
        b.load_genesis().expect("b"),
        None,
        "a relay change never reuses a pin"
    );
}

#[test]
fn public_state_wipe_removes_everything_and_is_idempotent() {
    let root = tempfile::tempdir().expect("tmp");
    let ps = PublicState::new(root.path(), &relay_origin());
    ps.save_genesis([1; 32]).expect("g");
    ps.save_chain(&[vec![1]]).expect("c");
    ps.wipe().expect("wipe");
    assert!(!ps.dir().exists(), "the slug dir is deleted");
    ps.wipe().expect("second wipe is Ok");
    assert_eq!(ps.load_genesis().expect("load"), None);
}

// ── wipe_all: the logout wipe (§G.6 step 3) ────────────────────────────────

#[test]
fn wipe_all_removes_seed_token_chain_and_pin() {
    let root = tempfile::tempdir().expect("tmp");
    let store = Arc::new(MemSeedStore::new());
    let ps = PublicState::new(root.path(), &relay_origin());
    store.store_seed(&seed(1)).expect("s");
    store.store_token(&token("t")).expect("t");
    ps.save_genesis([1; 32]).expect("g");
    ps.save_chain(&[vec![1]]).expect("c");

    wipe_all(store.as_ref(), &ps).expect("wipe_all");

    assert!(store.load_seed().expect("l").is_none());
    assert!(store.load_token().expect("l").is_none());
    assert_eq!(ps.load_genesis().expect("l"), None);
    assert!(ps.load_chain().expect("l").is_empty());
}

#[test]
fn wipe_all_on_an_empty_state_is_ok() {
    let root = tempfile::tempdir().expect("tmp");
    wipe_all(
        &MemSeedStore::new(),
        &PublicState::new(root.path(), &relay_origin()),
    )
    .expect("idempotent");
}

/// A store whose `wipe` fails: the public artifacts must still go (an
/// offline/locked-keychain logout must still leave no pin behind) and the
/// error must surface.
struct FailingWipe;
impl SeedStore for FailingWipe {
    fn load_seed(&self) -> io::Result<Option<Seed>> {
        Ok(None)
    }
    fn store_seed(&self, _: &Seed) -> io::Result<()> {
        Ok(())
    }
    fn load_token(&self) -> io::Result<Option<SessionToken>> {
        Ok(None)
    }
    fn store_token(&self, _: &SessionToken) -> io::Result<()> {
        Ok(())
    }
    fn wipe(&self) -> io::Result<()> {
        Err(io::Error::other("keychain locked"))
    }
}

#[test]
fn wipe_all_still_removes_public_state_when_the_store_wipe_fails_and_reports_it() {
    let root = tempfile::tempdir().expect("tmp");
    let ps = PublicState::new(root.path(), &relay_origin());
    ps.save_genesis([1; 32]).expect("g");
    let err = wipe_all(&FailingWipe, &ps).expect_err("the failure is reported");
    assert!(err.to_string().contains("keychain locked"), "{err}");
    assert_eq!(
        ps.load_genesis().expect("l"),
        None,
        "the pin is gone regardless"
    );
}
