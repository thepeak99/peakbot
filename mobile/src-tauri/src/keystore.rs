//! OS secure-storage `SeedStore` for the phone (P5 T7): the cloud seed and
//! session token live in the Android Keystore (via SharedPreferences,
//! `android-native-keyring-store`) or the iOS Keychain (generic passwords).
//!
//! Service `sh.shifu.mobile`, accounts `cloud-seed` / `cloud-token`. Both
//! are stored as UTF-8 text (the seed base64). Only "not found" is `None`;
//! every other failure — including undecodable or wrong-length data — is an
//! `Err`, because "no seed" would make the core mint a fresh genesis and
//! orphan the account. Secret bytes are never logged or put in an error.

use std::io;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use peakbot::cloud::{Seed, SessionToken};
use zeroize::Zeroizing;

const SERVICE: &str = "sh.shifu.mobile";
const SEED_ACCOUNT: &str = "cloud-seed";
const TOKEN_ACCOUNT: &str = "cloud-token";

/// `errSecItemNotFound`.
#[cfg(any(target_os = "ios", test))]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

/// Stored seed text → `Seed`. Not UTF-8, not base64, or not 64 bytes ⇒
/// `InvalidData` (never `None`). The message never contains the data.
fn decode_seed(stored: &[u8]) -> io::Result<Seed> {
    let text = std::str::from_utf8(stored).map_err(|_| invalid("stored seed is not UTF-8 text"))?;
    let bytes = Zeroizing::new(
        B64.decode(text)
            .map_err(|_| invalid("stored seed is not valid base64"))?,
    );
    let raw: [u8; 64] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| invalid(&format!("stored seed is {} bytes, not 64", bytes.len())))?;
    Ok(Seed::new(raw))
}

fn encode_seed(seed: &Seed) -> Zeroizing<String> {
    Zeroizing::new(B64.encode(seed.as_bytes()))
}

/// Stored token text → `SessionToken`; invalid UTF-8 is an error, not lossy.
fn decode_token(stored: Vec<u8>) -> io::Result<SessionToken> {
    String::from_utf8(stored)
        .map(SessionToken::new)
        .map_err(|_| invalid("stored token is not UTF-8 text"))
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// iOS keychain failure with its `OSStatus` (e.g. -34018 = missing
/// entitlement) so it is diagnosable from a log line.
#[cfg(any(target_os = "ios", test))]
fn status_error(op: &str, status: i32, message: Option<&str>) -> io::Error {
    let tail = message.map(|m| format!(": {m}")).unwrap_or_default();
    io::Error::other(format!(
        "iOS keychain {op} failed (OSStatus {status}){tail}"
    ))
}

#[cfg(any(target_os = "ios", test))]
fn is_item_not_found(status: i32) -> bool {
    status == ERR_SEC_ITEM_NOT_FOUND
}

/// One keychain/keystore read: absent ⇒ `None`, present ⇒ the bytes.
fn present(found: Option<Vec<u8>>) -> Option<Zeroizing<Vec<u8>>> {
    found.map(Zeroizing::new)
}

// ---------------------------------------------------------------- Android

#[cfg(target_os = "android")]
mod android {
    use std::sync::Arc;

    use keyring_core::api::CredentialStoreApi as _;
    use keyring_core::{Entry, Error as KrError};
    use peakbot::cloud::SeedStore;

    use super::*;

    /// Needs the `ndk-context` JavaVM/Context, which tao initialises at app
    /// start (before `setup`), so it must be built after that.
    pub struct AndroidKeystore {
        store: Arc<android_native_keyring_store::Store>,
    }

    impl AndroidKeystore {
        pub fn new() -> io::Result<Self> {
            let store = android_native_keyring_store::Store::new().map_err(kr_io)?;
            Ok(Self { store })
        }

        fn entry(&self, account: &str) -> io::Result<Entry> {
            self.store.build(SERVICE, account, None).map_err(kr_io)
        }

        fn get(&self, account: &str) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
            match self.entry(account)?.get_secret() {
                Ok(bytes) => Ok(present(Some(bytes))),
                Err(KrError::NoEntry) => Ok(present(None)),
                Err(e) => Err(kr_io(e)),
            }
        }

        fn delete(&self, account: &str) -> io::Result<()> {
            match self.entry(account)?.delete_credential() {
                Ok(()) | Err(KrError::NoEntry) => Ok(()),
                Err(e) => Err(kr_io(e)),
            }
        }
    }

    fn kr_io(e: KrError) -> io::Error {
        io::Error::other(format!("android keystore: {e}"))
    }

    impl SeedStore for AndroidKeystore {
        fn load_seed(&self) -> io::Result<Option<Seed>> {
            let result = self.get(SEED_ACCOUNT)?.map(|b| decode_seed(&b)).transpose();
            match &result {
                Ok(Some(_)) => tracing::debug!("load_seed: found"),
                Ok(None) => tracing::debug!("load_seed: not found"),
                Err(e) => tracing::debug!("load_seed: error: {}", e),
            }
            result
        }

        fn store_seed(&self, seed: &Seed) -> io::Result<()> {
            let result = self
                .entry(SEED_ACCOUNT)?
                .set_secret(encode_seed(seed).as_bytes())
                .map_err(kr_io);
            match &result {
                Ok(()) => tracing::debug!("store_seed: stored"),
                Err(e) => tracing::debug!("store_seed: error: {}", e),
            }
            result
        }

        fn load_token(&self) -> io::Result<Option<SessionToken>> {
            let result = self
                .get(TOKEN_ACCOUNT)?
                .map(|b| decode_token(b.to_vec()))
                .transpose();
            match &result {
                Ok(Some(t)) => tracing::debug!("load_token: found (len {})", t.as_str().len()),
                Ok(None) => tracing::debug!("load_token: not found"),
                Err(e) => tracing::debug!("load_token: error: {}", e),
            }
            result
        }

        fn store_token(&self, token: &SessionToken) -> io::Result<()> {
            let result = self
                .entry(TOKEN_ACCOUNT)?
                .set_secret(token.as_str().as_bytes())
                .map_err(kr_io);
            match &result {
                Ok(()) => tracing::debug!("store_token: stored (len {})", token.as_str().len()),
                Err(e) => tracing::debug!("store_token: error: {}", e),
            }
            result
        }

        fn clear_token(&self) -> io::Result<()> {
            let result = self.delete(TOKEN_ACCOUNT);
            match &result {
                Ok(()) => tracing::debug!("clear_token: cleared"),
                Err(e) => tracing::debug!("clear_token: error: {}", e),
            }
            result
        }

        fn wipe(&self) -> io::Result<()> {
            let seed = self.delete(SEED_ACCOUNT);
            let token = self.delete(TOKEN_ACCOUNT);
            tracing::debug!("wipe: seed={:?}, token={:?}", seed.is_ok(), token.is_ok());
            seed.and(token)
        }
    }
}

// -------------------------------------------------------------------- iOS

#[cfg(target_os = "ios")]
mod ios {
    use peakbot::cloud::SeedStore;
    use security_framework::access_control::{ProtectionMode, SecAccessControl};
    use security_framework::passwords::{
        PasswordOptions, delete_generic_password, generic_password, set_generic_password,
        set_generic_password_options,
    };

    use super::*;

    pub struct IosKeychain;

    fn sec_io(op: &str, e: security_framework::base::Error) -> io::Error {
        status_error(op, e.code(), e.message().as_deref())
    }

    fn get(account: &str) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
        match generic_password(PasswordOptions::new_generic_password(SERVICE, account)) {
            Ok(bytes) => Ok(present(Some(bytes))),
            Err(e) if is_item_not_found(e.code()) => Ok(present(None)),
            Err(e) => Err(sec_io("read", e)),
        }
    }

    /// New items are created AfterFirstUnlockThisDeviceOnly (readable by a
    /// backgrounded app, never migrated off-device). An existing item is
    /// updated in place: its accessibility is kept, and the access-control
    /// attribute must not be part of the update's match query.
    fn put(account: &str, value: &[u8]) -> io::Result<()> {
        if get(account)?.is_some() {
            return set_generic_password(SERVICE, account, value).map_err(|e| sec_io("update", e));
        }
        let access = SecAccessControl::create_with_protection(
            Some(ProtectionMode::AccessibleAfterFirstUnlockThisDeviceOnly),
            0,
        )
        .map_err(|e| sec_io("access-control", e))?;
        let mut options = PasswordOptions::new_generic_password(SERVICE, account);
        options.set_access_control(access);
        set_generic_password_options(value, options).map_err(|e| sec_io("add", e))
    }

    fn delete(account: &str) -> io::Result<()> {
        match delete_generic_password(SERVICE, account) {
            Ok(()) => Ok(()),
            Err(e) if is_item_not_found(e.code()) => Ok(()),
            Err(e) => Err(sec_io("delete", e)),
        }
    }

    impl SeedStore for IosKeychain {
        fn load_seed(&self) -> io::Result<Option<Seed>> {
            get(SEED_ACCOUNT)?.map(|b| decode_seed(&b)).transpose()
        }

        fn store_seed(&self, seed: &Seed) -> io::Result<()> {
            put(SEED_ACCOUNT, encode_seed(seed).as_bytes())
        }

        fn load_token(&self) -> io::Result<Option<SessionToken>> {
            get(TOKEN_ACCOUNT)?
                .map(|b| decode_token(b.to_vec()))
                .transpose()
        }

        fn store_token(&self, token: &SessionToken) -> io::Result<()> {
            put(TOKEN_ACCOUNT, token.as_str().as_bytes())
        }

        fn clear_token(&self) -> io::Result<()> {
            delete(TOKEN_ACCOUNT)
        }

        fn wipe(&self) -> io::Result<()> {
            let seed = delete(SEED_ACCOUNT);
            let token = delete(TOKEN_ACCOUNT);
            seed.and(token)
        }
    }
}

/// The phone's OS-backed store.
#[cfg(target_os = "android")]
pub fn new_store() -> io::Result<std::sync::Arc<dyn peakbot::cloud::SeedStore>> {
    Ok(std::sync::Arc::new(android::AndroidKeystore::new()?))
}

#[cfg(target_os = "ios")]
#[allow(clippy::unnecessary_wraps)]
pub fn new_store() -> io::Result<std::sync::Arc<dyn peakbot::cloud::SeedStore>> {
    Ok(std::sync::Arc::new(ios::IosKeychain))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> Vec<u8> {
        B64.encode(bytes).into_bytes()
    }

    #[test]
    fn seed_round_trips_through_its_stored_text() {
        let mut raw = [0u8; 64];
        for (i, b) in raw.iter_mut().enumerate() {
            *b = i as u8;
        }
        let stored = encode_seed(&Seed::new(raw));
        assert_eq!(decode_seed(stored.as_bytes()).unwrap().as_bytes(), &raw);
    }

    #[test]
    fn wrong_length_seed_is_invalid_data_not_none() {
        for len in [0usize, 1, 32, 63, 65, 128] {
            let err = decode_seed(&b64(&vec![7u8; len]))
                .err()
                .expect("must be Err");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "len {len}");
        }
    }

    #[test]
    fn undecodable_seed_is_invalid_data() {
        for junk in [&b"not base64!!"[..], b"\xff\xfe\x00", b""] {
            let err = decode_seed(junk).err().expect("must be Err");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn decode_errors_never_echo_the_secret() {
        let secret = B64.encode([0x5au8; 40]);
        let msg = decode_seed(secret.as_bytes()).err().unwrap().to_string();
        assert!(!msg.contains(&secret), "{msg}");
    }

    #[test]
    fn token_decodes_utf8_and_rejects_other_bytes() {
        assert_eq!(
            decode_token(b"abc-_123".to_vec()).unwrap().as_str(),
            "abc-_123"
        );
        let err = decode_token(vec![0xff, 0xfe]).err().expect("must be Err");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn only_item_not_found_counts_as_absent() {
        assert!(is_item_not_found(-25300));
        for status in [-34018, -25293, -25308, -25291, -50, 0] {
            assert!(!is_item_not_found(status), "{status}");
        }
    }

    #[test]
    fn status_error_carries_the_os_status() {
        let e = status_error("read", -34018, Some("missing entitlement"));
        let msg = e.to_string();
        assert!(msg.contains("-34018") && msg.contains("read"), "{msg}");
        assert!(msg.contains("missing entitlement"), "{msg}");
        assert!(
            status_error("add", -25308, None)
                .to_string()
                .contains("-25308")
        );
    }

    #[test]
    fn present_maps_absent_to_none() {
        assert!(present(None).is_none());
        assert_eq!(present(Some(vec![1, 2])).unwrap().as_slice(), &[1, 2]);
    }
}
