//! Relay protocol codecs (spec §10): envelope, message types, Hello
//! signature. Types/encoding only — no networking.
//!
//! Envelope: each WebSocket binary message is `u8(1) ‖
//! bincode_standard(msg)` with a decode limit (spec §10.1). An unknown
//! first byte is `UnknownVersion`.

use perma_crypto::v1;
use rand_core::CryptoRng;

use crate::ce::push_str;
use crate::constants;
use crate::domain;
use crate::{DeviceId, LetterId, Result, sign_tagged, verify_tagged};

/// Device → relay messages (spec §10.1). Variant index = declaration
/// order.
#[derive(Debug, Clone, PartialEq, Eq, bincode::Encode, bincode::Decode)]
pub enum ToRelay {
    /// Active device: `device` is the DeviceId, `sig` the Hello signature
    /// (§10.2).
    Hello { device: DeviceId, sig: Vec<u8> },
    /// Not-yet-enrolled device: `card` is the CE card bytes (§10.2).
    HelloPending { card: Vec<u8>, sig: Vec<u8> },
    /// Fetch chain entries from `from_seq` (§10.1).
    ChainFetch { from_seq: u64 },
    /// Append a CE chain entry at head+1 (§10.1).
    ChainAppend { entry: Vec<u8> },
    /// Genesis entry (seq 0) for an empty chain (§10.1).
    EnrollGenesis { entry: Vec<u8> },
    /// One pipe_msg to a peer (§8.2).
    Pipe { to: DeviceId, bytes: Vec<u8> },
    /// A sealed letter to a peer (§7).
    Post {
        to: DeviceId,
        letter_id: LetterId,
        sealed: Vec<u8>,
    },
    /// Acknowledge a letter (§7.5 step 8).
    Ack { letter_id: LetterId },
}

/// Relay → device messages (spec §10.1). Variant index = declaration
/// order.
#[derive(Debug, Clone, PartialEq, Eq, bincode::Encode, bincode::Decode)]
pub enum FromRelay {
    /// Per-connection challenge nonce, 32 fresh CSPRNG bytes (§10.1).
    Challenge { nonce: [u8; 32] },
    /// Greeting after Hello. `head_seq: None` = empty chain.
    Welcome {
        head_seq: Option<u64>,
        pending: bool,
    },
    /// A batch of CE chain entries, ≤ 256 per message.
    Chain { entries: Vec<Vec<u8>> },
    /// The chain head moved to `head_seq`.
    ChainChanged { head_seq: u64 },
    /// One pipe_msg from a peer; `from` is set by the relay from the
    /// authenticated Hello (§10.3).
    Pipe { from: DeviceId, bytes: Vec<u8> },
    /// A sealed letter; routing fields set by the relay.
    Letter {
        from: DeviceId,
        letter_id: LetterId,
        sealed: Vec<u8>,
    },
    /// The recipient is not connected.
    Unreachable { to: DeviceId },
    /// Relay error code (§10.1).
    Error { code: u16 },
}

/// Encode a device→relay envelope: `u8(1) ‖ bincode_standard(msg)`
/// (spec §10.1).
pub fn encode_to_relay(msg: &ToRelay) -> Vec<u8> {
    let mut out = vec![constants::PROTOCOL_VERSION];
    let body = bincode::encode_to_vec(msg, bincode::config::standard()).unwrap_or_default();
    out.extend_from_slice(&body);
    out
}

/// Decode a device→relay envelope. First byte ≠ 1 → `UnknownVersion`;
/// decode limit violations → `Malformed`.
pub fn decode_to_relay(bytes: &[u8]) -> Result<ToRelay> {
    let body = decode_envelope(bytes)?;
    let (msg, _) = bincode::decode_from_slice(
        body,
        bincode::config::standard().with_limit::<{ constants::MAX_ENVELOPE_PIPE }>(),
    )
    .map_err(|_| crate::Error::Malformed)?;
    Ok(msg)
}

/// Encode a relay→device envelope (spec §10.1).
pub fn encode_from_relay(msg: &FromRelay) -> Vec<u8> {
    let mut out = vec![constants::PROTOCOL_VERSION];
    let body = bincode::encode_to_vec(msg, bincode::config::standard()).unwrap_or_default();
    out.extend_from_slice(&body);
    out
}

/// Decode a relay→device envelope (spec §10.1).
pub fn decode_from_relay(bytes: &[u8]) -> Result<FromRelay> {
    let body = decode_envelope(bytes)?;
    let (msg, _) = bincode::decode_from_slice(
        body,
        bincode::config::standard().with_limit::<{ constants::MAX_ENVELOPE_CHAIN }>(),
    )
    .map_err(|_| crate::Error::Malformed)?;
    Ok(msg)
}

/// Strip and check the version byte (spec §10.1, V1: exact match).
fn decode_envelope(bytes: &[u8]) -> Result<&[u8]> {
    let (first, rest) = bytes.split_first().ok_or(crate::Error::Malformed)?;
    if *first != constants::PROTOCOL_VERSION {
        return Err(crate::Error::UnknownVersion);
    }
    Ok(rest)
}

/// The Hello signing input (spec §10.2):
/// `str(origin) ‖ fix[32](nonce) ‖ fix[32](device_id) ‖ u8(mode)` with
/// `mode`: 0 = Hello, 1 = HelloPending.
pub fn hello_tbs(origin: &str, nonce: [u8; 32], device_id: DeviceId, mode: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + origin.len() + 32 + 32 + 1);
    push_str(&mut out, origin);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&device_id);
    out.push(mode);
    out
}

/// Sign the Hello tbs with tag `pbc1 relay hello` (spec §10.2).
/// `sign_with_rng` (A5) for reproducible vectors.
pub fn sign_hello<R: CryptoRng>(sk: &v1::PrivateKey, tbs: &[u8], rng: &mut R) -> Result<Vec<u8>> {
    sign_tagged(sk, domain::RELAY_HELLO, tbs, rng)
}

/// Verify a Hello signature under the card's key (spec §10.2).
pub fn verify_hello(pk: &v1::PublicKey, tbs: &[u8], sig: &[u8]) -> Result<()> {
    verify_tagged(pk, domain::RELAY_HELLO, tbs, sig)
}
