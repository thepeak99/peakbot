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

use perma_crypto::v1;
use rand_core::CryptoRng;

use crate::ce::{self, Reader};
use crate::chain::VerifiedChain;
use crate::constants;
use crate::domain;
use crate::{DeviceId, Error, Head, Result, SessionId, h, sign_tagged, v1_err, verify_tagged};

const EPH_PK_LEN: usize = 1216;
const CT_LEN: usize = 1120;
const SIG_LEN: usize = 3309;

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
        let mut out = Vec::with_capacity(1 + 1 + 16 + 4 + CT_LEN + 40);
        out.push(constants::PIPE_VERSION);
        out.push(2);
        out.extend_from_slice(&self.sid);
        ce::push_bytes(&mut out, &self.ct);
        out.extend_from_slice(&self.head.ce());
        out
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
        let mut out = vec![constants::PIPE_VERSION];
        match self {
            PipeMsg::Hs1(m) => {
                out.push(1);
                out.extend_from_slice(&m.sid);
                out.extend_from_slice(&m.i);
                out.extend_from_slice(&m.r);
                ce::push_bytes(&mut out, &m.eph_pk);
                out.extend_from_slice(&m.head.ce());
            }
            PipeMsg::Hs2(m) => {
                out.extend_from_slice(&m.tbs()[1..]);
                ce::push_bytes(&mut out, &m.sig_r);
            }
            PipeMsg::Hs3(m) => {
                out.push(3);
                out.extend_from_slice(&m.sid);
                ce::push_bytes(&mut out, &m.sig_i);
            }
            PipeMsg::Data(m) => {
                out.push(4);
                out.extend_from_slice(&m.sid);
                out.extend_from_slice(&m.ctr.to_be_bytes());
                ce::push_bytes(&mut out, &m.ct);
            }
            PipeMsg::Close(m) => {
                out.push(5);
                out.extend_from_slice(&m.sid);
                out.push(m.reason);
            }
        }
        out
    }

    /// Parse CE pipe bytes. Version byte ≠ 1 → `UnknownVersion`; unknown
    /// type tag, wrong field lengths, trailing bytes → `Malformed`.
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let mut r = Reader::new(raw);
        if r.u8()? != constants::PIPE_VERSION {
            return Err(Error::UnknownVersion);
        }
        let msg = match r.u8()? {
            1 => {
                let sid = r.fix_array::<16>()?;
                let i = r.fix_array::<32>()?;
                let rr = r.fix_array::<32>()?;
                let eph_pk = r.bytes_exact(EPH_PK_LEN)?.to_vec();
                let head = Head::parse_ce(r.fix(40)?)?;
                PipeMsg::Hs1(Hs1 {
                    sid,
                    i,
                    r: rr,
                    eph_pk,
                    head,
                })
            }
            2 => {
                let sid = r.fix_array::<16>()?;
                let ct = r.bytes_exact(CT_LEN)?.to_vec();
                let head = Head::parse_ce(r.fix(40)?)?;
                let sig_r = r.bytes_exact(SIG_LEN)?.to_vec();
                PipeMsg::Hs2(Hs2 {
                    sid,
                    ct,
                    head,
                    sig_r,
                })
            }
            3 => {
                let sid = r.fix_array::<16>()?;
                let sig_i = r.bytes_exact(SIG_LEN)?.to_vec();
                PipeMsg::Hs3(Hs3 { sid, sig_i })
            }
            4 => {
                let sid = r.fix_array::<16>()?;
                let ctr = r.u64()?;
                let ct = r.bytes(constants::MAX_INNER + 16)?;
                if ct.len() < 16 {
                    return Err(Error::Malformed);
                }
                PipeMsg::Data(Data {
                    sid,
                    ctr,
                    ct: ct.to_vec(),
                })
            }
            5 => {
                let sid = r.fix_array::<16>()?;
                let reason = r.u8()?;
                PipeMsg::Close(Close { sid, reason })
            }
            // P3.
            _ => return Err(Error::Malformed),
        };
        if !r.at_end() {
            return Err(Error::Malformed);
        }
        Ok(msg)
    }
}

// ---------------------------------------------------------------------------
// Transcript (spec §8.3)
// ---------------------------------------------------------------------------

/// `th1 = H("pbc1 hs th1"; hs1)` — over the complete Hs1 pipe_msg bytes.
pub fn transcript_th1(hs1: &[u8]) -> [u8; 32] {
    h(domain::HS_TH1, hs1)
}

/// `th2 = H("pbc1 hs th2"; th1 ‖ hs2_tbs)`.
pub fn transcript_th2(th1: &[u8; 32], hs2_tbs: &[u8]) -> [u8; 32] {
    let mut m = Vec::with_capacity(32 + hs2_tbs.len());
    m.extend_from_slice(th1);
    m.extend_from_slice(hs2_tbs);
    h(domain::HS_TH2, &m)
}

/// `th3 = H("pbc1 hs th3"; th2 ‖ bytes(sig_R))`.
pub fn transcript_th3(th2: &[u8; 32], sig_r: &[u8]) -> [u8; 32] {
    let mut m = Vec::with_capacity(32 + 4 + sig_r.len());
    m.extend_from_slice(th2);
    ce::push_bytes(&mut m, sig_r);
    h(domain::HS_TH3, &m)
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
    SessionKeys {
        k_ir: hkdf32(ss, th3, domain::KEY_IR),
        k_ri: hkdf32(ss, th3, domain::KEY_RI),
    }
}

fn hkdf32(ikm: &[u8], salt: &[u8], info: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    // A non-empty salt always succeeds; the zero fallback is unreachable.
    if let Ok(v) = v1::hkdf_sha3_256(ikm, salt, info.as_bytes(), 32)
        && v.len() == 32
    {
        out.copy_from_slice(&v);
    }
    out
}

// ---------------------------------------------------------------------------
// State machine (spec §8.6)
// ---------------------------------------------------------------------------

/// The initiator's pending handshake. Holds the ephemeral KEM secret and
/// the transcript until the session is established.
pub struct Initiator {
    /// The (possibly synced) chain view; updated by `on_hs2`.
    chain: VerifiedChain,
    self_id: DeviceId,
    peer_id: DeviceId,
    /// Long-term key: signs `sig_I` (spec §8.6 step 6).
    sk: v1::PrivateKey,
    sid: SessionId,
    eph_sk: Option<v1::KemSecret>,
    th1: [u8; 32],
    keys: Option<SessionKeys>,
}

impl std::fmt::Debug for Initiator {
    // The ephemeral KEM secret has no Debug impl; log presence only.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Initiator")
            .field("chain", &self.chain)
            .field("self_id", &self.self_id)
            .field("peer_id", &self.peer_id)
            .field("sid", &self.sid)
            .field("has_eph_sk", &self.eph_sk.is_some())
            .field("th1", &self.th1)
            .field("keys", &self.keys)
            .finish()
    }
}

impl Initiator {
    /// I sends Hs1 (spec §8.6): draws `sid` (16 bytes) from `rng`,
    /// generates the ephemeral KEM key pair, fills `head_I = own.head`,
    /// starts the 30 s timer. Returns the state and the Hs1 to send.
    ///
    /// Preconditions: `self_id ∈ own.active`, `peer_id ∈ own.active`
    /// (spec §8.1), else `NotActive`.
    pub fn start<R: CryptoRng>(
        own: &VerifiedChain,
        self_id: DeviceId,
        peer_id: DeviceId,
        sk: &v1::PrivateKey,
        rng: &mut R,
    ) -> Result<(Self, Hs1)> {
        if !own.active.contains_key(&self_id) || !own.active.contains_key(&peer_id) {
            return Err(Error::NotActive);
        }
        let mut sid = [0u8; 16];
        rng.fill_bytes(&mut sid);
        let (eph_sk, eph_pk) = v1::kem_generate(v1::KemAlg::X25519MlKem768, rng).map_err(v1_err)?;
        let hs1 = Hs1 {
            sid,
            i: self_id,
            r: peer_id,
            eph_pk,
            head: own.head,
        };
        let th1 = transcript_th1(&PipeMsg::Hs1(hs1.clone()).encode());
        let me = Self {
            chain: own.clone(),
            self_id,
            peer_id,
            sk: sk.clone(),
            sid,
            eph_sk: Some(eph_sk),
            th1,
            keys: None,
        };
        Ok((me, hs1))
    }

    /// I on Hs2 (spec §8.6 table): parse (done by caller via `PipeMsg`),
    /// `sid` must match (else `Malformed`), relay `from == r` (else
    /// `Misrouted`), `check_head(head_R, r)`, verify `sig_R` under R's key
    /// from I's own chain (else `BadSignature`),
    /// `ss = kem_decapsulate(eph_sk, ct)` (else `DecryptionError`), derive
    /// keys, zeroize `eph_sk`/`ss`, send Hs3. The session is established
    /// for I.
    pub fn on_hs2<R: CryptoRng>(
        &mut self,
        relay_from: DeviceId,
        hs2: &Hs2,
        fetch: &dyn Fn(u64) -> Result<Vec<Vec<u8>>>,
        rng: &mut R,
    ) -> Result<Hs3> {
        if hs2.sid != self.sid {
            return Err(Error::Malformed);
        }
        if relay_from != self.peer_id {
            return Err(Error::Misrouted);
        }
        self.chain =
            crate::chain::check_head(&self.chain, self.self_id, hs2.head, self.peer_id, fetch)?;
        let th2 = transcript_th2(&self.th1, &hs2.tbs());
        let card = self
            .chain
            .active
            .get(&self.peer_id)
            .ok_or(Error::NotActive)?;
        let pk = card.public_key()?;
        verify_tagged(&pk, domain::HS_SIG_R, &th2, &hs2.sig_r)?;
        // Drop the ephemeral secret once decapsulated (zeroized on drop).
        let eph_sk = self.eph_sk.take().ok_or(Error::Protocol)?;
        let ss = v1::kem_decapsulate(&eph_sk, &hs2.ct).map_err(|_| Error::DecryptionError)?;
        let th3 = transcript_th3(&th2, &hs2.sig_r);
        self.keys = Some(derive_keys(ss.as_bytes(), &th3));
        // `sign_tagged` (spec §1.3/§8.3): sig_I covers `str(T14) ‖ th3`;
        // the keys do not depend on it.
        let sig_i = sign_tagged(&self.sk, domain::HS_SIG_I, &th3, rng)?;
        Ok(Hs3 {
            sid: self.sid,
            sig_i,
        })
    }

    /// The session keys, available after `on_hs2`.
    pub fn keys(&self) -> Result<SessionKeys> {
        self.keys.ok_or(Error::Protocol)
    }

    /// The session id.
    pub fn sid(&self) -> SessionId {
        self.sid
    }
}

/// The responder's pending handshake (spec §8.6: `Pending{sid, i, th3,
/// ss}` with a 30 s timer). At most one pending handshake per `(i, R)`:
/// a new Hs1 from `i` replaces the pending one; an established session is
/// not touched until the new handshake completes.
#[derive(Debug)]
pub struct Responder {
    sid: SessionId,
    /// The pending initiator id.
    i: DeviceId,
    th3: [u8; 32],
    /// Zeroized on drop; consumed (zeroized) once `on_hs3` establishes.
    ss: Option<v1::SharedSecret>,
}

impl Responder {
    /// R on Hs1 (spec §8.6 table): parse (caller), `r == self` and
    /// `i == relay_from` and `i ≠ self` (else `Misrouted`),
    /// `check_head(head_I, i)` (`Trust`/`StaleChain`/`NotActive`),
    /// `kem_encapsulate(eph_pk, rng)` (else `MalformedKey`), build
    /// `hs2_tbs`, compute `th1, th2, sig_R, th3`, send Hs2.
    pub fn on_hs1<R: CryptoRng>(
        own: &VerifiedChain,
        self_id: DeviceId,
        relay_from: DeviceId,
        hs1: &Hs1,
        fetch: &dyn Fn(u64) -> Result<Vec<Vec<u8>>>,
        sk: &v1::PrivateKey,
        rng: &mut R,
    ) -> Result<(Self, Hs2)> {
        if hs1.r != self_id || hs1.i != relay_from || hs1.i == self_id {
            return Err(Error::Misrouted);
        }
        crate::chain::check_head(own, self_id, hs1.head, hs1.i, fetch)?;
        let (ct, ss) = v1::kem_encapsulate(v1::KemAlg::X25519MlKem768, &hs1.eph_pk, rng)
            .map_err(|_| Error::MalformedKey)?;
        let th1 = transcript_th1(&PipeMsg::Hs1(hs1.clone()).encode());
        // R presents its OWN head (spec §8.2: Hs2 carries `head(head_R)`),
        // so a stale initiator learns R's newer head and syncs (§8.5).
        let hs2_tbs = {
            let mut m = Vec::with_capacity(1 + 1 + 16 + 4 + CT_LEN + 40);
            m.push(constants::PIPE_VERSION);
            m.push(2);
            m.extend_from_slice(&hs1.sid);
            ce::push_bytes(&mut m, &ct);
            m.extend_from_slice(&own.head.ce());
            m
        };
        let th2 = transcript_th2(&th1, &hs2_tbs);
        let sig_r = sign_tagged(sk, domain::HS_SIG_R, &th2, rng)?;
        let th3 = transcript_th3(&th2, &sig_r);
        let hs2 = Hs2 {
            sid: hs1.sid,
            ct,
            head: own.head,
            sig_r,
        };
        let me = Self {
            sid: hs1.sid,
            i: hs1.i,
            th3,
            ss: Some(ss),
        };
        Ok((me, hs2))
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
        if hs3.sid != self.sid || relay_from != self.i {
            return Err(Error::Malformed);
        }
        let card = own.active.get(&self.i).ok_or(Error::NotActive)?;
        let pk = card.public_key()?;
        verify_tagged(&pk, domain::HS_SIG_I, &self.th3, &hs3.sig_i)?;
        // Consume the shared secret: zeroized on drop.
        let ss = self.ss.take().ok_or(Error::Protocol)?;
        Ok(derive_keys(ss.as_bytes(), &self.th3))
    }

    /// The pending session id.
    pub fn sid(&self) -> SessionId {
        self.sid
    }

    /// The pending initiator id.
    pub fn initiator(&self) -> DeviceId {
        self.i
    }
}
