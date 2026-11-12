//! Handshake: SIGMA / TLS-1.3-shaped (spec §8). Sans-I/O state machine:
//! chain views and relay `fetch` are parameters; the relay-supplied
//! `from` is passed on every inbound message.
//!
//! Pipe message CE (spec §8.2):
//! ```text
//! pipe_msg = u8(1) ‖ u8(type) ‖ body
//! 1 Hs1  body = fix[16](sid) ‖ fix[32](i) ‖ fix[32](r) ‖ bytes(eph_pk,1216) ‖ head(head_I)
//! 2 Hs2  body = fix[16](sid) ‖ bytes(ct,1120) ‖ head(head_R) ‖ bytes(sig_R,3309)
//! 3 Hs3  body = fix[16](sid) ‖ bytes(sig_I,3309)
//! 4 Data body = fix[16](sid) ‖ u64(ctr) ‖ bytes(ct)
//! 5 Close body = fix[16](sid) ‖ u8(reason)
//! ```

use crate::Head;
use crate::chain::VerifiedChain;
use crate::{DeviceId, Result, SessionId};

// ---------------------------------------------------------------------------
// Pipe messages
// ---------------------------------------------------------------------------

/// Hs1: the complete handshake flight 1 (spec §8.2), 1342 bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hs1 {
    pub sid: SessionId,
    /// Initiator DeviceId.
    pub i: DeviceId,
    /// Responder DeviceId.
    pub r: DeviceId,
    /// Initiator's ephemeral KEM public key, 1216 bytes.
    pub eph_pk: Vec<u8>,
    /// Initiator's head.
    pub head: Head,
}

/// Hs2 (spec §8.2). `hs2_tbs` is the pipe_msg up to but excluding
/// `bytes(sig_R)` — 1182 bytes (spec §8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hs2 {
    pub sid: SessionId,
    /// R's KEM ciphertext, 1120 bytes.
    pub ct: Vec<u8>,
    /// Responder's head.
    pub head: Head,
    /// R's signature over th2 (3309 bytes).
    pub sig_r: Vec<u8>,
}

impl Hs2 {
    /// The `hs2_tbs` bytes: `u8(1) ‖ u8(2) ‖ sid ‖ bytes(ct) ‖ head`
    /// (spec §8.2).
    pub fn tbs(&self) -> Vec<u8> {
        todo!()
    }
}

/// Hs3 (spec §8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hs3 {
    pub sid: SessionId,
    /// I's signature over th3 (3309 bytes).
    pub sig_i: Vec<u8>,
}

/// Data (spec §8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Data {
    pub sid: SessionId,
    pub ctr: u64,
    /// AEAD ciphertext, 16 ≤ len ≤ MAX_INNER + 16.
    pub ct: Vec<u8>,
}

/// Close (spec §8.2, §9.3): not authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Close {
    pub sid: SessionId,
    pub reason: u8,
}

/// One pipe message (spec §8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipeMsg {
    Hs1(Hs1),
    Hs2(Hs2),
    Hs3(Hs3),
    Data(Data),
    Close(Close),
}

impl PipeMsg {
    /// Encode to CE bytes with the pipe version byte.
    pub fn encode(&self) -> Vec<u8> {
        todo!()
    }

    /// Parse CE pipe bytes. Version byte ≠ 1 → `UnknownVersion`; unknown
    /// type tag, wrong field lengths, trailing bytes → `Malformed`.
    pub fn parse(raw: &[u8]) -> Result<Self> {
        todo!()
    }
}

// ---------------------------------------------------------------------------
// Transcript (spec §8.3)
// ---------------------------------------------------------------------------

/// `th1 = H("pbc1 hs th1"; hs1)` — over the complete Hs1 pipe_msg bytes.
pub fn transcript_th1(hs1: &[u8]) -> [u8; 32] {
    todo!()
}

/// `th2 = H("pbc1 hs th2"; th1 ‖ hs2_tbs)`.
pub fn transcript_th2(th1: &[u8; 32], hs2_tbs: &[u8]) -> [u8; 32] {
    todo!()
}

/// `th3 = H("pbc1 hs th3"; th2 ‖ bytes(sig_R))`.
pub fn transcript_th3(th2: &[u8; 32], sig_r: &[u8]) -> [u8; 32] {
    todo!()
}

// ---------------------------------------------------------------------------
// Key schedule (spec §8.4)
// ---------------------------------------------------------------------------

/// The two directional session keys (spec §8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionKeys {
    /// Initiator→Responder data key: `hkdf(ss, salt=th3, info="pbc1 key IR")`.
    pub k_ir: [u8; 32],
    /// Responder→Initiator data key: `hkdf(ss, salt=th3, info="pbc1 key RI")`.
    pub k_ri: [u8; 32],
}

/// Derive both session keys from the shared secret and th3 (spec §8.4).
/// The keys commit to `eph_pk`, `ct`, both identities, both heads and
/// `sig_R` through the transcript; they do NOT depend on `sig_I`.
pub fn derive_keys(ss: &[u8; 64], th3: &[u8; 32]) -> SessionKeys {
    todo!()
}

// ---------------------------------------------------------------------------
// State machine (spec §8.6)
// ---------------------------------------------------------------------------

/// The initiator's pending handshake. Holds the ephemeral KEM secret and
/// the transcript until the session is established.
#[derive(Debug)]
pub struct Initiator {
    _private: (),
}

impl Initiator {
    /// I sends Hs1 (spec §8.6): draws `sid` (16 bytes) from `rng`,
    /// generates the ephemeral KEM key pair, fills `head_I = own.head`,
    /// starts the 30 s timer. Returns the state and the Hs1 to send.
    ///
    /// Preconditions: `self_id ∈ own.active`, `peer_id ∈ own.active`
    /// (spec §8.1), else `NotActive`.
    pub fn start<R: rand_core::CryptoRng>(
        own: &VerifiedChain,
        self_id: DeviceId,
        peer_id: DeviceId,
        rng: &mut R,
    ) -> Result<(Self, Hs1)> {
        todo!()
    }

    /// I on Hs2 (spec §8.6 table): parse (done by caller via `PipeMsg`),
    /// `sid` must match (else `Malformed`), relay `from == r` (else
    /// `Misrouted`), `check_head(head_R, r)`, verify `sig_R` under R's key
    /// from I's own chain (else `BadSignature`),
    /// `ss = kem_decapsulate(eph_sk, ct)` (else `DecryptionError`), derive
    /// keys, zeroize `eph_sk`/`ss`, send Hs3. The session is established
    /// for I.
    pub fn on_hs2(
        &mut self,
        relay_from: DeviceId,
        hs2: &Hs2,
        fetch: &dyn Fn(u64) -> Result<Vec<Vec<u8>>>,
    ) -> Result<Hs3> {
        todo!()
    }

    /// The session keys, available after `on_hs2`.
    pub fn keys(&self) -> Result<SessionKeys> {
        todo!()
    }

    /// The session id.
    pub fn sid(&self) -> SessionId {
        todo!()
    }
}

/// The responder's pending handshake (spec §8.6: `Pending{sid, i, th3,
/// ss}` with a 30 s timer). At most one pending handshake per `(i, R)`:
/// a new Hs1 from `i` replaces the pending one; an established session is
/// not touched until the new handshake completes.
#[derive(Debug)]
pub struct Responder {
    _private: (),
}

impl Responder {
    /// R on Hs1 (spec §8.6 table): parse (caller), `r == self` and
    /// `i == relay_from` and `i ≠ self` (else `Misrouted`),
    /// `check_head(head_I, i)` (`Trust`/`StaleChain`/`NotActive`),
    /// `kem_encapsulate(eph_pk, rng)` (else `MalformedKey`), build
    /// `hs2_tbs`, compute `th1, th2, sig_R, th3`, send Hs2.
    pub fn on_hs1<R: rand_core::CryptoRng>(
        own: &VerifiedChain,
        self_id: DeviceId,
        relay_from: DeviceId,
        hs1: &Hs1,
        fetch: &dyn Fn(u64) -> Result<Vec<Vec<u8>>>,
        rng: &mut R,
    ) -> Result<(Self, Hs2)> {
        todo!()
    }

    /// R on Hs3 (spec §8.6 table): `sid` must match the pending one (else
    /// drop: `Malformed`), relay `from == i` (else drop), `i ∈ active`
    /// re-checked against `own` (else `NotActive`), verify `sig_I` under
    /// the active card's key (else `BadSignature`), derive keys,
    /// zeroize `ss`. The session is established; any older established
    /// session for `(i→R)` is closed with reason `Replaced`.
    pub fn on_hs3(
        &mut self,
        own: &VerifiedChain,
        relay_from: DeviceId,
        hs3: &Hs3,
    ) -> Result<SessionKeys> {
        todo!()
    }

    /// The pending session id.
    pub fn sid(&self) -> SessionId {
        todo!()
    }

    /// The pending initiator id.
    pub fn initiator(&self) -> DeviceId {
        todo!()
    }
}
