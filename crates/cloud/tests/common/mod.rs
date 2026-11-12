//! Shared fixtures for the pbc1 RED tests (spec §15.1: deterministic
//! seeds A = 0x01×64, B = 0x02×64, C = 0x03×64, D = 0x04×64; fixed
//! `now = 1 800 000 000`).
#![allow(dead_code)]

use cloud::Head;
use cloud::card::DeviceCard;
use cloud::chain::{self, ChainOp, VerifiedChain, build_entry};
use perma_crypto::v1;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

/// Fixed `now` for all letter/time tests (spec §15.1).
pub const NOW: u64 = 1_800_000_000;

/// Long-term seed for device n (1 = A, 2 = B, 3 = C, 4 = D).
pub fn seed(n: u8) -> v1::SecretSeed {
    v1::SecretSeed::from_bytes([n; 64])
}

/// Deterministic key pair from the fixed seed (v1 is implemented; this
/// works today).
pub fn keypair(n: u8) -> v1::KeyPair {
    v1::KeyPair::from_seed(&seed(n)).expect("deterministic keygen")
}

/// A card for device n. Goes through the (skeleton) card API.
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

/// Freshly seeded per-role RNG (spec §15.1): A = 0xA1, B = 0xB2,
/// C = 0xC3, attacker = 0xD4.
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
    target: Option<[u8; 32]>,
    signed_by: &DeviceCard,
) -> Vec<u8> {
    build_entry(
        seq,
        prev,
        op,
        c.map(|c| c.encode()),
        target,
        signed_by.device_id(),
        keypair(signer_n).private(),
        &mut rng(0xA1),
    )
    .expect("build entry")
    .encode()
}

/// A verified two-device chain: A (genesis) + B (added by A).
pub fn chain_ab() -> VerifiedChain {
    let a = card_a();
    let b = card_b();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let h0 = chain::entry_hash(&e0);
    let e1 = entry(1, 1, h0, ChainOp::Add, Some(&b), None, &a);
    chain::extend(None, &[e0, e1], None).expect("chain verifies")
}

pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

pub fn hex32(b: [u8; 32]) -> String {
    hex(&b)
}

/// Independent base64url (RFC 4648 §5) of 32 bytes → 43 chars, no
/// padding, canonical (low 2 bits of the last character zero). Written
/// independently of the crate so link tests check the crate's codec.
pub fn b64url32(b: &[u8; 32]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(43);
    for c in 0..43u32 {
        if c == 42 {
            // Last char: the final 4 bits (low nibble of b[31]) shifted
            // left 2; the unused low 2 bits stay zero (RFC 4648).
            out.push(A[((b[31] & 0x0F) << 2) as usize] as char);
            break;
        }
        let bit = (c * 6) % 8;
        let byte = (c * 6) / 8;
        let v = ((b[byte as usize] as u16) << 8 | b[byte as usize + 1] as u16) >> (10 - bit);
        out.push(A[(v & 0x3F) as usize] as char);
    }
    out
}

/// The link/head/secret values used by the link tests.
pub const LINK_RELAY: &str = "https://relay.example";
pub const LINK_DEV: [u8; 32] = [0x11; 32];
pub const LINK_SECRET: [u8; 32] = [0x22; 32];
pub fn link_head() -> Head {
    Head {
        seq: 42,
        hash: [0x33; 32],
    }
}

/// The relay origin as it appears in the link: percent-encoded per the
/// spec §6.1 ABNF (`relay = 1*( unreserved / pct-encoded )`) — raw `:`
/// and `/` are not unreserved, so a raw origin is a BadLink.
pub fn encoded_relay() -> String {
    LINK_RELAY.replace(':', "%3A").replace('/', "%2F")
}

pub fn good_link() -> String {
    format!(
        "peakbot-cloud:v1?relay={}&dev={dev}&head=42:{h}&s={s}",
        encoded_relay(),
        dev = b64url32(&LINK_DEV),
        h = b64url32(&link_head().hash),
        s = b64url32(&LINK_SECRET),
    )
}

/// The genesis entry of device A's chain (seq 0, self-signed).
pub fn genesis_a() -> Vec<u8> {
    let a = card_a();
    entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a)
}
