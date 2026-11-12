//! DeviceCard (spec §4.1) and name validation (N1–N2).
//!
//! CE layout:
//! ```text
//! card = u8(1) ‖ str("ml-dsa-65") ‖ bytes(vk, 1952)
//!      ‖ str("x25519-ml-kem-768") ‖ bytes(kem_pk, 1216)
//!      ‖ str(name, 1..=64) ‖ u8(platform)
//! ```
//! Maximum card length: 3280 bytes.

use unicode_properties::general_category::{GeneralCategory, UnicodeGeneralCategory};

use perma_crypto::v1;

use crate::ce::{self, Reader};
use crate::{DeviceId, Error, Result};

const VK_LEN: usize = 1952;
const KEM_PK_LEN: usize = 1216;
/// Maximum card length (spec §4.1).
pub const MAX_CARD: usize = 3280;
const MAX_NAME: usize = 64;

/// A parsed device card. Has no field that can carry a secret (C7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCard {
    /// ML-DSA-65 verification key, 1952 bytes.
    pub vk: Vec<u8>,
    /// X25519 + ML-KEM-768 hybrid public key, 1216 bytes (layout
    /// `mlkem ‖ x25519`).
    pub kem_pk: Vec<u8>,
    /// Display name: 1–64 UTF-8 bytes, no Cc/Cf code points, no leading or
    /// trailing whitespace (N1). Not normalized (N2).
    pub name: String,
    /// Display hint: 0 unknown, 1 linux, 2 macos, 3 windows, 4 android,
    /// 5 ios. Other values are accepted and shown as "unknown"; never
    /// trusted for policy.
    pub platform: u8,
}

impl DeviceCard {
    /// Build a card from a key pair. Validates `name` (N1): an empty name,
    /// a name longer than 64 bytes, control (Cc) or format (Cf) code
    /// points, and leading or trailing whitespace are all rejected
    /// (`Error::Malformed`).
    pub fn new(kp: &v1::KeyPair, name: &str, platform: u8) -> Result<Self> {
        validate_name(name)?;
        let pk = kp.public();
        Ok(Self {
            vk: pk.verification_key().to_vec(),
            kem_pk: pk.kem_key().to_vec(),
            name: name.to_string(),
            platform,
        })
    }

    /// The card's `PublicKey` (spec §4.1: alg ids are the v1 literals;
    /// validation failure is `Error::MalformedKey`).
    pub fn public_key(&self) -> Result<v1::PublicKey> {
        v1::PublicKey::new(
            v1::SigAlg::MlDsa65,
            self.vk.clone(),
            v1::KemAlg::X25519MlKem768,
            self.kem_pk.clone(),
        )
        .map_err(|_| Error::MalformedKey)
    }

    /// `DeviceId = PublicKey::identity()` — recomputed, never claimed
    /// (spec §3). `v1::identity` is total, so this cannot fail.
    pub fn device_id(&self) -> DeviceId {
        v1::identity(
            v1::IDENTITY_DOMAIN,
            v1::SigAlg::MlDsa65,
            v1::KemAlg::X25519MlKem768,
            &self.vk,
            &self.kem_pk,
        )
    }

    /// Canonical Encoding (spec §4.1). Output length ≤ 3280.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + 13 + 4 + VK_LEN + 21 + 4 + KEM_PK_LEN + 4 + MAX_NAME + 1);
        out.push(1);
        ce::push_str(&mut out, v1::SigAlg::MlDsa65.alg_id());
        ce::push_bytes(&mut out, &self.vk);
        ce::push_str(&mut out, v1::KemAlg::X25519MlKem768.alg_id());
        ce::push_bytes(&mut out, &self.kem_pk);
        ce::push_str(&mut out, &self.name);
        out.push(self.platform);
        out
    }

    /// Parse CE card bytes. Enforces: exact consumption (P1), per-field
    /// limits (P2), the v1 algorithm id literals (`UnsupportedAlg`), key
    /// sizes (`MalformedKey`), and name rules N1 (`Malformed`).
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let mut r = Reader::new(raw);
        if r.u8()? != 1 {
            return Err(Error::UnknownVersion);
        }
        if r.str_field(MAX_NAME)?.as_str() != v1::SigAlg::MlDsa65.alg_id() {
            return Err(Error::UnsupportedAlg);
        }
        let vk = r.bytes_exact(VK_LEN)?.to_vec();
        if r.str_field(MAX_NAME)?.as_str() != v1::KemAlg::X25519MlKem768.alg_id() {
            return Err(Error::UnsupportedAlg);
        }
        let kem_pk = r.bytes_exact(KEM_PK_LEN)?.to_vec();
        let name = r.str_field(MAX_NAME)?;
        let platform = r.u8()?;
        if !r.at_end() {
            return Err(Error::Malformed);
        }
        // Key material must validate (spec §4.1 → MalformedKey).
        v1::PublicKey::new(
            v1::SigAlg::MlDsa65,
            vk.clone(),
            v1::KemAlg::X25519MlKem768,
            kem_pk.clone(),
        )
        .map_err(|_| Error::MalformedKey)?;
        validate_name(&name)?;
        Ok(Self {
            vk,
            kem_pk,
            name,
            platform,
        })
    }
}

/// N1: 1–64 UTF-8 bytes, no Cc (control) or Cf (format) code points, no
/// leading or trailing whitespace. N2: nothing is normalized.
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(Error::Malformed);
    }
    for c in name.chars() {
        let gc = c.general_category();
        if gc == GeneralCategory::Control || gc == GeneralCategory::Format {
            return Err(Error::Malformed);
        }
    }
    if name.starts_with(char::is_whitespace) || name.ends_with(char::is_whitespace) {
        return Err(Error::Malformed);
    }
    Ok(())
}
