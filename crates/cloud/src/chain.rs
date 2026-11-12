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
use rand_core::CryptoRng;

use crate::ce::{self, Reader};
use crate::card::{DeviceCard, MAX_CARD};
use crate::domain;
use crate::{DeviceId, Error, Head, Result, sign_tagged, verify_tagged};

const SIG_LEN: usize = 3309;

/// Chain operations (spec §4.2): 0 = Genesis, 1 = Add, 2 = Revoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainOp {
    Genesis,
    Add,
    Revoke,
}

impl ChainOp {
    fn tag(self) -> u8 {
        match self {
            ChainOp::Genesis => 0,
            ChainOp::Add => 1,
            ChainOp::Revoke => 2,
        }
    }

    fn parse(tag: u8) -> Result<Self> {
        match tag {
            0 => Ok(ChainOp::Genesis),
            1 => Ok(ChainOp::Add),
            2 => Ok(ChainOp::Revoke),
            // P3: unknown tag aborts.
            _ => Err(Error::Malformed),
        }
    }
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
        let mut r = Reader::new(raw);
        if r.u8()? != 1 {
            return Err(Error::UnknownVersion);
        }
        let seq = r.u64()?;
        let prev = r.fix_array::<32>()?;
        let op = ChainOp::parse(r.u8()?)?;
        let (card, target) = match op {
            ChainOp::Genesis | ChainOp::Add => {
                let c = r.bytes(MAX_CARD)?;
                // A card that does not parse makes the entry malformed
                // (spec §15 chain_neg: bad alg id / bad name → Malformed).
                DeviceCard::parse(c).map_err(|_| Error::Malformed)?;
                (Some(c.to_vec()), None)
            }
            ChainOp::Revoke => (None, Some(r.fix_array::<32>()?)),
        };
        let signer = r.fix_array::<32>()?;
        let sig = r.bytes_exact(SIG_LEN)?.to_vec();
        if !r.at_end() {
            return Err(Error::Malformed);
        }
        let body_len = raw.len() - 4 - SIG_LEN;
        Ok(Self {
            seq,
            prev,
            op,
            card,
            target,
            signer,
            body: raw[..body_len].to_vec(),
            sig,
            raw: raw.to_vec(),
        })
    }

    /// Re-encode to CE bytes. The output hashes to the same value as the
    /// parsed input for well-formed entries.
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(1);
        body.extend_from_slice(&self.seq.to_be_bytes());
        body.extend_from_slice(&self.prev);
        body.push(self.op.tag());
        match self.op {
            ChainOp::Genesis | ChainOp::Add => {
                ce::push_bytes(&mut body, self.card.as_deref().unwrap_or(&[]));
            }
            ChainOp::Revoke => {
                body.extend_from_slice(self.target.as_ref().map_or(&[], |t| t.as_slice()));
            }
        }
        body.extend_from_slice(&self.signer);
        let mut out = body;
        ce::push_bytes(&mut out, &self.sig);
        out
    }
}

/// `entry_hash = H("pbc1 chain hash"; entry)` — the full entry, signature
/// included (spec §4.3).
pub fn entry_hash(raw: &[u8]) -> [u8; 32] {
    crate::h(domain::CHAIN_HASH, raw)
}

/// Build and sign a chain entry (spec §4.2–§4.3).
///
/// `card` is required for Genesis/Add (CE bytes, ≤ 3280); `target` for
/// Revoke. The signature is over the exact `entry_body` bytes with tag
/// `pbc1 chain sig`, using `sign_with_rng` (A5) so vectors are
/// reproducible.
// Mirrors the spec §4.2 entry fields one-to-one, hence the argument count.
#[allow(clippy::too_many_arguments)]
pub fn build_entry<R: CryptoRng>(
    seq: u64,
    prev: [u8; 32],
    op: ChainOp,
    card: Option<Vec<u8>>,
    target: Option<DeviceId>,
    signer: DeviceId,
    sk: &v1::PrivateKey,
    rng: &mut R,
) -> Result<ChainEntry> {
    let op_body = match op {
        ChainOp::Genesis | ChainOp::Add => {
            let c = card.clone().ok_or(Error::Malformed)?;
            if c.len() > MAX_CARD {
                return Err(Error::Malformed);
            }
            let mut b = Vec::with_capacity(4 + c.len());
            ce::push_bytes(&mut b, &c);
            b
        }
        ChainOp::Revoke => {
            let t = target.ok_or(Error::Malformed)?;
            t.to_vec()
        }
    };
    let mut body = Vec::new();
    body.push(1);
    body.extend_from_slice(&seq.to_be_bytes());
    body.extend_from_slice(&prev);
    body.push(op.tag());
    body.extend_from_slice(&op_body);
    body.extend_from_slice(&signer);
    let sig = sign_tagged(sk, domain::CHAIN_SIG, &body, rng)?;
    let mut raw = body.clone();
    ce::push_bytes(&mut raw, &sig);
    Ok(ChainEntry {
        seq,
        prev,
        op,
        card,
        target,
        signer,
        body,
        sig,
        raw,
    })
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

/// Mutable verification state; EMPTY has no genesis and no head.
struct State {
    genesis: Option<[u8; 32]>,
    head: Option<Head>,
    hashes: Vec<[u8; 32]>,
    active: BTreeMap<DeviceId, DeviceCard>,
    revoked: BTreeSet<DeviceId>,
}

impl State {
    fn empty() -> Self {
        Self {
            genesis: None,
            head: None,
            hashes: Vec::new(),
            active: BTreeMap::new(),
            revoked: BTreeSet::new(),
        }
    }

    fn from_verified(v: &VerifiedChain) -> Self {
        Self {
            genesis: Some(v.genesis),
            head: Some(v.head),
            hashes: v.hashes.clone(),
            active: v.active.clone(),
            revoked: v.revoked.clone(),
        }
    }

    fn into_verified(self) -> Result<VerifiedChain> {
        // An EMPTY state with no entries has no genesis; the protocol never
        // verifies an empty chain, so fail closed.
        let (genesis, head) = match (self.genesis, self.head) {
            (Some(g), Some(h)) => (g, h),
            _ => return Err(Error::Malformed),
        };
        Ok(VerifiedChain {
            genesis,
            head,
            hashes: self.hashes,
            active: self.active,
            revoked: self.revoked,
        })
    }
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
    let mut s = match state {
        Some(v) => State::from_verified(&v),
        None => State::empty(),
    };
    for raw in entries {
        let e = ChainEntry::parse(raw)?;
        let h = entry_hash(raw);
        let expected_seq = match s.head {
            None => 0,
            Some(head) => head
                .seq
                .checked_add(1)
                .ok_or(Error::TooLong)?,
        };
        if e.seq != expected_seq {
            return Err(Error::Gap);
        }
        if e.seq == 0 {
            // C1: genesis.
            if e.op != ChainOp::Genesis || e.prev.iter().any(|&b| b != 0) {
                return Err(Error::BadGenesis);
            }
            let card = e.card.as_ref().ok_or(Error::Malformed)?;
            let c = DeviceCard::parse(card).map_err(|_| Error::Malformed)?;
            let id = c.device_id();
            if e.signer != id {
                return Err(Error::BadGenesis);
            }
            let pk = c.public_key()?;
            verify_tagged(&pk, domain::CHAIN_SIG, &e.body, &e.sig)?;
            if let Some(p) = pinned {
                if h != p {
                    return Err(Error::GenesisMismatch);
                }
            }
            s.genesis = Some(h);
            s.active.insert(id, c);
        } else {
            if e.op == ChainOp::Genesis {
                return Err(Error::BadGenesis);
            }
            let head = s.head.expect("non-genesis seq implies a head");
            if e.prev != head.hash {
                return Err(Error::BadPrev);
            }
            // C3: the signer must be active at seq-1.
            let signer_card = s.active.get(&e.signer).ok_or(Error::SignerNotActive)?;
            let pk = signer_card.public_key()?;
            verify_tagged(&pk, domain::CHAIN_SIG, &e.body, &e.sig)?;
            match e.op {
                ChainOp::Genesis => {
                    // Checked above; keep the match exhaustive.
                    return Err(Error::BadGenesis);
                }
                ChainOp::Add => {
                    let card = e.card.as_ref().ok_or(Error::Malformed)?;
                    let c = DeviceCard::parse(card).map_err(|_| Error::Malformed)?;
                    let id = c.device_id();
                    // C4: never re-add an active or revoked device.
                    if s.active.contains_key(&id) || s.revoked.contains(&id) {
                        return Err(Error::ReAdd);
                    }
                    s.active.insert(id, c);
                }
                ChainOp::Revoke => {
                    let t = e.target.ok_or(Error::Malformed)?;
                    // C5/C6.
                    if !s.active.contains_key(&t) {
                        return Err(Error::BadRevoke);
                    }
                    if s.active.len() < 2 {
                        return Err(Error::LastDevice);
                    }
                    s.active.remove(&t);
                    s.revoked.insert(t);
                }
            }
        }
        if s.hashes.len() >= crate::constants::MAX_CHAIN_ENTRIES {
            return Err(Error::TooLong);
        }
        s.hashes.push(h);
        s.head = Some(Head {
            seq: e.seq,
            hash: h,
        });
    }
    s.into_verified()
}

/// Sync from a relay `ChainFetch` batch (spec §4.6): `served` must start at
/// `state.head.seq + 1`; equivalent to `extend(Some(state), served,
/// pinned = Some(state.genesis))`.
pub fn sync(state: &VerifiedChain, served: &[Vec<u8>]) -> Result<VerifiedChain> {
    extend(Some(state.clone()), served, Some(state.genesis))
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
    let chain = if peer_head.seq <= own.head.seq {
        // F2: same seq, different hash → fork.
        if own.hashes[peer_head.seq as usize] != peer_head.hash {
            return Err(Error::Trust);
        }
        own.clone()
    } else {
        let served = fetch(own.head.seq + 1)?;
        let synced = sync(own, &served)?;
        if synced.head.seq < peer_head.seq {
            return Err(Error::StaleChain);
        }
        // F3: relay served up to the peer head but the hash differs.
        if synced.hashes[peer_head.seq as usize] != peer_head.hash {
            return Err(Error::Trust);
        }
        synced
    };
    if !chain.active.contains_key(&peer_id) || !chain.active.contains_key(&self_id) {
        return Err(Error::NotActive);
    }
    Ok(chain)
}
