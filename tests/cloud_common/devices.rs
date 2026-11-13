//! Deterministic test devices, mirroring `crates/cloud/tests/common`
//! (spec §15.1): seed A = 0x01×64, B = 0x02×64, C = 0x03×64; seeded
//! ChaCha20Rng per role.

use cloud::{
    DeviceId, Head, LetterId,
    card::DeviceCard,
    chain::{self, ChainEntry, ChainOp, build_entry},
    letter::{self, LetterBody, LetterKind, LetterKindBody},
};
use perma_crypto::v1;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

/// Fixed `now` (spec §15.1 style).
pub const NOW: u64 = 1_800_000_000;

/// Long-term seed for device n.
pub fn seed(n: u8) -> v1::SecretSeed {
    v1::SecretSeed::from_bytes([n; 64])
}

/// Deterministic key pair from the fixed seed.
pub fn keypair(n: u8) -> v1::KeyPair {
    v1::KeyPair::from_seed(&seed(n)).expect("deterministic keygen")
}

/// A card for device n.
pub fn card(n: u8, name: &str, platform: u8) -> DeviceCard {
    DeviceCard::new(&keypair(n), name, platform).expect("card build")
}

pub fn card_a() -> DeviceCard {
    card(1, "Laptop", 1)
}
pub fn card_b() -> DeviceCard {
    card(2, "Desktop", 1)
}
pub fn card_c() -> DeviceCard {
    card(3, "Pixel 9", 4)
}

pub fn id_a() -> DeviceId {
    card_a().device_id()
}
pub fn id_b() -> DeviceId {
    card_b().device_id()
}
pub fn id_c() -> DeviceId {
    card_c().device_id()
}

/// Freshly seeded per-role RNG.
pub fn rng(byte: u8) -> ChaCha20Rng {
    ChaCha20Rng::from_seed([byte; 32])
}

/// Build and CE-encode a chain entry signed by device `signer_n`.
pub fn entry(
    signer_n: u8,
    seq: u64,
    prev: [u8; 32],
    op: ChainOp,
    c: Option<&DeviceCard>,
    target: Option<DeviceId>,
) -> Vec<u8> {
    build_entry(
        seq,
        prev,
        op,
        c.map(|c| c.encode()),
        target,
        card(signer_n, "s", 1).device_id(),
        keypair(signer_n).private(),
        &mut rng(0xA1),
    )
    .expect("build entry")
    .encode()
}

/// The genesis entry of device A (seq 0, self-signed).
pub fn genesis_a() -> Vec<u8> {
    entry(1, 0, [0; 32], ChainOp::Genesis, Some(&card_a()), None)
}

/// A `Add` entry for `added` at `seq` after `prev`, signed by `signer_n`.
pub fn add_entry(signer_n: u8, seq: u64, prev: [u8; 32], added: &DeviceCard) -> Vec<u8> {
    entry(signer_n, seq, prev, ChainOp::Add, Some(added), None)
}

/// A `Revoke` entry for `target` at `seq` after `prev`, signed by `signer_n`.
pub fn revoke_entry(signer_n: u8, seq: u64, prev: [u8; 32], target: DeviceId) -> Vec<u8> {
    entry(signer_n, seq, prev, ChainOp::Revoke, None, Some(target))
}

/// The head of a chain of raw entries (`[0;32]` hash for empty).
pub fn head_of(entries: &[Vec<u8>]) -> Head {
    if entries.is_empty() {
        return Head {
            seq: 0,
            hash: [0; 32],
        };
    }
    let last = entries.last().expect("non-empty");
    let e = ChainEntry::parse(last).expect("parse");
    Head {
        seq: e.seq,
        hash: chain::entry_hash(last),
    }
}

/// b64url without padding.
pub fn b64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A fresh random letter id.
pub fn letter_id(n: u64) -> LetterId {
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&n.to_be_bytes());
    id
}

/// Forge a sealed `EnrollRequest` letter from device `from_n` to `to_id`
/// carrying `head_n` (the test seam for the stale-head / SAS cases; the
/// relay delivers it via `inject_letter`).
pub fn forge_enroll_request(
    from_n: u8,
    to_id: DeviceId,
    head_n: &Head,
    id: LetterId,
    now: u64,
) -> Vec<u8> {
    let from = card(from_n, "f", 1);
    let body = LetterBody {
        letter_id: id,
        from: from.device_id(),
        to: to_id,
        created_at: now,
        kind: LetterKind::EnrollRequest,
        body: LetterKindBody::EnrollRequest {
            card: from.encode(),
            head_n: *head_n,
            has_mac: false,
            mac: None,
        },
    };
    let signed =
        letter::signed_letter(&body, keypair(from_n).private(), &mut rng(0xC3)).expect("sign");
    let aad = letter::letter_aad(id, from.device_id(), to_id);
    // Seal to the recipient's KEM key: the recipient is whichever card's
    // DeviceId matches `to_id` (A=1, B=2, C=3 in the tests).
    let recipient = if to_id == id_a() {
        1
    } else if to_id == id_b() {
        2
    } else {
        3
    };
    let pk = card(recipient, "x", 1).public_key().expect("pk");
    letter::seal_letter(&pk, &signed, &aad, &mut rng(0xC3)).expect("seal")
}

/// b64url (no padding) decode length, without decoding: `-`/`_` and
/// A–Z a–z 0–9; returns the decoded byte length (None-ish → usize::MAX on
/// bad input).
pub fn b64url_decode_len(s: &str) -> usize {
    let valid = s
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'-' | b'_'));
    if !valid {
        return usize::MAX;
    }
    let n = s.len();
    let rem = n % 4;
    (n * 3) / 4
        - if rem == 2 {
            1
        } else if rem == 3 {
            0
        } else {
            0
        }
}
