//! pbc1 — Shifu Cloud Mode protocol core (spec: design/cloud-protocol.md).
//!
//! PURE crate: no I/O, no tokio, no `std::net`, no global RNG, no hidden
//! clock. Randomness is an explicit `rng: &mut R` parameter (spec §15.1);
//! time is an explicit `now: u64` parameter (spec §1.5).
//!
//! The peakbot binary does not depend on this crate yet (P1).

#![deny(unsafe_code)]
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod b64;
mod bip39;
pub mod card;
mod ce;
pub mod chain;
pub mod codec;
pub mod constants;
pub mod domain;
pub mod error;
pub mod frames;
pub mod handshake;
pub mod letter;
pub mod link;
pub mod sas;

pub use error::{Error, Result};

use perma_crypto::v1;
use rand_core::CryptoRng;

/// A device identity: `PublicKey::identity()` over the card's keys
/// (spec §3). It is always recomputed from the card, never sent as a
/// claim.
pub type DeviceId = [u8; 32];
/// Handshake session id (spec §3).
pub type SessionId = [u8; 16];
/// Letter id (spec §3).
pub type LetterId = [u8; 16];

/// Chain head (spec §3). CE: `head(h) = u64(h.seq) ‖ fix[32](h.hash)` — 40 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Head {
    pub seq: u64,
    pub hash: [u8; 32],
}

impl Head {
    /// Canonical Encoding of the head (spec §3): 40 bytes, big-endian.
    pub fn ce(&self) -> [u8; 40] {
        let mut out = [0u8; 40];
        out[..8].copy_from_slice(&self.seq.to_be_bytes());
        out[8..].copy_from_slice(&self.hash);
        out
    }

    /// Parse a 40-byte CE head; any other length is `Malformed`.
    pub fn parse_ce(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 40 {
            return Err(Error::Malformed);
        }
        let mut seq = [0u8; 8];
        seq.copy_from_slice(&bytes[..8]);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&bytes[8..]);
        Ok(Self {
            seq: u64::from_be_bytes(seq),
            hash,
        })
    }
}

/// `str(T)`: the length-prefixed tag (spec §1.3).
fn str_tag(tag: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + tag.len());
    out.extend_from_slice(&(tag.len() as u32).to_be_bytes());
    out.extend_from_slice(tag.as_bytes());
    out
}

/// Domain-separation tagged hash `H(T; m) = sha3_256(str(T) ‖ m)` (spec §1.3, A1).
pub fn h(tag: &str, message: &[u8]) -> [u8; 32] {
    let mut buf = str_tag(tag);
    buf.extend_from_slice(message);
    v1::sha3_256(&buf)
}

/// Domain-separation tagged MAC `MAC(k; T; m) = hmac_sha3_256(k, str(T) ‖ m)` (spec §1.3, A2).
pub fn mac(key: &[u8], tag: &str, message: &[u8]) -> [u8; 32] {
    let mut buf = str_tag(tag);
    buf.extend_from_slice(message);
    v1::hmac_sha3_256(key, &buf)
}

/// Constant-time tagged MAC verification (spec §1.3: comparisons MUST be
/// constant-time). Mismatch or wrong length is `Error::BadMac`.
pub fn mac_verify(key: &[u8], tag: &str, message: &[u8], mac: &[u8]) -> Result<()> {
    let mut buf = str_tag(tag);
    buf.extend_from_slice(message);
    v1::hmac_sha3_256_verify(key, &buf, mac).map_err(|_| Error::BadMac)
}

/// Sign `str(T) ‖ m` with the device key (spec §1.3). `sign_with_rng` (A5)
/// so test vectors are reproducible.
pub fn sign_tagged<R: CryptoRng>(
    sk: &v1::PrivateKey,
    tag: &str,
    message: &[u8],
    rng: &mut R,
) -> Result<Vec<u8>> {
    let mut buf = str_tag(tag);
    buf.extend_from_slice(message);
    sk.sign_with_rng(&buf, rng).map_err(v1_err)
}

/// Verify `str(T) ‖ m` against the card's key (spec §1.3).
pub fn verify_tagged(
    pk: &v1::PublicKey,
    tag: &str,
    message: &[u8],
    signature: &[u8],
) -> Result<()> {
    let mut buf = str_tag(tag);
    buf.extend_from_slice(message);
    pk.verify(&buf, signature).map_err(v1_err)
}

/// Map a v1 error onto the crate's single error type (spec preamble: every
/// named abort is a variant).
pub(crate) fn v1_err(e: v1::V1Error) -> Error {
    match e {
        v1::V1Error::MalformedKey => Error::MalformedKey,
        v1::V1Error::InvalidSignature => Error::BadSignature,
        v1::V1Error::DecryptionError => Error::DecryptionError,
        v1::V1Error::UnknownVersion => Error::UnknownVersion,
        _ => Error::Malformed,
    }
}
