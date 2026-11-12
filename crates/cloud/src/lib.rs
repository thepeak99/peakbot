//! pbc1 — Shifu Cloud Mode protocol core (spec: design/cloud-protocol.md).
//!
//! PURE crate: no I/O, no tokio, no `std::net`, no global RNG, no hidden
//! clock. Randomness is an explicit `rng: &mut R` parameter (spec §15.1);
//! time is an explicit `now: u64` parameter (spec §1.5).
//!
//! P1 state: TDD skeleton. Public types and function signatures exist so
//! the RED tests in `tests/` compile; every fallible body is `todo!()`.
//!
//! `unused_variables` is allowed crate-wide for the skeleton state: every
//! parameter is named exactly as the spec names it, and the bodies are
//! `todo!()`. REMOVE this allow as the implementation lands (the pre-commit
//! gate is `clippy -D warnings`).
#![deny(unsafe_code)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![allow(unused_variables)]

pub mod b64;
pub mod card;
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
        todo!()
    }

    /// Parse a 40-byte CE head; any other length is `Malformed`.
    pub fn parse_ce(bytes: &[u8]) -> Result<Self> {
        todo!()
    }
}

/// Domain-separation tagged hash `H(T; m) = sha3_256(str(T) ‖ m)` (spec §1.3, A1).
pub fn h(tag: &str, message: &[u8]) -> [u8; 32] {
    todo!()
}

/// Domain-separation tagged MAC `MAC(k; T; m) = hmac_sha3_256(k, str(T) ‖ m)` (spec §1.3, A2).
pub fn mac(key: &[u8], tag: &str, message: &[u8]) -> [u8; 32] {
    todo!()
}

/// Constant-time tagged MAC verification (spec §1.3: comparisons MUST be
/// constant-time). Mismatch or wrong length is `Error::BadMac`.
pub fn mac_verify(key: &[u8], tag: &str, message: &[u8], mac: &[u8]) -> Result<()> {
    todo!()
}

/// Sign `str(T) ‖ m` with the device key (spec §1.3). `sign_with_rng` (A5)
/// so test vectors are reproducible.
pub fn sign_tagged<R: rand_core::CryptoRng>(
    sk: &v1::PrivateKey,
    tag: &str,
    message: &[u8],
    rng: &mut R,
) -> Result<Vec<u8>> {
    todo!()
}

/// Verify `str(T) ‖ m` against the card's key (spec §1.3).
pub fn verify_tagged(
    pk: &v1::PublicKey,
    tag: &str,
    message: &[u8],
    signature: &[u8],
) -> Result<()> {
    todo!()
}
