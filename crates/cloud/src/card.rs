//! DeviceCard (spec §4.1) and name validation (N1–N2).
//!
//! CE layout:
//! ```text
//! card = u8(1) ‖ str("ml-dsa-65") ‖ bytes(vk, 1952)
//!      ‖ str("x25519-ml-kem-768") ‖ bytes(kem_pk, 1216)
//!      ‖ str(name, 1..=64) ‖ u8(platform)
//! ```
//! Maximum card length: 3280 bytes.

use perma_crypto::v1;

use crate::DeviceId;
use crate::Result;

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
        todo!()
    }

    /// The card's `PublicKey` (spec §4.1: alg ids are the v1 literals;
    /// validation failure is `Error::MalformedKey`).
    pub fn public_key(&self) -> Result<v1::PublicKey> {
        todo!()
    }

    /// `DeviceId = PublicKey::identity()` — recomputed, never claimed
    /// (spec §3).
    pub fn device_id(&self) -> DeviceId {
        todo!()
    }

    /// Canonical Encoding (spec §4.1). Output length ≤ 3280.
    pub fn encode(&self) -> Vec<u8> {
        todo!()
    }

    /// Parse CE card bytes. Enforces: exact consumption (P1), per-field
    /// limits (P2), the v1 algorithm id literals (`UnsupportedAlg`), key
    /// sizes (`MalformedKey`), and name rules N1 (`Malformed`).
    pub fn parse(raw: &[u8]) -> Result<Self> {
        todo!()
    }
}
