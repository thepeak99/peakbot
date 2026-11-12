//! Device chain: entries, verification (C1–C6), genesis pinning, sync,
//! fork detection (spec §4).
//!
//! CE layout (spec §4.2):
//! ```text
//! entry_body = u8(1) ‖ u64(seq) ‖ fix[32](prev) ‖ u8(op) ‖ op_body ‖ fix[32](signer)
//! op_body    = bytes(card)              if op ∈ {Genesis, Add}   (len ≤ 3280)
//!            | fix[32](target)          if op == Revoke
//! entry      = entry_body ‖ bytes(sig)  (len == 3309)
//! ```

use std::collections::{BTreeMap, BTreeSet};

use perma_crypto::v1;

use crate::Head;
use crate::card::DeviceCard;
use crate::{DeviceId, Result};

/// Chain operations (spec §4.2): 0 = Genesis, 1 = Add, 2 = Revoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainOp {
    Genesis,
    Add,
    Revoke,
}

/// A parsed chain entry. `body` and `raw` keep the exact received CE bytes:
/// re-encoding (RULE E1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainEntry {
    pub seq: u64,
    pub prev: [u8; 32],
    pub op: ChainOp,
    /// CE card bytes (Genesis/Add).
    pub card: Option<Vec<u8>>,
    /// Revoke target (Revoke).
    pub target: Option<DeviceId>,
    pub signer: DeviceId,
    /// Exact CE `entry_body` bytes (the signing input).
    pub body: Vec<u8>,
    /// ML-DSA-65 signature, 3309 bytes.
    pub sig: Vec<u8>,
    /// The full entry bytes as received.
    pub raw: Vec<u8>,
}

impl ChainEntry {
    /// Parse CE entry bytes. Enforces P1–P4, the per-field limits, and the
    /// signature length (3309). Anything else is `Error::Malformed`.
    pub fn parse(raw: &[u8]) -> Result<Self> {
        todo!()
    }

    /// Re-encode to CE bytes. The output hashes to the same value as the
    /// parsed input for well-formed entries.
    pub fn encode(&self) -> Vec<u8> {
        todo!()
    }
}

/// `entry_hash = H("pbc1 chain hash"; entry)` — the full entry, signature
/// included (spec §4.3).
pub fn entry_hash(raw: &[u8]) -> [u8; 32] {
    todo!()
}

/// Build and sign a chain entry (spec §4.2–§4.3).
///
/// `card` is required for Genesis/Add (CE bytes, ≤ 3280); `target` for
/// Revoke. The signature is over the exact `entry_body` bytes with tag
/// `pbc1 chain sig`, using `sign_with_rng` (A5) so vectors are
/// reproducible.
// Mirrors the spec §4.2 entry fields one-to-one, hence the argument count.
#[allow(clippy::too_many_arguments)]
pub fn build_entry<R: rand_core::CryptoRng>(
    seq: u64,
    prev: [u8; 32],
    op: ChainOp,
    card: Option<Vec<u8>>,
    target: Option<DeviceId>,
    signer: DeviceId,
    sk: &v1::PrivateKey,
    rng: &mut R,
) -> Result<ChainEntry> {
    todo!()
}

/// Verified chain state (spec §4.4). Constructible ONLY by `extend`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedChain {
    /// `entry_hash(entry 0)` — the value devices pin (spec §4.3, §4.5).
    pub genesis: [u8; 32],
    pub head: Head,
    /// `hashes[i]` is the hash of entry i.
    pub hashes: Vec<[u8; 32]>,
    /// Active devices: DeviceId → card.
    pub active: BTreeMap<DeviceId, DeviceCard>,
    /// Revoked devices.
    pub revoked: BTreeSet<DeviceId>,
}

/// The verification algorithm with invariants C1–C6 (spec §4.4).
///
/// `state` is `None` (empty) or a previously verified state; `entries` are
/// the raw CE entry bytes in order; `pinned` is `Some(genesis)` once a
/// device has a pin (spec §4.5). Any failure aborts the whole `extend`;
/// the previous state is never mutated.
pub fn extend(
    state: Option<VerifiedChain>,
    entries: &[Vec<u8>],
    pinned: Option<[u8; 32]>,
) -> Result<VerifiedChain> {
    todo!()
}

/// Sync from a relay `ChainFetch` batch (spec §4.6): `served` must start at
/// `state.head.seq + 1`; equivalent to `extend(Some(state), served,
/// pinned = Some(state.genesis))`.
pub fn sync(state: &VerifiedChain, served: &[Vec<u8>]) -> Result<VerifiedChain> {
    todo!()
}

/// Head handling shared by both handshake sides (spec §8.5).
///
/// 1. `peer_head.seq ≤ own.head.seq`: require `own.hashes[peer_head.seq] ==
///    peer_head.hash`, else `Trust` (fork F2).
/// 2. Else: `sync` via `fetch(from_seq)`. If the relay cannot serve up to
///    `peer_head.seq` (a shorter batch), `StaleChain`. If afterwards the
///    hash differs, `Trust` (fork F3).
/// 3. Require `peer_id ∈ active` and `self_id ∈ active`, evaluated after
///    any sync, else `NotActive`.
///
/// Returns the (possibly synced) chain state. `fetch` is the pure
/// stand-in for the relay: it takes the `from_seq` to request and returns
/// the served entry bytes (empty when the relay has nothing more).
pub fn check_head(
    own: &VerifiedChain,
    self_id: DeviceId,
    peer_head: Head,
    peer_id: DeviceId,
    fetch: &dyn Fn(u64) -> Result<Vec<Vec<u8>>>,
) -> Result<VerifiedChain> {
    todo!()
}
